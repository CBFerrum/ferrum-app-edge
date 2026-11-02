//! Devices that call a gateway and prove who they are. The directory says who may, built from what
//! the platform sent, so that checking a device needs no link and a lost gateway gives no secret away.
//! The hub knows who is connected now: whichever way a device came in, what is for it goes through here.

use crate::adapter::Failure;
use crate::adapters::transport::{Message, Out};
use ferrum_edge_contract::device_api;
use ferrum_edge_contract::driver::topic_matches;
use ferrum_edge_contract::edge::{Basic, DeviceLink, Provisioning};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// What can wait for a device before it counts as too slow to keep.
const MAILBOX: usize = 256;
/// Topic filters one session can hold.
const MAX_FILTERS: usize = 100;
/// Attributes kept per device, for it to ask for again.
const MAX_ATTRIBUTES: usize = 1000;
/// One piece of firmware, at most, and what a device gets by asking for no size in particular.
const MAX_CHUNK: usize = 1 << 20;

/// Devices that can add themselves in an hour. A leaked secret can fill a platform only this fast.
const MAX_JOINS: usize = 120;

pub fn hash(secret: &[u8]) -> String {
    hex::encode(Sha256::digest(secret))
}

#[derive(Default)]
struct Directory {
    settings: HashMap<String, Value>,
    tokens: HashMap<String, String>,
    basic: Vec<(Basic, String)>,
    certificates: HashMap<String, String>,
    via: HashMap<String, String>,
    shared: HashMap<String, BTreeMap<String, Value>>,
}

/// Firmware a device has been told about.
#[derive(Debug, Clone)]
pub struct Offered {
    pub title: String,
    pub version: String,
    pub path: PathBuf,
}

/// What a device said about itself when it connected.
#[derive(Debug, Default, Clone)]
pub struct Proof<'a> {
    pub client_id: &'a str,
    pub username: Option<&'a str>,
    pub password: Option<&'a [u8]>,
    /// SHA-256 of the certificate it presented, already proved to be its own by the handshake.
    pub certificate: Option<&'a str>,
}

impl Directory {
    fn of(devices: &[DeviceLink]) -> Directory {
        let mut d = Directory::default();
        for link in devices {
            let code = &link.code;
            d.settings.insert(code.clone(), link.settings.clone());
            if let Some(token) = &link.credentials.token {
                d.tokens.insert(token.clone(), code.clone());
            }
            if let Some(certificate) = &link.credentials.certificate {
                d.certificates.insert(certificate.to_lowercase(), code.clone());
            }
            if let Some(basic) = &link.credentials.basic {
                d.basic.push((basic.clone(), code.clone()));
            }
            if let Some(via) = &link.via {
                d.via.insert(code.clone(), via.clone());
            }
            d.shared.insert(code.clone(), link.shared.clone());
        }
        d
    }

    fn who(&self, proof: &Proof<'_>) -> Option<String> {
        if let Some(found) = proof.certificate.and_then(|c| self.certificates.get(c)) {
            return Some(found.clone());
        }
        // A token is the user name, with no password.
        if let (Some(user), None) = (proof.username, proof.password)
            && let Some(found) = self.tokens.get(&hash(user.as_bytes()))
        {
            return Some(found.clone());
        }
        let password = hash(proof.password?);
        let matches = |b: &Basic| {
            b.password == password
                && b.client_id.as_deref().is_none_or(|c| c == proof.client_id)
                && b.username.as_deref().is_none_or(|u| Some(u) == proof.username)
                // A password on its own proves too little: one of the two names must also match.
                && (b.client_id.is_some() || b.username.is_some())
        };
        self.basic.iter().find(|(b, _)| matches(b)).map(|(_, code)| code.clone())
    }
}

struct Session {
    device: String,
    client: String,
    filters: Vec<String>,
    tx: mpsc::Sender<Delivery>,
}

/// One message on its way to a device, under the topic it will hear.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub topic: String,
    pub body: Vec<u8>,
}

#[derive(Default)]
pub struct Hub {
    directory: RwLock<Directory>,
    sessions: Mutex<HashMap<u64, Session>>,
    next: AtomicU64,
    /// What each device last said about itself, for it to ask for after a restart of its own.
    reported: Mutex<HashMap<String, Map<String, Value>>>,
    provisioning: RwLock<Vec<Provisioning>>,
    /// Devices that added themselves here and the platform has not confirmed yet, by token hash.
    joined: Mutex<HashMap<String, String>>,
    joins: Mutex<Vec<Instant>>,
    firmware: Mutex<HashMap<String, Offered>>,
}

