//! KNX over IP, through a tunnelling interface: group addresses read and written, and whatever the
//! bus says arriving as it happens. What a group address holds is not in the protocol, so `subscribe`
//! says, as `1/2/3:dpt9`, and a step says the same in its `node`.
//!
//! `knx://interface:3671`

use crate::adapter::Failure;
use crate::adapters::tags::host_port;
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Instant, timeout};

const CONNECT: u16 = 0x0205;
const CONNECTED: u16 = 0x0206;
const STATE: u16 = 0x0207;
const STATE_ANSWER: u16 = 0x0208;
const DISCONNECT: u16 = 0x0209;
const TUNNEL: u16 = 0x0420;
const TUNNEL_ACK: u16 = 0x0421;
/// cEMI: a request from here, and what the bus said.
const L_DATA_REQ: u8 = 0x11;
const L_DATA_IND: u8 = 0x29;
const WRITE: u8 = 0x80;
const ANSWER: u8 = 0x40;
/// A tunnel lapses after two minutes of saying nothing.
const ALIVE: Duration = Duration::from_secs(60);

fn frame(service: u16, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0x06, 0x10];
    out.extend(service.to_be_bytes());
    out.extend(((body.len() + 6) as u16).to_be_bytes());
    out.extend(body);
    out
}

/// `1/2/3` as two bytes.
pub fn group(text: &str) -> Option<u16> {
    let parts: Vec<u16> = text.split('/').map(str::parse).collect::<Result<_, _>>().ok()?;
    let [main, middle, sub] = parts[..] else { return None };
    (main < 32 && middle < 8 && sub < 256).then_some(main << 11 | middle << 8 | sub)
}

pub fn group_text(address: u16) -> String {
    format!("{}/{}/{}", address >> 11, (address >> 8) & 7, address & 0xff)
}

/// KNX's own two-byte float.
pub fn float16(b: [u8; 2]) -> f64 {
    let n = u16::from_be_bytes(b);
    let exponent = (n >> 11) & 0xf;
    let mantissa = i32::from(n & 0x7ff) - if n & 0x8000 != 0 { 2048 } else { 0 };
    0.01 * f64::from(mantissa) * f64::from(1u32 << exponent)
}

pub fn float16_of(value: f64) -> [u8; 2] {
    let mut mantissa = value * 100.0;
    let mut exponent = 0u16;
    while !(-2048.0..2047.5).contains(&mantissa) && exponent < 15 {
        mantissa /= 2.0;
        exponent += 1;
    }
    let m = (mantissa.round() as i32).clamp(-2048, 2047);
    let n = (u16::from(m < 0) << 15) | (exponent << 11) | (m as u16 & 0x7ff);
    n.to_be_bytes()
}

/// A value as a datapoint type has it. Only the first number matters here: 9.001 and 9.004 differ in
/// meaning and not in form.
pub fn decode(dpt: &str, small: u8, b: &[u8]) -> Option<Value> {
    Some(match dpt.split('.').next()? {
        "1" => json!(small & 1 == 1),
        "5" if dpt == "5.001" => json!(f64::from(*b.first()?) * 100.0 / 255.0),
        "5" => json!(*b.first()?),
        "6" => json!(*b.first()? as i8),
        "7" => json!(u16::from_be_bytes(b.get(..2)?.try_into().ok()?)),
        "8" => json!(i16::from_be_bytes(b.get(..2)?.try_into().ok()?)),
        "9" => json!((float16(b.get(..2)?.try_into().ok()?) * 100.0).round() / 100.0),
        "12" => json!(u32::from_be_bytes(b.get(..4)?.try_into().ok()?)),
        "13" => json!(i32::from_be_bytes(b.get(..4)?.try_into().ok()?)),
        "14" => json!(f32::from_be_bytes(b.get(..4)?.try_into().ok()?)),
        _ => return None,
    })
}

