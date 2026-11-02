//! SNMP version 3, with the user-based security model: a user proves who it is with HMAC, and what is
//! said can be encrypted with AES. Authentication is SHA-1 or SHA-256, because that is what equipment
//! has, and a password never goes on the wire.
//!
//! `snmp://device:161?version=3&user=reader&auth={secret.a}&priv={secret.p}&auth_hash=sha256`

use crate::adapters::snmp::{BIND_NULL, GET, NEXT, OCTETS, REPORT, RESPONSE, SEQUENCE, SET, integer, oid_bytes, read, split_pdu, tlv};
use crate::adapters::tags::{ANSWER, Tags, Trouble, host_port, number, query};
use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use async_trait::async_trait;
use ferrum_edge_contract::tags::oid;
use hmac::{Hmac, Mac};
use serde_json::{Map, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio::net::UdpSocket;
use tokio::time::timeout;

const AUTH: u8 = 1;
const PRIVATE: u8 = 2;
const REPORTABLE: u8 = 4;
const USM: i64 = 3;
const MAX_WALK: usize = 5_000;
/// What a password is stretched over before it is hashed (RFC 3414, A.2).
const STRETCH: usize = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hash {
    Sha1,
    Sha256,
}

impl Hash {
    fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha1 => Sha1::digest(data).to_vec(),
            Hash::Sha256 => Sha256::digest(data).to_vec(),
        }
    }

    /// What goes on the wire: 96 bits of HMAC-SHA-1, or 192 of HMAC-SHA-256 (RFC 7860).
    fn tag_size(self) -> usize {
        match self {
            Hash::Sha1 => 12,
            Hash::Sha256 => 24,
        }
    }

    fn tag(self, key: &[u8], message: &[u8]) -> Vec<u8> {
        let mut out = match self {
            Hash::Sha1 => {
                let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key).expect("any length of key");
                mac.update(message);
                mac.finalize().into_bytes().to_vec()
            }
            Hash::Sha256 => {
                let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any length of key");
                mac.update(message);
                mac.finalize().into_bytes().to_vec()
            }
        };
        out.truncate(self.tag_size());
        out
    }
}

/// A password made into a key for one engine, so that a key lost from one device opens no other.
pub fn localized_key(hash: Hash, password: &str, engine: &[u8]) -> Vec<u8> {
    let stretched: Vec<u8> = password.bytes().cycle().take(STRETCH).collect();
    let ku = hash.digest(&stretched);
    hash.digest(&[&ku[..], engine, &ku[..]].concat())
}

/// AES in CFB mode, which runs the same forwards and, keyed the same, backwards (RFC 3826).
fn cfb(key: &[u8], iv: [u8; 16], data: &[u8], decrypting: bool) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(&key[..16]));
    let mut register = iv;
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let mut block = GenericArray::from(register);
        cipher.encrypt_block(&mut block);
        let result: Vec<u8> = chunk.iter().zip(block.iter()).map(|(a, b)| a ^ b).collect();
        // The next block's input is always what travels, whichever way this is going.
        register[..chunk.len()].copy_from_slice(if decrypting { chunk } else { &result });
        out.extend_from_slice(&result);
    }
    out
}

pub fn encrypted(key: &[u8], boots: i64, time: i64, salt: u64, plain: &[u8]) -> Vec<u8> {
    cfb(key, iv(boots, time, salt), plain, false)
}

pub fn decrypted(key: &[u8], boots: i64, time: i64, salt: u64, data: &[u8]) -> Vec<u8> {
    cfb(key, iv(boots, time, salt), data, true)
}

fn iv(boots: i64, time: i64, salt: u64) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&(boots as u32).to_be_bytes());
    out[4..8].copy_from_slice(&(time as u32).to_be_bytes());
    out[8..].copy_from_slice(&salt.to_be_bytes());
    out
}

#[derive(Default, Clone)]
struct Engine {
    id: Vec<u8>,
    boots: i64,
    time: i64,
}

