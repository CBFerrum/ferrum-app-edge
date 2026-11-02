//! Equipment read and written one named thing at a time: a PLC's memory, a building controller's
//! objects, a device's OIDs. Each protocol says how to connect, read and write, and this holds the
//! connection, so that a definition uses `node:` alike for all of them. What arrives is `{value}`.

use crate::adapter::Failure;
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::{Mutex, Notify, mpsc};

/// Longest a request gets before what became of it is unknown.
pub const ANSWER: Duration = Duration::from_secs(5);

/// What went wrong with a request.
#[derive(Debug)]
pub enum Trouble {
    /// The far end said no: the connection is fine, and nothing changed.
    Refused(String),
    /// The connection is gone, and with it any idea of what happened.
    Lost(String),
}

impl From<std::io::Error> for Trouble {
    fn from(e: std::io::Error) -> Trouble {
        Trouble::Lost(e.to_string())
    }
}

impl From<tokio::time::error::Elapsed> for Trouble {
    fn from(_: tokio::time::error::Elapsed) -> Trouble {
        Trouble::Lost("it did not answer".into())
    }
}

#[async_trait]
pub trait Tags: Send {
    async fn read(&mut self, tag: &str) -> Result<Value, Trouble>;
    async fn write(&mut self, tag: &str, value: &Value) -> Result<(), Trouble>;
}

type Opening = Pin<Box<dyn Future<Output = Result<Box<dyn Tags>, String>> + Send>>;

pub struct Tagged {
    open: Box<dyn Fn() -> Opening + Send + Sync>,
    session: Mutex<Option<Box<dyn Tags>>>,
    gone: Notify,
}

impl Tagged {
    pub fn new(open: impl Fn() -> Opening + Send + Sync + 'static) -> Tagged {
        Tagged { open: Box::new(open), session: Mutex::new(None), gone: Notify::new() }
    }
}

#[async_trait]
impl Transport for Tagged {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let session = (self.open)().await?;
        *self.session.lock().await = Some(session);
        let _ = ready.send(()).await;
        // These protocols only answer, so a connection that has gone is found by whoever next uses it.
        tokio::select! {
            () = tx.closed() => Ok(()),
            () = self.gone.notified() => Err("the connection was lost".into()),
        }
    }

    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        let Out::Node { node, value } = out else { return Err(Failure::NotSent("this takes a node".into())) };
        let mut held = self.session.lock().await;
        let session = held.as_mut().ok_or_else(|| Failure::NotSent("not connected".into()))?;
        let outcome = match &value {
            None => session.read(&node).await.map(Some),
            Some(v) => session.write(&node, v).await.map(|()| None),
        };
        match outcome {
            Ok(None) => Ok(None),
            Ok(Some(v)) => Ok(Some(Message { topic: node, body: json!({"value": v}).to_string().into_bytes(), status: None, from: None })),
            Err(Trouble::Refused(why)) => Err(Failure::NotSent(format!("{node}: {why}"))),
            Err(Trouble::Lost(why)) => {
                *held = None;
                self.gone.notify_one();
                // A write that got this far may have happened. A read changed nothing.
                Err(if value.is_some() { Failure::Unknown(why) } else { Failure::NotSent(why) })
            }
        }
    }
}

pub fn number(value: &Value) -> Result<f64, Trouble> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .ok_or_else(|| Trouble::Refused(format!("{value} is not a number")))
}

pub fn yes(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => matches!(s.as_str(), "true" | "1" | "on"),
        _ => false,
    }
}

/// `host` and `port` out of what comes after the scheme, ignoring the rest.
pub fn host_port(rest: &str, default: u16) -> (String, u16) {
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    match authority.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse().unwrap_or(default)),
        None => (authority.to_string(), default),
    }
}

pub fn query(rest: &str, key: &str) -> Option<String> {
    let (_, q) = rest.split_once('?')?;
    q.split('&').filter_map(|p| p.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
}
