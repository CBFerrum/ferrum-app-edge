//! A central system for EV chargers: they call in over a WebSocket, as OCPP has it, and each stays
//! connected. Which charger it is comes from the address it calls, as OCPP says, and a charger with
//! a token proves it by Basic authentication, OCPP's security profile 1, which `listen+ocpps` makes
//! profile 2. One with no token on its record is let in only if the address says `?open=1`.
//!
//! `listen+ocpp://0.0.0.0:9000/ocpp`, for chargers set to `ws://gateway:9000/ocpp/CP-1`.

use crate::adapter::Failure;
use crate::adapters::hub::{Hub, Proof};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use chrono::Utc;
use ferrum_edge_contract::ocpp;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message as Frame;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};

const CALL: i64 = 2;
const RESULT: i64 = 3;
const ERROR: i64 = 4;
/// What can wait to go to one charger.
const QUEUE: usize = 32;

pub struct OcppServer {
    pub address: String,
    pub open: bool,
    pub tls: Option<TlsAcceptor>,
    pub hub: Arc<Hub>,
    pub stations: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
    pub transactions: Arc<AtomicU32>,
}

/// Who may be on the other end of this request, and in which version of OCPP.
fn admitted(hub: &Hub, open: bool, request: &Request) -> Result<(String, String), &'static str> {
    let station =
        request.uri().path().trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).ok_or("say which charger in the address")?.to_string();
    let offered = request.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let version =
        offered.split(',').map(str::trim).find(|p| matches!(*p, "ocpp2.0.1" | "ocpp1.6")).ok_or("say ocpp1.6 or ocpp2.0.1 as a subprotocol")?.to_string();
    let password = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|t| String::from_utf8(crate::sinks::base64(t)?).ok())
        .and_then(|pair| pair.split_once(':').map(|(_, p)| p.to_string()));
    let proved = password.as_deref().and_then(|p| hub.who(&Proof { username: Some(p), ..Default::default() })).is_some_and(|code| code == station);
    if proved || (open && hub.known(&station)) {
        return Ok((station, version));
    }
    Err("that charger is not known here")
}

async fn serve(stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static, s: Shared) {
    let mut who = None;
    let check = |request: &Request, mut response: Response| -> Result<Response, ErrorResponse> {
        match admitted(&s.hub, s.open, request) {
            Ok((station, version)) => {
                if let Ok(value) = HeaderValue::from_str(&version) {
                    response.headers_mut().insert("sec-websocket-protocol", value);
                }
                who = Some(station);
                Ok(response)
            }
            Err(why) => {
                let mut refused = ErrorResponse::new(Some(why.to_string()));
                *refused.status_mut() = StatusCode::UNAUTHORIZED;
                Err(refused)
            }
        }
    };
    let Ok(socket) = tokio_tungstenite::accept_hdr_async(stream, check).await else { return };
    let Some(station) = who else { return };
    let (mut write, mut read) = socket.split();
    let (out, mut outgoing) = mpsc::channel::<String>(QUEUE);
    s.stations.lock().unwrap_or_else(|e| e.into_inner()).insert(station.clone(), out.clone());
    let online = |up: bool| Message {
        topic: format!("{}{station}/Connection", ocpp::PREFIX),
        body: json!({"connected": up}).to_string().into_bytes(),
        status: None,
        from: Some(station.clone()),
    };
    let _ = s.tx.send(online(true)).await;
    loop {
        tokio::select! {
            frame = read.next() => {
                let text = match frame {
                    Some(Ok(Frame::Text(text))) => text,
                    Some(Ok(Frame::Ping(_) | Frame::Pong(_) | Frame::Binary(_))) => continue,
                    _ => break,
                };
                if let Some(reply) = heard(&s, &station, text.as_str()).await
                    && write.send(Frame::text(reply)).await.is_err()
                {
                    break;
                }
            }
            sending = outgoing.recv() => match sending {
                Some(text) => if write.send(Frame::text(text)).await.is_err() { break },
                None => break,
            },
        }
    }
    {
        // Unless it has called again in the meantime, which is the newer connection.
        let mut stations = s.stations.lock().unwrap_or_else(|e| e.into_inner());
        if stations.get(&station).is_some_and(|known| known.same_channel(&out)) {
            stations.remove(&station);
        }
    }
    let _ = s.tx.send(online(false)).await;
}