/// What goes after the APCI: a small value in its last six bits, or bytes.
pub fn encode(dpt: &str, value: &Value) -> Result<(u8, Vec<u8>), String> {
    let number = || {
        value
            .as_f64()
            .or_else(|| value.as_str()?.parse().ok())
            .or_else(|| value.as_bool().map(|b| f64::from(u8::from(b))))
            .ok_or(format!("{value} is not a number"))
    };
    Ok(match dpt.split('.').next().unwrap_or_default() {
        "1" => (u8::from(number()? != 0.0), vec![]),
        "5" if dpt == "5.001" => (0, vec![(number()? * 255.0 / 100.0).round() as u8]),
        "5" => (0, vec![number()? as u8]),
        "6" => (0, vec![number()? as i8 as u8]),
        "7" => (0, (number()? as u16).to_be_bytes().to_vec()),
        "8" => (0, (number()? as i16).to_be_bytes().to_vec()),
        "9" => (0, float16_of(number()?).to_vec()),
        "12" => (0, (number()? as u32).to_be_bytes().to_vec()),
        "13" => (0, (number()? as i32).to_be_bytes().to_vec()),
        "14" => (0, (number()? as f32).to_be_bytes().to_vec()),
        other => return Err(format!("this does not write datapoint type {other}")),
    })
}

/// `1/2/3:dpt9` as the address and the type.
fn split(node: &str) -> Result<(u16, String), String> {
    let (address, dpt) = node.split_once(":dpt").ok_or_else(|| format!("{node}: say what it holds, as 1/2/3:dpt9"))?;
    Ok((group(address).ok_or_else(|| format!("{address} is not a group address"))?, dpt.to_string()))
}

fn cemi(address: u16, apci: u8, small: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![L_DATA_REQ, 0, 0xbc, 0xe0, 0, 0];
    out.extend(address.to_be_bytes());
    out.push((data.len() + 1) as u8);
    out.push(0);
    out.push(apci | (small & 0x3f));
    out.extend(data);
    out
}

pub struct Knx {
    pub address: String,
    pub watch: Vec<String>,
    socket: Mutex<Option<(Arc<UdpSocket>, u8)>>,
    sequence: AtomicU8,
}

impl Knx {
    pub fn new(rest: &str, watch: &[String]) -> Knx {
        let (host, port) = host_port(rest, 3671);
        Knx { address: format!("{host}:{port}"), watch: watch.to_vec(), socket: Mutex::new(None), sequence: AtomicU8::new(0) }
    }
}

const NOBODY: [u8; 8] = [8, 1, 0, 0, 0, 0, 0, 0];

