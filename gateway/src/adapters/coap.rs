//! CoAP for devices that call in, for those too small for TCP: `coap://host/api/v1/{token}/telemetry`,
//! as ThingsBoard's devices do, with observing for what a device wants to hear.
//!
//! UDP loses things, so a device repeats itself until it is answered, and a repeat must not count
//! twice: what was answered lately is kept, and a repeat gets the same answer again and goes no
//! further. There is no DTLS here, so the token crosses the network in the clear: this is for a site's
//! own network, and a device on any other should use MQTT over TLS.

use crate::adapter::Failure;
use crate::adapters::hub::{Hub, Proof};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, mpsc};

const CON: u8 = 0;
const NON: u8 = 1;
const ACK: u8 = 2;
const RST: u8 = 3;

const GET: u8 = 1;
const POST: u8 = 2;
const PUT: u8 = 3;

const CHANGED: u8 = 0x44;
const CONTENT: u8 = 0x45;
const CONTINUE: u8 = 0x5f;
const BAD_REQUEST: u8 = 0x80;
const UNAUTHORIZED: u8 = 0x81;
const NOT_FOUND: u8 = 0x84;
const NOT_ALLOWED: u8 = 0x85;
const INCOMPLETE: u8 = 0x88;
const TOO_LARGE: u8 = 0x8d;
const UNAVAILABLE: u8 = 0xa3;

const OBSERVE: u16 = 6;
const URI_PATH: u16 = 11;
const CONTENT_FORMAT: u16 = 12;
const URI_QUERY: u16 = 15;
const BLOCK1: u16 = 27;

/// What arrives in one datagram, at most. RFC 7252 asks for no more than this.
const MAX_DATAGRAM: usize = 1500;
/// What block-wise transfer can add up to.
const MAX_BODY: usize = 1 << 20;
/// How long an answer is kept for a repeat: RFC 7252's EXCHANGE_LIFETIME.
const REMEMBER: Duration = Duration::from_secs(247);
/// Answers kept for repeats, and transfers and observers held.
const MAX_REMEMBERED: usize = 20_000;
const MAX_TRANSFERS: usize = 1024;
const MAX_OBSERVERS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Coap {
    pub kind: u8,
    pub code: u8,
    pub id: u16,
    pub token: Vec<u8>,
    pub options: Vec<(u16, Vec<u8>)>,
    pub payload: Vec<u8>,
}

fn nibble(extra: &mut &[u8], n: u8) -> Option<u16> {
    match n {
        0..=12 => Some(u16::from(n)),
        13 => {
            let (b, rest) = extra.split_first()?;
            *extra = rest;
            Some(u16::from(*b) + 13)
        }
        14 => {
            if extra.len() < 2 {
                return None;
            }
            let v = u16::from_be_bytes([extra[0], extra[1]]);
            *extra = &extra[2..];
            v.checked_add(269)
        }
        _ => None,
    }
}

impl Coap {
    pub fn parse(bytes: &[u8]) -> Option<Coap> {
        let [first, code, id_hi, id_lo, rest @ ..] = bytes else { return None };
        if first >> 6 != 1 {
            return None;
        }
        let tokens = usize::from(first & 0x0f);
        if tokens > 8 || rest.len() < tokens {
            return None;
        }
        let (token, mut rest) = rest.split_at(tokens);
        let mut options = Vec::new();
        let mut number = 0u16;
        let mut payload = Vec::new();
        while let Some((byte, tail)) = rest.split_first() {
            if *byte == 0xff {
                // A marker with nothing after it is a mistake, not an empty message.
                if tail.is_empty() {
                    return None;
                }
                payload = tail.to_vec();
                break;
            }
            let mut extra = tail;
            let delta = nibble(&mut extra, byte >> 4)?;
            let length = usize::from(nibble(&mut extra, byte & 0x0f)?);
            number = number.checked_add(delta)?;
            if extra.len() < length {
                return None;
            }
            let (value, tail) = extra.split_at(length);
            options.push((number, value.to_vec()));
            rest = tail;
        }
        Some(Coap { kind: (first >> 4) & 3, code: *code, id: u16::from_be_bytes([*id_hi, *id_lo]), token: token.to_vec(), options, payload })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![0x40 | (self.kind << 4) | self.token.len() as u8, self.code];
        out.extend_from_slice(&self.id.to_be_bytes());
        out.extend_from_slice(&self.token);
        let mut options = self.options.clone();
        options.sort_by_key(|o| o.0);
        let mut last = 0u16;
        for (number, value) in &options {
            let piece = |n: u16, extra: &mut Vec<u8>| -> u8 {
                match n {
                    0..=12 => n as u8,
                    13..=268 => {
                        extra.push((n - 13) as u8);
                        13
                    }
                    _ => {
                        extra.extend_from_slice(&(n - 269).to_be_bytes());
                        14
                    }
                }
            };
            let mut extra = Vec::new();
            let delta = piece(number - last, &mut extra);
            let length = piece(value.len() as u16, &mut extra);
            out.push(delta << 4 | length);
            out.extend_from_slice(&extra);
            out.extend_from_slice(value);
            last = *number;
        }
        if !self.payload.is_empty() {
            out.push(0xff);
            out.extend_from_slice(&self.payload);
        }
        out
    }

