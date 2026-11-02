//! The other places a batch can go: message buses, time-series stores and other people's clouds. Each
//! answers yes only once the far end has taken all of it, and anything else leaves the batch queued,
//! so delivery is at least once. A secret is named by the export and kept at the site, and never
//! comes from the platform. Protocols with a small client side are spoken here, because they are
//! small, and Kafka is left to `rskafka`, because it is not.
//!
//! Connections to a bus last one batch. A site sends a batch every few seconds at most, so this costs
//! a handshake each time and saves a connection to watch.

use crate::adapter::Failure;
use crate::adapters::transport::{Message, Out, Transport};
use chrono::{DateTime, Utc};
use ferrum_edge_contract::sparkplug;
use hmac::{Hmac, Mac};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

const PATIENCE: Duration = Duration::from_secs(15);

/// `{device}` and `{metric}`, in a form every bus takes in a subject or a routing key.
pub fn named(template: &str, body: &Value) -> String {
    let part = |k: &str| body[k].as_str().unwrap_or("unknown").replace([' ', '\t', '\r', '\n'], "_");
    template.replace("{device}", &part("device")).replace("{metric}", &part("metric"))
}

pub fn query(url: &Url, key: &str) -> Option<String> {
    url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

async fn tcp(url: &Url, default_port: u16) -> Result<TcpStream, String> {
    let host = url.host_str().ok_or("it names no host")?;
    let port = url.port().unwrap_or(default_port);
    timeout(PATIENCE, TcpStream::connect((host, port))).await.map_err(|_| format!("{host}:{port} did not answer"))?.map_err(|e| format!("{host}:{port}: {e}"))
}

async fn secure(stream: TcpStream, host: &str) -> Result<Box<dyn Stream>, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    crate::adapters::transport::crypto();
    let config = ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    let name = ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
    let tls = timeout(PATIENCE, TlsConnector::from(Arc::new(config)).connect(name, stream)).await.map_err(|_| "the handshake took too long")?;
    Ok(Box::new(tls.map_err(|e| e.to_string())?))
}

/// NATS. `PING` is answered once everything before it has been taken, so `PONG` is the yes.
pub async fn nats(url: &Url, subject: &str, bodies: &[String], secret: Option<&str>) -> Result<(), String> {
    let mut plain = tcp(url, 4222).await?;
    let mut hello = Vec::new();
    while !hello.ends_with(b"\n") {
        hello.push(timeout(PATIENCE, plain.read_u8()).await.map_err(|_| "it said nothing")?.map_err(|e| e.to_string())?);
    }
    if !hello.starts_with(b"INFO") {
        return Err("that is not a NATS server".into());
    }
    let mut stream: Box<dyn Stream> = if url.scheme() == "nats+tls" { secure(plain, url.host_str().unwrap_or_default()).await? } else { Box::new(plain) };
    let mut connect = json!({"verbose": false, "pedantic": false, "name": "ferrum-site", "lang": "rust", "version": env!("CARGO_PKG_VERSION"), "protocol": 0});
    match (url.username(), secret) {
        ("", Some(token)) => connect["auth_token"] = json!(token),
        (user, Some(password)) => (connect["user"], connect["pass"]) = (json!(user), json!(password)),
        _ => {}
    }
    let mut out = format!("CONNECT {connect}\r\n").into_bytes();
    for body in bodies {
        let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        out.extend(format!("PUB {} {}\r\n", named(subject, &value), body.len()).as_bytes());
        out.extend(body.as_bytes());
        out.extend(b"\r\n");
    }
    out.extend(b"PING\r\n");
    stream.write_all(&out).await.map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let n = timeout(PATIENCE, reader.read_line(&mut line)).await.map_err(|_| "it did not say it had taken them")?.map_err(|e| e.to_string())?;
        match line.trim_end() {
            _ if n == 0 => return Err("it hung up".into()),
            "PONG" => return Ok(()),
            error if error.starts_with("-ERR") => return Err(error.to_string()),
            _ => {}
        }
    }
}

const FRAME_END: u8 = 0xce;
const METHOD: u8 = 1;
const HEADER: u8 = 2;
const BODY: u8 = 3;
const HEARTBEAT_FRAME: u8 = 8;
const CHANNEL: u16 = 1;
/// How many seconds a broker and a reader may each be silent for.
const HEARTBEAT: u16 = 30;
/// How many messages can be on their way at once.
const PREFETCH: u16 = 100;

fn short(out: &mut Vec<u8>, text: &str) {
    out.push(text.len().min(255) as u8);
    out.extend(&text.as_bytes()[..text.len().min(255)]);
}

fn long(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend(bytes);
}

fn method(class: u16, id: u16) -> Vec<u8> {
    let mut out = class.to_be_bytes().to_vec();
    out.extend(id.to_be_bytes());
    out
}

/// AMQP 0-9-1, as RabbitMQ speaks it: frames, with a method or part of a message in each.
struct Amqp {
    stream: BufReader<Box<dyn Stream>>,
    pending: Vec<u8>,
}