pub struct Snmp3 {
    socket: UdpSocket,
    user: String,
    hash: Hash,
    auth: Option<String>,
    private: Option<String>,
    auth_key: Vec<u8>,
    priv_key: Vec<u8>,
    engine: Engine,
    id: i64,
    salt: u64,
}

fn number_of(body: &[u8]) -> i64 {
    body.iter().fold(if body.first().is_some_and(|b| b & 0x80 != 0) { -1 } else { 0 }, |n, b| n << 8 | i64::from(*b))
}

/// The security parameters, with room left for a signature.
fn parameters(e: &Engine, user: &str, tag: &[u8], salt: &[u8]) -> Vec<u8> {
    let inner = [tlv(OCTETS, &e.id), integer(e.boots), integer(e.time), tlv(OCTETS, user.as_bytes()), tlv(OCTETS, tag), tlv(OCTETS, salt)].concat();
    tlv(SEQUENCE, &inner)
}

fn scoped(engine: &[u8], pdu: Vec<u8>) -> Vec<u8> {
    tlv(SEQUENCE, &[tlv(OCTETS, engine), tlv(OCTETS, b""), pdu].concat())
}

fn whole(id: i64, flags: u8, security: &[u8], data: Vec<u8>) -> Vec<u8> {
    let header = tlv(SEQUENCE, &[integer(id), integer(65_507), tlv(OCTETS, &[flags]), integer(USM)].concat());
    tlv(SEQUENCE, &[integer(3), header, tlv(OCTETS, security), data].concat())
}

/// What an answer holds: its flags, its security parameters, and what follows.
fn opened(message: &[u8]) -> Option<(u8, Vec<&[u8]>, &[u8], u8)> {
    let (SEQUENCE, body, _) = read(message)? else { return None };
    let (_, _, rest) = read(body)?;
    let (SEQUENCE, header, rest) = read(rest)? else { return None };
    let (_, _, h) = read(header)?;
    let (_, _, h) = read(h)?;
    let (OCTETS, flags, _) = read(h)? else { return None };
    let (OCTETS, security, data) = read(rest)? else { return None };
    let (SEQUENCE, mut fields, _) = read(security)? else { return None };
    let mut parts = Vec::new();
    while !fields.is_empty() {
        let (_, value, more) = read(fields)?;
        parts.push(value);
        fields = more;
    }
    let (kind, _, _) = read(data)?;
    Some((*flags.first()?, parts, data, kind))
}

/// Who may send traps in version 3. A sender's engine is the authority for its own traps, so its
/// keys are worked out for whichever engine a trap says it came from.
// ponytail: boots and time are not checked against a sender's clock, so a recorded trap can be
// replayed. Keeping a clock per sender is the fix, and a trap only ever says something happened.
pub struct TrapUser {
    user: String,
    hash: Hash,
    auth: Option<String>,
    private: Option<String>,
}

impl TrapUser {
    /// From `?user=u&auth=..&priv=..&auth_hash=sha256`, if it names a user.
    pub fn from_address(rest: &str) -> Option<TrapUser> {
        let hash = if query(rest, "auth_hash").as_deref() == Some("sha256") { Hash::Sha256 } else { Hash::Sha1 };
        Some(TrapUser { user: query(rest, "user")?, hash, auth: query(rest, "auth"), private: query(rest, "priv") })
    }