/// Takes one frame, and says what to answer. A charger's calls go on as what it said.
async fn heard(s: &Shared, station: &str, text: &str) -> Option<String> {
    let frame: Value = serde_json::from_str(text).ok()?;
    let from = Some(station.to_string());
    let prefix = ocpp::PREFIX;
    match frame[0].as_i64()? {
        CALL => {
            let (id, action) = (frame[1].as_str()?, frame[2].as_str()?);
            let said = &frame[3];
            let message = Message { topic: format!("{prefix}{station}/{action}"), body: said.to_string().into_bytes(), status: None, from };
            if s.tx.send(message).await.is_err() {
                return Some(json!([ERROR, id, "InternalError", "this gateway is not taking anything now", {}]).to_string());
            }
            let list = s.hub.setting(station, "id_tags");
            let number = s.transactions.fetch_add(1, Ordering::Relaxed);
            Some(json!([RESULT, id, ocpp::answer(action, said, list.as_ref(), number, Utc::now())]).to_string())
        }
        RESULT => {
            let topic = format!("{prefix}{station}/answer/{}", frame[1].as_str()?);
            let _ = s.tx.send(Message { topic, body: frame[2].to_string().into_bytes(), status: None, from }).await;
            None
        }
        ERROR => {
            let topic = format!("{prefix}{station}/answer/{}", frame[1].as_str()?);
            let body = json!({"error": frame[2], "description": frame[3]});
            let _ = s.tx.send(Message { topic, body: body.to_string().into_bytes(), status: None, from }).await;
            None
        }
        _ => None,
    }
}

#[derive(Clone)]
struct Shared {
    hub: Arc<Hub>,
    open: bool,
    tx: mpsc::Sender<Message>,
    stations: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
    transactions: Arc<AtomicU32>,
}

#[async_trait]
impl Transport for OcppServer {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let listener = TcpListener::bind(&self.address).await.map_err(|e| format!("{}: {e}", self.address))?;
        let _ = ready.send(()).await;
        let shared =
            Shared { hub: self.hub.clone(), open: self.open, tx: tx.clone(), stations: self.stations.clone(), transactions: self.transactions.clone() };
        loop {
            let accepted = tokio::select! {
                () = tx.closed() => return Ok(()),
                got = listener.accept() => got,
            };
            let Ok((stream, _)) = accepted else { continue };
            let (s, tls) = (shared.clone(), self.tls.clone());
            tokio::spawn(async move {
                match tls {
                    Some(tls) => {
                        if let Ok(stream) = tls.accept(stream).await {
                            serve(stream, s).await;
                        }
                    }
                    None => serve(stream, s).await,
                }
            });
        }
    }

    /// `ocpp/{id}/{call}/{Action}`. A charger that is not connected has not been told.
    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        let Out::Publish { topic, body, .. } = out else { return Err(Failure::NotSent("a charger takes a call".into())) };
        let parts: Vec<&str> = topic.strip_prefix(ocpp::PREFIX).unwrap_or_default().split('/').collect();
        let [station, id, action] = parts[..] else { return Err(Failure::NotSent(format!("{topic} is not a call"))) };
        let payload: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
        let sender = self.stations.lock().unwrap_or_else(|e| e.into_inner()).get(station).cloned();
        let sender = sender.ok_or_else(|| Failure::NotSent(format!("{station} is not connected")))?;
        sender.send(json!([CALL, id, action, payload]).to_string()).await.map_err(|_| Failure::NotSent(format!("{station} is not connected")))?;
        Ok(None)
    }

    fn listening(&self) -> bool {
        true
    }
}