impl Amqp {
    /// Logged in with a channel open, and the largest frame the broker takes. `heartbeat` is how
    /// many seconds either side may be silent for, where nought means for as long as it likes.
    async fn open(url: &Url, user: &str, password: &str, heartbeat: u16) -> Result<(Amqp, u32), String> {
        let plain = tcp(url, if url.scheme() == "amqps" { 5671 } else { 5672 }).await?;
        let stream: Box<dyn Stream> = if url.scheme() == "amqps" { secure(plain, url.host_str().unwrap_or_default()).await? } else { Box::new(plain) };
        let mut a = Amqp { stream: BufReader::new(stream), pending: Vec::new() };
        a.stream.get_mut().write_all(b"AMQP\x00\x00\x09\x01").await.map_err(|e| e.to_string())?;
        a.expect(10, 10).await?;
        let user = if user.is_empty() { "guest" } else { user };
        let mut started = method(10, 11);
        long(&mut started, b"");
        short(&mut started, "PLAIN");
        long(&mut started, format!("\0{user}\0{password}").as_bytes());
        short(&mut started, "en_US");
        a.send(METHOD, 0, &started).await?;
        let tune = a.expect(10, 30).await?;
        let frame_max = tune.get(2..6).map_or(131_072, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        let frame_max = if frame_max == 0 { 131_072 } else { frame_max };
        let mut tuned = method(10, 31);
        tuned.extend(1u16.to_be_bytes());
        tuned.extend(frame_max.to_be_bytes());
        tuned.extend(heartbeat.to_be_bytes());
        a.send(METHOD, 0, &tuned).await?;
        let mut open = method(10, 40);
        short(&mut open, &decoded(url.path().trim_start_matches('/').split('/').next().filter(|v| !v.is_empty()).unwrap_or("/")));
        short(&mut open, "");
        open.push(0);
        a.send(METHOD, 0, &open).await?;
        a.expect(10, 41).await?;
        let mut channel = method(20, 10);
        short(&mut channel, "");
        a.send(METHOD, CHANNEL, &channel).await?;
        a.expect(20, 11).await?;
        Ok((a, frame_max))
    }

    /// The next frame of any kind, or nothing if none came in time. Safe to give up on: what has
    /// arrived of a frame stays here for the next call.
    async fn frame(&mut self, wait: Duration) -> Result<Option<(u8, Vec<u8>)>, String> {
        loop {
            if let Some(head) = self.pending.get(..7) {
                let size = u32::from_be_bytes([head[3], head[4], head[5], head[6]]) as usize;
                if size > 1 << 20 {
                    return Err("that is not AMQP".into());
                }
                if self.pending.len() > 7 + size {
                    let whole: Vec<u8> = self.pending.drain(..8 + size).collect();
                    if whole[7 + size] != FRAME_END {
                        return Err("that is not AMQP".into());
                    }
                    return Ok(Some((whole[0], whole[7..7 + size].to_vec())));
                }
            }
            match timeout(wait, self.stream.read_buf(&mut self.pending)).await {
                Err(_) => return Ok(None),
                Ok(Ok(0)) => return Err("it hung up".into()),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(e.to_string()),
            }
        }
    }

    async fn send(&mut self, kind: u8, channel: u16, payload: &[u8]) -> Result<(), String> {
        let mut frame = vec![kind];
        frame.extend(channel.to_be_bytes());
        long(&mut frame, payload);
        frame.push(FRAME_END);
        self.stream.get_mut().write_all(&frame).await.map_err(|e| e.to_string())
    }

    /// The next method, and what comes with it. A close is why this ends, so it comes back as that.
    async fn next(&mut self) -> Result<(u16, u16, Vec<u8>), String> {
        loop {
            let mut head = [0u8; 7];
            timeout(PATIENCE, self.stream.read_exact(&mut head)).await.map_err(|_| "it went quiet")?.map_err(|e| e.to_string())?;
            let size = u32::from_be_bytes([head[3], head[4], head[5], head[6]]) as usize;
            if size > 1 << 20 {
                return Err("that is not AMQP".into());
            }
            let mut payload = vec![0u8; size + 1];
            timeout(PATIENCE, self.stream.read_exact(&mut payload)).await.map_err(|_| "it went quiet")?.map_err(|e| e.to_string())?;
            if payload.pop() != Some(FRAME_END) {
                return Err("that is not AMQP".into());
            }
            if head[0] != METHOD || size < 4 {
                continue;
            }
            let class = u16::from_be_bytes([payload[0], payload[1]]);
            let id = u16::from_be_bytes([payload[2], payload[3]]);
            if matches!((class, id), (10, 50) | (20, 40)) {
                return Err(why_closed(&payload));
            }
            return Ok((class, id, payload[4..].to_vec()));
        }
    }

    async fn expect(&mut self, class: u16, id: u16) -> Result<Vec<u8>, String> {
        let (c, i, args) = self.next().await?;
        if (c, i) == (class, id) { Ok(args) } else { Err(format!("it said {c}.{i} and not {class}.{id}")) }
    }
}

/// RabbitMQ and the like. `topic` is the routing key and `?exchange=` where it goes, and publisher
/// confirms are the yes: a message is confirmed once the broker has taken responsibility for it.
pub async fn amqp(url: &Url, key: &str, bodies: &[String], secret: Option<&str>) -> Result<(), String> {
    let (mut a, frame_max) = Amqp::open(url, url.username(), secret.unwrap_or("guest"), 0).await?;
    let mut confirm = method(85, 10);
    confirm.push(0);
    a.send(METHOD, CHANNEL, &confirm).await?;
    a.expect(85, 11).await?;

    let exchange = query(url, "exchange").unwrap_or_default();
    for body in bodies {
        let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let mut publish = method(60, 40);
        publish.extend(0u16.to_be_bytes());
        short(&mut publish, &exchange);
        short(&mut publish, &named(key, &value));
        publish.push(0);
        a.send(METHOD, CHANNEL, &publish).await?;
        let mut header = 60u16.to_be_bytes().to_vec();
        header.extend(0u16.to_be_bytes());
        header.extend((body.len() as u64).to_be_bytes());
        // Content type and delivery mode are present, and persistent means it outlives a restart.
        header.extend(0x9000u16.to_be_bytes());
        short(&mut header, "application/json");
        header.push(2);
        a.send(HEADER, CHANNEL, &header).await?;
        for piece in body.as_bytes().chunks(frame_max as usize - 8) {
            a.send(BODY, CHANNEL, piece).await?;
        }
    }
    let mut confirmed = 0u64;
    while confirmed < bodies.len() as u64 {
        let (class, id, args) = a.next().await?;
        let tag = args.get(..8).map_or(0, |b| u64::from_be_bytes(b.try_into().unwrap_or_default()));
        let multiple = args.get(8).is_some_and(|f| f & 1 == 1);
        match (class, id) {
            (60, 80) => confirmed = if multiple { tag } else { confirmed + 1 },
            (60, 120) => return Err("the broker refused a message".into()),
            _ => {}
        }
    }
    let mut close = method(10, 50);
    close.extend(200u16.to_be_bytes());
    short(&mut close, "done");
    close.extend([0u8; 4]);
    let _ = a.send(METHOD, 0, &close).await;
    Ok(())
}

/// Queues as a way in. Each message is acknowledged once it has been handed on, so one that was not
/// comes again after a reconnect, and one may arrive twice.
pub async fn amqp_read(url: &Url, login: (&str, &str), queues: &[String], tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
    // Half the agreed silence, and so a broker that says nothing for two of these has gone.
    let beat = Duration::from_secs(u64::from(HEARTBEAT) / 2);
    let (mut a, _) = Amqp::open(url, login.0, login.1, HEARTBEAT).await?;
    let mut qos = method(60, 10);
    qos.extend(0u32.to_be_bytes());
    qos.extend(PREFETCH.to_be_bytes());
    qos.push(0);
    a.send(METHOD, CHANNEL, &qos).await?;
    a.expect(60, 11).await?;
    for queue in queues {
        let mut consume = method(60, 20);
        consume.extend(0u16.to_be_bytes());
        short(&mut consume, queue);
        short(&mut consume, "");
        consume.push(0);
        consume.extend(0u32.to_be_bytes());
        a.send(METHOD, CHANNEL, &consume).await?;
        a.expect(60, 21).await?;
    }
    let _ = ready.send(()).await;
    // A message comes in three frames: a method, a header saying how long, and then its body.
    let (mut quiet, mut delivery, mut expected, mut body) = (0, None, 0, Vec::new());
    loop {
        let got = tokio::select! {
            () = tx.closed() => return Ok(()),
            got = a.frame(beat) => got?,
        };
        let Some((kind, payload)) = got else {
            quiet += 1;
            if quiet > 4 {
                return Err("it went quiet".into());
            }
            a.send(HEARTBEAT_FRAME, 0, &[]).await?;
            continue;
        };
        quiet = 0;
        match kind {
            METHOD if payload.len() >= 4 => {
                let (class, id) = (u16::from_be_bytes([payload[0], payload[1]]), u16::from_be_bytes([payload[2], payload[3]]));
                if matches!((class, id), (10, 50) | (20, 40)) {
                    return Err(why_closed(&payload));
                }
                if (class, id) == (60, 60) {
                    delivery = delivered(&payload[4..]);
                    body.clear();
                    expected = usize::MAX;
                }
            }
            HEADER if payload.len() >= 12 => expected = u64::from_be_bytes(payload[4..12].try_into().unwrap_or_default()) as usize,
            BODY => body.extend(payload),
            _ => {}
        }
        if expected <= body.len()
            && let Some((tag, key)) = delivery.take()
        {
            if tx.send(Message { topic: key, body: std::mem::take(&mut body), status: None, from: None }).await.is_err() {
                return Ok(());
            }
            let mut ack = method(60, 80);
            ack.extend(tag.to_be_bytes());
            ack.push(0);
            a.send(METHOD, CHANNEL, &ack).await?;
        }
    }
}

/// A delivery's tag and routing key, from after its consumer tag.
fn delivered(args: &[u8]) -> Option<(u64, String)> {
    let at = 1 + usize::from(*args.first()?);
    let tag = u64::from_be_bytes(args.get(at..at + 8)?.try_into().ok()?);
    let exchange = at + 9;
    let key = exchange + 1 + usize::from(*args.get(exchange)?);
    let size = usize::from(*args.get(key)?);
    Some((tag, String::from_utf8_lossy(args.get(key + 1..key + 1 + size)?).into_owned()))
}

fn why_closed(payload: &[u8]) -> String {
    // A close says why: a reply code and then a short string.
    let length = payload.get(6).copied().unwrap_or(0) as usize;
    payload.get(7..7 + length).map_or_else(|| "it closed the connection".to_string(), |t| String::from_utf8_lossy(t).into_owned())
}

fn tag(text: &str) -> String {
    text.replace('\\', "\\\\").replace(',', "\\,").replace('=', "\\=").replace(' ', "\\ ").replace('\n', "\\n")
}

/// A measurement in InfluxDB's line protocol, or nothing for something that is not one.
pub fn line(body: &Value) -> Option<String> {
    let value = body["value"].as_f64()?;
    let at = body["at"].as_str()?.parse::<DateTime<Utc>>().ok()?.timestamp_millis();
    Some(format!("{},device={} value={value} {at}", tag(body["metric"].as_str()?), tag(body["device"].as_str()?)))
}

/// InfluxDB 2 and 3. A batch either lands or is refused as a whole.
pub async fn influx(http: &reqwest::Client, url: &Url, bodies: &[String], secret: Option<&str>) -> Result<(), String> {
    let lines: Vec<String> = bodies.iter().filter_map(|b| line(&serde_json::from_str(b).ok()?)).collect();
    if lines.is_empty() {
        return Ok(());
    }
    let scheme = if url.scheme() == "influxs" { "https" } else { "http" };
    let (org, bucket) = (query(url, "org").ok_or("say ?org=")?, query(url, "bucket").ok_or("say ?bucket=")?);
    let host = url.host_str().ok_or("it names no host")?;
    let port = url.port().unwrap_or(8086);
    let mut request =
        http.post(format!("{scheme}://{host}:{port}/api/v2/write")).query(&[("org", org.as_str()), ("bucket", bucket.as_str()), ("precision", "ms")]);
    if let Some(token) = secret {
        request = request.header("authorization", format!("Token {token}"));
    }
    said(request.body(lines.join("\n"))).await.map(drop)
}

/// ClickHouse, over its HTTP interface. Rows are the measurements as they are, one to a line.
pub async fn clickhouse(http: &reqwest::Client, url: &Url, bodies: &[String], secret: Option<&str>) -> Result<(), String> {
    let table = url.path().trim_matches('/');
    if table.is_empty() || !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        return Err("say which table: clickhouse://host:8123/measurements".into());
    }
    let scheme = if url.scheme() == "clickhouses" { "https" } else { "http" };
    let host = url.host_str().ok_or("it names no host")?;
    let mut request = http
        .post(format!("{scheme}://{host}:{}/", url.port().unwrap_or(8123)))
        .query(&[("query", format!("INSERT INTO {table} FORMAT JSONEachRow"))])
        .query(&[("date_time_input_format", "best_effort")]);
    if let Some(password) = secret {
        request = request.header("x-clickhouse-user", if url.username().is_empty() { "default" } else { url.username() }).header("x-clickhouse-key", password);
    }
    said(request.body(bodies.join("\n"))).await.map(drop)
}

