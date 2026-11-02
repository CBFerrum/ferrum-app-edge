//! An LwM2M server: constrained devices register here over CoAP, and are read, written and watched
//! by object, instance and resource. A device is known by its endpoint name, and it has to have a
//! record. Plain, nothing proves it is who it says, so keep that on a private network, as a cellular
//! APN is. `listen+lwm2ms` is DTLS, and a device has to register under the identity it proved.
//!
//! Writes go as TLV, the one format LwM2M 1.0 requires of every device, and reads ask for plain text.
//!
//! `listen+lwm2m://0.0.0.0:5683`, with `subscribe: [/3303/0/5700]` for what to watch on each device.

use crate::adapter::Failure;
use crate::adapters::coap::Coap;
use crate::adapters::dtls::Wire;
use crate::adapters::hub::Hub;
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use tokio::sync::{Mutex, mpsc};

const CON: u8 = 0;
const ACK: u8 = 2;
const GET: u8 = 1;
const POST: u8 = 2;
const PUT: u8 = 3;
const DELETE: u8 = 4;
const CREATED: u8 = 0x41;
const DELETED: u8 = 0x42;
const CHANGED: u8 = 0x44;
const BAD: u8 = 0x80;
const FORBIDDEN: u8 = 0x83;
const OBSERVE: u16 = 6;
const LOCATION: u16 = 8;
const URI_PATH: u16 = 11;
const ACCEPT: u16 = 17;
const CONTENT_FORMAT: u16 = 12;
/// Plain text, for asking for a single resource's value.
const TEXT: u8 = 0;
/// `application/vnd.oma.lwm2m+tlv`.
const TLV: [u8; 2] = [0x2d, 0x16];

/// One resource as TLV: a whole number in as few bytes as it takes, a float in eight, text as it is.
pub fn tlv(resource: u16, value: &Value) -> Option<Vec<u8>> {
    let held = match value {
        Value::Bool(b) => vec![u8::from(*b)],
        Value::String(s) => s.clone().into_bytes(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => {
                let bytes = i.to_be_bytes();
                let skip = [7, 6, 4, 0].into_iter().find(|&at| i64::from_be_bytes(sign_extended(&bytes[at..])) == i)?;
                bytes[skip..].to_vec()
            }
            None => n.as_f64()?.to_be_bytes().to_vec(),
        },
        _ => return None,
    };
    // A resource with a value, its id in one or two bytes, and its length in the type or after the id.
    let (wide, id) = if resource > 255 { (0x20, resource.to_be_bytes().to_vec()) } else { (0, vec![resource as u8]) };
    let mut out = match held.len() {
        n @ 0..=7 => vec![0xc0 | wide | n as u8],
        _ if held.len() <= 0xff => vec![0xc8 | wide],
        _ if held.len() <= 0xffff => vec![0xd0 | wide],
        _ => return None,
    };
    out.extend(id);
    match held.len() {
        0..=7 => {}
        n if n <= 0xff => out.push(n as u8),
        n => out.extend((n as u16).to_be_bytes()),
    }
    out.extend(held);
    Some(out)
}

fn sign_extended(bytes: &[u8]) -> [u8; 8] {
    let fill = if bytes[0] & 0x80 != 0 { 0xff } else { 0 };
    let mut whole = [fill; 8];
    whole[8 - bytes.len()..].copy_from_slice(bytes);
    whole
}

/// What a device registered with.
pub fn registration(m: &Coap) -> Option<String> {
    (m.path() == ["rd"] && m.code == POST).then(|| m.query("ep").map(str::to_string)).flatten()
}

/// What a resource's text means, if it means a number.
pub fn value(text: &[u8]) -> Value {
    let text = String::from_utf8_lossy(text);
    text.parse::<f64>().map_or_else(|_| json!(text), |n| json!(n))
}

#[derive(Default)]
struct Registered {
    /// By endpoint: where it is, and the id it registered under.
    endpoints: HashMap<String, (SocketAddr, u16)>,
    by_id: HashMap<u16, String>,
    /// What an answer's token was asking about: `{endpoint}{path}`.
    asked: HashMap<Vec<u8>, String>,
}

pub struct Lwm2m {
    pub address: String,
    pub watch: Vec<String>,
    pub hub: Arc<Hub>,
    secure: bool,
    socket: Mutex<Option<Wire>>,
    registered: Arc<Mutex<Registered>>,
    next: AtomicU16,
}