    /// What a trap says, if it is from this user, is signed as it should be, and reads.
    pub fn open(&self, message: &[u8]) -> Option<(u8, Vec<(String, Value, u8)>)> {
        let (flags, parts, data, _) = opened(message)?;
        let [engine, boots, time, user, tag, salt] = parts[..] else { return None };
        if user != self.user.as_bytes() {
            return None;
        }
        if let Some(password) = &self.auth {
            if flags & AUTH == 0 {
                return None;
            }
            let at = message.windows(tag.len()).position(|w| w == tag)?;
            let mut blanked = message.to_vec();
            blanked[at..at + tag.len()].fill(0);
            if self.hash.tag(&localized_key(self.hash, password, engine), &blanked) != tag {
                return None;
            }
        }
        let plain = match &self.private {
            Some(password) => {
                if flags & PRIVATE == 0 {
                    return None;
                }
                let (OCTETS, cipher, _) = read(data)? else { return None };
                let key = localized_key(self.hash, password, engine);
                decrypted(&key, number_of(boots), number_of(time), u64::from_be_bytes(salt.try_into().ok()?), cipher)
            }
            None => data.to_vec(),
        };
        let (SEQUENCE, body, _) = read(&plain)? else { return None };
        let (_, _, rest) = read(body)?;
        let (_, _, rest) = read(rest)?;
        let (kind, pdu, _) = read(rest)?;
        Some((kind, split_pdu(pdu)?.2))
    }
}

impl Snmp3 {
    pub async fn connect(rest: &str) -> Result<Box<dyn Tags>, String> {
        let (host, port) = host_port(rest, 161);
        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
        socket.connect((host.as_str(), port)).await.map_err(|e| format!("{host}:{port}: {e}"))?;
        let user = query(rest, "user").ok_or("say ?user=")?;
        let hash = if query(rest, "auth_hash").as_deref() == Some("sha256") { Hash::Sha256 } else { Hash::Sha1 };
        let (auth, private) = (query(rest, "auth"), query(rest, "priv"));
        if private.is_some() && auth.is_none() {
            return Err("encryption needs authentication too".into());
        }
        let mut s = Snmp3 { socket, user, hash, auth, private, auth_key: vec![], priv_key: vec![], engine: Engine::default(), id: 0, salt: rand::random() };
        s.discover().await.map_err(|e| match e {
            Trouble::Lost(why) | Trouble::Refused(why) => why,
        })?;
        Ok(Box::new(s))
    }

    /// Asking with nothing is how an agent's id and clock are learned, and they make the keys.
    async fn discover(&mut self) -> Result<(), Trouble> {
        self.id += 1;
        let pdu = tlv(GET, &[integer(self.id), integer(0), integer(0), tlv(SEQUENCE, &[])].concat());
        let security = parameters(&Engine::default(), "", b"", b"");
        let message = whole(self.id, REPORTABLE, &security, scoped(b"", pdu));
        for _ in 0..3 {
            self.socket.send(&message).await?;
            let mut buffer = vec![0u8; 65_507];
            let Ok(got) = timeout(ANSWER / 3, self.socket.recv(&mut buffer)).await else { continue };
            let n = got?;
            let Some((_, parts, _, _)) = opened(&buffer[..n]) else { continue };
            let [id, boots, time, ..] = parts[..] else { continue };
            self.engine = Engine { id: id.to_vec(), boots: number_of(boots), time: number_of(time) };
            if let Some(password) = &self.auth {
                self.auth_key = localized_key(self.hash, password, id);
            }
            if let Some(password) = &self.private {
                self.priv_key = localized_key(self.hash, password, id);
            }
            return Ok(());
        }
        Err(Trouble::Lost("it did not answer".into()))
    }

    fn message(&mut self, kind: u8, arcs: &[u32], value: Vec<u8>) -> Vec<u8> {
        self.id += 1;
        let binding = tlv(SEQUENCE, &[oid_bytes(arcs), value].concat());
        let pdu = tlv(kind, &[integer(self.id), integer(0), integer(0), tlv(SEQUENCE, &binding)].concat());
        let mut plain = scoped(&self.engine.id, pdu);
        let mut flags = REPORTABLE;
        let mut salt = Vec::new();
        if self.private.is_some() {
            flags |= PRIVATE;
            self.salt = self.salt.wrapping_add(1);
            salt = self.salt.to_be_bytes().to_vec();
            plain = tlv(OCTETS, &encrypted(&self.priv_key, self.engine.boots, self.engine.time, self.salt, &plain));
        }
        if self.auth.is_none() {
            return whole(self.id, flags, &parameters(&self.engine, &self.user, b"", b""), plain);
        }
        flags |= AUTH;
        // Signed with the signature's place full of zeros, and the signature then put there.
        let blank = vec![0u8; self.hash.tag_size()];
        let unsigned = whole(self.id, flags, &parameters(&self.engine, &self.user, &blank, &salt), plain.clone());
        let tag = self.hash.tag(&self.auth_key, &unsigned);
        whole(self.id, flags, &parameters(&self.engine, &self.user, &tag, &salt), plain)
    }

