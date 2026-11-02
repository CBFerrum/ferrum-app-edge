//! SNMP versions 1 and 2c: reading and setting by OID, walking a table, and hearing traps. `community`
//! is a password sent in the clear, so that it stays on a network that can bear it. Version 3 is in
//! [`super::snmp3`].
//!
//! `snmp://10.0.0.7:161?community=public&version=2c`, and `listen+snmp://0.0.0.0:162` for traps.

use crate::adapter::Failure;
use crate::adapters::tags::{ANSWER, Tags, Trouble, host_port, number, query};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use ferrum_edge_contract::tags::oid;
use serde_json::{Map, Value, json};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::timeout;

pub const GET: u8 = 0xa0;
pub const NEXT: u8 = 0xa1;
pub const RESPONSE: u8 = 0xa2;
pub const SET: u8 = 0xa3;
pub const REPORT: u8 = 0xa8;
const TRAP_V1: u8 = 0xa4;
const INFORM: u8 = 0xa6;
const TRAP_V2: u8 = 0xa7;
const INTEGER: u8 = 0x02;
pub const OCTETS: u8 = 0x04;
const NULL: u8 = 0x05;
const OID: u8 = 0x06;
pub const SEQUENCE: u8 = 0x30;
/// A variable asked about, which has no value yet.
pub const BIND_NULL: [u8; 2] = [NULL, 0];
/// What a walk will bring back, at most.
const MAX_WALK: usize = 5_000;

pub fn tlv(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    match body.len() {
        n @ 0..=127 => out.push(n as u8),
        n @ 128..=255 => out.extend([0x81, n as u8]),
        n => {
            out.push(0x82);
            out.extend((n as u16).to_be_bytes());
        }
    }
    out.extend(body);
    out
}

pub fn integer(n: i64) -> Vec<u8> {
    let bytes = n.to_be_bytes();
    let start = (0..7).find(|i| !((bytes[*i] == 0 && bytes[i + 1] < 0x80) || (bytes[*i] == 0xff && bytes[i + 1] >= 0x80))).unwrap_or(7);
    tlv(INTEGER, &bytes[start..])
}

pub fn oid_bytes(arcs: &[u32]) -> Vec<u8> {
    let mut out = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for arc in &arcs[2..] {
        let mut groups = vec![(*arc & 0x7f) as u8];
        let mut rest = *arc >> 7;
        while rest > 0 {
            groups.push((rest & 0x7f) as u8 | 0x80);
            rest >>= 7;
        }
        groups.reverse();
        out.extend(groups);
    }
    tlv(OID, &out)
}

fn oid_text(b: &[u8]) -> String {
    let Some(first) = b.first() else { return String::new() };
    let mut arcs = vec![u32::from(first / 40), u32::from(first % 40)];
    let mut n = 0u32;
    for byte in &b[1..] {
        n = n << 7 | u32::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            arcs.push(n);
            n = 0;
        }
    }
    arcs.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

/// One tag, length and value from the front of `b`, and what comes after it.
pub fn read(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let kind = *b.first()?;
    let first = *b.get(1)?;
    let (length, at) = match first {
        0x81 => (usize::from(*b.get(2)?), 3),
        0x82 => (usize::from(u16::from_be_bytes([*b.get(2)?, *b.get(3)?])), 4),
        n if n < 0x80 => (usize::from(n), 2),
        _ => return None,
    };
    Some((kind, b.get(at..at + length)?, b.get(at + length..)?))
}

pub fn number_of(body: &[u8], signed: bool) -> i64 {
    let start = if signed && body.first().is_some_and(|b| b & 0x80 != 0) { -1 } else { 0 };
    body.iter().fold(start, |n, b| n << 8 | i64::from(*b))
}

fn json_of(kind: u8, body: &[u8]) -> Value {
    match kind {
        INTEGER => json!(number_of(body, true)),
        // Counter32, Gauge32, TimeTicks and Counter64 are all unsigned numbers.
        0x41 | 0x42 | 0x43 | 0x46 => json!(number_of(body, false)),
        OCTETS => std::str::from_utf8(body).map_or_else(|_| json!(hex::encode(body)), |t| json!(t)),
        OID => json!(oid_text(body)),
        0x40 if body.len() == 4 => json!(body.iter().map(u8::to_string).collect::<Vec<_>>().join(".")),
        _ => Value::Null,
    }
}

/// A request or a response: its id, its error, and its variables.
pub struct Pdu {
    pub kind: u8,
    pub id: i64,
    pub error: i64,
    pub bindings: Vec<(String, Value, u8)>,
}

