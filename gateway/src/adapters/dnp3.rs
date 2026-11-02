//! DNP3 as a master over TCP: an outstation in a substation or a water works, keeping its events until
//! they are confirmed. Each arrives as its point, `analog-input:3`, holding `{value, flags}`, with `at`
//! when the outstation stamped it. It is asked for everything when it starts and after it restarts,
//! polled for events, and what it sends unasked is taken too.
//!
//! A node of `interrogate` asks for everything, and a point on its own asks for that point. Commands
//! are `binary-output:2` with true or false to latch it, or `trip`, `close` or `pulse`, and
//! `analog-output:4` with a number, each selected and then operated unless `direct=true` says the
//! equipment wants otherwise. There is no Secure Authentication and no TLS: a control network is
//! what keeps this safe.
//!
//! `dnp3://rtu:20000?master=1&outstation=10&poll=30`, with `&unsolicited=false` for an outstation
//! that should only be asked, and `&time=true` to set its clock when it says it needs that.

use crate::adapter::Failure;
use crate::adapters::tags::{ANSWER, host_port, query};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use chrono::Utc;
use ferrum_edge_contract::tags::Dnp3Point;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;

const CONFIRM: u8 = 0x00;
const READ: u8 = 0x01;
const WRITE: u8 = 0x02;
const SELECT: u8 = 0x03;
const OPERATE: u8 = 0x04;
const DIRECT: u8 = 0x05;
const ENABLE_UNSOLICITED: u8 = 0x14;
const DISABLE_UNSOLICITED: u8 = 0x15;
const RESPONSE: u8 = 0x81;
const UNSOLICITED: u8 = 0x82;

const FIR: u8 = 0x80;
const FIN: u8 = 0x40;
const CON: u8 = 0x20;
const UNS: u8 = 0x10;

/// Class 1, 2 and 3 events, and then class 0, which is everything as it stands.
const INTEGRITY: [u8; 12] = [60, 2, 6, 60, 3, 6, 60, 4, 6, 60, 1, 6];
const EVENTS: [u8; 9] = [60, 2, 6, 60, 3, 6, 60, 4, 6];
/// In the first byte of the internal indications.
const RESTARTED: u8 = 0x80;
const NEEDS_TIME: u8 = 0x10;
const EVENTS_WAITING: u8 = 0x0e;
const MAX_USER: usize = 250;

/// CRC-16/DNP, low byte first.
pub fn crc(data: &[u8]) -> [u8; 2] {
    let mut c = 0u16;
    for byte in data {
        c ^= u16::from(*byte);
        for _ in 0..8 {
            c = if c & 1 == 1 { (c >> 1) ^ 0xa6bc } else { c >> 1 };
        }
    }
    (!c).to_le_bytes()
}

/// A link frame, its data in blocks of sixteen, each with its own check.
pub fn link(control: u8, to: u16, from: u16, user: &[u8]) -> Vec<u8> {
    let mut out = vec![0x05, 0x64, 5 + user.len() as u8, control];
    out.extend(to.to_le_bytes());
    out.extend(from.to_le_bytes());
    out.extend(crc(&out));
    for block in user.chunks(16) {
        out.extend(block);
        out.extend(crc(block));
    }
    out
}

/// A link frame taken off the wire.
#[derive(Debug, PartialEq)]
pub struct Frame {
    pub control: u8,
    pub to: u16,
    pub from: u16,
    pub user: Vec<u8>,
}

/// One frame from the front of what has arrived, if a whole one has. Rubbish is dropped a byte at a
/// time until the start of a frame turns up, as a damaged frame would otherwise stop everything.
pub fn take(buffer: &mut Vec<u8>) -> Option<Frame> {
    loop {
        let start = buffer.windows(2).position(|w| w == [0x05, 0x64]);
        buffer.drain(..start.unwrap_or(buffer.len().saturating_sub(1)));
        let head = buffer.get(..10)?;
        if crc(&head[..8]) != head[8..10] || head[2] < 5 {
            buffer.drain(..1);
            continue;
        }
        let user = usize::from(head[2]) - 5;
        let size = 10 + user + user.div_ceil(16) * 2;
        if buffer.len() < size {
            return None;
        }
        let (mut data, mut good) = (Vec::with_capacity(user), true);
        let mut at = 10;
        let mut left = user;
        while left > 0 {
            let piece = left.min(16);
            good &= crc(&buffer[at..at + piece]) == buffer[at + piece..at + piece + 2];
            data.extend_from_slice(&buffer[at..at + piece]);
            at += piece + 2;
            left -= piece;
        }
        let (control, to, from) = (buffer[3], u16::from_le_bytes([buffer[4], buffer[5]]), u16::from_le_bytes([buffer[6], buffer[7]]));
        if !good {
            buffer.drain(..1);
            continue;
        }
        buffer.drain(..size);
        return Some(Frame { control, to, from, user: data });
    }
}

