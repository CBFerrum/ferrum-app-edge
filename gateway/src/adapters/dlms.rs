//! DLMS/COSEM over TCP, as smart electricity meters speak it (IEC 62056, and IS 15959 in India):
//! objects by OBIS code, read through the wrapper a meter or its modem offers. A Register's value
//! comes back scaled by what the meter says its scaler is. HDLC on a serial or optical port is not
//! here, and neither is anything above low-level security: those meters need a head end of their own.
//!
//! `dlms://meter:4059?client=16&server=1`, and `&password=` for a client that logs in. A node is an
//! OBIS code, `1.0.1.8.0.255`, or `class:obis:attribute`, as `1:0.0.96.1.0.255:2`.

use crate::adapters::tags::{ANSWER, Tags, Trouble, host_port, query};
use async_trait::async_trait;
use ferrum_edge_contract::tags::cosem;
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const REGISTER: u16 = 3;
const VALUE: u8 = 2;
const SCALER_UNIT: u8 = 3;
const MAX_ANSWER: usize = 1 << 20;
/// `2.16.756.5.8.1.1`: logical names, no ciphering.
const CONTEXT: [u8; 11] = [0xa1, 0x09, 0x06, 0x07, 0x60, 0x85, 0x74, 0x05, 0x08, 0x01, 0x01];
/// `2.16.756.5.8.2.1`: a password.
const LOW_SECURITY: [u8; 9] = [0x8b, 0x07, 0x60, 0x85, 0x74, 0x05, 0x08, 0x02, 0x01];
/// An InitiateRequest: DLMS version 6, get and set by logical name, and any size of answer.
const INITIATE: [u8; 18] = [0xbe, 0x10, 0x04, 0x0e, 0x01, 0, 0, 0, 0x06, 0x5f, 0x1f, 0x04, 0x00, 0x00, 0x1e, 0x1d, 0xff, 0xff];

pub fn aarq(password: Option<&str>) -> Vec<u8> {
    let mut body = CONTEXT.to_vec();
    if let Some(password) = password {
        body.extend([0x8a, 0x02, 0x07, 0x80]);
        body.extend(LOW_SECURITY);
        body.extend([0xac, password.len() as u8 + 2, 0x80, password.len() as u8]);
        body.extend(password.as_bytes());
    }
    body.extend(INITIATE);
    let mut out = vec![0x60, body.len() as u8];
    out.extend(body);
    out
}

/// What an association answer says: whether it was accepted.
pub fn accepted(aare: &[u8]) -> Option<bool> {
    if *aare.first()? != 0x61 {
        return None;
    }
    let mut at = 2;
    while at + 1 < aare.len() {
        let (tag, size) = (aare[at], usize::from(aare[at + 1]));
        if tag == 0xa2 {
            return aare.get(at + 2..at + 2 + size)?.last().map(|result| *result == 0);
        }
        at += 2 + size;
    }
    None
}

pub fn get(class: u16, object: [u8; 6], attribute: u8) -> Vec<u8> {
    let mut out = vec![0xc0, 0x01, 0xc1];
    out.extend(class.to_be_bytes());
    out.extend(object);
    out.extend([attribute, 0]);
    out
}

/// A length: one byte, or `0x81` and `0x82` saying one or two more follow.
fn length(b: &[u8]) -> Option<(usize, &[u8])> {
    match *b.first()? {
        0x81 => Some((usize::from(*b.get(1)?), b.get(2..)?)),
        0x82 => Some((usize::from(u16::from_be_bytes(b.get(1..3)?.try_into().ok()?)), b.get(3..)?)),
        size if size < 0x80 => Some((usize::from(size), &b[1..])),
        _ => None,
    }
}

/// One value of a meter's data, and what is left after it.
pub fn data(b: &[u8]) -> Option<(Value, &[u8])> {
    let (kind, rest) = b.split_first()?;
    let take = |n: usize| rest.get(..n).map(|v| (v, &rest[n..]));
    Some(match kind {
        0x00 => (Value::Null, rest),
        0x03 => {
            let (v, r) = take(1)?;
            (json!(v[0] != 0), r)
        }
        0x05 => {
            let (v, r) = take(4)?;
            (json!(i32::from_be_bytes(v.try_into().ok()?)), r)
        }
        0x06 => {
            let (v, r) = take(4)?;
            (json!(u32::from_be_bytes(v.try_into().ok()?)), r)
        }
        0x0f => {
            let (v, r) = take(1)?;
            (json!(v[0] as i8), r)
        }
        0x10 => {
            let (v, r) = take(2)?;
            (json!(i16::from_be_bytes(v.try_into().ok()?)), r)
        }
        0x11 | 0x16 => {
            let (v, r) = take(1)?;
            (json!(v[0]), r)
        }
        0x12 => {
            let (v, r) = take(2)?;
            (json!(u16::from_be_bytes(v.try_into().ok()?)), r)
        }
        0x14 => {
            let (v, r) = take(8)?;
            (json!(i64::from_be_bytes(v.try_into().ok()?)), r)
        }
        0x15 => {
            let (v, r) = take(8)?;
            (json!(u64::from_be_bytes(v.try_into().ok()?)), r)
        }
        0x17 => {
            let (v, r) = take(4)?;
            (json!(f32::from_be_bytes(v.try_into().ok()?)), r)
        }
        0x18 => {
            let (v, r) = take(8)?;
            (json!(f64::from_be_bytes(v.try_into().ok()?)), r)
        }
        0x09 | 0x0a => {
            let (size, r) = length(rest)?;
            let v = r.get(..size)?;
            let text =
                if *kind == 0x0a || v.iter().all(|c| c.is_ascii_graphic() || *c == b' ') { json!(String::from_utf8_lossy(v)) } else { json!(hex::encode(v)) };
            (text, &r[size..])
        }
        0x01 | 0x02 => {
            let (count, mut r) = length(rest)?;
            let mut out = Vec::with_capacity(count.min(r.len()));
            for _ in 0..count {
                let (v, more) = data(r)?;
                out.push(v);
                r = more;
            }
            (Value::Array(out), r)
        }
        _ => return None,
    })
}

