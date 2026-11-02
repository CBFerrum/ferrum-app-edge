//! IEC 60870-5-104: a substation or a remote terminal unit sending measurements as they change, by
//! information object address. Each arrives as its address, holding `{value, quality}`. `node:
//! interrogate` asks for everything, as a poll does for a station that only sends on change, and
//! `single:4001`, `double:4002` and `setpoint:4003` are commands. It is plain TCP: the standard's
//! security is another layer, and a control network is what keeps this safe.
//!
//! `iec104://rtu:2404?ca=1`

use crate::adapter::Failure;
use crate::adapters::tags::{host_port, query};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;

const STARTDT: [u8; 6] = [0x68, 4, 0x07, 0, 0, 0];
const TESTFR_ANSWER: [u8; 6] = [0x68, 4, 0x83, 0, 0, 0];
const INTERROGATION: u8 = 100;
const SINGLE: u8 = 45;
const DOUBLE: u8 = 46;
const SETPOINT: u8 = 50;
const ACTIVATE: u8 = 6;
/// A station that says nothing for this long gets asked if it is there.
const IDLE: Duration = Duration::from_secs(20);

/// How long each kind of element is, and what it means: `(bytes, reader)`.
fn element(kind: u8) -> Option<usize> {
    Some(match kind {
        1 | 3 => 1,
        5 => 2,
        7 | 9 | 11 => 5 - 2 * usize::from(kind != 7),
        13 | 15 => 5,
        30 | 31 => 8,
        32 => 9,
        34 | 35 => 10,
        36 | 37 => 12,
        _ => return None,
    })
}

/// What one element says: its value and its quality.
pub fn read(kind: u8, b: &[u8]) -> Option<(Value, u8)> {
    Some(match kind {
        1 | 30 => (json!(b[0] & 1 == 1), b[0] & 0xf0),
        3 | 31 => (json!(b[0] & 3), b[0] & 0xf0),
        5 | 32 => (json!(((b[0] & 0x7f) as i8) << 1 >> 1), *b.get(1)?),
        7 | 33 => (json!(u32::from_le_bytes(b.get(..4)?.try_into().ok()?)), *b.get(4)?),
        9 | 34 => (json!(f64::from(i16::from_le_bytes(b.get(..2)?.try_into().ok()?)) / 32768.0), *b.get(2)?),
        11 | 35 => (json!(i16::from_le_bytes(b.get(..2)?.try_into().ok()?)), *b.get(2)?),
        13 | 36 => (json!(f32::from_le_bytes(b.get(..4)?.try_into().ok()?)), *b.get(4)?),
        15 | 37 => (json!(i32::from_le_bytes(b.get(..4)?.try_into().ok()?)), *b.get(4)?),
        _ => return None,
    })
}

/// Every value in an information unit: `(address, value, quality)`.
pub fn values(asdu: &[u8]) -> Vec<(u32, Value, u8)> {
    let mut out = Vec::new();
    let Some(&[kind, vsq]) = asdu.get(..2).map(|b| <&[u8; 2]>::try_from(b).unwrap_or(&[0, 0])) else { return out };
    let Some(size) = element(kind) else { return out };
    let (count, in_sequence) = (usize::from(vsq & 0x7f), vsq & 0x80 != 0);
    let address = |b: &[u8]| u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16;
    let mut at = 6;
    if in_sequence {
        let Some(base) = asdu.get(at..at + 3).map(address) else { return out };
        at += 3;
        for i in 0..count {
            let Some(item) = asdu.get(at + i * size..at + (i + 1) * size) else { break };
            if let Some((value, quality)) = read(kind, item) {
                out.push((base + i as u32, value, quality));
            }
        }
    } else {
        for i in 0..count {
            let start = at + i * (size + 3);
            let (Some(place), Some(item)) = (asdu.get(start..start + 3), asdu.get(start + 3..start + 3 + size)) else { break };
            if let Some((value, quality)) = read(kind, item) {
                out.push((address(place), value, quality));
            }
        }
    }
    out
}