async fn said(request: reqwest::RequestBuilder) -> Result<String, String> {
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_success() { Ok(text) } else { Err(format!("it answered {status}: {}", text.chars().take(300).collect::<String>())) }
}

fn hmac(key: &[u8], text: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any length of key");
    mac.update(text);
    mac.finalize().into_bytes().to_vec()
}

/// Signature Version 4, which every AWS service asks for, and so does anything that speaks S3.
pub struct Aws<'a> {
    pub access: &'a str,
    pub secret: &'a str,
    pub region: &'a str,
    pub service: &'a str,
}

impl Aws<'_> {
    /// `path` and `query` go as they will on the wire. Returns the headers to send, these included.
    pub fn sign(&self, method: &str, host: &str, path: &str, query: &str, headers: &[(&str, &str)], body: &[u8], now: DateTime<Utc>) -> Vec<(String, String)> {
        let (date, stamp) = (now.format("%Y%m%d").to_string(), now.format("%Y%m%dT%H%M%SZ").to_string());
        let payload = hex::encode(Sha256::digest(body));
        let mut signed: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_lowercase(), v.trim().to_string())).collect();
        signed.push(("host".into(), host.into()));
        signed.push(("x-amz-date".into(), stamp.clone()));
        if self.service == "s3" {
            signed.push(("x-amz-content-sha256".into(), payload.clone()));
        }
        signed.sort();
        let canonical: String = signed.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let names = signed.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        let request = format!("{method}\n{path}\n{query}\n{canonical}\n{names}\n{payload}");
        let scope = format!("{date}/{}/{}/aws4_request", self.region, self.service);
        let to_sign = format!("AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}", hex::encode(Sha256::digest(request.as_bytes())));
        let mut key = hmac(format!("AWS4{}", self.secret).as_bytes(), date.as_bytes());
        for part in [self.region, self.service, "aws4_request"] {
            key = hmac(&key, part.as_bytes());
        }
        let signature = hex::encode(hmac(&key, to_sign.as_bytes()));
        signed.push(("authorization".into(), format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}", self.access)));
        signed
    }
}