/// A device that has just added itself: what it gets, and what the platform is to be told.
#[derive(Debug, PartialEq)]
pub struct Joined {
    pub token: String,
    pub token_hash: String,
    pub definition: String,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hub")
    }
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hub {
    /// Who may connect changes with the platform's word. A device no longer listed, or whose
    /// credentials went, is gone at once: taking a device away must not wait for it to reconnect.
    pub fn set(&self, devices: &[DeviceLink]) {
        let mut directory = Directory::of(devices);
        let known: HashMap<&str, &DeviceLink> = devices.iter().map(|d| (d.code.as_str(), d)).collect();
        // One that joined stays known until the platform has confirmed it, or it would be turned away.
        let mut joined = locked(&self.joined);
        joined.retain(|_, code| !known.contains_key(code.as_str()));
        directory.tokens.extend(joined.iter().map(|(hash, code)| (hash.clone(), code.clone())));
        drop(joined);
        locked(&self.sessions).retain(|_, s| known.get(s.device.as_str()).is_some_and(|d| !d.credentials.is_empty()));
        *self.directory.write().unwrap_or_else(|e| e.into_inner()) = directory;
    }

    /// A device's key for DTLS: the hash of its token, which is what is held here.
    pub fn key_of(&self, device: &str) -> Option<Vec<u8>> {
        let directory = self.directory.read().unwrap_or_else(|e| e.into_inner());
        directory.tokens.iter().find(|(_, code)| *code == device).and_then(|(token, _)| hex::decode(token).ok())
    }

    pub fn who(&self, proof: &Proof<'_>) -> Option<String> {
        self.directory.read().unwrap_or_else(|e| e.into_inner()).who(proof)
    }

    pub fn known(&self, device: &str) -> bool {
        self.directory.read().unwrap_or_else(|e| e.into_inner()).settings.contains_key(device)
    }

    /// One of a device's settings, for a protocol that needs one to answer it.
    pub fn setting(&self, device: &str, key: &str) -> Option<Value> {
        self.directory.read().unwrap_or_else(|e| e.into_inner()).settings.get(device)?.get(key).cloned()
    }

    /// A session for a device. One client id has one session: a second one with the same id replaces
    /// the first, which is how a device that lost its connection without noticing comes back.
    pub fn open(&self, device: &str, client: &str) -> (u64, mpsc::Receiver<Delivery>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(MAILBOX);
        let mut sessions = locked(&self.sessions);
        sessions.retain(|_, s| !(s.device == device && s.client == client && !client.is_empty()));
        sessions.insert(id, Session { device: device.to_string(), client: client.to_string(), filters: Vec::new(), tx });
        (id, rx)
    }

    pub fn close(&self, id: u64) {
        locked(&self.sessions).remove(&id);
    }

    /// Whether this device has any session left.
    pub fn online(&self, device: &str) -> bool {
        locked(&self.sessions).values().any(|s| s.device == device)
    }

    /// False when it holds as many as it may.
    pub fn listen(&self, id: u64, filter: &str) -> bool {
        let filter = device_api::heard_by(filter);
        let mut sessions = locked(&self.sessions);
        let Some(s) = sessions.get_mut(&id) else { return false };
        if s.filters.iter().any(|f| f == filter) {
            return true;
        }
        if s.filters.len() >= MAX_FILTERS {
            return false;
        }
        s.filters.push(filter.to_string());
        true
    }

    pub fn stop_listening(&self, id: u64, filter: &str) {
        let filter = device_api::heard_by(filter);
        if let Some(s) = locked(&self.sessions).get_mut(&id) {
            s.filters.retain(|f| f != filter);
        }
    }

    /// Passes something on to whoever is listening for it, and says how many took it. A session that
    /// cannot keep up is dropped: holding its messages would cost everyone else.
    pub fn deliver(&self, topic: &str, body: &[u8]) -> usize {
        let mut gone = Vec::new();
        let mut taken = 0;
        let mut sessions = locked(&self.sessions);
        for (id, s) in sessions.iter() {
            let heard = device_api::as_heard(topic, &s.device);
            if !s.filters.iter().any(|f| topic_matches(f, &heard).is_some()) {
                continue;
            }
            match s.tx.try_send(Delivery { topic: heard, body: body.to_vec() }) {
                Ok(()) => taken += 1,
                Err(_) => gone.push(*id),
            }
        }
        for id in gone {
            sessions.remove(&id);
        }
        taken
    }

    pub fn allow_joining(&self, provisioning: Vec<Provisioning>) {
        *self.provisioning.write().unwrap_or_else(|e| e.into_inner()) = provisioning;
    }

    /// A device adding itself, with the key and secret of its kind. It gets a token of its own, made
    /// here, so this works with the link down. An existing device cannot be taken over this way.
    pub fn join(&self, code: &str, key: &str, secret: &str) -> Result<Joined, &'static str> {
        let definition = {
            let provisioning = self.provisioning.read().unwrap_or_else(|e| e.into_inner());
            let found = provisioning.iter().find(|p| p.key == key).filter(|p| p.secret == hash(secret.as_bytes())).ok_or("the key or the secret is wrong")?;
            found.definition.clone()
        };
        if code.trim().is_empty() || code.len() > 80 {
            return Err("a device needs a name of at most 80 characters");
        }
        {
            let mut joins = locked(&self.joins);
            joins.retain(|at| at.elapsed() < Duration::from_secs(3600));
            if joins.len() >= MAX_JOINS {
                return Err("too many devices have joined lately");
            }
            let directory = self.directory.read().unwrap_or_else(|e| e.into_inner());
            if directory.shared.contains_key(code) || locked(&self.joined).values().any(|c| c == code) {
                return Err("a device of that name already exists");
            }
            joins.push(Instant::now());
        }
        let token = hex::encode(rand::random::<[u8; 24]>());
        let token_hash = hash(token.as_bytes());
        locked(&self.joined).insert(token_hash.clone(), code.to_string());
        self.directory.write().unwrap_or_else(|e| e.into_inner()).tokens.insert(token_hash.clone(), code.to_string());
        Ok(Joined { token, token_hash, definition })
    }

    /// The device API's answer to a device adding itself, and the message that tells the platform.
    pub async fn joining(&self, tx: &mpsc::Sender<Message>, body: &[u8]) -> Value {
        let asked: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let text = |k: &str| asked[k].as_str().unwrap_or_default();
        match self.join(text("deviceName"), text("provisionDeviceKey"), text("provisionDeviceSecret")) {
            Ok(joined) => {
                let told = Message {
                    topic: device_api::joined(text("deviceName")),
                    body: json!({"token_hash": joined.token_hash, "definition": joined.definition}).to_string().into_bytes(),
                    status: None,
                    from: None,
                };
                let _ = tx.send(told).await;
                json!({"status": "SUCCESS", "credentialsType": "ACCESS_TOKEN", "credentialsValue": joined.token})
            }
            Err(why) => json!({"status": "FAILURE", "errorMsg": why}),
        }
    }

    /// Takes something a device said, under its own name, whichever way it came in. A request for
    /// attributes is answered here, because that needs nobody but this gateway. False means nothing is
    /// taking what devices say any more.
    pub async fn said(&self, tx: &mpsc::Sender<Message>, device: &str, topic: &str, body: Vec<u8>) -> bool {
        let topic = device_api::named(topic, device);
        if let Some(id) = topic.strip_prefix("v1/devices/").and_then(|t| t.strip_prefix(device)).and_then(|t| t.strip_prefix("/attributes/request/")) {
            let asked: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let found = self.attributes(device, asked["clientKeys"].as_str(), asked["sharedKeys"].as_str());
            self.deliver(&format!("v1/devices/{device}/attributes/response/{id}"), found.to_string().as_bytes());
            return true;
        }
        if let Some((request, chunk)) = device_api::firmware_asked(&topic) {
            let size = String::from_utf8_lossy(&body).trim().parse().unwrap_or(0);
            let piece = self.firmware_chunk(device, None, None, size, chunk).unwrap_or_default();
            self.deliver(&format!("v1/devices/{device}/fw/response/{request}/chunk/{chunk}"), &piece);
            return true;
        }
        tx.send(Message { topic, body, status: None, from: Some(device.to_string()) }).await.is_ok()
    }

    /// Asks something for a device that cannot stay to listen, and waits for the answer. `Err` means
    /// nothing is taking what devices say any more; `None`, that nothing answered in time.
    pub async fn asked(&self, tx: &mpsc::Sender<Message>, device: &str, body: Vec<u8>, patience: Duration) -> Result<Option<Vec<u8>>, ()> {
        let id = self.next_request();
        let (session, mut mailbox) = self.open(device, "");
        self.listen(session, &format!("v1/devices/me/rpc/response/{id}"));
        let taken = self.said(tx, device, &format!("v1/devices/me/rpc/request/{id}"), body).await;
        let answer = if taken { tokio::time::timeout(patience, mailbox.recv()).await.ok().flatten().map(|d| d.body) } else { None };
        self.close(session);
        if taken { Ok(answer) } else { Err(()) }
    }

    /// What a device may now fetch. It can fetch what it was told about, and nothing else.
    pub fn offer_firmware(&self, device: &str, firmware: Offered) {
        locked(&self.firmware).insert(device.to_string(), firmware);
    }

    /// A piece of what a device was offered, `size` bytes at chunk `chunk`. The title and version, if
    /// it says them, have to be the ones it was told. Past the end there is nothing, which is how it knows.
    pub fn firmware_chunk(&self, device: &str, title: Option<&str>, version: Option<&str>, size: usize, chunk: usize) -> Option<Vec<u8>> {
        let offered = locked(&self.firmware).get(device).cloned()?;
        if title.is_some_and(|t| t != offered.title) || version.is_some_and(|v| v != offered.version) {
            return None;
        }
        let size = if size == 0 { MAX_CHUNK } else { size.min(MAX_CHUNK) };
        let mut file = std::fs::File::open(&offered.path).ok()?;
        file.seek(SeekFrom::Start((chunk * size) as u64)).ok()?;
        let mut piece = Vec::with_capacity(size);
        file.take(size as u64).read_to_end(&mut piece).ok()?;
        Some(piece)
    }

    /// Sends what a definition published to whoever is listening for it. Nobody listening means it
    /// did not go, which is what makes it safe to try again.
    pub fn publish(&self, out: Out) -> Result<Option<Message>, Failure> {
        let Out::Publish { topic, body, .. } = out else { return Err(Failure::NotSent("a device hears what is published to it".into())) };
        match self.deliver(&topic, &body) {
            0 => Err(Failure::NotSent(format!("nothing connected is listening for {topic}"))),
            _ => Ok(None),
        }
    }

    /// An id for a device asking something where its transport gives none.
    pub fn next_request(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Keeps what a device said about itself, for when it asks.
    pub fn heard(&self, device: &str, key: &str, value: &Value) {
        let mut reported = locked(&self.reported);
        let own = reported.entry(device.to_string()).or_default();
        if own.len() < MAX_ATTRIBUTES || own.contains_key(key) {
            own.insert(key.to_string(), value.clone());
        }
    }

    /// Remembers a value a device was asked to take, for when it asks after a restart.
    pub fn shared(&self, device: &str, key: &str, value: &Value) {
        let mut directory = self.directory.write().unwrap_or_else(|e| e.into_inner());
        let shared = directory.shared.entry(device.to_string()).or_default();
        if shared.len() < MAX_ATTRIBUTES || shared.contains_key(key) {
            shared.insert(key.to_string(), value.clone());
        }
    }

    /// The device API's answer to a request for attributes: those named, or all of them.
    pub fn attributes(&self, device: &str, client_keys: Option<&str>, shared_keys: Option<&str>) -> Value {
        let wanted = |keys: Option<&str>, k: &str| keys.is_none_or(|list| list.split(',').any(|w| w.trim() == k));
        let client: Map<String, Value> = locked(&self.reported)
            .get(device)
            .map(|m| m.iter().filter(|(k, _)| wanted(client_keys, k)).map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let directory = self.directory.read().unwrap_or_else(|e| e.into_inner());
        let shared: Map<String, Value> = directory
            .shared
            .get(device)
            .map(|m| m.iter().filter(|(k, _)| wanted(shared_keys, k)).map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        json!({"client": client, "shared": shared})
    }

    /// Whether `device` is reached through `gateway`.
    pub fn behind(&self, device: &str, gateway: &str) -> bool {
        self.directory.read().unwrap_or_else(|e| e.into_inner()).via.get(device).is_some_and(|v| v == gateway)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrum_edge_contract::edge::Credentials;

    fn device(code: &str, credentials: Credentials) -> DeviceLink {
        DeviceLink { code: code.into(), definition: "d".into(), credentials, ..Default::default() }
    }

    fn hub() -> Hub {
        let hub = Hub::default();
        hub.set(&[
            device("TOKEN", Credentials { token: Some(hash(b"tok")), ..Default::default() }),
            device(
                "BASIC",
                Credentials { basic: Some(Basic { client_id: Some("cli".into()), username: Some("usr".into()), password: hash(b"pw") }), ..Default::default() },
            ),
            device(
                "USER",
                Credentials { basic: Some(Basic { client_id: None, username: Some("only-user".into()), password: hash(b"pw2") }), ..Default::default() },
            ),
            device("CERT", Credentials { certificate: Some("ABCD".into()), ..Default::default() }),
        ]);
        hub
    }

    #[test]
    fn a_token_is_a_user_name_with_no_password() {
        let h = hub();
        assert_eq!(h.who(&Proof { username: Some("tok"), ..Default::default() }).as_deref(), Some("TOKEN"));
        assert_eq!(h.who(&Proof { username: Some("wrong"), ..Default::default() }), None);
        assert_eq!(h.who(&Proof { username: Some("tok"), password: Some(b"anything"), ..Default::default() }), None, "with a password it is not a token");
    }

    #[test]
    fn a_password_goes_with_the_names_it_was_given_for() {
        let h = hub();
        let good = Proof { client_id: "cli", username: Some("usr"), password: Some(b"pw"), ..Default::default() };
        assert_eq!(h.who(&good).as_deref(), Some("BASIC"));
        assert_eq!(h.who(&Proof { client_id: "other", ..good.clone() }), None);
        assert_eq!(h.who(&Proof { username: Some("other"), ..good.clone() }), None);
        assert_eq!(h.who(&Proof { password: Some(b"wrong"), ..good.clone() }), None);
        assert_eq!(h.who(&Proof { client_id: "any", username: Some("only-user"), password: Some(b"pw2"), ..Default::default() }).as_deref(), Some("USER"));
        assert_eq!(h.who(&Proof::default()), None, "saying nothing proves nothing");
    }

    #[test]
    fn a_certificate_is_known_by_its_fingerprint() {
        let h = hub();
        assert_eq!(h.who(&Proof { certificate: Some("abcd"), ..Default::default() }).as_deref(), Some("CERT"));
        assert_eq!(h.who(&Proof { certificate: Some("ffff"), ..Default::default() }), None);
    }

    #[tokio::test]
    async fn only_what_is_for_a_device_reaches_it() {
        let h = hub();
        let (a, mut heard_a) = h.open("TOKEN", "a");
        let (b, mut heard_b) = h.open("BASIC", "b");
        assert!(h.listen(a, "v1/devices/me/rpc/request/+"));
        assert!(h.listen(b, "v1/devices/me/rpc/request/+"));
        assert_eq!(h.deliver("v1/devices/TOKEN/rpc/request/1", b"x"), 1);
        assert_eq!(heard_a.recv().await.unwrap(), Delivery { topic: "v1/devices/me/rpc/request/1".into(), body: b"x".to_vec() });
        assert!(heard_b.try_recv().is_err(), "the other device hears nothing");
        assert_eq!(h.deliver("v1/devices/NOBODY/rpc/request/1", b"x"), 0, "nobody took it, so it did not go");
    }

    #[test]
    fn a_device_taken_away_is_gone_at_once() {
        let h = hub();
        h.open("TOKEN", "a");
        assert!(h.online("TOKEN"));
        h.set(&[device("TOKEN", Credentials::default())]);
        assert!(!h.online("TOKEN"), "its credentials went");
        assert_eq!(h.who(&Proof { username: Some("tok"), ..Default::default() }), None);
    }

    #[test]
    fn a_client_id_has_one_session() {
        let h = hub();
        let (first, _keep) = h.open("TOKEN", "same");
        let (second, _keep2) = h.open("TOKEN", "same");
        assert!(!h.listen(first, "x"), "replaced");
        assert!(h.listen(second, "x"));
        let (other, _keep3) = h.open("TOKEN", "different");
        assert!(h.listen(other, "x"), "a second connection of its own is fine");
    }

    #[test]
    fn a_session_holds_so_many_filters() {
        let h = hub();
        let (id, _keep) = h.open("TOKEN", "a");
        for i in 0..MAX_FILTERS {
            assert!(h.listen(id, &format!("f/{i}")));
        }
        assert!(h.listen(id, "f/0"), "one it holds is no more");
        assert!(!h.listen(id, "one/too/many"));
    }

    #[tokio::test]
    async fn one_that_cannot_keep_up_is_let_go() {
        let h = hub();
        let (id, _not_reading) = h.open("TOKEN", "a");
        h.listen(id, "#");
        for _ in 0..MAILBOX {
            assert_eq!(h.deliver("t", b"x"), 1);
        }
        assert_eq!(h.deliver("t", b"x"), 0);
        assert!(!h.online("TOKEN"));
    }

    fn joinable() -> Hub {
        let h = hub();
        h.allow_joining(vec![Provisioning { definition: "devices".into(), key: "k".into(), secret: hash(b"s") }]);
        h
    }

    #[test]
    fn a_device_with_the_secret_of_its_kind_can_add_itself() {
        let h = joinable();
        let joined = h.join("NEW-1", "k", "s").unwrap();
        assert_eq!(joined.definition, "devices");
        assert_eq!(joined.token_hash, hash(joined.token.as_bytes()));
        assert_eq!(h.who(&Proof { username: Some(&joined.token), ..Default::default() }).as_deref(), Some("NEW-1"), "it can connect at once");
    }

    #[test]
    fn it_needs_the_key_and_the_secret() {
        let h = joinable();
        assert!(h.join("NEW-1", "k", "wrong").is_err());
        assert!(h.join("NEW-1", "wrong", "s").is_err());
        assert!(h.join("", "k", "s").is_err(), "a device needs a name");
        assert!(Hub::default().join("NEW-1", "k", "s").is_err(), "nobody allowed it");
    }

    #[test]
    fn no_device_can_be_taken_over_by_joining_as_it() {
        let h = joinable();
        assert!(h.join("TOKEN", "k", "s").is_err(), "that one exists");
        h.join("NEW-1", "k", "s").unwrap();
        assert!(h.join("NEW-1", "k", "s").is_err(), "and this one now does");
    }

    #[test]
    fn a_leaked_secret_adds_devices_only_so_fast() {
        let h = joinable();
        for i in 0..MAX_JOINS {
            h.join(&format!("D-{i}"), "k", "s").unwrap();
        }
        assert!(h.join("ONE-MORE", "k", "s").is_err());
    }

    #[test]
    fn one_that_joined_stays_until_the_platform_has_it() {
        let h = joinable();
        let joined = h.join("NEW-1", "k", "s").unwrap();
        let proof = Proof { username: Some(&joined.token), ..Default::default() };
        h.set(&[device("TOKEN", Credentials { token: Some(hash(b"tok")), ..Default::default() })]);
        assert_eq!(h.who(&proof).as_deref(), Some("NEW-1"), "word from the platform that predates it does not end it");
        h.set(&[device("NEW-1", Credentials { token: Some(joined.token_hash.clone()), ..Default::default() })]);
        assert_eq!(h.who(&proof).as_deref(), Some("NEW-1"));
        h.set(&[]);
        assert_eq!(h.who(&proof), None, "once it is the platform's, the platform decides");
    }

    #[test]
    fn a_device_can_ask_what_it_and_we_know() {
        let h = hub();
        h.heard("TOKEN", "firmware", &json!("1.2"));
        h.heard("TOKEN", "model", &json!("X"));
        h.shared("TOKEN", "limit", &json!(40));
        h.shared("TOKEN", "mode", &json!("eco"));
        assert_eq!(h.attributes("TOKEN", Some("firmware"), Some("limit, other")), json!({"client": {"firmware": "1.2"}, "shared": {"limit": 40}}));
        assert_eq!(h.attributes("TOKEN", None, None)["shared"].as_object().unwrap().len(), 2);
        assert_eq!(h.attributes("NOBODY", None, None), json!({"client": {}, "shared": {}}));
    }
}