pub fn command(kind: u8, cause: u8, station: u16, address: u32, element: &[u8]) -> Vec<u8> {
    let mut out = vec![kind, 1, cause, 0];
    out.extend(station.to_le_bytes());
    out.extend(&address.to_le_bytes()[..3]);
    out.extend(element);
    out
}

pub struct Iec104 {
    pub address: String,
    pub station: u16,
    writer: Mutex<Option<OwnedWriteHalf>>,
    sent: AtomicU16,
    received: AtomicU16,
}

impl Iec104 {
    pub fn new(rest: &str) -> Iec104 {
        let (host, port) = host_port(rest, 2404);
        Iec104 {
            address: format!("{host}:{port}"),
            station: query(rest, "ca").and_then(|c| c.parse().ok()).unwrap_or(1),
            writer: Mutex::new(None),
            sent: AtomicU16::new(0),
            received: AtomicU16::new(0),
        }
    }

    async fn information(&self, asdu: &[u8]) -> Result<(), String> {
        let mut held = self.writer.lock().await;
        let writer = held.as_mut().ok_or("not connected")?;
        let (sent, received) = (self.sent.fetch_add(1, Ordering::Relaxed), self.received.load(Ordering::Relaxed));
        let mut frame = vec![0x68, (asdu.len() + 4) as u8];
        frame.extend((sent << 1).to_le_bytes());
        frame.extend((received << 1).to_le_bytes());
        frame.extend(asdu);
        writer.write_all(&frame).await.map_err(|e| e.to_string())
    }
}

