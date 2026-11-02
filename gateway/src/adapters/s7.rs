//! Siemens S7 over ISO-on-TCP, the classic protocol of the S7-300, 400, 1200 and 1500: reads and writes
//! of data blocks, markers, inputs and outputs. A 1200 or 1500 has to allow it: optimized block access
//! off and PUT/GET on, in TIA Portal. It has no security of its own, so keep it on a plant's network.
//!
//! `s7://10.0.0.7:102?rack=0&slot=1`

use crate::adapters::tags::{ANSWER, Tags, Trouble, host_port, number, query, yes};
use async_trait::async_trait;
use ferrum_edge_contract::tags::{Kind, S7Address};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const READ: u8 = 0x04;
const WRITE: u8 = 0x05;
/// Transport sizes of a request: a bit, and bytes.
const BIT: u8 = 0x01;
const BYTES: u8 = 0x02;

pub struct S7 {
    stream: TcpStream,
    reference: u16,
    /// Most a single message can carry, as agreed.
    pdu: usize,
}

fn tpkt(payload: &[u8]) -> Vec<u8> {
    let mut out = vec![3, 0];
    out.extend(((payload.len() + 4) as u16).to_be_bytes());
    out.extend(payload);
    out
}

/// An S7 message, in a COTP data unit, in a TPKT.
fn job(reference: u16, parameters: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = vec![0x02, 0xf0, 0x80, 0x32, 0x01, 0, 0];
    m.extend(reference.to_be_bytes());
    m.extend((parameters.len() as u16).to_be_bytes());
    m.extend((data.len() as u16).to_be_bytes());
    m.extend(parameters);
    m.extend(data);
    tpkt(&m)
}

/// One thing asked of memory. An S7 address counts in bits.
fn item(a: &S7Address, transport: u8, count: u16) -> [u8; 12] {
    let bits = a.byte * 8 + u32::from(a.bit);
    let [_, b1, b2, b3] = bits.to_be_bytes();
    let [c1, c2] = count.to_be_bytes();
    let [d1, d2] = a.block.to_be_bytes();
    [0x12, 0x0a, 0x10, transport, c1, c2, d1, d2, a.area, b1, b2, b3]
}

pub fn decode(kind: Kind, b: &[u8], bit: u8) -> Option<Value> {
    Some(match kind {
        Kind::Bool => json!(b.first()? >> bit & 1 == 1),
        Kind::Byte => json!(*b.first()?),
        Kind::Int => json!(i16::from_be_bytes(b.get(..2)?.try_into().ok()?)),
        Kind::Word => json!(u16::from_be_bytes(b.get(..2)?.try_into().ok()?)),
        Kind::Dint => json!(i32::from_be_bytes(b.get(..4)?.try_into().ok()?)),
        Kind::Dword => json!(u32::from_be_bytes(b.get(..4)?.try_into().ok()?)),
        Kind::Real => json!(f32::from_be_bytes(b.get(..4)?.try_into().ok()?)),
        Kind::Lreal => json!(f64::from_be_bytes(b.get(..8)?.try_into().ok()?)),
        // A PLC string says how long it can be, and how long it is.
        Kind::Text(_) => {
            let length = *b.get(1)? as usize;
            json!(String::from_utf8_lossy(b.get(2..2 + length)?))
        }
    })
}

pub fn encode(kind: Kind, value: &Value) -> Result<Vec<u8>, Trouble> {
    Ok(match kind {
        Kind::Bool => vec![u8::from(yes(value))],
        Kind::Byte => vec![number(value)? as u8],
        Kind::Int => (number(value)? as i16).to_be_bytes().to_vec(),
        Kind::Word => (number(value)? as u16).to_be_bytes().to_vec(),
        Kind::Dint => (number(value)? as i32).to_be_bytes().to_vec(),
        Kind::Dword => (number(value)? as u32).to_be_bytes().to_vec(),
        Kind::Real => (number(value)? as f32).to_be_bytes().to_vec(),
        Kind::Lreal => number(value)?.to_be_bytes().to_vec(),
        Kind::Text(max) => {
            let text = value.as_str().ok_or_else(|| Trouble::Refused("that is not text".into()))?;
            let length = text.len().min(max);
            let mut out = vec![max as u8, length as u8];
            out.extend(&text.as_bytes()[..length]);
            out
        }
    })
}

/// Reads one TPKT, and leaves the COTP header out of it.
async fn receive(stream: &mut TcpStream) -> Result<Vec<u8>, Trouble> {
    let mut head = [0u8; 4];
    timeout(ANSWER, stream.read_exact(&mut head)).await??;
    let size = usize::from(u16::from_be_bytes([head[2], head[3]]));
    if head[0] != 3 || size < 7 {
        return Err(Trouble::Lost("that is not an S7 device".into()));
    }
    let mut body = vec![0u8; size - 4];
    timeout(ANSWER, stream.read_exact(&mut body)).await??;
    let cotp = usize::from(body[0]) + 1;
    Ok(body.get(cotp..).unwrap_or_default().to_vec())
}