    async fn ask(&mut self, kind: u8, arcs: &[u32], value: Vec<u8>) -> Result<Vec<(String, Value, u8)>, Trouble> {
        let mut resynced = false;
        loop {
            let message = self.message(kind, arcs, value.clone());
            let mut got_reply = None;
            for _ in 0..3 {
                self.socket.send(&message).await?;
                let mut buffer = vec![0u8; 65_507];
                let Ok(got) = timeout(ANSWER / 3, self.socket.recv(&mut buffer)).await else { continue };
                let n = got?;
                got_reply = Some(buffer[..n].to_vec());
                break;
            }
            let reply = got_reply.ok_or_else(|| Trouble::Lost("it did not answer".into()))?;
            let (flags, parts, data, _) = opened(&reply).ok_or_else(|| Trouble::Lost("that is not SNMP".into()))?;
            let [id, boots, time, _, tag, salt] = parts[..] else { return Err(Trouble::Lost("that is not SNMP version 3".into())) };
            if self.auth.is_some() && flags & AUTH != 0 {
                // Checked against the reply with its own signature blanked, as it was signed.
                let at = reply.windows(tag.len()).position(|w| w == tag).ok_or_else(|| Trouble::Lost("no signature".into()))?;
                let mut blanked = reply.clone();
                blanked[at..at + tag.len()].fill(0);
                if self.hash.tag(&self.auth_key, &blanked) != tag {
                    return Err(Trouble::Lost("the answer's signature is wrong".into()));
                }
            }
            let plain = if flags & PRIVATE != 0 {
                let (OCTETS, cipher, _) = read(data).ok_or_else(|| Trouble::Lost("that is not SNMP".into()))? else {
                    return Err(Trouble::Lost("that is not encrypted".into()));
                };
                let salt = u64::from_be_bytes(salt.try_into().map_err(|_| Trouble::Lost("no salt".into()))?);
                decrypted(&self.priv_key, number_of(boots), number_of(time), salt, cipher)
            } else {
                data.to_vec()
            };
            let (SEQUENCE, scoped_body, _) = read(&plain).ok_or_else(|| Trouble::Lost("that did not decrypt".into()))? else {
                return Err(Trouble::Lost("that did not decrypt: check the password".into()));
            };
            let (_, _, rest) = read(scoped_body).ok_or_else(|| Trouble::Lost("that is not SNMP".into()))?;
            let (_, _, rest) = read(rest).ok_or_else(|| Trouble::Lost("that is not SNMP".into()))?;
            let (pdu_kind, pdu, _) = read(rest).ok_or_else(|| Trouble::Lost("that is not SNMP".into()))?;
            let (_, error, bindings) = split_pdu(pdu).ok_or_else(|| Trouble::Lost("that is not SNMP".into()))?;
            match pdu_kind {
                RESPONSE if error != 0 => return Err(Trouble::Refused(format!("it answered error {error}"))),
                RESPONSE => return Ok(bindings),
                // A clock out of step: an agent says the right time in the report, and it is asked once more.
                REPORT if !resynced && bindings.first().is_some_and(|(name, ..)| name == "1.3.6.1.6.3.15.1.1.2.0") => {
                    self.engine.boots = number_of(boots);
                    self.engine.time = number_of(time);
                    self.engine.id = id.to_vec();
                    resynced = true;
                }
                REPORT => {
                    let why = bindings.first().map_or("?".to_string(), |(name, ..)| name.clone());
                    return Err(Trouble::Refused(format!("it refused the user or its password: {why}")));
                }
                other => return Err(Trouble::Lost(format!("it sent {other:#x}"))),
            }
        }
    }
}