/// One thing a response says about a point.
#[derive(Debug, PartialEq)]
pub struct Said {
    pub point: String,
    pub value: Value,
    pub flags: u8,
    /// Milliseconds since 1970, if the outstation stamped it.
    pub at: Option<i64>,
}

/// How long an object is, by group and variation, and what it holds. `None` for what cannot be read
/// here, and then the rest of a fragment cannot be either, since nothing says where the next starts.
fn size(group: u8, variation: u8) -> Option<usize> {
    Some(match (group, variation) {
        (1 | 3 | 10, 2) | (2 | 4 | 11 | 13, 1) => 1,
        (2 | 4 | 11 | 13, 2) => 7,
        (2 | 4, 3) => 3,
        (20..=23, 1) | (30, 1 | 5) | (32 | 40 | 42 | 43, 1) | (32 | 42 | 43, 5) | (40, 3) => 5,
        (20..=23, 2) | (30, 2) | (32 | 40 | 42 | 43, 2) => 3,
        (21, 9) | (20, 5) | (30, 3) => 4,
        (21, 10) | (20, 6) | (30, 4) | (52, _) => 2,
        (21..=23, 5) | (32 | 42 | 43, 3 | 7) => 11,
        (21..=23, 6) | (32 | 42 | 43, 4) | (30, 6) | (40, 4) | (32 | 42 | 43, 6) => 9,
        (32 | 42 | 43, 8) => 15,
        (34, 1) => 2,
        (34, 2 | 3) => 4,
        (50, 1) | (51, _) => 6,
        (50, 4) => 11,
        (12, 1) => 11,
        (41, 1 | 3) => 5,
        (41, 2) => 3,
        (41, 4) => 9,
        (110 | 111, v) => usize::from(v),
        _ => return None,
    })
}

fn kind(group: u8) -> Option<&'static str> {
    Some(match group {
        1 | 2 => "binary-input",
        3 | 4 => "double-bit",
        10 | 11 => "binary-output",
        20 | 22 => "counter",
        21 | 23 => "frozen-counter",
        30 | 32 => "analog-input",
        40 | 42 => "analog-output",
        _ => return None,
    })
}

fn time(b: &[u8]) -> Option<i64> {
    let mut whole = [0u8; 8];
    whole[..6].copy_from_slice(b.get(..6)?);
    // An outstation that has never been told the time says nought, which is no time at all.
    Some(i64::from_le_bytes(whole)).filter(|at| *at > 0)
}

/// The value, its flags and its time, from an object of a known size.
fn object(group: u8, variation: u8, b: &[u8]) -> Option<(Value, u8, Option<i64>)> {
    let i16at = |at: usize| Some(i16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?));
    let i32at = |at: usize| Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?));
    let u16at = |at: usize| Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?));
    let u32at = |at: usize| Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?));
    let f32at = |at: usize| Some(f32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?));
    let f64at = |at: usize| Some(f64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?));
    let flags = *b.first()?;
    Some(match (group, variation) {
        (1 | 2 | 10 | 11, _) => (json!(flags & 0x80 != 0), flags & 0x7f, if variation == 2 && group != 1 && group != 10 { time(&b[1..]) } else { None }),
        (3 | 4, _) => (json!(flags >> 6), flags & 0x3f, if variation == 2 && group == 4 { time(&b[1..]) } else { None }),
        (20..=23, 1) => (json!(u32at(1)?), flags, None),
        (20..=23, 2) => (json!(u16at(1)?), flags, None),
        (21..=23, 5) => (json!(u32at(1)?), flags, time(&b[5..])),
        (21..=23, 6) => (json!(u16at(1)?), flags, time(&b[3..])),
        (20 | 21, 5 | 9) => (json!(u32at(0)?), 1, None),
        (20 | 21, 6 | 10) => (json!(u16at(0)?), 1, None),
        (30 | 32 | 40 | 42, 1) => (json!(i32at(1)?), flags, None),
        (30 | 32 | 40 | 42, 2) => (json!(i16at(1)?), flags, None),
        (30, 3) => (json!(i32at(0)?), 1, None),
        (30, 4) => (json!(i16at(0)?), 1, None),
        (30 | 32 | 42, 5) | (40, 3) => (json!(f32at(1)?), flags, None),
        (30 | 32 | 42, 6) | (40, 4) => (json!(f64at(1)?), flags, None),
        (32 | 42, 3) => (json!(i32at(1)?), flags, time(&b[5..])),
        (32 | 42, 4) => (json!(i16at(1)?), flags, time(&b[3..])),
        (32 | 42, 7) => (json!(f32at(1)?), flags, time(&b[5..])),
        (32 | 42, 8) => (json!(f64at(1)?), flags, time(&b[9..])),
        _ => return None,
    })
}

