//! TwinCAT ADS over TCP, as Beckhoff's controllers speak it: variables read and written by their
//! names in the PLC program. A controller answers only routes it has been told about, so this
//! gateway's AMS net id needs a route there first.
//!
//! `ads://10.0.0.7:48898?target=5.1.204.160.1.1&port=851&source=10.0.0.2.1.1`

use crate::adapters::tags::{ANSWER, Tags, Trouble, host_port, number, query, yes};
use async_trait::async_trait;
use ferrum_edge_contract::tags::{Kind, symbol};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const READ: u16 = 2;
const WRITE: u16 = 3;
const READ_WRITE: u16 = 9;
const REQUEST: u16 = 4;
const SYMBOL_HANDLE: u32 = 0xf003;
const SYMBOL_VALUE: u32 = 0xf005;

pub struct Ads {
    stream: TcpStream,
    target: [u8; 8],
    source: [u8; 8],
    invoke: u32,
    handles: HashMap<String, u32>,
}

fn net_id(text: &str, port: u16) -> Result<[u8; 8], String> {
    let parts = text.split('.').map(str::parse).collect::<Result<Vec<u8>, _>>().map_err(|_| format!("{text} is not an AMS net id"))?;
    let parts: [u8; 6] = parts.try_into().map_err(|_| format!("{text} is not an AMS net id"))?;
    let mut out = [0u8; 8];
    out[..6].copy_from_slice(&parts);
    out[6..].copy_from_slice(&port.to_le_bytes());
    Ok(out)
}

pub fn decode(kind: Kind, b: &[u8]) -> Option<Value> {
    Some(match kind {
        Kind::Bool => json!(*b.first()? != 0),
        Kind::Byte => json!(*b.first()?),
        Kind::Int => json!(i16::from_le_bytes(b.get(..2)?.try_into().ok()?)),
        Kind::Word => json!(u16::from_le_bytes(b.get(..2)?.try_into().ok()?)),
        Kind::Dint => json!(i32::from_le_bytes(b.get(..4)?.try_into().ok()?)),
        Kind::Dword => json!(u32::from_le_bytes(b.get(..4)?.try_into().ok()?)),
        Kind::Real => json!(f32::from_le_bytes(b.get(..4)?.try_into().ok()?)),
        Kind::Lreal => json!(f64::from_le_bytes(b.get(..8)?.try_into().ok()?)),
        Kind::Text(_) => json!(String::from_utf8_lossy(b.split(|c| *c == 0).next()?)),
    })
}

pub fn encode(kind: Kind, value: &Value) -> Result<Vec<u8>, Trouble> {
    Ok(match kind {
        Kind::Bool => vec![u8::from(yes(value))],
        Kind::Byte => vec![number(value)? as u8],
        Kind::Int => (number(value)? as i16).to_le_bytes().to_vec(),
        Kind::Word => (number(value)? as u16).to_le_bytes().to_vec(),
        Kind::Dint => (number(value)? as i32).to_le_bytes().to_vec(),
        Kind::Dword => (number(value)? as u32).to_le_bytes().to_vec(),
        Kind::Real => (number(value)? as f32).to_le_bytes().to_vec(),
        Kind::Lreal => number(value)?.to_le_bytes().to_vec(),
        Kind::Text(n) => {
            let mut out = value.as_str().ok_or_else(|| Trouble::Refused("that is not text".into()))?.as_bytes().to_vec();
            out.resize(n + 1, 0);
            out
        }
    })
}

impl Ads {
    pub async fn connect(rest: &str) -> Result<Box<dyn Tags>, String> {
        let (host, port) = host_port(rest, 48898);
        let target = net_id(&query(rest, "target").ok_or("say ?target=, its AMS net id")?, query(rest, "port").and_then(|p| p.parse().ok()).unwrap_or(851))?;
        let source = net_id(&query(rest, "source").ok_or("say ?source=, the AMS net id routed to this gateway")?, 32905)?;
        let stream = timeout(ANSWER, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| format!("{host}:{port} did not answer"))?
            .map_err(|e| format!("{host}:{port}: {e}"))?;
        Ok(Box::new(Ads { stream, target, source, invoke: 0, handles: HashMap::new() }))
    }

