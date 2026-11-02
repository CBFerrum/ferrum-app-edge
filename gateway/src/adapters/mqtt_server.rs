//! MQTT for devices that call in: enough of a server for a device to prove who it is, say things and
//! hear what is for it. It is not a broker. Nothing passes from one device to another, nothing is
//! retained, and a device that needs a broker's other talents gets Mosquitto beside the gateway.
//!
//! The codec is written here, for 3.1, 3.1.1 and 5.0, because what arrives on this port comes from
//! anything at all, and a server that panics on a bad packet takes every device with it. Nothing is
//! trusted: lengths are checked before they are read, and what makes no sense ends that connection.

use crate::adapter::Failure;
use crate::adapters::hub::{Hub, Proof};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use ferrum_edge_contract::device_api;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;

/// Largest packet taken.
const MAX_PACKET: usize = 1 << 20;
/// Connections held. Past this one is turned away, not queued.
const MAX_SESSIONS: usize = 10_000;
/// How long a connection gets to say who it is.
const HELLO: Duration = Duration::from_secs(10);
/// How long a device that asked for no keepalive can stay silent.
const IDLE: Duration = Duration::from_secs(3600);

const CONNECT: u8 = 1;
const PUBLISH: u8 = 3;
const PUBACK: u8 = 4;
const PUBREC: u8 = 5;
const PUBREL: u8 = 6;
const PUBCOMP: u8 = 7;
const SUBSCRIBE: u8 = 8;
const UNSUBSCRIBE: u8 = 10;
const PINGREQ: u8 = 12;
const DISCONNECT: u8 = 14;

/// Reads through a packet, and refuses to run past its end.
struct Cursor<'a> {
    bytes: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.bytes.len() {
            return None;
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Some(head)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()?;
        self.take(n as usize)
    }

    fn text(&mut self) -> Option<&'a str> {
        std::str::from_utf8(self.bytes()?).ok()
    }

    fn varint(&mut self) -> Option<usize> {
        let mut value = 0usize;
        for i in 0..4 {
            let b = self.byte()?;
            value |= usize::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    /// MQTT 5's properties: nothing here needs them, so they are stepped over.
    fn properties(&mut self) -> Option<()> {
        let n = self.varint()?;
        self.take(n).map(drop)
    }

    fn rest(&self) -> &'a [u8] {
        self.bytes
    }

    fn empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

#[derive(Debug, PartialEq)]
pub struct Connect<'a> {
    pub version: u8,
    pub keepalive: u16,
    pub client_id: &'a str,
    pub username: Option<&'a str>,
    pub password: Option<&'a [u8]>,
}

pub fn parse_connect(body: &[u8]) -> Option<Connect<'_>> {
    let mut c = Cursor { bytes: body };
    let name = c.text()?;
    let version = c.byte()?;
    if !matches!((name, version), ("MQIsdp", 3) | ("MQTT", 4 | 5)) {
        return None;
    }
    let flags = c.byte()?;
    let keepalive = c.u16()?;
    if version == 5 {
        c.properties()?;
    }
    let client_id = c.text()?;
    if flags & 0x04 != 0 {
        if version == 5 {
            c.properties()?;
        }
        c.bytes()?;
        c.bytes()?;
    }
    let username = if flags & 0x80 != 0 { Some(c.text()?) } else { None };
    let password = if flags & 0x40 != 0 { Some(c.bytes()?) } else { None };
    Some(Connect { version, keepalive, client_id, username, password })
}

#[derive(Debug, PartialEq)]
pub struct Publish<'a> {
    pub topic: &'a str,
    pub id: Option<u16>,
    pub qos: u8,
    pub payload: &'a [u8],
}

pub fn parse_publish(flags: u8, version: u8, body: &[u8]) -> Option<Publish<'_>> {
    let qos = (flags >> 1) & 3;
    if qos == 3 {
        return None;
    }
    let mut c = Cursor { bytes: body };
    let topic = c.text()?;
    if topic.is_empty() || topic.contains(['+', '#', '\0']) {
        return None;
    }
    let id = if qos > 0 { Some(c.u16()?) } else { None };
    if version == 5 {
        c.properties()?;
    }
    Some(Publish { topic, id, qos, payload: c.rest() })
}