fn little(b: &[u8]) -> Option<usize> {
    (b.len() <= 4 && !b.is_empty()).then(|| b.iter().rev().fold(0, |n, byte| (n << 8) | usize::from(*byte)))
}

/// Every point a fragment's objects mention. It stops at the first thing it cannot read, keeping what
/// came before, because the objects after that one could start anywhere.
pub fn objects(mut b: &[u8]) -> Vec<Said> {
    let mut out = Vec::new();
    while let [group, variation, qualifier, rest @ ..] = b {
        let (group, variation) = (*group, *variation);
        let prefix = match qualifier >> 4 {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => return out,
        };
        let code = qualifier & 0x0f;
        // A range says which points, and a count leaves it to a prefix on each.
        let (first, count, mut r) = match code {
            0..=2 => {
                let width = 1 << code;
                let (Some(start), Some(stop)) = (rest.get(..width).and_then(little), rest.get(width..2 * width).and_then(little)) else { return out };
                if stop < start {
                    return out;
                }
                (start, stop - start + 1, &rest[2 * width..])
            }
            6 => (0, 0, rest),
            7..=9 => {
                let width = 1 << (code - 7);
                let Some(count) = rest.get(..width).and_then(little) else { return out };
                (0, count, &rest[width..])
            }
            _ => return out,
        };
        if variation == 1 && matches!(group, 1 | 10 | 3 | 80) {
            let bits = if group == 3 { 2 } else { 1 };
            let bytes = (count * bits).div_ceil(8);
            let Some(packed) = r.get(..bytes) else { return out };
            for i in 0..count {
                let cell = packed[i * bits / 8] >> (i * bits % 8);
                if let Some(name) = kind(group) {
                    let value = if group == 3 { json!(cell & 3) } else { json!(cell & 1 == 1) };
                    out.push(Said { point: format!("{name}:{}", first + i), value, flags: 1, at: None });
                }
            }
            r = &r[bytes..];
        } else {
            let Some(each) = size(group, variation) else { return out };
            for i in 0..count {
                let Some(index) = (if prefix == 0 { Some(first + i) } else { r.get(..prefix).and_then(little) }) else { return out };
                let Some(body) = r.get(prefix..prefix + each) else { return out };
                if let (Some(name), Some((value, flags, at))) = (kind(group), object(group, variation, body)) {
                    out.push(Said { point: format!("{name}:{index}"), value, flags, at });
                }
                r = &r[prefix + each..];
            }
        }
        b = r;
    }
    out
}