    async fn ask(&mut self, command: u16, data: &[u8]) -> Result<Vec<u8>, Trouble> {
        self.invoke = self.invoke.wrapping_add(1);
        let mut frame = vec![0, 0];
        frame.extend(((32 + data.len()) as u32).to_le_bytes());
        frame.extend(self.target);
        frame.extend(self.source);
        frame.extend(command.to_le_bytes());
        frame.extend(REQUEST.to_le_bytes());
        frame.extend((data.len() as u32).to_le_bytes());
        frame.extend(0u32.to_le_bytes());
        frame.extend(self.invoke.to_le_bytes());
        frame.extend(data);
        self.stream.write_all(&frame).await?;
        let mut tcp = [0u8; 6];
        timeout(ANSWER, self.stream.read_exact(&mut tcp)).await??;
        let size = u32::from_le_bytes([tcp[2], tcp[3], tcp[4], tcp[5]]) as usize;
        if !(32..=1 << 20).contains(&size) {
            return Err(Trouble::Lost("that is not ADS".into()));
        }
        let mut body = vec![0u8; size];
        timeout(ANSWER, self.stream.read_exact(&mut body)).await??;
        let error = u32::from_le_bytes([body[24], body[25], body[26], body[27]]);
        if error != 0 {
            return Err(Trouble::Refused(format!("it answered AMS error {error:#x}")));
        }
        let answer = body[32..].to_vec();
        let result = answer.get(..4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if result != 0 {
            return Err(Trouble::Refused(format!("it answered ADS error {result:#x}")));
        }
        Ok(answer)
    }

    async fn handle(&mut self, name: &str) -> Result<u32, Trouble> {
        if let Some(h) = self.handles.get(name) {
            return Ok(*h);
        }
        let mut data = SYMBOL_HANDLE.to_le_bytes().to_vec();
        data.extend(0u32.to_le_bytes());
        data.extend(4u32.to_le_bytes());
        data.extend(((name.len() + 1) as u32).to_le_bytes());
        data.extend(name.as_bytes());
        data.push(0);
        let answer = self.ask(READ_WRITE, &data).await?;
        let handle = answer.get(8..12).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| Trouble::Lost("it gave no handle".into()))?;
        self.handles.insert(name.to_string(), handle);
        Ok(handle)
    }
}

#[async_trait]
impl Tags for Ads {
    async fn read(&mut self, tag: &str) -> Result<Value, Trouble> {
        let (name, kind) = symbol(tag).map_err(Trouble::Refused)?;
        let kind = kind.ok_or_else(|| Trouble::Refused(format!("say what {name} is: {name}:dint")))?;
        let handle = self.handle(name).await?;
        let mut data = SYMBOL_VALUE.to_le_bytes().to_vec();
        data.extend(handle.to_le_bytes());
        data.extend((kind.size() as u32).to_le_bytes());
        let answer = self.ask(READ, &data).await?;
        decode(kind, answer.get(8..).unwrap_or_default()).ok_or_else(|| Trouble::Refused("it sent too little".into()))
    }

    async fn write(&mut self, tag: &str, value: &Value) -> Result<(), Trouble> {
        let (name, kind) = symbol(tag).map_err(Trouble::Refused)?;
        let kind = kind.ok_or_else(|| Trouble::Refused(format!("say what {name} is: {name}:dint")))?;
        let handle = self.handle(name).await?;
        let bytes = encode(kind, value)?;
        let mut data = SYMBOL_VALUE.to_le_bytes().to_vec();
        data.extend(handle.to_le_bytes());
        data.extend((bytes.len() as u32).to_le_bytes());
        data.extend(&bytes);
        self.ask(WRITE, &data).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_net_id_is_six_numbers_and_a_port() {
        assert_eq!(net_id("5.1.204.160.1.1", 851).unwrap(), [5, 1, 204, 160, 1, 1, 0x53, 0x03]);
        assert!(net_id("5.1.204", 851).is_err());
    }

    #[test]
    fn values_are_little_endian() {
        assert_eq!(decode(Kind::Real, &0.5f32.to_le_bytes()), Some(json!(0.5)));
        assert_eq!(decode(Kind::Text(5), b"hi\0\0"), Some(json!("hi")));
        assert_eq!(encode(Kind::Text(3), &json!("hi")).unwrap(), b"hi\0\0");
    }
}
