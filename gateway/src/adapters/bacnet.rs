//! BACnet/IP: the object and property a building's controllers are read and written by. It talks to
//! one device at a known address, and does not listen for Who-Is: the devices of a site are known.
//!
//! `bacnet://10.0.0.7:47808`

use crate::adapters::tags::{ANSWER, Tags, Trouble, host_port, number, query, yes};
use async_trait::async_trait;
use ferrum_edge_contract::tags::BacnetAddress;
use serde_json::{Value, json};
use tokio::net::UdpSocket;
use tokio::time::timeout;

const READ_PROPERTY: u8 = 12;
const WRITE_PROPERTY: u8 = 15;

pub struct Bacnet {
    socket: UdpSocket,
    invoke: u8,
    /// Who a write speaks as: 16 is the lowest in the priority array, and the default.
    priority: Option<u8>,
}

fn tag(number: u8, length: usize, out: &mut Vec<u8>) {
    out.push(number << 4 | 0x08 | length as u8);
}

/// The object and property every request starts with.
fn address(a: &BacnetAddress, out: &mut Vec<u8>) {
    tag(0, 4, out);
    out.extend(((u32::from(a.object_type) << 22) | (a.instance & 0x3f_ffff)).to_be_bytes());
    if a.property < 256 {
        tag(1, 1, out);
        out.push(a.property as u8);
    } else {
        tag(1, 2, out);
        out.extend((a.property as u16).to_be_bytes());
    }
}

/// An application-tagged value, read from the front of `b`.
pub fn value(b: &[u8]) -> Option<(Value, usize)> {
    let first = *b.first()?;
    if first & 0x08 != 0 {
        return None;
    }
    let kind = first >> 4;
    let (mut start, mut length) = (1, usize::from(first & 7));
    if kind == 1 {
        return Some((json!(length != 0), 1));
    }
    if length == 5 {
        length = usize::from(*b.get(1)?);
        start = 2;
    }
    let body = b.get(start..start + length)?;
    let unsigned = || body.iter().fold(0u64, |n, c| n << 8 | u64::from(*c));
    let read = match kind {
        0 => Value::Null,
        2 | 9 => json!(unsigned()),
        3 => {
            let shift = 64 - length * 8;
            json!(((unsigned() << shift) as i64) >> shift)
        }
        4 => json!(f32::from_be_bytes(body.try_into().ok()?)),
        5 => json!(f64::from_be_bytes(body.try_into().ok()?)),
        7 => json!(String::from_utf8_lossy(body.get(1..)?)),
        _ => json!(hex::encode(body)),
    };
    Some((read, start + length))
}

/// A value in the form its property expects: what is binary is enumerated, the rest is real.
fn encoded(a: &BacnetAddress, v: &Value, out: &mut Vec<u8>) -> Result<(), Trouble> {
    match a.object_type {
        3..=5 => out.extend([0x91, u8::from(yes(v))]),
        13 | 14 | 19 => {
            out.push(0x21);
            out.push(number(v)? as u8);
        }
        _ => {
            out.push(0x44);
            out.extend((number(v)? as f32).to_be_bytes());
        }
    }
    Ok(())
}

impl Bacnet {
    pub async fn connect(rest: &str) -> Result<Box<dyn Tags>, String> {
        let (host, port) = host_port(rest, 47808);
        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
        socket.connect((host.as_str(), port)).await.map_err(|e| format!("{host}:{port}: {e}"))?;
        Ok(Box::new(Bacnet { socket, invoke: 0, priority: query(rest, "priority").and_then(|p| p.parse().ok()) }))
    }