/// A control relay output block, on one point.
pub fn relay(index: u16, value: &Value) -> Option<Vec<u8>> {
    const PULSE: u8 = 1;
    const LATCH_ON: u8 = 3;
    const LATCH_OFF: u8 = 4;
    const CLOSE: u8 = 0x40;
    const TRIP: u8 = 0x80;
    let (code, on_ms) = match value {
        Value::Bool(true) => (LATCH_ON, 0u32),
        Value::Bool(false) => (LATCH_OFF, 0),
        Value::String(s) if s == "pulse" => (PULSE, 1000),
        Value::String(s) if s == "close" => (CLOSE | PULSE, 1000),
        Value::String(s) if s == "trip" => (TRIP | PULSE, 1000),
        _ => return None,
    };
    let mut out = vec![12, 1, 0x28, 1, 0];
    out.extend(index.to_le_bytes());
    out.extend([code, 1]);
    out.extend(on_ms.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.push(0);
    Some(out)
}

/// A setpoint on one point: whole numbers as they are, and anything else as a float.
pub fn setpoint(index: u16, value: &Value) -> Option<Vec<u8>> {
    let mut out = vec![41];
    let body = match value.as_i64().and_then(|n| i32::try_from(n).ok()) {
        Some(n) => {
            out.push(1);
            n.to_le_bytes().to_vec()
        }
        None => {
            out.push(3);
            (value.as_f64()? as f32).to_le_bytes().to_vec()
        }
    };
    out.extend([0x28, 1, 0]);
    out.extend(index.to_le_bytes());
    out.extend(body);
    out.push(0);
    Some(out)
}

/// What a command's echo says.
fn refusal(status: u8) -> &'static str {
    match status {
        1 => "it timed out between select and operate",
        2 => "it had no selection to operate",
        3 => "it could not read the request",
        4 => "that point does not take this",
        5 => "it is already busy with that",
        6 => "a hardware fault",
        7 => "it is in local control",
        8 => "too many at once",
        9 => "it is not authorized",
        10 => "automation is inhibited",
        12 => "that value is out of range",
        _ => "it refused",
    }
}

pub struct Dnp3 {
    address: String,
    poll: Duration,
    direct: bool,
    unsolicited: bool,
    set_time: bool,
    shared: Arc<Shared>,
}

/// A response's indications and objects.
type Reply = ([u8; 2], Vec<u8>);

struct Shared {
    master: u16,
    outstation: u16,
    writer: Mutex<Option<OwnedWriteHalf>>,
    /// One request at a time: a second one would cancel the first.
    turn: Mutex<()>,
    waiting: std::sync::Mutex<Option<(u8, oneshot::Sender<Reply>)>>,
    sequence: AtomicU8,
    segment: AtomicU8,
}

impl Shared {
    fn fresh(master: u16, outstation: u16) -> Shared {
        Shared {
            master,
            outstation,
            writer: Mutex::new(None),
            turn: Mutex::new(()),
            waiting: std::sync::Mutex::new(None),
            sequence: AtomicU8::new(0),
            segment: AtomicU8::new(0),
        }
    }

    async fn wire(&self, bytes: &[u8]) -> Result<(), String> {
        let mut held = self.writer.lock().await;
        held.as_mut().ok_or("not connected")?.write_all(bytes).await.map_err(|e| e.to_string())
    }

    /// Whole application fragments, cut for the link. The transport's sequence runs across them.
    async fn fragment(&self, application: &[u8]) -> Result<(), String> {
        let pieces: Vec<&[u8]> = application.chunks(MAX_USER - 1).collect();
        for (i, piece) in pieces.iter().enumerate() {
            let number = self.segment.fetch_add(1, Ordering::Relaxed) & 0x3f;
            let mut user = vec![number | if i == 0 { 0x40 } else { 0 } | if i + 1 == pieces.len() { 0x80 } else { 0 }];
            user.extend_from_slice(piece);
            self.wire(&link(0xc4, self.outstation, self.master, &user)).await?;
        }
        Ok(())
    }

    /// Asks and waits for the last fragment of the answer, whose objects a command's echo is in.
    async fn ask(&self, function: u8, objects: &[u8]) -> Result<Reply, String> {
        let _turn = self.turn.lock().await;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) & 0x0f;
        let (done, answer) = oneshot::channel();
        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = Some((sequence, done));
        let mut request = vec![FIR | FIN | sequence, function];
        request.extend_from_slice(objects);
        self.fragment(&request).await?;
        match timeout(ANSWER, answer).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err("the connection went".into()),
            Err(_) => Err("it did not answer".into()),
        }
    }

    /// Asks for what it holds, and looks after what its indications say is wanted.
    async fn gather(&self, what: &[u8], set_time: bool) -> Result<(), String> {
        let (iin, _) = self.ask(READ, what).await?;
        self.tend(iin, set_time).await
    }

    async fn tend(&self, iin: [u8; 2], set_time: bool) -> Result<(), String> {
        if iin[0] & RESTARTED != 0 {
            // Index 7 of the indications, cleared, and then all of it again: what it held is gone.
            self.ask(WRITE, &[80, 1, 0, 7, 7, 0]).await?;
            self.ask(READ, &INTEGRITY).await?;
        }
        if set_time && iin[0] & NEEDS_TIME != 0 {
            let mut now = vec![50, 1, 0x07, 1];
            now.extend(&Utc::now().timestamp_millis().to_le_bytes()[..6]);
            self.ask(WRITE, &now).await?;
        }
        if iin[0] & EVENTS_WAITING != 0 {
            self.ask(READ, &EVENTS).await?;
        }
        Ok(())
    }
}