#[async_trait]
impl Tags for Snmp3 {
    async fn read(&mut self, tag: &str) -> Result<Value, Trouble> {
        let (arcs, walk) = oid(tag).map_err(Trouble::Refused)?;
        if !walk {
            let got = self.ask(GET, &arcs, BIND_NULL.to_vec()).await?;
            let (_, value, kind) = got.into_iter().next().ok_or_else(|| Trouble::Refused("it sent nothing".into()))?;
            return if matches!(kind, 0x80..=0x82) { Err(Trouble::Refused("that is not there".into())) } else { Ok(value) };
        }
        let root = format!("{}.", arcs.iter().map(u32::to_string).collect::<Vec<_>>().join("."));
        let (mut at, mut out) = (arcs, Map::new());
        while out.len() < MAX_WALK {
            let got = self.ask(NEXT, &at, BIND_NULL.to_vec()).await?;
            let Some((name, value, kind)) = got.into_iter().next() else { break };
            if matches!(kind, 0x80..=0x82) || !name.starts_with(&root) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trap_is_believed_only_from_its_user() {
        let engine = Engine { id: vec![0x80, 0, 0, 1], boots: 3, time: 90 };
        let pdu = tlv(0xa7, &[integer(1), integer(0), integer(0), tlv(SEQUENCE, &[])].concat());
        let plain = scoped(&engine.id, pdu);
        let key = localized_key(Hash::Sha1, "auth-password", &engine.id);
        let private = localized_key(Hash::Sha1, "priv-password", &engine.id);
        let salt = 7u64;
        let cipher = tlv(OCTETS, &encrypted(&private, engine.boots, engine.time, salt, &plain));
        let blank = vec![0u8; Hash::Sha1.tag_size()];
        let unsigned = whole(1, AUTH | PRIVATE, &parameters(&engine, "watcher", &blank, &salt.to_be_bytes()), cipher.clone());
        let tag = Hash::Sha1.tag(&key, &unsigned);
        let signed = whole(1, AUTH | PRIVATE, &parameters(&engine, "watcher", &tag, &salt.to_be_bytes()), cipher);

        let user = |name: &str, auth: &str| TrapUser { user: name.into(), hash: Hash::Sha1, auth: Some(auth.into()), private: Some("priv-password".into()) };
        assert_eq!(user("watcher", "auth-password").open(&signed).map(|(kind, _)| kind), Some(0xa7));
        assert!(user("watcher", "wrong").open(&signed).is_none(), "a wrong password fails its signature");
        assert!(user("somebody", "auth-password").open(&signed).is_none(), "another user");
        let mut altered = signed.clone();
        *altered.last_mut().unwrap() ^= 1;
        assert!(user("watcher", "auth-password").open(&altered).is_none(), "and anything changed on the way");
    }

    #[test]
    fn a_password_becomes_the_key_rfc_3414_says() {
        // RFC 3414, A.3.2: "maplesyrup" for SHA-1, at engine 00...02.
        let engine = hex::decode("000000000000000000000002").unwrap();
        assert_eq!(hex::encode(localized_key(Hash::Sha1, "maplesyrup", &engine)), "6695febc9288e36282235fc7151f128497b38f3f");
    }

    #[test]
    fn what_is_encrypted_comes_back() {
        let key = [7u8; 16];
        let plain = b"a scoped message, longer than one block of the cipher";
        let sealed = encrypted(&key, 1, 2, 3, plain);
        assert_ne!(&sealed[..], &plain[..]);
        assert_eq!(decrypted(&key, 1, 2, 3, &sealed), plain);
    }

    #[test]
    fn a_tag_is_as_long_as_the_standard_says() {
        assert_eq!(Hash::Sha1.tag(b"k", b"m").len(), 12);
        assert_eq!(Hash::Sha256.tag(b"k", b"m").len(), 24);
    }
}