/// Where an AWS call goes: its real address, or a stand-in named by `?endpoint=`.
fn endpoint(url: &Url, real: String) -> Result<(String, String), String> {
    let at = query(url, "endpoint").unwrap_or(real);
    let parsed = Url::parse(&at).map_err(|e| format!("{at}: {e}"))?;
    let host = parsed.host_str().ok_or("it names no host")?;
    let host = parsed.port().map_or_else(|| host.to_string(), |p| format!("{host}:{p}"));
    Ok((at.trim_end_matches('/').to_string(), host))
}

fn keys(secret: Option<&str>) -> Result<(&str, &str), String> {
    secret.and_then(|s| s.split_once(':')).ok_or_else(|| "this needs a secret holding ACCESS_KEY:SECRET_KEY".into())
}

fn region_of(url: &Url) -> Result<String, String> {
    query(url, "region").or_else(|| url.host_str()?.split('.').nth(1).map(str::to_string)).ok_or_else(|| "say ?region=".into())
}

/// A part of an address with its escapes undone: a vhost is often `%2F`.
fn decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(bytes.len()), 0);
    while i < bytes.len() {
        match bytes.get(i + 1..i + 3).filter(|_| bytes[i] == b'%').and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()) {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn encoded(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// Amazon S3 or anything that speaks it. A batch is an object of one JSON value to a line, named by
/// what is in it, so a batch sent twice is the same object.
pub async fn s3(http: &reqwest::Client, url: &Url, bodies: &[String], secret: Option<&str>, now: DateTime<Utc>) -> Result<(), String> {
    let (access, key) = keys(secret)?;
    let region = query(url, "region").ok_or("say ?region=")?;
    let bucket = url.host_str().ok_or("say which bucket: s3://bucket/prefix")?;
    let body = bodies.join("\n").into_bytes();
    let name = format!("{}/{}.jsonl", now.format("%Y/%m/%d/%H"), &hex::encode(Sha256::digest(&body))[..32]);
    let prefix = url.path().trim_matches('/');
    let object = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
    let (base, host) = endpoint(url, format!("https://{bucket}.s3.{region}.amazonaws.com"))?;
    // A stand-in has one name for every bucket, so the bucket goes in the path.
    let path = if query(url, "endpoint").is_some() { format!("/{bucket}/{object}") } else { format!("/{object}") };
    let aws = Aws { access, secret: key, region: &region, service: "s3" };
    let mut request = http.put(format!("{base}{path}")).body(body.clone());
    for (k, v) in aws.sign("PUT", &host, &path, "", &[], &body, now) {
        if k != "host" {
            request = request.header(k, v);
        }
    }
    said(request).await.map(drop)
}

/// Amazon SQS, ten messages to a call. One refused is the whole batch again: at least once.
pub async fn sqs(http: &reqwest::Client, url: &Url, bodies: &[String], secret: Option<&str>, now: DateTime<Utc>) -> Result<(), String> {
    let (access, key) = keys(secret)?;
    let region = region_of(url)?;
    let host = url.host_str().ok_or("it names no host")?;
    let queue = format!("https://{host}{}", url.path());
    let (base, host) = endpoint(url, format!("https://{host}"))?;
    let aws = Aws { access, secret: key, region: &region, service: "sqs" };
    for chunk in bodies.chunks(10) {
        let entries: Vec<Value> = chunk.iter().enumerate().map(|(i, b)| json!({"Id": i.to_string(), "MessageBody": b})).collect();
        let body = json!({"QueueUrl": queue, "Entries": entries}).to_string().into_bytes();
        let headers = [("content-type", "application/x-amz-json-1.0"), ("x-amz-target", "AmazonSQS.SendMessageBatch")];
        let mut request = http.post(format!("{base}/")).body(body.clone());
        for (k, v) in aws.sign("POST", &host, "/", "", &headers, &body, now) {
            if k != "host" {
                request = request.header(k, v);
            }
        }
        let answer: Value = serde_json::from_str(&said(request).await?).unwrap_or(Value::Null);
        if answer["Failed"].as_array().is_some_and(|f| !f.is_empty()) {
            return Err(format!("it refused some: {}", answer["Failed"]));
        }
    }
    Ok(())
}

/// Amazon SNS, ten messages to a call.
pub async fn sns(http: &reqwest::Client, url: &Url, bodies: &[String], secret: Option<&str>, now: DateTime<Utc>) -> Result<(), String> {
    let (access, key) = keys(secret)?;
    let region = region_of(url)?;
    let topic = query(url, "topic").ok_or("say ?topic=arn:aws:sns:...")?;
    let (base, host) = endpoint(url, format!("https://{}", url.host_str().ok_or("it names no host")?))?;
    let aws = Aws { access, secret: key, region: &region, service: "sns" };
    for chunk in bodies.chunks(10) {
        let mut form = format!("Action=PublishBatch&Version=2010-03-31&TopicArn={}", encoded(&topic));
        for (i, b) in chunk.iter().enumerate() {
            let n = i + 1;
            form.push_str(&format!("&PublishBatchRequestEntries.member.{n}.Id={i}&PublishBatchRequestEntries.member.{n}.Message={}", encoded(b)));
        }
        let headers = [("content-type", "application/x-www-form-urlencoded")];
        let mut request = http.post(format!("{base}/")).body(form.clone());
        for (k, v) in aws.sign("POST", &host, "/", "", &headers, form.as_bytes(), now) {
            if k != "host" {
                request = request.header(k, v);
            }
        }
        let answer = said(request).await?;
        if answer.contains("<SenderFault>") {
            return Err("it refused some".into());
        }
    }
    Ok(())
}

/// A shared access signature, which is how Azure IoT Hub and Event Hubs are told who is calling.
pub fn shared_access(resource: &str, key_base64: &str, policy: Option<&str>, expires: i64) -> Result<String, String> {
    let key = base64(key_base64).ok_or("that key is not base64")?;
    let resource = encoded(resource);
    let signature = base64_of(&hmac(&key, format!("{resource}\n{expires}").as_bytes()));
    let mut token = format!("SharedAccessSignature sr={resource}&sig={}&se={expires}", encoded(&signature));
    if let Some(policy) = policy {
        token.push_str(&format!("&skn={}", encoded(policy)));
    }
    Ok(token)
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_of(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = u32::from(chunk[0]) << 16 | u32::from(*chunk.get(1).unwrap_or(&0)) << 8 | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

pub fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut bits, mut n) = (0u32, 0u32);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        bits = bits << 6 | ALPHABET.iter().position(|a| *a == c)? as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    Some(out)
}

/// Azure: Event Hubs from `eventhub://namespace.servicebus.windows.net/hub`, and IoT Hub from
/// `azure://hub.azure-devices.net/device`. The secret is `policy:key`, or the device's key alone.
pub async fn azure(http: &reqwest::Client, url: &Url, bodies: &[String], secret: Option<&str>, now: DateTime<Utc>) -> Result<(), String> {
    let secret = secret.ok_or("this needs a secret holding the key")?;
    let (policy, key) = secret.split_once(':').map_or((None, secret), |(p, k)| (Some(p), k));
    let host = url.host_str().ok_or("it names no host")?;
    let name = url.path().trim_matches('/');
    let (resource, address, kind, items) = if url.scheme() == "eventhub" {
        let items: Vec<Value> = bodies.iter().map(|b| json!({"Body": b})).collect();
        (format!("https://{host}/{name}"), format!("/{name}/messages?timeout=60&api-version=2014-01"), "application/vnd.microsoft.servicebus.json", items)
    } else {
        let items: Vec<Value> = bodies.iter().map(|b| json!({"body": base64_of(b.as_bytes()), "base64Encoded": true})).collect();
        (format!("{host}/devices/{name}"), format!("/devices/{name}/messages/events?api-version=2021-04-12"), "application/vnd.microsoft.iothub.json", items)
    };
    let (base, _) = endpoint(url, format!("https://{host}"))?;
    let token = shared_access(&resource, key, policy, now.timestamp() + 3600)?;
    said(http.post(format!("{base}{address}")).header("authorization", token).header("content-type", kind).body(Value::Array(items).to_string()))
        .await
        .map(drop)
}

/// A signed request for a token, from a service account's key, as Google asks (RFC 7523).
pub fn google_assertion(account: &Value, scope: &str, now: i64) -> Result<String, String> {
    let (email, pem, audience) = (account["client_email"].as_str(), account["private_key"].as_str(), account["token_uri"].as_str());
    let (Some(email), Some(pem), Some(audience)) = (email, pem, audience) else { return Err("that is not a service account's key".into()) };
    let der: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let key = ring::signature::RsaKeyPair::from_pkcs8(&base64(&der).ok_or("that key is not base64")?).map_err(|e| e.to_string())?;
    let url = |bytes: &[u8]| base64_of(bytes).trim_end_matches('=').replace('+', "-").replace('/', "_");
    let claims = json!({"iss": email, "scope": scope, "aud": audience, "iat": now, "exp": now + 3600});
    let text = format!("{}.{}", url(br#"{"alg":"RS256","typ":"JWT"}"#), url(claims.to_string().as_bytes()));
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(&ring::signature::RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), text.as_bytes(), &mut signature).map_err(|e| e.to_string())?;
    Ok(format!("{text}.{}", url(&signature)))
}

/// Google Cloud Pub/Sub, from `pubsub://project/topic`. With `?endpoint=` it talks to the emulator,
/// which asks for no token.
pub async fn pubsub(http: &reqwest::Client, url: &Url, bodies: &[String], secret: Option<&str>, now: DateTime<Utc>) -> Result<(), String> {
    let project = url.host_str().ok_or("say which project: pubsub://project/topic")?;
    let topic = url.path().trim_matches('/');
    let (base, _) = endpoint(url, "https://pubsub.googleapis.com".into())?;
    let messages: Vec<Value> = bodies.iter().map(|b| json!({"data": base64_of(b.as_bytes())})).collect();
    let mut request = http.post(format!("{base}/v1/projects/{project}/topics/{topic}:publish")).json(&json!({"messages": messages}));
    if query(url, "endpoint").is_none() {
        let account: Value = serde_json::from_str(secret.ok_or("this needs a secret holding a service account's key")?).map_err(|e| e.to_string())?;
        let assertion = google_assertion(&account, "https://www.googleapis.com/auth/pubsub", now.timestamp())?;
        let form = format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}");
        let token: Value = serde_json::from_str(
            &said(http.post(account["token_uri"].as_str().unwrap_or_default()).header("content-type", "application/x-www-form-urlencoded").body(form)).await?,
        )
        .map_err(|e| e.to_string())?;
        request = request.bearer_auth(token["access_token"].as_str().ok_or("it gave no token")?);
    }
    said(request).await.map(drop)
}

/// A gateway as a Sparkplug edge node, for a SCADA system to see what it reads: each device with a
/// birth certificate, then its data, and it starts again if asked. `sparkplug://broker:1883/group/node`.
pub struct EdgeNode {
    group: String,
    node: String,
    /// Says which birth a death belongs to. It lasts as long as the process, as its will does.
    birth: u64,
    again: AtomicBool,
    state: tokio::sync::Mutex<Birthed>,
}

#[derive(Default)]
struct Birthed {
    born: bool,
    seq: u64,
    devices: BTreeMap<String, BTreeMap<String, f64>>,
}

const REBIRTH: &str = "Node Control/Rebirth";

impl EdgeNode {
    pub fn new(url: &Url) -> Result<EdgeNode, String> {
        let mut path = url.path().trim_matches('/').split('/');
        let (Some(group), Some(node)) = (path.next().filter(|g| !g.is_empty()), path.next()) else {
            return Err("say the group and the node: sparkplug://broker/group/node".into());
        };
        Ok(EdgeNode {
            group: group.into(),
            node: node.into(),
            birth: Utc::now().timestamp().unsigned_abs() % 256,
            again: AtomicBool::new(false),
            state: Default::default(),
        })
    }

    pub fn topic(&self, kind: &str, device: Option<&str>) -> String {
        let (group, node) = (&self.group, &self.node);
        device.map_or_else(|| format!("spBv1.0/{group}/{kind}/{node}"), |d| format!("spBv1.0/{group}/{kind}/{node}/{d}"))
    }

    /// What the broker says if this goes away.
    pub fn will(&self) -> Result<(String, Vec<u8>), String> {
        let body =
            sparkplug::encode(&json!({"timestamp": Utc::now().timestamp_millis(), "metrics": [{"name": "bdSeq", "value": self.birth, "type": "int64"}]}))?;
        Ok((self.topic("NDEATH", None), body))
    }

    /// A command to this node. The only one it takes is to start again.
    pub fn heard(&self, body: &[u8]) {
        let asked = sparkplug::decode(body).is_some_and(|p| p["metrics"].as_array().into_iter().flatten().any(|m| m["name"] == REBIRTH && m["value"] == true));
        if asked {
            self.again.store(true, Ordering::SeqCst);
        }
    }

    async fn say(&self, broker: &dyn Transport, state: &mut Birthed, kind: &str, device: Option<&str>, metrics: Value) -> Result<(), String> {
        let payload = json!({"timestamp": Utc::now().timestamp_millis(), "seq": state.seq, "metrics": metrics});
        state.seq = (state.seq + 1) % 256;
        let out = Out::Publish { topic: self.topic(kind, device), body: sparkplug::encode(&payload)?, expiry: None, retain: false };
        broker.send(out).await.map(drop).map_err(|e| match e {
            Failure::NotSent(w) | Failure::Unknown(w) => w,
        })
    }

    async fn birth_of(&self, broker: &dyn Transport, state: &mut Birthed, device: &str) -> Result<(), String> {
        let metrics: Vec<Value> = state.devices[device].iter().map(|(name, value)| json!({"name": name, "value": value, "type": "double"})).collect();
        self.say(broker, state, "DBIRTH", Some(device), json!(metrics)).await
    }

    /// Everything this node has said so far, again, if it has to be. Then the new readings.
    pub async fn send(&self, broker: &dyn Transport, bodies: &[String]) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if !state.born || self.again.swap(false, Ordering::SeqCst) {
            state.seq = 0;
            let metrics = json!([{"name": "bdSeq", "value": self.birth, "type": "int64"}, {"name": REBIRTH, "value": false, "type": "boolean"}]);
            self.say(broker, &mut state, "NBIRTH", None, metrics).await?;
            state.born = true;
            let devices: Vec<String> = state.devices.keys().cloned().collect();
            for device in devices {
                self.birth_of(broker, &mut state, &device).await?;
            }
        }
        let mut by_device: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
        for body in bodies {
            let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            if let (Some(device), Some(metric), Some(value)) = (v["device"].as_str(), v["metric"].as_str(), v["value"].as_f64()) {
                by_device.entry(device.to_string()).or_default().push((metric.to_string(), value));
            }
        }
        for (device, readings) in by_device {
            let known = state.devices.entry(device.clone()).or_default();
            let new = readings.iter().any(|(metric, _)| !known.contains_key(metric));
            known.extend(readings.iter().cloned());
            if new {
                self.birth_of(broker, &mut state, &device).await?;
            } else {
                let metrics: Vec<Value> = readings.iter().map(|(name, value)| json!({"name": name, "value": value, "type": "double"})).collect();
                self.say(broker, &mut state, "DDATA", Some(&device), json!(metrics)).await?;
            }
        }
        Ok(())
    }
}

#[cfg(feature = "kafka")]
mod kafka {
    use super::*;
    use rskafka::client::partition::{Compression, OffsetAt, UnknownTopicHandling};
    use rskafka::client::{Client, ClientBuilder, Credentials, SaslConfig};
    use rskafka::record::Record;
    use std::collections::HashMap;

    /// A client stays connected and finds the leaders itself.
    #[derive(Default)]
    pub struct Kafka {
        clients: tokio::sync::Mutex<HashMap<String, Arc<Client>>>,
    }

    fn partition_of(device: &str, partitions: i32) -> i32 {
        let hash = device.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
        (hash % partitions.max(1) as u32) as i32
    }

    impl Kafka {
        /// A device's messages stay in order, by keeping each to one partition.
        pub async fn send(&self, url: &Url, topic: &str, bodies: &[String], secret: Option<&str>) -> Result<(), String> {
            let client = self.client(url, secret).await?;
            let topics = client.list_topics().await.map_err(|e| e.to_string())?;
            let partitions = topics.iter().find(|t| t.name == topic).map_or(1, |t| t.partitions.len() as i32);
            let mut by_partition: BTreeMap<i32, Vec<Record>> = BTreeMap::new();
            for body in bodies {
                let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let device = value["device"].as_str().unwrap_or_default();
                let record =
                    Record { key: Some(device.as_bytes().to_vec()), value: Some(body.clone().into_bytes()), headers: BTreeMap::new(), timestamp: Utc::now() };
                by_partition.entry(partition_of(device, partitions)).or_default().push(record);
            }
            for (partition, records) in by_partition {
                let producer = client.partition_client(topic, partition, UnknownTopicHandling::Error).await.map_err(|e| e.to_string())?;
                producer.produce(records, Compression::NoCompression).await.map_err(|e| e.to_string())?;
            }
            Ok(())
        }

        /// Topics as a way in, from where this last got to. That is kept in a file beside the
        /// journal, every few seconds, so after a crash a few seconds may arrive twice.
        // ponytail: no consumer group, so two gateways on a topic each hear all of it. Sharing a
        // topic out needs a coordinator, which this client does not have.
        pub async fn read(
            &self,
            url: &Url,
            secret: Option<&str>,
            reading: Reading<'_>,
            tx: mpsc::Sender<Message>,
            ready: mpsc::Sender<()>,
        ) -> Result<(), String> {
            let Reading { topics, from_start, kept } = reading;
            let client = self.client(url, secret).await?;
            let known = client.list_topics().await.map_err(|e| e.to_string())?;
            let saved: HashMap<String, i64> = kept.and_then(|f| serde_json::from_slice(&std::fs::read(f).ok()?).ok()).unwrap_or_default();
            let offsets = Arc::new(std::sync::Mutex::new(saved));
            let mut tasks = tokio::task::JoinSet::new();
            for topic in topics {
                let partitions: Vec<i32> = known.iter().find(|t| t.name == *topic).map_or(vec![0], |t| t.partitions.iter().copied().collect());
                for partition in partitions {
                    tasks.spawn(watch(client.clone(), topic.clone(), partition, from_start, offsets.clone(), tx.clone()));
                }
            }
            let _ = ready.send(()).await;
            let keep = |offsets: &std::sync::Mutex<HashMap<String, i64>>| {
                let Some(file) = kept else { return };
                let Ok(body) = serde_json::to_vec(&*offsets.lock().unwrap_or_else(|e| e.into_inner())) else { return };
                let fresh = file.with_extension("new");
                if let Err(e) = std::fs::write(&fresh, body).and_then(|()| std::fs::rename(&fresh, file)) {
                    tracing::warn!("cannot keep where this got to in {}: {e}", file.display());
                }
            };
            let mut every = tokio::time::interval(Duration::from_secs(5));
            let ended = loop {
                tokio::select! {
                    () = tx.closed() => break Ok(()),
                    _ = every.tick() => keep(&offsets),
                    ended = tasks.join_next() => match ended {
                        Some(Ok(Err(why))) => break Err(why),
                        Some(_) => {}
                        None => break Ok(()),
                    },
                }
            };
            keep(&offsets);
            ended
        }

        async fn client(&self, url: &Url, secret: Option<&str>) -> Result<Arc<Client>, String> {
            let key = format!("{}:{}", url.host_str().unwrap_or_default(), url.port().unwrap_or(9092));
            let mut clients = self.clients.lock().await;
            if let Some(client) = clients.get(&key) {
                return Ok(client.clone());
            }
            let mut builder = ClientBuilder::new(vec![key.clone()]);
            if url.scheme() == "kafkas" {
                let mut roots = RootCertStore::empty();
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                crate::adapters::transport::crypto();
                builder = builder.tls_config(Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()));
            }
            if let Some(password) = secret {
                builder = builder.sasl_config(SaslConfig::Plain(Credentials::new(url.username().to_string(), password.to_string())));
            }
            let client = Arc::new(builder.build().await.map_err(|e| e.to_string())?);
            clients.insert(key, client.clone());
            Ok(client)
        }
    }

    /// What to read, and where to keep how far this has got.
    pub struct Reading<'a> {
        pub topics: &'a [String],
        /// Where to begin with nothing kept: the oldest record, or whatever comes next.
        pub from_start: bool,
        pub kept: Option<&'a std::path::Path>,
    }

    async fn watch(
        client: Arc<Client>,
        topic: String,
        partition: i32,
        from_start: bool,
        offsets: Arc<std::sync::Mutex<HashMap<String, i64>>>,
        tx: mpsc::Sender<Message>,
    ) -> Result<(), String> {
        let reader = client.partition_client(topic.clone(), partition, UnknownTopicHandling::Retry).await.map_err(|e| e.to_string())?;
        let key = format!("{topic}/{partition}");
        let oldest = reader.get_offset(OffsetAt::Earliest).await.map_err(|e| e.to_string())?;
        let saved = offsets.lock().unwrap_or_else(|e| e.into_inner()).get(&key).copied();
        let mut at = match saved {
            // What was kept may have aged out while this was away.
            Some(at) => at.max(oldest),
            None if from_start => oldest,
            None => reader.get_offset(OffsetAt::Latest).await.map_err(|e| e.to_string())?,
        };
        loop {
            let (records, _) = reader.fetch_records(at, 1..1_000_000, 1_000).await.map_err(|e| e.to_string())?;
            for r in records {
                at = r.offset + 1;
                if tx.send(Message { topic: topic.clone(), body: r.record.value.unwrap_or_default(), status: None, from: None }).await.is_err() {
                    return Ok(());
                }
                offsets.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), at);
            }
        }
    }

    #[test]
    fn a_device_keeps_to_one_partition() {
        assert_eq!(partition_of("PRESS-1", 8), partition_of("PRESS-1", 8));
        assert!((0..4).contains(&partition_of("PRESS-2", 4)));
        assert_eq!(partition_of("anything", 1), 0);
    }
}