    fn path(&self) -> Vec<&str> {
        self.options.iter().filter(|o| o.0 == URI_PATH).filter_map(|o| std::str::from_utf8(&o.1).ok()).collect()
    }

    fn query(&self, key: &str) -> Option<&str> {
        self.options.iter().filter(|o| o.0 == URI_QUERY).find_map(|o| std::str::from_utf8(&o.1).ok()?.strip_prefix(key)?.strip_prefix('='))
    }

    fn option(&self, number: u16) -> Option<&[u8]> {
        self.options.iter().find(|o| o.0 == number).map(|o| o.1.as_slice())
    }

    /// The answer to this: in the same exchange if it was confirmable, and under its token.
    fn answer(&self, code: u8, payload: Vec<u8>) -> Coap {
        Coap { kind: if self.kind == CON { ACK } else { NON }, code, id: self.id, token: self.token.clone(), options: Vec::new(), payload }
    }
}

fn uint(mut n: u32) -> Vec<u8> {
    let mut out = Vec::new();
    while n > 0 {
        out.insert(0, (n & 0xff) as u8);
        n >>= 8;
    }
    out
}

fn number(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |n, b| (n << 8) | u32::from(*b))
}

/// A device's transfer in pieces, as RFC 7959 describes.
struct Transfer {
    body: Vec<u8>,
    next: u32,
    at: Instant,
}

#[derive(Default)]
struct Memory {
    answers: HashMap<(SocketAddr, u16), (Instant, Vec<u8>)>,
    order: VecDeque<(Instant, (SocketAddr, u16))>,
    /// By who is sending and where to, because RFC 7959 lets each block have a token of its own.
    transfers: HashMap<(SocketAddr, String), Transfer>,
    observers: HashMap<(SocketAddr, Vec<u8>), u64>,
}

impl Memory {
    fn repeat(&self, peer: SocketAddr, id: u16) -> Option<&Vec<u8>> {
        self.answers.get(&(peer, id)).filter(|(at, _)| at.elapsed() < REMEMBER).map(|(_, a)| a)
    }

    fn keep(&mut self, peer: SocketAddr, id: u16, answer: Vec<u8>) {
        let now = Instant::now();
        while let Some((at, key)) = self.order.front().copied() {
            if now.duration_since(at) < REMEMBER && self.order.len() < MAX_REMEMBERED {
                break;
            }
            self.order.pop_front();
            if self.answers.get(&key).is_some_and(|(kept, _)| *kept == at) {
                self.answers.remove(&key);
            }
        }
        self.answers.insert((peer, id), (now, answer));
        self.order.push_back((now, (peer, id)));
    }
}

pub struct CoapServer {
    pub address: String,
    pub hub: Arc<Hub>,
    pub socket: Mutex<Option<Arc<UdpSocket>>>,
}

/// The block option's parts: number, whether more follows, and the size of a block.
fn block(value: &[u8]) -> (u32, bool, usize) {
    let v = number(value);
    (v >> 4, v & 8 != 0, 16usize << (v & 7).min(6))
}

/// What pieces of a body add up to so far, or the answer that ends the matter.
fn assemble(memory: &mut Memory, peer: SocketAddr, m: &Coap, value: &[u8]) -> Result<Option<Vec<u8>>, Coap> {
    let (num, more, _) = block(value);
    let key = (peer, m.path().join("/"));
    memory.transfers.retain(|_, t| t.at.elapsed() < REMEMBER);
    if num == 0 {
        if memory.transfers.len() >= MAX_TRANSFERS {
            return Err(m.answer(UNAVAILABLE, Vec::new()));
        }
        memory.transfers.insert(key.clone(), Transfer { body: Vec::new(), next: 0, at: Instant::now() });
    }
    let Some(t) = memory.transfers.get_mut(&key) else { return Err(m.answer(INCOMPLETE, Vec::new())) };
    if t.next != num {
        memory.transfers.remove(&key);
        return Err(m.answer(INCOMPLETE, Vec::new()));
    }
    if t.body.len() + m.payload.len() > MAX_BODY {
        memory.transfers.remove(&key);
        return Err(m.answer(TOO_LARGE, Vec::new()));
    }
    t.body.extend_from_slice(&m.payload);
    t.next += 1;
    if more {
        let mut piece = m.answer(CONTINUE, Vec::new());
        piece.options.push((BLOCK1, value.to_vec()));
        return Err(piece);
    }
    Ok(memory.transfers.remove(&key).map(|t| t.body))
}

