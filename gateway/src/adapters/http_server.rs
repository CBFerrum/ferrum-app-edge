//! HTTP for devices that call in, as ThingsBoard's devices do: `POST /api/v1/{token}/telemetry`. The
//! token in the path is the device's whole proof, so this belongs behind TLS on any network that is
//! not the site's own. A device that wants to hear commands keeps a request open, and has as long as
//! it asked for.

use crate::adapter::Failure;
use crate::adapters::hub::{Delivery, Hub, Proof};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

/// Largest body taken.
const MAX_BODY: usize = 1 << 20;
/// Longest a device can keep a request open.
const MAX_WAIT: Duration = Duration::from_secs(60);
/// What it gets when it does not say.
const WAIT: Duration = Duration::from_secs(10);

pub struct HttpServer {
    pub address: String,
    pub tls: Option<TlsAcceptor>,
    pub hub: Arc<Hub>,
}

#[derive(Clone)]
struct Served {
    hub: Arc<Hub>,
    tx: mpsc::Sender<Message>,
}

fn query<'a>(q: Option<&'a str>, key: &str) -> Option<&'a str> {
    q?.split('&').find_map(|pair| pair.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

fn patience(q: Option<&str>) -> Duration {
    query(q, "timeout").and_then(|t| t.parse().ok()).map_or(WAIT, Duration::from_millis).min(MAX_WAIT)
}

/// Waits for what a device wants to hear, on a session of its own for as long as the request lasts.
async fn hear(hub: &Hub, device: &str, filter: &str, wait: Duration) -> Option<Delivery> {
    let (id, mut mailbox) = hub.open(device, "");
    hub.listen(id, filter);
    let heard = tokio::time::timeout(wait, mailbox.recv()).await.ok().flatten();
    hub.close(id);
    heard
}

fn json_of(status: StatusCode, v: &Value) -> Response {
    (status, [("content-type", "application/json")], v.to_string()).into_response()
}

async fn handle(State(s): State<Served>, req: Request) -> Response {
    let (head, body) = req.into_parts();
    let path = head.uri.path().trim_matches('/').to_string();
    let parts: Vec<&str> = path.split('/').collect();
    let ["api", "v1", token, rest @ ..] = parts.as_slice() else { return StatusCode::NOT_FOUND.into_response() };
    let joining = *token == "provision" && rest.is_empty() && head.method == Method::POST;
    let device = s.hub.who(&Proof { username: Some(token), ..Default::default() });
    // Who a caller is comes first, so that a stranger cannot make this read a megabyte for nothing.
    if device.is_none() && !joining {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(body): Result<Bytes, _> = axum::body::to_bytes(body, MAX_BODY).await else { return StatusCode::PAYLOAD_TOO_LARGE.into_response() };
    let Some(device) = device else { return json_of(StatusCode::OK, &s.hub.joining(&s.tx, &body).await) };
    let q = head.uri.query();
    let say = async |topic: &str, body: Vec<u8>| -> Response {
        if s.hub.said(&s.tx, &device, topic, body).await { StatusCode::OK.into_response() } else { StatusCode::SERVICE_UNAVAILABLE.into_response() }
    };
    let is_json = || serde_json::from_slice::<Value>(&body).is_ok();
    match (&head.method, rest) {
        (&Method::POST, ["telemetry"]) if is_json() => say("v1/devices/me/telemetry", body.to_vec()).await,
        (&Method::POST, ["attributes"]) if is_json() => say("v1/devices/me/attributes", body.to_vec()).await,
        (&Method::POST, ["claim"]) => say("v1/devices/me/claim", if body.is_empty() { b"{}".to_vec() } else { body.to_vec() }).await,
        (&Method::POST, ["telemetry" | "attributes"]) => StatusCode::BAD_REQUEST.into_response(),
        (&Method::GET, ["attributes"]) => json_of(StatusCode::OK, &s.hub.attributes(&device, query(q, "clientKeys"), query(q, "sharedKeys"))),
        (&Method::GET, ["attributes", "updates"]) => match hear(&s.hub, &device, "v1/devices/me/attributes", patience(q)).await {
            Some(d) => (StatusCode::OK, [("content-type", "application/json")], d.body).into_response(),
            None => StatusCode::REQUEST_TIMEOUT.into_response(),
        },
        (&Method::GET, ["rpc"]) => match hear(&s.hub, &device, "v1/devices/me/rpc/request/+", patience(q)).await {
            Some(d) => {
                let id = d.topic.rsplit('/').next().unwrap_or_default().to_string();
                let mut call: Value = serde_json::from_slice(&d.body).unwrap_or(json!({}));
                call["id"] = id.parse::<u64>().map_or(json!(id), |n| json!(n));
                json_of(StatusCode::OK, &call)
            }
            None => StatusCode::REQUEST_TIMEOUT.into_response(),
        },
        (&Method::POST, ["rpc", id]) => say(&format!("v1/devices/me/rpc/response/{id}"), body.to_vec()).await,
        (&Method::POST, ["rpc"]) if is_json() => match s.hub.asked(&s.tx, &device, body.to_vec(), patience(q)).await {
            Ok(Some(answer)) => (StatusCode::OK, [("content-type", "application/json")], answer).into_response(),
            Ok(None) => StatusCode::REQUEST_TIMEOUT.into_response(),
            Err(()) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        (_, ["telemetry" | "attributes" | "rpc" | "claim", ..]) => StatusCode::METHOD_NOT_ALLOWED.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Connections finished with their handshakes, so that one stalled in it holds up nobody else.
struct Secured {
    ready: mpsc::Receiver<(TlsStream<TcpStream>, std::net::SocketAddr)>,
}

impl axum::serve::Listener for Secured {
    type Io = TlsStream<TcpStream>;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(next) => next,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

#[async_trait]
impl Transport for HttpServer {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let listener = TcpListener::bind(&self.address).await.map_err(|e| format!("{}: {e}", self.address))?;
        let app = Router::new().fallback(handle).with_state(Served { hub: self.hub.clone(), tx: tx.clone() });
        let _ = ready.send(()).await;
        let closed = async move { tx.closed().await };
        let Some(acceptor) = self.tls.clone() else {
            return axum::serve(listener, app).with_graceful_shutdown(closed).await.map_err(|e| e.to_string());
        };
        let (done, handshaken) = mpsc::channel(64);
        tokio::spawn(async move {
            while !done.is_closed() {
                let Ok((stream, peer)) = listener.accept().await else { continue };
                let (acceptor, done) = (acceptor.clone(), done.clone());
                tokio::spawn(async move {
                    if let Ok(Ok(secured)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await {
                        let _ = done.send((secured, peer)).await;
                    }
                });
            }
        });
        axum::serve(Secured { ready: handshaken }, app.into_make_service()).with_graceful_shutdown(closed).await.map_err(|e| e.to_string())
    }

    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        self.hub.publish(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_says_how_long_it_will_wait() {
        assert_eq!(patience(None), WAIT);
        assert_eq!(patience(Some("timeout=2000")), Duration::from_secs(2));
        assert_eq!(patience(Some("timeout=999999")), MAX_WAIT, "and it is not for ever");
        assert_eq!(patience(Some("timeout=soon")), WAIT);
        assert_eq!(query(Some("a=1&clientKeys=x,y"), "clientKeys"), Some("x,y"));
    }
}