impl Lwm2m {
    pub fn new(address: String, watch: &[String], hub: Arc<Hub>, secure: bool) -> Lwm2m {
        Lwm2m { address, watch: watch.to_vec(), hub, secure, socket: Mutex::new(None), registered: Arc::default(), next: AtomicU16::new(rand::random()) }
    }

    fn request(&self, code: u8, path: &str, observe: bool, payload: Vec<u8>) -> Coap {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut options: Vec<(u16, Vec<u8>)> = path.split('/').filter(|p| !p.is_empty()).map(|p| (URI_PATH, p.as_bytes().to_vec())).collect();
        if observe {
            options.insert(0, (OBSERVE, Vec::new()));
        }
        if code == GET {
            options.push((ACCEPT, vec![TEXT]));
        } else if !payload.is_empty() {
            options.push((CONTENT_FORMAT, TLV.to_vec()));
        }
        options.sort_by_key(|(n, _)| *n);
        Coap { kind: CON, code, id, token: id.to_be_bytes().to_vec(), options, payload }
    }

    async fn ask(&self, endpoint: &str, peer: SocketAddr, request: Coap, path: &str) -> Result<(), String> {
        let socket = self.socket.lock().await.clone().ok_or("not listening")?;
        self.registered.lock().await.asked.insert(request.token.clone(), format!("{endpoint}{path}"));
        socket.send_to(&request.to_bytes(), peer).await.map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[async_trait]
impl Transport for Lwm2m {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let socket = if self.secure { Wire::secured(&self.address, self.hub.clone()).await? } else { Wire::plain(&self.address).await? };
        *self.socket.lock().await = Some(socket.clone());
        let _ = ready.send(()).await;
        let mut buffer = vec![0u8; 1500];
        loop {
            let (n, peer) = tokio::select! {
                () = tx.closed() => return Ok(()),
                got = socket.recv_from(&mut buffer) => got.map_err(|e| e.to_string())?,
            };
            let Some(m) = Coap::parse(&buffer[..n]) else { continue };
            if (1..=31).contains(&m.code) {
                let path = m.path();
                let answer = match (m.code, path.as_slice()) {
                    (POST, ["rd"]) => {
                        // Over DTLS, who it proved it was is who it may say it is.
                        let vouched = |e: &String| !socket.encrypted() || socket.identity(peer).as_ref() == Some(e);
                        let Some(endpoint) = registration(&m).filter(|e| self.hub.known(e) && vouched(e)) else {
                            let reply = m.answer(if registration(&m).is_some() { FORBIDDEN } else { BAD }, Vec::new());
                            let _ = socket.send_to(&reply.to_bytes(), peer).await;
                            continue;
                        };
                        let id = self.next.fetch_add(1, Ordering::Relaxed);
                        {
                            let mut r = self.registered.lock().await;
                            r.endpoints.insert(endpoint.clone(), (peer, id));
                            r.by_id.insert(id, endpoint.clone());
                        }
                        let mut reply = m.answer(CREATED, Vec::new());
                        reply.options.extend([(LOCATION, b"rd".to_vec()), (LOCATION, id.to_string().into_bytes())]);
                        let _ = socket.send_to(&reply.to_bytes(), peer).await;
                        let up = json!({"objects": String::from_utf8_lossy(&m.payload)}).to_string().into_bytes();
                        let _ = tx.send(Message { topic: format!("{endpoint}/registered"), body: up, status: None, from: Some(endpoint.clone()) }).await;
                        for path in &self.watch {
                            let request = self.request(GET, path, true, Vec::new());
                            let _ = self.ask(&endpoint, peer, request, path).await;
                        }
                        continue;
                    }
                    (POST, ["rd", id]) => {
                        let mut r = self.registered.lock().await;
                        let known = id.parse::<u16>().ok().and_then(|id| r.by_id.get(&id).cloned());
                        match known {
                            Some(endpoint) => {
                                // A device behind a NAT moves, and this is how that is learned.
                                if let Some(place) = r.endpoints.get_mut(&endpoint) {
                                    place.0 = peer;
                                }
                                m.answer(CHANGED, Vec::new())
                            }
                            None => m.answer(0x84, Vec::new()),
                        }
                    }
                    (DELETE, ["rd", id]) => {
                        let mut r = self.registered.lock().await;
                        if let Some(endpoint) = id.parse::<u16>().ok().and_then(|id| r.by_id.remove(&id)) {
                            r.endpoints.remove(&endpoint);
                        }
                        m.answer(DELETED, Vec::new())
                    }
                    _ => m.answer(0x84, Vec::new()),
                };
                let _ = socket.send_to(&answer.to_bytes(), peer).await;
                continue;
            }
            if m.kind == CON {
                let ack = Coap { kind: ACK, code: 0, id: m.id, ..Default::default() };
                let _ = socket.send_to(&ack.to_bytes(), peer).await;
            }
            let Some(asked) = self.registered.lock().await.asked.get(&m.token).cloned() else { continue };
            let Some((endpoint, _)) = asked.split_once('/') else { continue };
            let body = if m.code >= 0x80 { json!({"error": format!("{}.{:02}", m.code >> 5, m.code & 0x1f)}) } else { json!({"value": value(&m.payload)}) };
            let message = Message { topic: asked.clone(), body: body.to_string().into_bytes(), status: None, from: Some(endpoint.to_string()) };
            if tx.send(message).await.is_err() {
                return Ok(());
            }
        }
    }

    /// `node` here is `{endpoint}/3/0/4`. Reading comes back as anything from that device does.
    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        let Out::Node { node, value } = out else { return Err(Failure::NotSent("this takes a node".into())) };
        let (endpoint, path) = node.split_once('/').ok_or_else(|| Failure::NotSent(format!("{node}: say the device and the path")))?;
        let path = format!("/{path}");
        let peer = self.registered.lock().await.endpoints.get(endpoint).map(|e| e.0);
        let peer = peer.ok_or_else(|| Failure::NotSent(format!("{endpoint} has not registered")))?;
        let request = match &value {
            None => self.request(GET, &path, false, Vec::new()),
            // Nothing to write means execute: POST on a resource, which is how LwM2M runs one.
            Some(Value::Null) => self.request(POST, &path, false, Vec::new()),
            Some(written) => {
                let wrong = || Failure::NotSent(format!("{node}: a resource takes a number, text or true or false"));
                let resource = path.rsplit('/').next().and_then(|r| r.parse().ok()).ok_or_else(wrong)?;
                self.request(PUT, &path, false, tlv(resource, written).ok_or_else(wrong)?)
            }
        };
        self.ask(endpoint, peer, request, &path).await.map_err(Failure::NotSent)?;
        Ok(None)
    }