impl S7 {
    pub async fn connect(rest: &str) -> Result<Box<dyn Tags>, String> {
        let (host, port) = host_port(rest, 102);
        let rack: u8 = query(rest, "rack").and_then(|r| r.parse().ok()).unwrap_or(0);
        let slot: u8 = query(rest, "slot").and_then(|s| s.parse().ok()).unwrap_or(1);
        let mut stream = timeout(ANSWER, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| format!("{host}:{port} did not answer"))?
            .map_err(|e| format!("{host}:{port}: {e}"))?;
        let opened = async {
            // The far end's TSAP says rack and slot: 1 is a PG connection.
            let request = [0x11, 0xe0, 0, 0, 0, 1, 0, 0xc0, 1, 0x0a, 0xc1, 2, 1, 0, 0xc2, 2, 1, rack * 32 + slot];
            stream.write_all(&tpkt(&request)).await?;
            let mut head = [0u8; 4];
            timeout(ANSWER, stream.read_exact(&mut head)).await??;
            let mut rest = vec![0u8; usize::from(u16::from_be_bytes([head[2], head[3]])).saturating_sub(4)];
            timeout(ANSWER, stream.read_exact(&mut rest)).await??;
            if rest.get(1).is_none_or(|t| t & 0xf0 != 0xd0) {
                return Err(Trouble::Lost("it refused the connection: check rack and slot".into()));
            }
            let setup = [0xf0, 0, 0, 1, 0, 1, 0x03, 0xc0];
            stream.write_all(&job(1, &setup, &[])).await?;
            let answer = receive(&mut stream).await?;
            let pdu = answer.get(18..20).map_or(240, |b| usize::from(u16::from_be_bytes([b[0], b[1]])));
            Ok::<_, Trouble>(pdu.max(64))
        }
        .await;
        match opened {
            Ok(pdu) => Ok(Box::new(S7 { stream, reference: 1, pdu })),
            Err(Trouble::Lost(why) | Trouble::Refused(why)) => Err(why),
        }
    }

    async fn ask(&mut self, parameters: &[u8], data: &[u8]) -> Result<Vec<u8>, Trouble> {
        self.reference = self.reference.wrapping_add(1);
        self.stream.write_all(&job(self.reference, parameters, data)).await?;
        let answer = receive(&mut self.stream).await?;
        // 0x32, kind, two reserved, reference, parameter and data lengths, error class and code.
        if answer.len() < 12 || answer[0] != 0x32 || u16::from_be_bytes([answer[4], answer[5]]) != self.reference {
            return Err(Trouble::Lost("that was not the answer to this".into()));
        }
        if answer[10] != 0 || answer[11] != 0 {
            return Err(Trouble::Refused(format!("it answered error {:02x}{:02x}", answer[10], answer[11])));
        }
        let parameters = usize::from(u16::from_be_bytes([answer[6], answer[7]]));
        Ok(answer.get(12 + parameters..).unwrap_or_default().to_vec())
    }
}

fn outcome(code: u8) -> Result<(), Trouble> {
    match code {
        0xff => Ok(()),
        0x05 => Err(Trouble::Refused("that address does not exist on this PLC".into())),
        0x03 => Err(Trouble::Refused("this PLC does not allow it: check PUT/GET and optimized access".into())),
        0x0a => Err(Trouble::Refused("that is not there".into())),
        other => Err(Trouble::Refused(format!("it answered {other:02x}"))),
    }
}

#[async_trait]
impl Tags for S7 {
    async fn read(&mut self, tag: &str) -> Result<Value, Trouble> {
        let a = S7Address::parse(tag).map_err(Trouble::Refused)?;
        let size = a.kind.size();
        if size + 32 > self.pdu {
            return Err(Trouble::Refused("that is more than one message can carry".into()));
        }
        let whole = S7Address { bit: 0, ..a };
        let mut parameters = vec![READ, 1];
        parameters.extend(item(&whole, BYTES, size as u16));
        let data = self.ask(&parameters, &[]).await?;
        outcome(*data.first().ok_or_else(|| Trouble::Lost("it said nothing".into()))?)?;
        decode(a.kind, data.get(4..).unwrap_or_default(), a.bit).ok_or_else(|| Trouble::Refused("it sent too little".into()))
    }

    async fn write(&mut self, tag: &str, value: &Value) -> Result<(), Trouble> {
        let a = S7Address::parse(tag).map_err(Trouble::Refused)?;
        let bytes = encode(a.kind, value)?;
        let mut parameters = vec![WRITE, 1];
        let mut data = vec![0];
        if a.kind == Kind::Bool {
            parameters.extend(item(&a, BIT, 1));
            data.extend([0x03, 0, 1]);
        } else {
            parameters.extend(item(&a, BYTES, bytes.len() as u16));
            data.push(0x04);
            data.extend(((bytes.len() * 8) as u16).to_be_bytes());
        }
        data.extend(&bytes);
        let answer = self.ask(&parameters, &data).await?;
        outcome(*answer.first().ok_or_else(|| Trouble::Lost("it said nothing".into()))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_big_endian_as_a_plc_keeps_them() {
        assert_eq!(decode(Kind::Real, &[0x41, 0xc8, 0, 0], 0), Some(json!(25.0)));
        assert_eq!(decode(Kind::Int, &[0xff, 0xfe], 0), Some(json!(-2)));
        assert_eq!(decode(Kind::Bool, &[0b0000_0100], 2), Some(json!(true)));
        assert_eq!(decode(Kind::Text(10), &[10, 2, b'h', b'i', 0, 0], 0), Some(json!("hi")));
        assert_eq!(decode(Kind::Dint, &[0, 1], 0), None, "too little is nothing");
        assert_eq!(encode(Kind::Real, &json!(25)).unwrap(), [0x41, 0xc8, 0, 0]);
        assert_eq!(encode(Kind::Text(4), &json!("hello")).unwrap(), [4, 4, b'h', b'e', b'l', b'l']);
        assert!(encode(Kind::Int, &json!("x")).is_err());
    }

    #[test]
    fn an_item_counts_in_bits() {
        let a = S7Address::parse("DB1.DBD4:real").unwrap();
        assert_eq!(item(&a, BYTES, 4), [0x12, 0x0a, 0x10, 0x02, 0, 4, 0, 1, 0x84, 0, 0, 32]);
    }
}