#[async_trait]
impl Transport for Knx {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?);
        socket.connect(&self.address).await.map_err(|e| format!("{}: {e}", self.address))?;
        let request = [&NOBODY[..], &NOBODY[..], &[4, 4, 2, 0]].concat();
        socket.send(&frame(CONNECT, &request)).await.map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 512];
        let n = timeout(Duration::from_secs(5), socket.recv(&mut buffer)).await.map_err(|_| "it did not answer")?.map_err(|e| e.to_string())?;
        if n < 8 || u16::from_be_bytes([buffer[2], buffer[3]]) != CONNECTED || buffer[7] != 0 {
            return Err(format!("it refused a tunnel: {:#x}", buffer.get(7).copied().unwrap_or(0)));
        }
        let channel = buffer[6];
        self.sequence.store(0, Ordering::Relaxed);
        *self.socket.lock().await = Some((socket.clone(), channel));
        let _ = ready.send(()).await;
        let types: HashMap<u16, String> = self.watch.iter().filter_map(|w| split(w).ok()).collect();
        let mut next_check = Instant::now() + ALIVE;
        let mut last_heard: Option<u8> = None;
        loop {
            let got = tokio::select! {
                () = tx.closed() => {
                    let _ = socket.send(&frame(DISCONNECT, &[&[channel, 0][..], &NOBODY[..]].concat())).await;
                    return Ok(());
                }
                got = timeout(next_check.saturating_duration_since(Instant::now()), socket.recv(&mut buffer)) => got,
            };
            let Ok(got) = got else {
                socket.send(&frame(STATE, &[&[channel, 0][..], &NOBODY[..]].concat())).await.map_err(|e| e.to_string())?;
                next_check = Instant::now() + ALIVE;
                continue;
            };
            let n = got.map_err(|e| e.to_string())?;
            let b = &buffer[..n];
            if n < 6 {
                continue;
            }
            match u16::from_be_bytes([b[2], b[3]]) {
                TUNNEL if n >= 21 => {
                    let sequence = b[8];
                    socket.send(&frame(TUNNEL_ACK, &[4, channel, sequence, 0])).await.map_err(|e| e.to_string())?;
                    // A repeat is the interface not having heard the ack, and has been seen already.
                    if last_heard == Some(sequence) {
                        continue;
                    }
                    last_heard = Some(sequence);
                    let cemi = &b[10..];
                    if cemi[0] != L_DATA_IND {
                        continue;
                    }
                    let skip = 2 + usize::from(cemi[1]);
                    let frame = cemi.get(skip..).unwrap_or_default();
                    if frame.len() < 9 || frame[1] & 0x80 == 0 {
                        continue;
                    }
                    let address = u16::from_be_bytes([frame[4], frame[5]]);
                    let (apci, small) = (frame[8] & 0xc0, frame[8] & 0x3f);
                    if apci != WRITE && apci != ANSWER {
                        continue;
                    }
                    let value = types.get(&address).and_then(|dpt| decode(dpt, small, &frame[9..]));
                    let body = value.map_or_else(|| json!({"raw": hex::encode(&frame[9..]), "small": small}), |v| json!({"value": v}));
                    if tx.send(Message { topic: group_text(address), body: body.to_string().into_bytes(), status: None, from: None }).await.is_err() {
                        return Ok(());
                    }
                }
                STATE_ANSWER if b.get(7).is_some_and(|s| *s != 0) => return Err("the interface has forgotten this tunnel".into()),
                DISCONNECT => return Err("the interface hung up".into()),
                _ => {}
            }
        }
    }

    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        let Out::Node { node, value } = out else { return Err(Failure::NotSent("KNX takes a group address".into())) };
        let (address, dpt) = split(&node).map_err(Failure::NotSent)?;
        let (socket, channel) = self.socket.lock().await.clone().ok_or_else(|| Failure::NotSent("not connected".into()))?;
        let message = match value {
            // What answers comes back as anything the bus says does.
            None => cemi(address, 0, 0, &[]),
            Some(v) => {
                let (small, data) = encode(&dpt, &v).map_err(Failure::NotSent)?;
                cemi(address, WRITE, small, &data)
            }
        };
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let body = [&[4, channel, sequence, 0][..], &message].concat();
        socket.send(&frame(TUNNEL, &body)).await.map_err(|e| Failure::NotSent(e.to_string()))?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_addresses_are_three_levels() {
        assert_eq!(group("1/2/3"), Some(0x0a03));
        assert_eq!(group_text(0x0a03), "1/2/3");
        assert_eq!(group("32/0/0"), None);
        assert_eq!(group("1/2"), None);
    }

    #[test]
    fn a_two_byte_float_is_as_knx_has_it() {
        // Examples from the KNX datapoint types: 0.01 × M × 2^E.
        assert!((float16([0x0c, 0x1a]) - 21.0).abs() < 0.2, "{}", float16([0x0c, 0x1a]));
        assert_eq!(float16([0x00, 0x00]), 0.0);
        assert_eq!(float16([0x87, 0x9c]), -1.0);
        for value in [0.0, 21.5, -1.0, 100.0, 6700.0, -300.0] {
            let there_and_back = float16(float16_of(value));
            assert!((there_and_back - value).abs() <= value.abs() * 0.01 + 0.01, "{value} became {there_and_back}");
        }
    }

    #[test]
    fn values_by_their_type() {
        assert_eq!(decode("1", 1, &[]), Some(json!(true)));
        assert_eq!(decode("5.001", 0, &[255]), Some(json!(100.0)));
        assert_eq!(decode("14.056", 0, &0.5f32.to_be_bytes()), Some(json!(0.5)));
        assert_eq!(decode("2", 0, &[]), None);
        assert_eq!(encode("1", &json!(true)).unwrap(), (1, vec![]));
        assert_eq!(encode("5.001", &json!(100)).unwrap(), (0, vec![255]));
        assert!(encode("99", &json!(1)).is_err());
        assert_eq!(cemi(0x0a03, WRITE, 1, &[]), [0x11, 0, 0xbc, 0xe0, 0, 0, 0x0a, 0x03, 1, 0, 0x81]);
        assert_eq!(cemi(0x0a03, WRITE, 0, &[1, 2]), [0x11, 0, 0xbc, 0xe0, 0, 0, 0x0a, 0x03, 3, 0, 0x80, 1, 2]);
    }
}