/// Keeps a device told of what it is watching, for as long as it wants to know.
fn watch(
    hub: Arc<Hub>,
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    token: Vec<u8>,
    session: u64,
    mut mailbox: mpsc::Receiver<crate::adapters::hub::Delivery>,
    stop: mpsc::Sender<Message>,
) {
    tokio::spawn(async move {
        let mut count = 1u32;
        let mut id = rand::random::<u16>();
        loop {
            let next = tokio::select! {
                _ = stop.closed() => None,
                d = mailbox.recv() => d,
            };
            let Some(d) = next else { break };
            count = (count + 1) & 0x00ff_ffff;
            id = id.wrapping_add(1);
            let mut body = d.body;
            // What a device watching for calls needs is the same as what HTTP gives it: which one this is.
            if let Some(call) = d.topic.strip_prefix("v1/devices/me/rpc/request/")
                && let Ok(mut v) = serde_json::from_slice::<Value>(&body)
            {
                v["id"] = call.parse::<u64>().map_or(json!(call), |n| json!(n));
                body = v.to_string().into_bytes();
            }
            let note =
                Coap { kind: NON, code: CONTENT, id, token: token.clone(), options: vec![(OBSERVE, uint(count)), (CONTENT_FORMAT, uint(50))], payload: body };
            if socket.send_to(&note.to_bytes(), peer).await.is_err() {
                break;
            }
        }
        hub.close(session);
    });
}

struct Served {
    hub: Arc<Hub>,
    tx: mpsc::Sender<Message>,
    socket: Arc<UdpSocket>,
    memory: Mutex<Memory>,
}

impl Served {
    async fn observe(&self, peer: SocketAddr, m: &Coap, device: &str, filter: &str) -> Coap {
        let key = (peer, m.token.clone());
        let mut memory = self.memory.lock().await;
        if m.option(OBSERVE).map(number) == Some(1) {
            if let Some(session) = memory.observers.remove(&key) {
                self.hub.close(session);
            }
            return m.answer(CONTENT, Vec::new());
        }
        if memory.observers.len() >= MAX_OBSERVERS {
            return m.answer(UNAVAILABLE, Vec::new());
        }
        if let Some(old) = memory.observers.get(&key) {
            self.hub.close(*old);
        }
        let (session, mailbox) = self.hub.open(device, "");
        self.hub.listen(session, filter);
        memory.observers.insert(key, session);
        watch(self.hub.clone(), self.socket.clone(), peer, m.token.clone(), session, mailbox, self.tx.clone());
        let mut answer = m.answer(CONTENT, Vec::new());
        answer.options.push((OBSERVE, uint(1)));
        answer
    }

    async fn say(&self, m: &Coap, device: &str, topic: &str, body: Vec<u8>) -> Coap {
        if self.hub.said(&self.tx, device, topic, body).await { m.answer(CHANGED, Vec::new()) } else { m.answer(UNAVAILABLE, Vec::new()) }
    }