/// What a GET's answer holds, or why not.
pub fn answer(apdu: &[u8]) -> Result<Value, Trouble> {
    let [0xc4, 0x01, _, choice, rest @ ..] = apdu else { return Err(Trouble::Refused("that was not an answer this can read".into())) };
    match choice {
        0 => data(rest).map(|(v, _)| v).ok_or_else(|| Trouble::Refused("this cannot read that value".into())),
        _ => Err(Trouble::Refused(format!("it refused: access error {}", rest.first().copied().unwrap_or(0)))),
    }
}

/// A long answer comes in blocks: whether this is the last, its number, and this part of the data.
pub fn block(apdu: &[u8]) -> Option<Result<(bool, u32, &[u8]), Trouble>> {
    let [0xc4, 0x02, _, last, a, b, c, d, result, rest @ ..] = apdu else { return None };
    if *result != 0 {
        return Some(Err(Trouble::Refused(format!("it refused: access error {}", rest.first().copied().unwrap_or(0)))));
    }
    let Some((size, data)) = length(rest) else { return Some(Err(Trouble::Refused("this cannot read that block".into()))) };
    Some(Ok((*last != 0, u32::from_be_bytes([*a, *b, *c, *d]), data.get(..size).unwrap_or(data))))
}

pub fn next_block(number: u32) -> Vec<u8> {
    let mut out = vec![0xc0, 0x02, 0xc1];
    out.extend(number.to_be_bytes());
    out
}

pub struct Dlms {
    stream: TcpStream,
    client: u16,
    server: u16,
    /// What each Register's number is to be multiplied by, which it says once.
    scalers: HashMap<[u8; 6], f64>,
}

impl Dlms {
    pub async fn connect(rest: &str) -> Result<Box<dyn Tags>, String> {
        let (host, port) = host_port(rest, 4059);
        let number = |key, default| query(rest, key).and_then(|n| n.parse().ok()).unwrap_or(default);
        let stream = timeout(ANSWER, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| format!("{host}:{port} did not answer"))?
            .map_err(|e| format!("{host}:{port}: {e}"))?;
        let mut d = Dlms { stream, client: number("client", 16), server: number("server", 1), scalers: HashMap::new() };
        let answer = d.ask(&aarq(query(rest, "password").as_deref())).await.map_err(|e| match e {
            Trouble::Lost(why) | Trouble::Refused(why) => why,
        })?;
        match accepted(&answer) {
            Some(true) => Ok(Box::new(d)),
            Some(false) => Err("it refused the association: check client, server and password".into()),
            None => Err("that is not a meter".into()),
        }
    }

    async fn ask(&mut self, apdu: &[u8]) -> Result<Vec<u8>, Trouble> {
        let mut frame = vec![0, 1];
        frame.extend(self.client.to_be_bytes());
        frame.extend(self.server.to_be_bytes());
        frame.extend((apdu.len() as u16).to_be_bytes());
        frame.extend(apdu);
        self.stream.write_all(&frame).await?;
        let mut head = [0u8; 8];
        timeout(ANSWER, self.stream.read_exact(&mut head)).await??;
        if head[..2] != [0, 1] {
            return Err(Trouble::Lost("that is not the DLMS wrapper".into()));
        }
        let mut body = vec![0u8; usize::from(u16::from_be_bytes([head[6], head[7]]))];
        timeout(ANSWER, self.stream.read_exact(&mut body)).await??;
        Ok(body)
    }

    /// An answer, however many blocks it comes in. A profile can be long, and this stops at what
    /// a value can sensibly be.
    async fn whole(&mut self, request: &[u8]) -> Result<Value, Trouble> {
        let mut reply = self.ask(request).await?;
        let mut parts = Vec::new();
        loop {
            let Some(got) = block(&reply) else { return answer(&reply) };
            let (last, number, part) = got?;
            parts.extend_from_slice(part);
            if last {
                return data(&parts).map(|(v, _)| v).ok_or_else(|| Trouble::Refused("this cannot read that value".into()));
            }
            if parts.len() > MAX_ANSWER {
                return Err(Trouble::Refused("that answer is too long to take here".into()));
            }
            reply = self.ask(&next_block(number)).await?;
        }
    }