#[async_trait]
impl Transport for Iec104 {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let stream = timeout(Duration::from_secs(5), TcpStream::connect(&self.address))
            .await
            .map_err(|_| format!("{} did not answer", self.address))?
            .map_err(|e| format!("{}: {e}", self.address))?;
        let (mut reader, mut writer) = stream.into_split();
        writer.write_all(&STARTDT).await.map_err(|e| e.to_string())?;
        *self.writer.lock().await = Some(writer);
        self.sent.store(0, Ordering::Relaxed);
        self.received.store(0, Ordering::Relaxed);
        let mut started = false;
        let mut unacknowledged = 0u16;
        loop {
            let mut head = [0u8; 2];
            let got = tokio::select! {
                () = tx.closed() => return Ok(()),
                got = timeout(IDLE, reader.read_exact(&mut head)) => got,
            };
            let Ok(got) = got else {
                let mut held = self.writer.lock().await;
                let writer = held.as_mut().ok_or("not connected")?;
                writer.write_all(&[0x68, 4, 0x43, 0, 0, 0]).await.map_err(|e| e.to_string())?;
                continue;
            };
            got.map_err(|e| e.to_string())?;
            if head[0] != 0x68 {
                return Err("that is not IEC 104".into());
            }
            let mut body = vec![0u8; usize::from(head[1])];
            reader.read_exact(&mut body).await.map_err(|e| e.to_string())?;
            if body.len() < 4 {
                continue;
            }
            if body[0] & 1 == 0 {
                self.received.fetch_add(1, Ordering::Relaxed);
                unacknowledged += 1;
                for (address, value, quality) in values(&body[4..]) {
                    let said = json!({"value": value, "quality": quality}).to_string().into_bytes();
                    if tx.send(Message { topic: address.to_string(), body: said, status: None, from: None }).await.is_err() {
                        return Ok(());
                    }
                }
                // Told every few frames, as the standard's `w` has it.
                if unacknowledged >= 8 {
                    unacknowledged = 0;
                    let mut frame = vec![0x68, 4, 1, 0];
                    frame.extend((self.received.load(Ordering::Relaxed) << 1).to_le_bytes());
                    if let Some(writer) = self.writer.lock().await.as_mut() {
                        writer.write_all(&frame).await.map_err(|e| e.to_string())?;
                    }
                }
            } else if body[0] & 3 == 3 {
                match body[0] {
                    0x0b if !started => {
                        started = true;
                        let _ = ready.send(()).await;
                    }
                    0x43 => {
                        if let Some(writer) = self.writer.lock().await.as_mut() {
                            writer.write_all(&TESTFR_ANSWER).await.map_err(|e| e.to_string())?;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        let Out::Node { node, value } = out else { return Err(Failure::NotSent("this takes a node".into())) };
        let number = |v: &Value| {
            v.as_f64()
                .or_else(|| v.as_bool().map(|b| f64::from(u8::from(b))))
                .or_else(|| v.as_str()?.parse().ok())
                .ok_or(Failure::NotSent(format!("{v} is not a number")))
        };
        let asdu = match (node.split_once(':'), &value) {
            (None, None) if node == "interrogate" => command(INTERROGATION, ACTIVATE, self.station, 0, &[20]),
            (Some(("single", a)), Some(v)) => command(SINGLE, ACTIVATE, self.station, parse(a)?, &[u8::from(number(v)? != 0.0)]),
            (Some(("double", a)), Some(v)) => command(DOUBLE, ACTIVATE, self.station, parse(a)?, &[if number(v)? != 0.0 { 2 } else { 1 }]),
            (Some(("setpoint", a)), Some(v)) => {
                let mut element = (number(v)? as f32).to_le_bytes().to_vec();
                element.push(0);
                command(SETPOINT, ACTIVATE, self.station, parse(a)?, &element)
            }
            _ => return Err(Failure::NotSent(format!("{node}: say interrogate, single:4001, double:4002 or setpoint:4003"))),
        };
        self.information(&asdu).await.map_err(if value.is_some() { Failure::Unknown } else { Failure::NotSent })?;
        Ok(None)
    }
}

fn parse(address: &str) -> Result<u32, Failure> {
    address.parse().map_err(|_| Failure::NotSent(format!("{address} is not an address")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurements_in_a_list_and_in_a_run() {
        // Short floats at 1001 and 2001.
        let mut listed = vec![13, 2, 3, 0, 1, 0];
        listed.extend([0xe9, 0x03, 0]);
        listed.extend(21.5f32.to_le_bytes());
        listed.push(0);
        listed.extend([0xd1, 0x07, 0]);
        listed.extend(1.0f32.to_le_bytes());
        listed.push(0x80);
        assert_eq!(values(&listed), vec![(1001, json!(21.5), 0), (2001, json!(1.0), 0x80)]);

        // Three single points in a row, from 100, the third invalid.
        let run = [1, 0x83, 20, 0, 1, 0, 100, 0, 0, 1, 0, 0x81];
        assert_eq!(values(&run), vec![(100, json!(true), 0), (101, json!(false), 0), (102, json!(true), 0x80)]);
    }

    #[test]
    fn a_short_or_unknown_unit_is_no_reading() {
        assert_eq!(values(&[]), vec![]);
        assert_eq!(values(&[13, 1, 3, 0, 1, 0, 1]), vec![]);
        assert_eq!(values(&[200, 1, 3, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0]), vec![]);
    }

    #[test]
    fn numbers_as_the_standard_keeps_them() {
        assert_eq!(read(9, &[0, 0x40, 0]), Some((json!(0.5), 0)), "normalized");
        assert_eq!(read(11, &[0xff, 0xff, 0x10]), Some((json!(-1), 0x10)), "scaled");
        assert_eq!(read(3, &[2]), Some((json!(2), 0)), "double point: on");
        assert_eq!(read(15, &[5, 0, 0, 0, 0]), Some((json!(5), 0)), "a counter");
        assert_eq!(command(SINGLE, ACTIVATE, 1, 4001, &[1]), [45, 1, 6, 0, 1, 0, 0xa1, 0x0f, 0, 1]);
    }
}