    async fn answer(&self, peer: SocketAddr, m: &Coap) -> Coap {
        let path = m.path();
        if path == ["api", "v1", "provision"] && m.code == POST {
            let mut answer = m.answer(CONTENT, self.hub.joining(&self.tx, &m.payload).await.to_string().into_bytes());
            answer.options.push((CONTENT_FORMAT, uint(50)));
            return answer;
        }
        let ["api", "v1", token, rest @ ..] = path.as_slice() else { return m.answer(NOT_FOUND, Vec::new()) };
        let Some(device) = self.hub.who(&Proof { username: Some(token), ..Default::default() }) else { return m.answer(UNAUTHORIZED, Vec::new()) };
        let body = match m.option(BLOCK1) {
            None => m.payload.clone(),
            Some(value) => match assemble(&mut *self.memory.lock().await, peer, m, value) {
                Ok(Some(body)) => body,
                Ok(None) => return m.answer(INCOMPLETE, Vec::new()),
                Err(reply) => return reply,
            },
        };
        let is_json = serde_json::from_slice::<Value>(&body).is_ok();
        match (m.code, rest) {
            (POST | PUT, ["telemetry"]) if is_json => self.say(m, &device, "v1/devices/me/telemetry", body).await,
            (POST | PUT, ["attributes"]) if is_json => self.say(m, &device, "v1/devices/me/attributes", body).await,
            (POST | PUT, ["telemetry" | "attributes"]) => m.answer(BAD_REQUEST, Vec::new()),
            (POST, ["claim"]) => self.say(m, &device, "v1/devices/me/claim", if body.is_empty() { b"{}".to_vec() } else { body }).await,
            (POST, ["rpc", id]) => self.say(m, &device, &format!("v1/devices/me/rpc/response/{id}"), body).await,
            (POST, ["rpc"]) if is_json => {
                let id = self.hub.next_request();
                self.say(m, &device, &format!("v1/devices/me/rpc/request/{id}"), body).await
            }
            (GET, ["attributes"]) if m.option(OBSERVE).is_some() => self.observe(peer, m, &device, "v1/devices/me/attributes").await,
            (GET, ["rpc"]) if m.option(OBSERVE).is_some() => self.observe(peer, m, &device, "v1/devices/me/rpc/request/+").await,
            (GET, ["attributes"]) => {
                let found = self.hub.attributes(&device, m.query("clientKeys"), m.query("sharedKeys"));
                let mut answer = m.answer(CONTENT, found.to_string().into_bytes());
                answer.options.push((CONTENT_FORMAT, uint(50)));
                answer
            }
            (_, ["telemetry" | "attributes" | "rpc" | "claim", ..]) => m.answer(NOT_ALLOWED, Vec::new()),
            _ => m.answer(NOT_FOUND, Vec::new()),
        }
    }

    async fn take(&self, peer: SocketAddr, bytes: &[u8]) {
        let Some(m) = Coap::parse(bytes) else {
            // Something from a device that cannot be read, and is at least long enough to say which.
            if bytes.len() >= 4 && bytes[0] >> 6 == 1 && (bytes[0] >> 4) & 3 == CON {
                let reset = Coap { kind: RST, code: 0, id: u16::from_be_bytes([bytes[2], bytes[3]]), ..Default::default() };
                let _ = self.socket.send_to(&reset.to_bytes(), peer).await;
            }
            return;
        };
        match (m.kind, m.code) {
            // A reset says it no longer wants what it was watching.
            (RST, _) => {
                let mut memory = self.memory.lock().await;
                let gone: Vec<_> = memory.observers.keys().filter(|(p, _)| *p == peer).cloned().collect();
                for key in gone {
                    if let Some(session) = memory.observers.remove(&key) {
                        self.hub.close(session);
                    }
                }
            }
            // A ping: a device checking the gateway is there.
            (CON, 0) => {
                let reset = Coap { kind: RST, code: 0, id: m.id, ..Default::default() };
                let _ = self.socket.send_to(&reset.to_bytes(), peer).await;
            }
            (CON | NON, 1..=31) => {
                if let Some(again) = self.memory.lock().await.repeat(peer, m.id).cloned() {
                    let _ = self.socket.send_to(&again, peer).await;
                    return;
                }
                let mut reply = self.answer(peer, &m).await;
                // The last piece is answered with its own block option, which says the rest was taken.
                if let Some(value) = m.option(BLOCK1).filter(|_| reply.option(BLOCK1).is_none()) {
                    reply.options.push((BLOCK1, value.to_vec()));
                }
                let bytes = reply.to_bytes();
                self.memory.lock().await.keep(peer, m.id, bytes.clone());
                let _ = self.socket.send_to(&bytes, peer).await;
            }
            _ => {}
        }
    }
}