    fn listening(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_written_as_the_specification_has_it() {
        assert_eq!(tlv(1, &json!(5)), Some(vec![0xc1, 1, 5]));
        assert_eq!(tlv(1, &json!(-1)), Some(vec![0xc1, 1, 0xff]));
        assert_eq!(tlv(1, &json!(300)), Some(vec![0xc2, 1, 0x01, 0x2c]));
        assert_eq!(tlv(1, &json!(200)), Some(vec![0xc2, 1, 0, 200]), "200 in one byte would read as negative");
        assert_eq!(tlv(1, &json!(70_000)), Some(vec![0xc4, 1, 0, 1, 0x11, 0x70]));
        assert_eq!(tlv(1, &json!(5_000_000_000i64)).map(|v| v.len()), Some(11), "a byte each for its type, id and length, and eight");
        assert_eq!(tlv(1, &json!(true)), Some(vec![0xc1, 1, 1]));
        assert_eq!(tlv(1, &json!("+05:30")), Some([&[0xc6, 1][..], b"+05:30"].concat()));
        assert_eq!(tlv(5700, &json!(1)), Some(vec![0xe1, 0x16, 0x44, 1]), "an id past a byte takes two");
        assert_eq!(tlv(1, &json!(2.5)), Some([&[0xc8, 1, 8][..], &2.5f64.to_be_bytes()].concat()), "eight bytes go after the id");
        let long = "x".repeat(300);
        assert_eq!(tlv(1, &json!(long)).map(|v| v[..4].to_vec()), Some(vec![0xd0, 1, 0x01, 0x2c]));
        assert_eq!(tlv(1, &json!(["a"])), None);
    }

    #[test]
    fn a_resource_is_a_number_or_it_is_text() {
        assert_eq!(value(b"21.5"), json!(21.5));
        assert_eq!(value(b"Acme"), json!("Acme"));
    }

    #[test]
    fn a_registration_says_who() {
        let m = Coap { code: POST, options: vec![(URI_PATH, b"rd".to_vec()), (15, b"ep=meter-1".to_vec()), (15, b"lt=300".to_vec())], ..Default::default() };
        assert_eq!(registration(&m).as_deref(), Some("meter-1"));
        let other = Coap { code: GET, options: vec![(URI_PATH, b"rd".to_vec())], ..Default::default() };
        assert_eq!(registration(&other), None);
    }
}
