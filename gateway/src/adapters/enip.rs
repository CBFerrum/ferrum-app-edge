//! EtherNet/IP with CIP, as Rockwell's ControlLogix and CompactLogix speak it: tags read and written
//! by name, explicit messaging only. No I/O connections: that is a controller's business, not this.
//!
//! `enip://10.0.0.7:44818?slot=0`. Without `slot` it talks to the device itself and not through a
//! backplane, as a CompactLogix or a simulator expects.

use crate::adapters::tags::{ANSWER, Tags, Trouble, host_port, number, query, yes};
use async_trait::async_trait;
use ferrum_edge_contract::tags::{Kind, symbol};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const REGISTER: u16 = 0x65;
const SEND_RR: u16 = 0x6f;
const READ_TAG: u8 = 0x4c;
const WRITE_TAG: u8 = 0x4d;
const UNCONNECTED_SEND: u8 = 0x52;
const BOOL: u16 = 0xc1;
const SINT: u16 = 0xc2;
const INT: u16 = 0xc3;
const DINT: u16 = 0xc4;
const LINT: u16 = 0xc5;
const REAL: u16 = 0xca;
const LREAL: u16 = 0xcb;

pub struct Enip {
    stream: TcpStream,
    session: u32,
    slot: Option<u8>,
    /// What each tag turned out to be, because a write has to say so.
    types: HashMap<String, u16>,
}

fn encapsulated(command: u16, session: u32, data: &[u8]) -> Vec<u8> {
    let mut out = command.to_le_bytes().to_vec();
    out.extend((data.len() as u16).to_le_bytes());
    out.extend(session.to_le_bytes());
    out.extend([0u8; 16]);
    out.extend(data);
    out
}

/// `Program:Main.Speed[3]` as a CIP path: a symbol for each name, and an element for each index.
pub fn path(tag: &str) -> Result<Vec<u8>, Trouble> {
    let mut out = Vec::new();
    for part in tag.split('.') {
        let (name, index) = match part.split_once('[') {
            Some((name, rest)) => (name, Some(rest.trim_end_matches(']'))),
            None => (part, None),
        };
        if name.is_empty() || name.len() > 255 {
            return Err(Trouble::Refused(format!("{tag} is not a tag")));
        }
        out.extend([0x91, name.len() as u8]);
        out.extend(name.as_bytes());
        if out.len() % 2 == 1 {
            out.push(0);
        }
        if let Some(index) = index {
            let i: u32 = index.parse().map_err(|_| Trouble::Refused(format!("{tag} is not a tag")))?;
            match i {
                0..=0xff => out.extend([0x28, i as u8]),
                0x100..=0xffff => {
                    out.extend([0x29, 0]);
                    out.extend((i as u16).to_le_bytes());
                }
                _ => {
                    out.extend([0x2a, 0]);
                    out.extend(i.to_le_bytes());
                }
            }
        }
    }
    Ok(out)
}

fn request(service: u8, tag_path: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = vec![service, (tag_path.len() / 2) as u8];
    out.extend(tag_path);
    out.extend(data);
    out
}

/// A request sent through the Connection Manager to a slot on a backplane.
fn routed(inner: &[u8], slot: u8) -> Vec<u8> {
    let mut data = vec![0x0a, 0x0e];
    data.extend((inner.len() as u16).to_le_bytes());
    data.extend(inner);
    if inner.len() % 2 == 1 {
        data.push(0);
    }
    data.extend([1, 0, 1, slot]);
    request(UNCONNECTED_SEND, &[0x20, 0x06, 0x24, 0x01], &data)
}

pub fn decode(kind: u16, b: &[u8]) -> Option<Value> {
    Some(match kind {
        BOOL => json!(*b.first()? != 0),
        SINT => json!(*b.first()? as i8),
        INT => json!(i16::from_le_bytes(b.get(..2)?.try_into().ok()?)),
        DINT => json!(i32::from_le_bytes(b.get(..4)?.try_into().ok()?)),
        LINT => json!(i64::from_le_bytes(b.get(..8)?.try_into().ok()?)),
        REAL => json!(f32::from_le_bytes(b.get(..4)?.try_into().ok()?)),
        LREAL => json!(f64::from_le_bytes(b.get(..8)?.try_into().ok()?)),
        _ => return None,
    })
}

pub fn encode(kind: u16, value: &Value) -> Result<Vec<u8>, Trouble> {
    Ok(match kind {
        BOOL => vec![if yes(value) { 0xff } else { 0 }],
        SINT => vec![number(value)? as i8 as u8],
        INT => (number(value)? as i16).to_le_bytes().to_vec(),
        DINT => (number(value)? as i32).to_le_bytes().to_vec(),
        LINT => (number(value)? as i64).to_le_bytes().to_vec(),
        REAL => (number(value)? as f32).to_le_bytes().to_vec(),
        LREAL => number(value)?.to_le_bytes().to_vec(),
        other => return Err(Trouble::Refused(format!("this cannot write type {other:#x}"))),
    })
}

fn kind_of(k: Kind) -> Result<u16, Trouble> {
    Ok(match k {
        Kind::Bool => BOOL,
        Kind::Byte => SINT,
        Kind::Int | Kind::Word => INT,
        Kind::Dint | Kind::Dword => DINT,
        Kind::Real => REAL,
        Kind::Lreal => LREAL,
        Kind::Text(_) => return Err(Trouble::Refused("this does not write strings".into())),
    })
}