#[async_trait]
impl Transport for CoapServer {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let socket = Arc::new(UdpSocket::bind(&self.address).await.map_err(|e| format!("{}: {e}", self.address))?);
        *self.socket.lock().await = Some(socket.clone());
        let _ = ready.send(()).await;
        let served = Arc::new(Served { hub: self.hub.clone(), tx: tx.clone(), socket: socket.clone(), memory: Mutex::default() });
        let mut buffer = vec![0u8; MAX_DATAGRAM];
        loop {
            let (n, peer) = tokio::select! {
                _ = tx.closed() => return Ok(()),
                got = socket.recv_from(&mut buffer) => got.map_err(|e| e.to_string())?,
            };
            // On its own task, so that a full queue behind one device holds up no other.
            let (served, bytes) = (served.clone(), buffer[..n].to_vec());
            tokio::spawn(async move { served.take(peer, &bytes).await });
        }
    }

    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        self.hub.publish(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(path: &[&str], payload: &[u8]) -> Coap {
        Coap {
            kind: CON,
            code: POST,
            id: 7,
            token: vec![1, 2],
            options: path.iter().map(|p| (URI_PATH, p.as_bytes().to_vec())).collect(),
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn a_message_is_what_it_was_when_it_comes_back() {
        let m = post(&["api", "v1", "tok", "telemetry"], b"{}");
        assert_eq!(Coap::parse(&m.to_bytes()).unwrap(), m);
        let long = Coap {
            kind: NON,
            code: GET,
            id: 1,
            token: vec![],
            options: vec![(OBSERVE, vec![]), (URI_PATH, vec![b'a'; 300]), (BLOCK1, vec![1, 2, 3])],
            payload: vec![],
        };
        assert_eq!(Coap::parse(&long.to_bytes()).unwrap(), long);
    }

    #[test]
    fn it_reads_rfc_7252s_own_example() {
        // Figure 16: a confirmable GET with a token and one URI path, "temperature".
        let bytes = [&[0x44, 0x01, 0x7d, 0x34][..], &[0x20, 0xfa, 0x00, 0x00], &[0xbb], b"temperature"].concat();
        let m = Coap::parse(&bytes).unwrap();
        assert_eq!((m.kind, m.code, m.id, m.path()), (CON, GET, 0x7d34, vec!["temperature"]));
    }

    #[test]
    fn nothing_arriving_can_run_past_the_end() {
        let good = post(&["api", "v1", "tok", "telemetry"], b"{\"a\":1}").to_bytes();
        for n in 0..good.len() {
            let _ = Coap::parse(&good[..n]);
        }
        for i in 0..good.len() {
            for flip in [0x01, 0x10, 0x80, 0xff] {
                let mut bad = good.clone();
                bad[i] ^= flip;
                let _ = Coap::parse(&bad);
            }
        }
        for first in 0..=255u8 {
            let _ = Coap::parse(&[0x40, 1, 0, 0, first, 0xe1, 0xff, 0xff]);
        }
    }

    #[test]
    fn what_makes_no_sense_is_refused() {
        assert_eq!(Coap::parse(&[0x80, 1, 0, 0]), None, "version 2");
        assert_eq!(Coap::parse(&[0x49, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9]), None, "a token of nine");
        assert_eq!(Coap::parse(&[0x40, 1, 0, 0, 0xff]), None, "a marker with nothing after it");
        assert_eq!(Coap::parse(&[0x40, 1, 0, 0, 0xf1, 0]), None, "a reserved option");
        assert_eq!(Coap::parse(&[0x40, 1]), None);
    }

    #[test]
    fn a_block_says_where_it_is() {
        assert_eq!(block(&[0x0e]), (0, true, 1024), "first of 1024, with more");
        assert_eq!(block(&[0x16]), (1, false, 1024), "second and last");
    }

    #[test]
    fn pieces_add_up_in_order_or_not_at_all() {
        let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let mut memory = Memory::default();
        let piece = |n: u8, more: bool, data: &[u8]| Coap { payload: data.to_vec(), ..post(&["x"], b"") }.with_block(n, more);
        let first = piece(0, true, b"ab");
        assert!(matches!(assemble(&mut memory, peer, &first, first.option(BLOCK1).unwrap()), Err(r) if r.code == CONTINUE));
        let last = piece(1, false, b"cd");
        assert_eq!(assemble(&mut memory, peer, &last, last.option(BLOCK1).unwrap()).unwrap(), Some(b"abcd".to_vec()));
        let skipped = piece(3, false, b"x");
        assert!(matches!(assemble(&mut memory, peer, &skipped, skipped.option(BLOCK1).unwrap()), Err(r) if r.code == INCOMPLETE), "one out of the blue");
    }

    impl Coap {
        fn with_block(mut self, n: u8, more: bool) -> Coap {
            self.options.push((BLOCK1, vec![n << 4 | u8::from(more) << 3 | 6]));
            self
        }
    }

    #[test]
    fn an_answer_is_kept_for_a_repeat() {
        let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let mut memory = Memory::default();
        memory.keep(peer, 7, vec![1]);
        assert_eq!(memory.repeat(peer, 7), Some(&vec![1]));
        assert_eq!(memory.repeat(peer, 8), None);
        assert_eq!(memory.repeat("127.0.0.1:2".parse().unwrap(), 7), None, "the same id from someone else is someone else's");
    }

    #[test]
    fn what_is_kept_has_a_limit() {
        let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let mut memory = Memory::default();
        for i in 0..(MAX_REMEMBERED + 100) {
            memory.keep(peer, (i % 65536) as u16, vec![]);
        }
        assert!(memory.order.len() <= MAX_REMEMBERED + 1);
    }
}