pub fn parse_subscribe(version: u8, body: &[u8], unsubscribe: bool) -> Option<(u16, Vec<(&str, u8)>)> {
    let mut c = Cursor { bytes: body };
    let id = c.u16()?;
    if version == 5 {
        c.properties()?;
    }
    let mut filters = Vec::new();
    while !c.empty() {
        let filter = c.text()?;
        let qos = if unsubscribe { 0 } else { c.byte()? & 3 };
        filters.push((filter, qos));
    }
    (!filters.is_empty()).then_some((id, filters))
}

fn varint(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut b = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            return;
        }
    }
}

fn packet(first: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![first];
    varint(body.len(), &mut out);
    out.extend_from_slice(body);
    out
}

fn connack(version: u8, accepted: bool) -> Vec<u8> {
    let code = match (accepted, version) {
        (true, _) => 0,
        (false, 5) => 0x87,
        (false, _) => 5,
    };
    packet(0x20, &if version == 5 { vec![0, code, 0] } else { vec![0, code] })
}

fn ack(kind: u8, flags: u8, id: u16) -> Vec<u8> {
    packet(kind << 4 | flags, &id.to_be_bytes())
}

fn suback(version: u8, id: u16, codes: &[u8], first: u8) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    if version == 5 {
        body.push(0);
    }
    body.extend_from_slice(codes);
    packet(first, &body)
}

fn publish(version: u8, topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(topic.len() + payload.len() + 3);
    body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    if version == 5 {
        body.push(0);
    }
    body.extend_from_slice(payload);
    packet(0x30, &body)
}

/// One packet: its type, its flags and what follows. `None` is a connection that ended or a packet
/// that makes no sense, and neither is worth keeping the connection for.
async fn read_packet<R: AsyncRead + Unpin>(r: &mut R) -> Option<(u8, u8, Vec<u8>)> {
    let first = r.read_u8().await.ok()?;
    let mut len = 0usize;
    for i in 0..4 {
        let b = r.read_u8().await.ok()?;
        len |= usize::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            break;
        }
        if i == 3 {
            return None;
        }
    }
    if len > MAX_PACKET {
        return None;
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.ok()?;
    Some((first >> 4, first & 0xf, body))
}

pub struct MqttServer {
    pub address: String,
    pub tls: Option<TlsAcceptor>,
    pub hub: Arc<Hub>,
    /// The device API: a device says only what that lets it, and is asked for nothing else.
    pub api: bool,
}

struct Live {
    hub: Arc<Hub>,
    tx: mpsc::Sender<Message>,
    api: bool,
    count: AtomicUsize,
}

/// A device with no credentials yet: all it can do is add itself, on `/provision`, and hear how that
/// went on `/provision/response`.
async fn joining<R, W>(r: &mut R, w: &mut W, version: u8, keepalive: u16, live: &Live)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let idle = if keepalive == 0 { HELLO } else { Duration::from_secs(u64::from(keepalive) * 3 / 2 + 1).min(HELLO * 6) };
    loop {
        let Ok(Some((kind, flags, body))) = tokio::time::timeout(idle, read_packet(r)).await else { return };
        let reply = match kind {
            SUBSCRIBE => {
                let Some((id, filters)) = parse_subscribe(version, &body, false) else { return };
                let refused = if version == 5 { 0x87 } else { 0x80 };
                let codes: Vec<u8> = filters.iter().map(|(f, _)| if *f == "/provision/response" { 0 } else { refused }).collect();
                suback(version, id, &codes, 0x90)
            }
            PUBLISH => {
                let Some(p) = parse_publish(flags, version, &body) else { return };
                if p.topic != "/provision" {
                    return;
                }
                let answer = live.hub.joining(&live.tx, p.payload).await;
                if let Some(id) = p.id.filter(|_| p.qos == 1)
                    && w.write_all(&ack(PUBACK, 0, id)).await.is_err()
                {
                    return;
                }
                publish(version, "/provision/response", answer.to_string().as_bytes())
            }
            PINGREQ => vec![0xd0, 0],
            _ => return,
        };
        if w.write_all(&reply).await.is_err() {
            return;
        }
    }
}