    async fn scaler(&mut self, object: [u8; 6]) -> Result<f64, Trouble> {
        if let Some(found) = self.scalers.get(&object) {
            return Ok(*found);
        }
        let got = answer(&self.ask(&get(REGISTER, object, SCALER_UNIT)).await?)?;
        let power = got.get(0).and_then(Value::as_i64).unwrap_or(0);
        let scaler = 10f64.powi(power as i32);
        self.scalers.insert(object, scaler);
        Ok(scaler)
    }
}

#[async_trait]
impl Tags for Dlms {
    async fn read(&mut self, tag: &str) -> Result<Value, Trouble> {
        let (class, object, attribute) = cosem(tag).map_err(Trouble::Refused)?;
        let value = self.whole(&get(class, object, attribute)).await?;
        if class == REGISTER
            && attribute == VALUE
            && let Some(n) = value.as_f64()
        {
            let scaled = n * self.scaler(object).await?;
            return Ok(json!((scaled * 1e6).round() / 1e6));
        }
        Ok(value)
    }

    async fn write(&mut self, tag: &str, _: &Value) -> Result<(), Trouble> {
        Err(Trouble::Refused(format!("{tag}: a meter is only read here")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_association_is_asked_for_as_the_green_book_has_it() {
        let plain = aarq(None);
        assert_eq!(plain[..2], [0x60, 0x1d]);
        assert_eq!(plain[2..13], CONTEXT);
        let logged_in = aarq(Some("12345678"));
        assert_eq!(logged_in[1] as usize, logged_in.len() - 2);
        assert!(logged_in.windows(10).any(|w| w == [0x80, 8, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8']));
    }

    #[test]
    fn an_answer_says_whether_it_was_accepted() {
        assert_eq!(accepted(&[0x61, 0x10, 0xa1, 0x09, 6, 7, 0x60, 0x85, 0x74, 5, 8, 1, 1, 0xa2, 3, 2, 1, 0]), Some(true));
        assert_eq!(accepted(&[0x61, 0x10, 0xa1, 0x09, 6, 7, 0x60, 0x85, 0x74, 5, 8, 1, 1, 0xa2, 3, 2, 1, 1]), Some(false));
        assert_eq!(accepted(b"nope"), None);
    }

    #[test]
    fn a_get_says_which_object() {
        assert_eq!(get(3, [1, 0, 1, 8, 0, 255], 2), [0xc0, 1, 0xc1, 0, 3, 1, 0, 1, 8, 0, 255, 2, 0]);
    }

    #[test]
    fn long_answers_come_in_blocks() {
        let part = [0xc4, 2, 0xc1, 0, 0, 0, 0, 1, 0, 0x03, 0x02, 2, 0x12];
        let Some(Ok((last, number, held))) = block(&part) else { panic!("a block") };
        assert_eq!((last, number, held), (false, 1, &[0x02, 2, 0x12][..]));
        assert_eq!(next_block(1), [0xc0, 2, 0xc1, 0, 0, 0, 1]);
        assert!(matches!(block(&[0xc4, 2, 0xc1, 1, 0, 0, 0, 2, 1, 3]), Some(Err(Trouble::Refused(_)))));
        assert!(block(&[0xc4, 1, 0xc1, 0, 0x06, 0, 0, 0, 7]).is_none(), "a plain answer is not one");
        let long: Vec<u8> = [&[0x09, 0x81, 200][..], &[b'a'; 200]].concat();
        assert_eq!(data(&long).unwrap().0.as_str().map(str::len), Some(200));
        assert!(data(&[0x09, 0x83, 1, 1, 1]).is_none());
    }

    #[test]
    fn values_as_a_meter_sends_them() {
        assert_eq!(data(&[0x06, 0, 0, 0x30, 0x39]).unwrap().0, json!(12345));
        assert_eq!(data(&[0x0f, 0xff]).unwrap().0, json!(-1));
        assert_eq!(data(&[0x12, 0x01, 0x00]).unwrap().0, json!(256));
        assert_eq!(data(&[0x09, 3, b'A', b'B', b'C']).unwrap().0, json!("ABC"));
        assert_eq!(data(&[0x02, 2, 0x0f, 0xfe, 0x16, 30]).unwrap().0, json!([-2, 30]), "a scaler and a unit");
        assert!(data(&[0x06, 0]).is_none());
        assert_eq!(answer(&[0xc4, 1, 0xc1, 0, 0x06, 0, 0, 0, 7]).ok(), Some(json!(7)));
        assert!(matches!(answer(&[0xc4, 1, 0xc1, 1, 3]), Err(Trouble::Refused(_))));
    }
}