#[cfg(feature = "kafka")]
pub use kafka::{Kafka, Reading};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_escape_in_an_address_is_undone() {
        assert_eq!(decoded("%2F"), "/");
        assert_eq!(decoded("plant"), "plant");
        assert_eq!(decoded("a%2"), "a%2", "half an escape is not one");
    }

    #[test]
    fn names_become_a_subject() {
        let body = json!({"device": "Press 1", "metric": "temp.c"});
        assert_eq!(named("ferrum.{device}.{metric}", &body), "ferrum.Press_1.temp.c");
        assert_eq!(named("x.{device}", &json!(1)), "x.unknown");
    }

    #[test]
    fn line_protocol_escapes_what_it_must() {
        let body = json!({"device": "Press 1,a=b", "metric": "t c", "value": 21.5, "at": "2026-11-02T00:00:00Z"});
        assert_eq!(line(&body).unwrap(), "t\\ c,device=Press\\ 1\\,a\\=b value=21.5 1793577600000");
        assert_eq!(line(&json!({"anything": "else"})), None, "what is not a measurement is not one here either");
    }

    #[test]
    fn aws_signatures_match_amazons_own_example() {
        // `get-vanilla` from Amazon's published SigV4 test suite.
        let aws = Aws { access: "AKIDEXAMPLE", secret: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", region: "us-east-1", service: "service" };
        let at = "2015-08-30T12:36:00Z".parse().unwrap();
        let headers = aws.sign("GET", "example.amazonaws.com", "/", "", &[], b"", at);
        let (_, authorization) = headers.iter().find(|(k, _)| k == "authorization").unwrap();
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn base64_round_trips() {
        for text in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            assert_eq!(base64(&base64_of(text.as_bytes())).unwrap(), text.as_bytes());
        }
        assert_eq!(base64_of(b"foobar"), "Zm9vYmFy", "RFC 4648");
        assert_eq!(base64_of(b"fo"), "Zm8=");
        assert_eq!(base64("not base64!"), None);
    }

    #[test]
    fn a_shared_access_signature_is_what_azure_documents() {
        // The string to sign is the encoded resource, a newline and the expiry, under the decoded key.
        let token = shared_access("myhub.azure-devices.net/devices/d1", "c2VjcmV0", None, 1_800_000_000).unwrap();
        let signature = base64_of(&hmac(b"secret", b"myhub.azure-devices.net%2Fdevices%2Fd1\n1800000000"));
        assert_eq!(token, format!("SharedAccessSignature sr=myhub.azure-devices.net%2Fdevices%2Fd1&sig={}&se=1800000000", encoded(&signature)));
        assert!(shared_access("r", "c2VjcmV0", Some("owner"), 1).unwrap().ends_with("&skn=owner"));
    }

    #[test]
    fn a_key_that_is_not_one_says_so() {
        assert!(keys(Some("no colon")).is_err());
        assert_eq!(keys(Some("a:b:c")).unwrap(), ("a", "b:c"));
        assert!(google_assertion(&json!({"client_email": "x"}), "s", 0).unwrap_err().contains("service account"));
    }
}