    async fn ask(&mut self, service: u8, body: &[u8]) -> Result<Vec<u8>, Trouble> {
        self.invoke = self.invoke.wrapping_add(1);
        let mut apdu = vec![0x00, 0x05, self.invoke, service];
        apdu.extend(body);
        let mut frame = vec![0x81, 0x0a];
        frame.extend(((apdu.len() + 6) as u16).to_be_bytes());
        frame.extend([0x01, 0x04]);
        frame.extend(&apdu);
        // UDP loses things, and asking again is harmless: a repeat of a write sets the same value.
        for _ in 0..3 {
            self.socket.send(&frame).await?;
            let mut buffer = [0u8; 1500];
            let Ok(got) = timeout(ANSWER / 3, self.socket.recv(&mut buffer)).await else { continue };
            let n = got?;
            let b = &buffer[..n];
            if b.len() < 8 || b[0] != 0x81 {
                continue;
            }
            // The network layer can say where a message came through, which is not of interest here.
            let control = b[5];
            let mut at = 6;
            if control & 0x20 != 0 {
                at += 3 + usize::from(*b.get(at + 2).unwrap_or(&0));
            }
            if control & 0x08 != 0 {
                at += 3 + usize::from(*b.get(at + 2).unwrap_or(&0));
            }
            if control & 0x20 != 0 {
                at += 1;
            }
            let apdu = b.get(at..).unwrap_or_default();
            match apdu.first().map(|t| t >> 4) {
                Some(2) if apdu.get(1) == Some(&self.invoke) => return Ok(Vec::new()),
                Some(3) if apdu.get(1) == Some(&self.invoke) => return Ok(apdu.get(3..).unwrap_or_default().to_vec()),
                Some(5) if apdu.get(1) == Some(&self.invoke) => {
                    let rest = apdu.get(3..).unwrap_or_default();
                    let (class, used) = value(rest).unwrap_or((Value::Null, 0));
                    let code = value(&rest[used..]).map_or(Value::Null, |v| v.0);
                    return Err(Trouble::Refused(format!("it refused: error class {class}, code {code}")));
                }
                Some(6 | 7) if apdu.get(1) == Some(&self.invoke) => return Err(Trouble::Refused("it rejected that".into())),
                _ => {}
            }
        }
        Err(Trouble::Lost("it did not answer".into()))
    }
}

#[async_trait]
impl Tags for Bacnet {
    async fn read(&mut self, tag_text: &str) -> Result<Value, Trouble> {
        let a = BacnetAddress::parse(tag_text).map_err(Trouble::Refused)?;
        let mut body = Vec::new();
        address(&a, &mut body);
        let answer = self.ask(READ_PROPERTY, &body).await?;
        // The object and property again, then the value between two context tags.
        let open = answer.iter().position(|b| *b == 0x3e).ok_or_else(|| Trouble::Lost("it sent no value".into()))?;
        value(&answer[open + 1..]).map(|v| v.0).ok_or_else(|| Trouble::Refused("this cannot read that value".into()))
    }

    async fn write(&mut self, tag_text: &str, v: &Value) -> Result<(), Trouble> {
        let a = BacnetAddress::parse(tag_text).map_err(Trouble::Refused)?;
        let mut body = Vec::new();
        address(&a, &mut body);
        body.push(0x3e);
        encoded(&a, v, &mut body)?;
        body.push(0x3f);
        if let Some(priority) = self.priority {
            body.extend([0x49, priority]);
        }
        self.ask(WRITE_PROPERTY, &body).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_names_an_object_in_22_bits() {
        let mut out = Vec::new();
        address(&BacnetAddress { object_type: 2, instance: 3, property: 85 }, &mut out);
        assert_eq!(out, [0x0c, 0x00, 0x80, 0x00, 0x03, 0x19, 0x55]);
    }

    #[test]
    fn values_say_what_they_are() {
        assert_eq!(value(&[0x44, 0x41, 0xc8, 0, 0]), Some((json!(25.0), 5)));
        assert_eq!(value(&[0x11]), Some((json!(true), 1)));
        assert_eq!(value(&[0x10]), Some((json!(false), 1)));
        assert_eq!(value(&[0x91, 1]), Some((json!(1), 2)));
        assert_eq!(value(&[0x21, 200]), Some((json!(200), 2)));
        assert_eq!(value(&[0x31, 0xff]), Some((json!(-1), 2)));
        assert_eq!(value(&[0x75, 3, 0, b'h', b'i']), Some((json!("hi"), 5)));
        assert_eq!(value(&[]), None);
    }
}