/// What is inside a request or a response: its id, its error, and its variables.
pub fn split_pdu(pdu: &[u8]) -> Option<(i64, i64, Vec<(String, Value, u8)>)> {
    let (INTEGER, a, r) = read(pdu)? else { return None };
    let (INTEGER, b, r) = read(r)? else { return None };
    let (INTEGER, _, r) = read(r)? else { return None };
    Some((number_of(a, true), number_of(b, true), variables(r)?))
}

fn variables(bound: &[u8]) -> Option<Vec<(String, Value, u8)>> {
    let (SEQUENCE, mut list, _) = read(bound)? else { return None };
    let mut bindings = Vec::new();
    while !list.is_empty() {
        let (SEQUENCE, pair, more) = read(list)? else { return None };
        let (OID, name, r) = read(pair)? else { return None };
        let (value_kind, value, _) = read(r)?;
        bindings.push((oid_text(name), json_of(value_kind, value), value_kind));
        list = more;
    }
    Some(bindings)
}

pub fn parse(message: &[u8]) -> Option<Pdu> {
    let (SEQUENCE, body, _) = read(message)? else { return None };
    let (INTEGER, _, rest) = read(body)? else { return None };
    let (OCTETS, _, rest) = read(rest)? else { return None };
    let (kind, pdu, _) = read(rest)?;
    if kind == TRAP_V1 {
        // Enterprise, agent address, generic, specific and time, then the variables.
        let mut rest = pdu;
        for _ in 0..5 {
            rest = read(rest)?.2;
        }
        return Some(Pdu { kind, id: 0, error: 0, bindings: variables(rest)? });
    }
    let (id, error, bindings) = split_pdu(pdu)?;
    Some(Pdu { kind, id, error, bindings })
}

pub fn request(version: i64, community: &str, kind: u8, id: i64, arcs: &[u32], value: Vec<u8>) -> Vec<u8> {
    let binding = tlv(SEQUENCE, &[oid_bytes(arcs), value].concat());
    let pdu = tlv(kind, &[integer(id), integer(0), integer(0), tlv(SEQUENCE, &binding)].concat());
    tlv(SEQUENCE, &[integer(version), tlv(OCTETS, community.as_bytes()), pdu].concat())
}

pub struct Snmp {
    socket: UdpSocket,
    version: i64,
    community: String,
    id: i64,
}

impl Snmp {
    pub async fn connect(rest: &str) -> Result<Box<dyn Tags>, String> {
        let (host, port) = host_port(rest, 161);
        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
        socket.connect((host.as_str(), port)).await.map_err(|e| format!("{host}:{port}: {e}"))?;
        let version = if query(rest, "version").as_deref() == Some("1") { 0 } else { 1 };
        let mut s = Snmp { socket, version, community: query(rest, "community").unwrap_or_else(|| "public".into()), id: 0 };
        // A device that does not answer this may just be choosy, so this does not stop on that.
        let _ = s.ask(GET, &[1, 3, 6, 1, 2, 1, 1, 3, 0], tlv(NULL, &[])).await;
        Ok(Box::new(s))
    }

    async fn ask(&mut self, kind: u8, arcs: &[u32], value: Vec<u8>) -> Result<Pdu, Trouble> {
        self.id += 1;
        let message = request(self.version, &self.community, kind, self.id, arcs, value);
        for _ in 0..3 {
            self.socket.send(&message).await?;
            let mut buffer = vec![0u8; 65_535];
            let Ok(got) = timeout(ANSWER / 3, self.socket.recv(&mut buffer)).await else { continue };
            let n = got?;
            if let Some(answer) = parse(&buffer[..n]).filter(|p| p.kind == RESPONSE && p.id == self.id) {
                if answer.error != 0 {
                    return Err(Trouble::Refused(format!("it answered error {}", answer.error)));
                }
                return Ok(answer);
            }
        }
        Err(Trouble::Lost("it did not answer".into()))
    }
}

/// Exceptions that say a variable is not there, in place of a value.
fn missing(kind: u8) -> bool {
    matches!(kind, 0x80..=0x82)
}