impl Enip {
    pub async fn connect(rest: &str) -> Result<Box<dyn Tags>, String> {
        let (host, port) = host_port(rest, 44818);
        let slot = query(rest, "slot").and_then(|s| s.parse().ok());
        let mut stream = timeout(ANSWER, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| format!("{host}:{port} did not answer"))?
            .map_err(|e| format!("{host}:{port}: {e}"))?;
        let opened = async {
            stream.write_all(&encapsulated(REGISTER, 0, &[1, 0, 0, 0])).await?;
            let (session, status, _) = receive(&mut stream).await?;
            if status != 0 || session == 0 {
                return Err(Trouble::Lost(format!("it refused a session: {status:#x}")));
            }
            Ok::<_, Trouble>(session)
        }
        .await;
        match opened {
            Ok(session) => Ok(Box::new(Enip { stream, session, slot, types: HashMap::new() })),
            Err(Trouble::Lost(why) | Trouble::Refused(why)) => Err(why),
        }
    }

    /// One CIP request and its answer: `(status, data)`.
    async fn ask(&mut self, cip: Vec<u8>) -> Result<(u8, Vec<u8>), Trouble> {
        let cip = match self.slot {
            Some(slot) => routed(&cip, slot),
            None => cip,
        };
        let mut data = vec![0; 6];
        data.extend(2u16.to_le_bytes());
        data.extend([0, 0, 0, 0, 0xb2, 0]);
        data.extend((cip.len() as u16).to_le_bytes());
        data.extend(&cip);
        self.stream.write_all(&encapsulated(SEND_RR, self.session, &data)).await?;
        let (_, status, body) = receive(&mut self.stream).await?;
        if status != 0 {
            return Err(Trouble::Lost(format!("it answered {status:#x}")));
        }
        // Interface, timeout, two items and the header of each, then a CIP reply.
        let reply = body.get(16..).filter(|r| r.len() >= 4).ok_or_else(|| Trouble::Lost("that was not an answer".into()))?;
        let extended = usize::from(reply[3]) * 2;
        Ok((reply[2], reply.get(4 + extended..).unwrap_or_default().to_vec()))
    }

    async fn read_value(&mut self, name: &str) -> Result<(u16, Value), Trouble> {
        let (status, data) = self.ask(request(READ_TAG, &path(name)?, &[1, 0])).await?;
        if status != 0 {
            return Err(Trouble::Refused(format!("it answered status {status:#x}")));
        }
        let kind = u16::from_le_bytes(data.get(..2).and_then(|b| b.try_into().ok()).ok_or_else(|| Trouble::Lost("it said nothing".into()))?);
        let value = decode(kind, &data[2..]).ok_or_else(|| Trouble::Refused(format!("this cannot read type {kind:#x}")))?;
        self.types.insert(name.to_string(), kind);
        Ok((kind, value))
    }
}

async fn receive(stream: &mut TcpStream) -> Result<(u32, u32, Vec<u8>), Trouble> {
    let mut head = [0u8; 24];
    timeout(ANSWER, stream.read_exact(&mut head)).await??;
    let mut body = vec![0u8; usize::from(u16::from_le_bytes([head[2], head[3]]))];
    timeout(ANSWER, stream.read_exact(&mut body)).await??;
    Ok((u32::from_le_bytes([head[4], head[5], head[6], head[7]]), u32::from_le_bytes([head[8], head[9], head[10], head[11]]), body))
}

#[async_trait]
impl Tags for Enip {
    async fn read(&mut self, tag: &str) -> Result<Value, Trouble> {
        let (name, _) = symbol(tag).map_err(Trouble::Refused)?;
        self.read_value(name).await.map(|(_, v)| v)
    }

    async fn write(&mut self, tag: &str, value: &Value) -> Result<(), Trouble> {
        let (name, kind) = symbol(tag).map_err(Trouble::Refused)?;
        let kind = match (kind, self.types.get(name)) {
            (Some(k), _) => kind_of(k)?,
            (None, Some(k)) => *k,
            (None, None) => self.read_value(name).await?.0,
        };
        let mut data = kind.to_le_bytes().to_vec();
        data.extend([1, 0]);
        data.extend(encode(kind, value)?);
        let (status, _) = self.ask(request(WRITE_TAG, &path(name)?, &data)).await?;
        if status != 0 {
            return Err(Trouble::Refused(format!("it answered status {status:#x}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_becomes_a_path_padded_to_words() {
        assert_eq!(path("Speed").unwrap(), [0x91, 5, b'S', b'p', b'e', b'e', b'd', 0]);
        assert_eq!(path("Tags[3]").unwrap(), [0x91, 4, b'T', b'a', b'g', b's', 0x28, 3]);
        assert_eq!(path("A.B").unwrap(), [0x91, 1, b'A', 0, 0x91, 1, b'B', 0]);
        assert_eq!(path("A[300]").unwrap(), [0x91, 1, b'A', 0, 0x29, 0, 0x2c, 1]);
        assert!(path("").is_err());
    }

    #[test]
    fn values_are_little_endian_with_their_type() {
        assert_eq!(decode(DINT, &[1, 0, 0, 0]), Some(json!(1)));
        assert_eq!(decode(REAL, &0.5f32.to_le_bytes()), Some(json!(0.5)));
        assert_eq!(decode(BOOL, &[0xff]), Some(json!(true)));
        assert_eq!(encode(INT, &json!(-1)).unwrap(), [0xff, 0xff]);
        assert_eq!(decode(0xa0, &[0]), None);
    }
}