async fn session<S>(stream: S, certificate: Option<String>, live: Arc<Live>)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut r, mut w) = tokio::io::split(stream);
    let Ok(Some((CONNECT, 0, body))) = tokio::time::timeout(HELLO, read_packet(&mut r)).await else { return };
    let Some(hello) = parse_connect(&body) else { return };
    let version = hello.version;
    let proof = Proof { client_id: hello.client_id, username: hello.username, password: hello.password, certificate: certificate.as_deref() };
    let Some(device) = live.hub.who(&proof) else {
        if live.api && hello.username == Some("provision") {
            let keepalive = hello.keepalive;
            if w.write_all(&connack(version, true)).await.is_ok() {
                joining(&mut r, &mut w, version, keepalive, &live).await;
            }
        } else {
            let _ = w.write_all(&connack(version, false)).await;
        }
        return;
    };
    let keepalive = hello.keepalive;
    let client = hello.client_id.to_string();
    let idle = if keepalive == 0 { IDLE } else { Duration::from_secs(u64::from(keepalive) * 3 / 2 + 1) };
    let (id, mut mailbox) = live.hub.open(&device, &client);
    if w.write_all(&connack(version, true)).await.is_err() {
        live.hub.close(id);
        return;
    }
    let online = Message { topic: device_api::session(&device, true), body: Vec::new(), status: None, from: Some(device.clone()) };
    let _ = live.tx.send(online).await;
    let mut held: HashSet<u16> = HashSet::new();
    loop {
        tokio::select! {
            _ = live.tx.closed() => break,
            next = mailbox.recv() => {
                // Nothing here means it was replaced or let go, and it has nothing more to come.
                let Some(d) = next else { break };
                if w.write_all(&publish(version, &d.topic, &d.body)).await.is_err() {
                    break;
                }
            }
            read = tokio::time::timeout(idle, read_packet(&mut r)) => {
                let Ok(Some((kind, flags, body))) = read else { break };
                let reply = match kind {
                    PUBLISH => {
                        let Some(p) = parse_publish(flags, version, &body) else { break };
                        if live.api && !device_api::may_say(p.topic) {
                            break;
                        }
                        let repeat = p.qos == 2 && p.id.is_some_and(|i| held.contains(&i));
                        if !repeat && !live.hub.said(&live.tx, &device, p.topic, p.payload.to_vec()).await {
                            break;
                        }
                        match (p.qos, p.id) {
                            (1, Some(i)) => ack(PUBACK, 0, i),
                            (2, Some(i)) => {
                                held.insert(i);
                                ack(PUBREC, 0, i)
                            }
                            _ => continue,
                        }
                    }
                    PUBREL => {
                        let Some(i) = (Cursor { bytes: &body }).u16() else { break };
                        held.remove(&i);
                        ack(PUBCOMP, 0, i)
                    }
                    SUBSCRIBE => {
                        let Some((packet_id, filters)) = parse_subscribe(version, &body, false) else { break };
                        let refused = if version == 5 { 0x87 } else { 0x80 };
                        let codes: Vec<u8> = filters
                            .iter()
                            .map(|(f, _)| if (!live.api || device_api::may_hear(f)) && live.hub.listen(id, f) { 0 } else { refused })
                            .collect();
                        suback(version, packet_id, &codes, 0x90)
                    }
                    UNSUBSCRIBE => {
                        let Some((packet_id, filters)) = parse_subscribe(version, &body, true) else { break };
                        for (f, _) in &filters {
                            live.hub.stop_listening(id, f);
                        }
                        let codes = if version == 5 { vec![0; filters.len()] } else { Vec::new() };
                        suback(version, packet_id, &codes, 0xb0)
                    }
                    PINGREQ => vec![0xd0, 0],
                    // A device acknowledging what it was sent, which is sent at most once anyway.
                    PUBACK | PUBREC | PUBCOMP => continue,
                    DISCONNECT => break,
                    _ => break,
                };
                if w.write_all(&reply).await.is_err() {
                    break;
                }
            }
        }
    }
    live.hub.close(id);
    if !live.hub.online(&device) {
        let _ = live.tx.try_send(Message { topic: device_api::session(&device, false), body: Vec::new(), status: None, from: Some(device.clone()) });
    }
}