#[async_trait]
impl Tags for Snmp {
    async fn read(&mut self, tag: &str) -> Result<Value, Trouble> {
        let (arcs, walk) = oid(tag).map_err(Trouble::Refused)?;
        if !walk {
            let answer = self.ask(GET, &arcs, tlv(NULL, &[])).await?;
            let (_, value, kind) = answer.bindings.into_iter().next().ok_or_else(|| Trouble::Refused("it sent nothing".into()))?;
            return if missing(kind) { Err(Trouble::Refused("that is not there".into())) } else { Ok(value) };
        }
        let root = format!("{}.", arcs.iter().map(u32::to_string).collect::<Vec<_>>().join("."));
        let (mut at, mut out) = (arcs, Map::new());
        while out.len() < MAX_WALK {
            let answer = self.ask(NEXT, &at, tlv(NULL, &[])).await?;
            let Some((name, value, kind)) = answer.bindings.into_iter().next() else { break };
            if missing(kind) || !name.starts_with(&root) {
                break;
            }
            at = oid(&name).map_err(Trouble::Refused)?.0;
            out.insert(name, value);
        }
        Ok(Value::Object(out))
    }

    async fn write(&mut self, tag: &str, value: &Value) -> Result<(), Trouble> {
        let (arcs, _) = oid(tag).map_err(Trouble::Refused)?;
        let encoded = match value {
            Value::String(s) => tlv(OCTETS, s.as_bytes()),
            other => integer(number(other)? as i64),
        };
        self.ask(SET, &arcs, encoded).await.map(drop)
    }
}

/// Traps and informs arriving at a port. Each becomes `{source}/{trap}`, holding its variables. An
/// inform asks for an answer, so it gets one.
pub struct Traps {
    pub address: String,
}

#[async_trait]
impl Transport for Traps {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let socket = UdpSocket::bind(&self.address).await.map_err(|e| format!("{}: {e}", self.address))?;
        let _ = ready.send(()).await;
        let mut buffer = vec![0u8; 65_535];
        loop {
            let (n, peer) = tokio::select! {
                () = tx.closed() => return Ok(()),
                got = socket.recv_from(&mut buffer) => got.map_err(|e| e.to_string())?,
            };
            let Some(pdu) = parse(&buffer[..n]).filter(|p| matches!(p.kind, TRAP_V1 | TRAP_V2 | INFORM)) else { continue };
            if pdu.kind == INFORM {
                let mut answer = buffer[..n].to_vec();
                if let Some(at) = answer.iter().position(|b| *b == INFORM) {
                    answer[at] = RESPONSE;
                    let _ = socket.send_to(&answer, peer).await;
                }
            }
            let trap = pdu.bindings.iter().find(|(name, ..)| name == "1.3.6.1.6.3.1.1.4.1.0").and_then(|(_, v, _)| v.as_str()).unwrap_or("trap").to_string();
            let variables: Map<String, Value> = pdu.bindings.into_iter().map(|(name, value, _)| (name, value)).collect();
            let message = Message { topic: format!("{}/{trap}", peer.ip()), body: Value::Object(variables).to_string().into_bytes(), status: None, from: None };
            if tx.send(message).await.is_err() {
                return Ok(());
            }
        }
    }

    async fn send(&self, _: Out) -> Result<Option<Message>, Failure> {
        Err(Failure::NotSent("this only listens".into()))
    }

    fn listening(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_what_rfc_1157_says() {
        // GetRequest for sysDescr.0, community public, id 1, as every SNMP tutorial prints it.
        let m = request(0, "public", GET, 1, &[1, 3, 6, 1, 2, 1, 1, 1, 0], tlv(NULL, &[]));
        assert_eq!(hex::encode(&m), "3026020100040670756262".replace("70756262", "7075626c6963") + "a019020101020100020100300e300c06082b060102010101000500");
        let parsed = parse(&m).unwrap();
        assert_eq!((parsed.kind, parsed.id), (GET, 1));
        assert_eq!(parsed.bindings[0].0, "1.3.6.1.2.1.1.1.0");
    }

    #[test]
    fn numbers_go_either_way() {
        assert_eq!(integer(127), [2, 1, 127]);
        assert_eq!(integer(128), [2, 2, 0, 128]);
        assert_eq!(integer(-1), [2, 1, 255]);
        assert_eq!(number_of(&[0xff], true), -1);
        assert_eq!(number_of(&[0xff], false), 255);
        assert_eq!(json_of(0x41, &[0xff, 0xff]), json!(65535), "a counter is never negative");
        assert_eq!(json_of(0x40, &[10, 0, 0, 1]), json!("10.0.0.1"));
        assert_eq!(oid_text(&[0x2b, 6, 1, 0x82, 0x01]), "1.3.6.1.257");
        assert_eq!(oid_bytes(&[1, 3, 6, 1, 257]), [6, 5, 0x2b, 6, 1, 0x82, 0x01]);
    }

    #[test]
    fn nonsense_is_not_a_message() {
        assert!(parse(&[]).is_none() && parse(&[0x30, 0x7f, 1]).is_none() && parse(b"hello world").is_none());
    }
}
