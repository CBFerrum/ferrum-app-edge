//! NATS as a way in: subjects in `subscribe` arrive as they are published, by subject, and a step
//! with a `topic` publishes. Core NATS, so what is published while this is away is gone, unless a
//! stream keeps it.
//!
//! `nats://bus:4222`. A token, or a password with `user@`, goes in the address as `{secret.name}`.

use crate::adapter::Failure;
use crate::adapters::tags::{ANSWER, host_port};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;

pub struct Nats {
    host: String,
    port: u16,
    token: Option<String>,
    subjects: Vec<String>,
    writer: Mutex<Option<OwnedWriteHalf>>,
}

impl Nats {
    pub fn new(rest: &str, subjects: &[String]) -> Nats {
        let (login, place) = rest.rsplit_once('@').map_or((None, rest), |(l, p)| (Some(l), p));
        let (host, port) = host_port(place, 4222);
        Nats { host, port, token: login.map(str::to_string), subjects: subjects.to_vec(), writer: Mutex::new(None) }
    }
}

#[async_trait]
impl Transport for Nats {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let stream = timeout(ANSWER, TcpStream::connect((self.host.as_str(), self.port))).await.map_err(|_| "it did not answer")?.map_err(|e| e.to_string())?;
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        timeout(ANSWER, reader.read_line(&mut line)).await.map_err(|_| "it said nothing")?.map_err(|e| e.to_string())?;
        if !line.starts_with("INFO") {
            return Err("that is not a NATS server".into());
        }
        let mut hello =
            json!({"verbose": false, "pedantic": false, "name": "ferrum-site", "lang": "rust", "version": env!("CARGO_PKG_VERSION"), "protocol": 0});
        match self.token.as_deref().map(|t| t.split_once(':')) {
            Some(Some((user, password))) => (hello["user"], hello["pass"]) = (json!(user), json!(password)),
            Some(None) => hello["auth_token"] = json!(self.token),
            None => {}
        }
        let mut opening = format!("CONNECT {hello}\r\n");
        for (i, subject) in self.subjects.iter().enumerate() {
            opening.push_str(&format!("SUB {subject} {i}\r\n"));
        }
        opening.push_str("PING\r\n");
        write.write_all(opening.as_bytes()).await.map_err(|e| e.to_string())?;
        *self.writer.lock().await = Some(write);
        let mut up = false;
        loop {
            line.clear();
            let n = tokio::select! {
                () = tx.closed() => return Ok(()),
                got = reader.read_line(&mut line) => got.map_err(|e| e.to_string())?,
            };
            if n == 0 {
                return Err("it hung up".into());
            }
            let said = line.trim_end();
            if said == "PONG" && !up {
                up = true;
                let _ = ready.send(()).await;
            } else if said == "PING" {
                if let Some(writer) = self.writer.lock().await.as_mut() {
                    writer.write_all(b"PONG\r\n").await.map_err(|e| e.to_string())?;
                }
            } else if said.starts_with("-ERR") {
                return Err(said.to_string());
            } else if let Some(rest) = said.strip_prefix("MSG ") {
                let parts: Vec<&str> = rest.split(' ').collect();
                let Some(size) = parts.last().and_then(|n| n.parse::<usize>().ok()) else { continue };
                let mut body = vec![0u8; size + 2];
                reader.read_exact(&mut body).await.map_err(|e| e.to_string())?;
                body.truncate(size);
                if tx.send(Message { topic: parts[0].to_string(), body, status: None, from: None }).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        let Out::Publish { topic, body, .. } = out else { return Err(Failure::NotSent("NATS takes a topic".into())) };
        let mut held = self.writer.lock().await;
        let writer = held.as_mut().ok_or_else(|| Failure::NotSent("not connected".into()))?;
        let mut frame = format!("PUB {topic} {}\r\n", body.len()).into_bytes();
        frame.extend(&body);
        frame.extend(b"\r\n");
        writer.write_all(&frame).await.map_err(|e| Failure::Unknown(e.to_string()))?;
        Ok(None)
    }
}