/// SHA-256 of the certificate a device presented, if it did.
pub(crate) fn fingerprint(stream: &tokio_rustls::server::TlsStream<tokio::net::TcpStream>) -> Option<String> {
    stream.get_ref().1.peer_certificates()?.first().map(|c| crate::adapters::hub::hash(c.as_ref()))
}

#[async_trait]
impl Transport for MqttServer {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let listener = TcpListener::bind(&self.address).await.map_err(|e| format!("{}: {e}", self.address))?;
        let live = Arc::new(Live { hub: self.hub.clone(), tx: tx.clone(), api: self.api, count: AtomicUsize::new(0) });
        let _ = ready.send(()).await;
        loop {
            let (stream, _) = tokio::select! {
                _ = tx.closed() => return Ok(()),
                accepted = listener.accept() => accepted.map_err(|e| e.to_string())?,
            };
            if live.count.load(Ordering::Relaxed) >= MAX_SESSIONS {
                continue;
            }
            live.count.fetch_add(1, Ordering::Relaxed);
            let (live, tls) = (live.clone(), self.tls.clone());
            tokio::spawn(async move {
                match tls {
                    None => session(stream, None, live.clone()).await,
                    Some(acceptor) => {
                        // A handshake that stalls holds a slot, so it gets as long as a hello does.
                        if let Ok(Ok(secured)) = tokio::time::timeout(HELLO, acceptor.accept(stream)).await {
                            let certificate = fingerprint(&secured);
                            session(secured, certificate, live.clone()).await;
                        }
                    }
                }
                live.count.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }

    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        self.hub.publish(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_in_each_version() {
        // 3.1.1, clean session, user and password, client "c".
        let v4 = [&[0, 4][..], b"MQTT", &[4, 0b1100_0010, 0, 60], &[0, 1], b"c", &[0, 1], b"u", &[0, 1], b"p"].concat();
        assert_eq!(parse_connect(&v4), Some(Connect { version: 4, keepalive: 60, client_id: "c", username: Some("u"), password: Some(b"p") }));
        // 3.1 has another name.
        let v3 = [&[0, 6][..], b"MQIsdp", &[3, 2, 0, 0], &[0, 0]].concat();
        assert_eq!(parse_connect(&v3).unwrap().version, 3);
        // 5.0 has properties, and a will with properties of its own, to step over.
        let v5 = [&[0, 4][..], b"MQTT", &[5, 0b1000_0110, 0, 30], &[2, 33, 1], &[0, 1], b"c", &[1, 0], &[0, 1], b"t", &[0, 1], b"w", &[0, 1], b"u"].concat();
        assert_eq!(parse_connect(&v5), Some(Connect { version: 5, keepalive: 30, client_id: "c", username: Some("u"), password: None }));
    }

    #[test]
    fn a_hello_that_makes_no_sense_is_not_one() {
        assert_eq!(parse_connect(b""), None);
        assert_eq!(parse_connect(&[0, 200, 1]), None, "a length running past the end");
        assert_eq!(parse_connect(&[&[0, 4][..], b"MQTT", &[9, 0, 0, 0], &[0, 0]].concat()), None, "no such version");
        assert_eq!(parse_connect(&[&[0, 4][..], b"HTTP", &[4, 0, 0, 0], &[0, 0]].concat()), None);
        let cut_short = [&[0, 4][..], b"MQTT", &[4, 0b1000_0000, 0, 60], &[0, 0]].concat();
        assert_eq!(parse_connect(&cut_short), None, "says it has a user name and has none");
    }

    #[test]
    fn nothing_arriving_can_run_past_the_end() {
        // Every shorter version of a good packet, and every byte of it flipped, must be an answer, not a panic.
        let good =
            [&[0, 4][..], b"MQTT", &[5, 0b1100_0110, 0, 30], &[0], &[0, 1], b"c", &[0], &[0, 1], b"t", &[0, 1], b"w", &[0, 1], b"u", &[0, 1], b"p"].concat();
        for n in 0..good.len() {
            let _ = parse_connect(&good[..n]);
            let _ = parse_publish(2, 5, &good[..n]);
            let _ = parse_subscribe(5, &good[..n], false);
        }
        for i in 0..good.len() {
            for flip in [0x01, 0x80, 0xff] {
                let mut bad = good.clone();
                bad[i] ^= flip;
                let _ = parse_connect(&bad);
                let _ = parse_publish(4, 5, &bad);
                let _ = parse_subscribe(5, &bad, true);
            }
        }
    }

    #[test]
    fn a_publish_in_each_quality() {
        let zero = [&[0, 1][..], b"t", b"hello"].concat();
        assert_eq!(parse_publish(0, 4, &zero), Some(Publish { topic: "t", id: None, qos: 0, payload: b"hello" }));
        let one = [&[0, 1][..], b"t", &[0, 9], b"x"].concat();
        assert_eq!(parse_publish(2, 4, &one), Some(Publish { topic: "t", id: Some(9), qos: 1, payload: b"x" }));
        let five = [&[0, 1][..], b"t", &[0, 9], &[3, 1, 1, 1], b"x"].concat();
        assert_eq!(parse_publish(2, 5, &five).unwrap().payload, b"x", "properties stepped over");
    }

    #[test]
    fn a_publish_cannot_name_a_wildcard_or_quality_three() {
        assert_eq!(parse_publish(0, 4, &[&[0, 3][..], b"a/+"].concat()), None);
        assert_eq!(parse_publish(0, 4, &[&[0, 1][..], b"#"].concat()), None);
        assert_eq!(parse_publish(0, 4, &[0, 0]), None, "no topic");
        assert_eq!(parse_publish(6, 4, &[&[0, 1][..], b"t", &[0, 1]].concat()), None);
    }

    #[test]
    fn a_subscribe_lists_what_it_wants() {
        let body = [&[0, 7][..], &[0, 1], b"a", &[1], &[0, 1], b"b", &[0]].concat();
        assert_eq!(parse_subscribe(4, &body, false), Some((7, vec![("a", 1), ("b", 0)])));
        assert_eq!(parse_subscribe(4, &[0, 7], false), None, "one that wants nothing");
        let gone = [&[0, 7][..], &[0, 1], b"a"].concat();
        assert_eq!(parse_subscribe(4, &gone, true), Some((7, vec![("a", 0)])));
    }

    #[test]
    fn lengths_go_on_the_wire_as_the_standard_says() {
        let mut out = Vec::new();
        varint(321, &mut out);
        assert_eq!(out, [0xc1, 0x02], "MQTT 3.1.1, 2.2.3");
        assert_eq!(packet(0xd0, &[]), [0xd0, 0]);
    }

    #[test]
    fn a_refusal_says_so_in_its_own_version() {
        assert_eq!(connack(4, true), [0x20, 2, 0, 0]);
        assert_eq!(connack(4, false), [0x20, 2, 0, 5]);
        assert_eq!(connack(5, false), [0x20, 3, 0, 0x87, 0]);
    }

    #[tokio::test]
    async fn a_length_too_long_ends_it_before_anything_is_held() {
        let mut stream: &[u8] = &[0x30, 0xff, 0xff, 0xff, 0x7f];
        assert_eq!(read_packet(&mut stream).await, None);
        let mut endless: &[u8] = &[0x30, 0xff, 0xff, 0xff, 0xff, 0x01];
        assert_eq!(read_packet(&mut endless).await, None, "more than four length bytes");
    }
}