impl Dnp3 {
    pub fn new(rest: &str) -> Dnp3 {
        let (host, port) = host_port(rest, 20000);
        let number = |key, default| query(rest, key).and_then(|n| n.parse().ok()).unwrap_or(default);
        let flag = |key, default| query(rest, key).map_or(default, |v| v == "true");
        Dnp3 {
            address: format!("{host}:{port}"),
            poll: Duration::from_secs(number("poll", 30).max(1)),
            direct: flag("direct", false),
            unsolicited: flag("unsolicited", true),
            set_time: flag("time", false),
            shared: Arc::new(Shared::fresh(number("master", 1) as u16, number("outstation", 10) as u16)),
        }
    }
}

/// How a fragment is taken in: its values handed on, then confirmed if it asks, since an outstation
/// forgets what is confirmed. Only the last of a response ends a request.
async fn heard(shared: &Shared, fragment: &[u8], tx: &mpsc::Sender<Message>) -> Result<bool, String> {
    let [control, function, iin1, iin2, rest @ ..] = fragment else { return Ok(true) };
    if !matches!(*function, RESPONSE | UNSOLICITED) {
        return Ok(true);
    }
    for said in objects(rest) {
        let mut body = json!({"value": said.value, "flags": said.flags});
        if let Some(at) = said.at {
            body["at"] = json!(at);
        }
        if tx.send(Message { topic: said.point, body: body.to_string().into_bytes(), status: None, from: None }).await.is_err() {
            return Ok(false);
        }
    }
    if control & CON != 0 {
        shared.fragment(&[FIR | FIN | (control & UNS) | (control & 0x0f), CONFIRM]).await?;
    }
    if *function == RESPONSE && control & FIN != 0 {
        let mut waiting = shared.waiting.lock().unwrap_or_else(|e| e.into_inner());
        if waiting.as_ref().is_some_and(|(sequence, _)| *sequence == control & 0x0f)
            && let Some((_, done)) = waiting.take()
        {
            let _ = done.send(([*iin1, *iin2], rest.to_vec()));
        }
    }
    Ok(true)
}

#[async_trait]
impl Transport for Dnp3 {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let stream = timeout(ANSWER, TcpStream::connect(&self.address))
            .await
            .map_err(|_| format!("{} did not answer", self.address))?
            .map_err(|e| format!("{}: {e}", self.address))?;
        let (mut reader, writer) = stream.into_split();
        let shared = self.shared.clone();
        *shared.writer.lock().await = Some(writer);

        // Requests wait on what the loop below reads, so they run beside it.
        let (poll, unsolicited, set_time) = (self.poll, self.unsolicited, self.set_time);
        let keeper = shared.clone();
        let mut polling = tokio::spawn(async move {
            keeper.ask(DISABLE_UNSOLICITED, &EVENTS).await?;
            let _ = ready.send(()).await;
            keeper.gather(&INTEGRITY, set_time).await?;
            if unsolicited {
                keeper.ask(ENABLE_UNSOLICITED, &EVENTS).await?;
            }
            loop {
                tokio::time::sleep(poll).await;
                keeper.gather(&EVENTS, set_time).await?;
            }
            #[allow(unreachable_code)]
            Ok::<(), String>(())
        });

