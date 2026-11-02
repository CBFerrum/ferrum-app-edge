//! An LwM2M server: constrained devices register here over CoAP, and are read, written and watched
//! by object, instance and resource. A device is known by its endpoint name, and it has to have a
//! record. There is no DTLS, so nothing here proves it is who it says: keep this on a private
//! network, as a cellular APN is, or put it behind something that does.
//!
//! `listen+lwm2m://0.0.0.0:5683`, with `subscribe: [/3303/0/5700]` for what to watch on each device.

use crate::adapter::Failure;
use crate::adapters::coap::Coap;
use crate::adapters::hub::Hub;
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use tokio::net::UdpSocket;
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
/// Plain text: a single resource's value, which every LwM2M version takes.
const TEXT: u8 = 0;

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
    socket: Mutex<Option<Arc<UdpSocket>>>,
    registered: Arc<Mutex<Registered>>,
    next: AtomicU16,
}

impl Lwm2m {
    pub fn new(address: String, watch: &[String], hub: Arc<Hub>) -> Lwm2m {
        Lwm2m { address, watch: watch.to_vec(), hub, socket: Mutex::new(None), registered: Arc::default(), next: AtomicU16::new(rand::random()) }
    }

    fn request(&self, code: u8, path: &str, observe: bool, payload: Vec<u8>) -> Coap {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut options: Vec<(u16, Vec<u8>)> = path.split('/').filter(|p| !p.is_empty()).map(|p| (URI_PATH, p.as_bytes().to_vec())).collect();
        if observe {
            options.insert(0, (OBSERVE, Vec::new()));
        }
        if code == GET {
            options.push((ACCEPT, vec![TEXT]));
        } else {
            options.push((12, vec![TEXT]));
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
        let socket = Arc::new(UdpSocket::bind(&self.address).await.map_err(|e| format!("{}: {e}", self.address))?);
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
                        let Some(endpoint) = registration(&m).filter(|e| self.hub.known(e)) else {
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
            Some(Value::String(s)) => self.request(PUT, &path, false, s.clone().into_bytes()),
            Some(other) => self.request(PUT, &path, false, other.to_string().into_bytes()),
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