        let (mut pending, mut segments) = (Vec::new(), Vec::new());
        let ended = 'run: loop {
            tokio::select! {
                () = tx.closed() => break Ok(()),
                polled = &mut polling => break polled.map_err(|e| e.to_string()).and_then(|r| r),
                got = reader.read_buf(&mut pending) => match got {
                    Ok(0) => break Err("it hung up".to_string()),
                    Ok(_) => {}
                    Err(e) => break Err(e.to_string()),
                },
            }
            while let Some(frame) = take(&mut pending) {
                if frame.to != shared.master || frame.from != shared.outstation {
                    continue;
                }
                let (primary, function) = (frame.control & 0x40 != 0, frame.control & 0x0f);
                if primary {
                    // What the link itself asks of a master: an acknowledgement, or that it is there.
                    let reply = match function {
                        0 | 2 | 3 => Some(0x80),
                        9 => Some(0x8b),
                        _ => None,
                    };
                    if let Some(control) = reply
                        && let Err(why) = shared.wire(&link(control, shared.outstation, shared.master, &[])).await
                    {
                        break 'run Err(why);
                    }
                }
                if !primary || !matches!(function, 3 | 4) {
                    continue;
                }
                let Some((header, body)) = frame.user.split_first() else { continue };
                if header & 0x40 != 0 {
                    segments.clear();
                }
                segments.extend_from_slice(body);
                if header & 0x80 != 0 {
                    let fragment = std::mem::take(&mut segments);
                    match heard(&shared, &fragment, &tx).await {
                        Ok(true) => {}
                        Ok(false) => break 'run Ok(()),
                        Err(why) => break 'run Err(why),
                    }
                }
            }
        };
        polling.abort();
        *shared.writer.lock().await = None;
        shared.waiting.lock().unwrap_or_else(|e| e.into_inner()).take();
        ended
    }

    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        let Out::Node { node, value } = out else { return Err(Failure::NotSent("this takes a node".into())) };
        let point = Dnp3Point::parse(&node).map_err(Failure::NotSent)?;
        let shared = &self.shared;
        let Some(value) = value else {
            let what = match point {
                Dnp3Point::Everything => INTEGRITY.to_vec(),
                Dnp3Point::One { group, index } => {
                    let mut one = vec![group, 0, 0x01];
                    one.extend(index.to_le_bytes());
                    one.extend(index.to_le_bytes());
                    one
                }
            };
            shared.gather(&what, self.set_time).await.map_err(Failure::NotSent)?;
            return Ok(None);
        };
        let wrong = || Failure::NotSent(format!("{node}: say true, false, trip, close or pulse for a binary output, and a number for an analog one"));
        let request = match point {
            Dnp3Point::One { group: 10, index } => relay(index, &value).ok_or_else(wrong)?,
            Dnp3Point::One { group: 40, index } => setpoint(index, &value).ok_or_else(wrong)?,
            _ => return Err(Failure::NotSent(format!("{node} cannot be commanded: an output can"))),
        };
        let checked = |(iin, echo): Reply, unknown: bool| {
            let status = echo.last().copied().filter(|_| !echo.is_empty());
            match status {
                Some(0) if iin[1] & 0x07 == 0 => Ok(()),
                Some(status) => Err(refusal(status)),
                None if iin[1] & 0x07 != 0 => Err("it did not take that request"),
                None if unknown => Ok(()),
                None => Err(refusal(255)),
            }
        };
        if !self.direct {
            // Nothing moves on a select, so a failure here leaves nothing to wonder about.
            let selected = shared.ask(SELECT, &request).await.map_err(Failure::NotSent)?;
            checked(selected, false).map_err(|why| Failure::NotSent(format!("{node}: {why}")))?;
        }
        let function = if self.direct { DIRECT } else { OPERATE };
        // Past this point it may have happened, unless the outstation says that it did not.
        let reply = shared.ask(function, &request).await.map_err(Failure::Unknown)?;
        checked(reply, false).map_err(|why| Failure::NotSent(format!("{node}: {why}")))?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_is_the_standards() {
        assert_eq!(u16::from_le_bytes(crc(b"123456789")), 0xea82);
        // A request for link status from 1 to 10, as the specification's own example frames go.
        assert_eq!(link(0xc9, 10, 1, &[])[..8], [0x05, 0x64, 5, 0xc9, 10, 0, 1, 0]);
    }

    #[test]
    fn frames_survive_being_cut_and_damaged() {
        let user: Vec<u8> = (0..40).collect();
        let whole = link(0xc4, 1, 10, &user);
        let mut buffer = whole[..20].to_vec();
        assert_eq!(take(&mut buffer), None, "not all here yet");
        buffer.extend_from_slice(&whole[20..]);
        assert_eq!(take(&mut buffer), Some(Frame { control: 0xc4, to: 1, from: 10, user: user.clone() }));
        assert!(buffer.is_empty());

        let mut noise = vec![1, 2, 3];
        noise.extend_from_slice(&whole);
        assert!(take(&mut noise).is_some(), "rubbish in front of a frame is skipped");

        let mut hit = whole.clone();
        hit[15] ^= 1;
        hit.extend_from_slice(&whole);
        assert_eq!(take(&mut hit).map(|f| f.user), Some(user), "a damaged frame is dropped and the next one read");
    }

    #[test]
    fn what_a_response_says() {
        // Group 30 variation 1 from 0 to 1, with flags, by a one-byte range.
        let mut b = vec![30, 1, 0, 0, 1];
        b.extend([1, 7, 0, 0, 0, 1, 0xfe, 0xff, 0xff, 0xff]);
        // Group 1 variation 1, packed, 3 to 5.
        b.extend([1, 1, 0, 3, 5, 0b101]);
        // Group 32 variation 7, a float with time, by a two-byte index and a count of one.
        b.extend([32, 7, 0x28, 1, 0, 9, 1, 1]);
        b.extend(2.5f32.to_le_bytes());
        b.extend(&1_700_000_000_000i64.to_le_bytes()[..6]);
        let said = objects(&b);
        let shown: Vec<(&str, Value, u8)> = said.iter().map(|s| (s.point.as_str(), s.value.clone(), s.flags)).collect();
        assert_eq!(
            shown,
            [
                ("analog-input:0", json!(7), 1),
                ("analog-input:1", json!(-2), 1),
                ("binary-input:3", json!(true), 1),
                ("binary-input:4", json!(false), 1),
                ("binary-input:5", json!(true), 1),
                ("analog-input:265", json!(2.5), 1),
            ]
        );
        assert_eq!(said[5].at, Some(1_700_000_000_000));
    }

    #[test]
    fn a_state_is_in_the_top_bit_and_a_time_counted_from_1970() {
        let [said] = &objects(&[2, 2, 0x17, 1, 4, 0x81, 0xe8, 3, 0, 0, 0, 0])[..] else { panic!("one") };
        assert_eq!((said.point.as_str(), &said.value, said.flags, said.at), ("binary-input:4", &json!(true), 1, Some(1000)));
    }

    #[test]
    fn what_cannot_be_read_stops_the_reading_and_keeps_what_came_before() {
        let mut b = vec![30, 4, 0, 2, 2, 5, 0];
        b.extend([200, 1, 0, 0, 0, 1]);
        assert_eq!(objects(&b).len(), 1);
        assert_eq!(objects(&[30, 1, 0, 5, 2]), vec![], "a range that runs backwards");
        assert_eq!(objects(&[30, 1, 0, 0, 9, 1, 0]), vec![], "shorter than it said");
        assert_eq!(objects(&[]), vec![]);
    }

    #[test]
    fn commands_are_written_as_the_standard_has_them() {
        let latch = relay(2, &json!(true)).unwrap();
        assert_eq!(latch, [12, 1, 0x28, 1, 0, 2, 0, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(relay(2, &json!(false)).unwrap()[7], 4);
        assert_eq!(relay(2, &json!("trip")).unwrap()[7], 0x81);
        assert_eq!(relay(2, &json!("close")).unwrap()[7], 0x41);
        assert_eq!(relay(2, &json!(7)), None);
        assert_eq!(setpoint(4, &json!(7)).unwrap(), [41, 1, 0x28, 1, 0, 4, 0, 7, 0, 0, 0, 0]);
        assert_eq!(setpoint(4, &json!(2.5)).unwrap()[..8], [41, 3, 0x28, 1, 0, 4, 0, 0]);
        assert_eq!(setpoint(4, &json!("x")), None);
    }

    #[test]
    fn addresses_have_defaults() {
        let d = Dnp3::new("rtu?poll=5&outstation=7&direct=true");
        assert_eq!((d.address.as_str(), d.poll, d.direct, d.shared.master, d.shared.outstation), ("rtu:20000", Duration::from_secs(5), true, 1, 7));
    }
}
