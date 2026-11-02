//! DTLS by pre-shared key on a UDP port, so that CoAP and LwM2M can leave a private network. A device's
//! identity is its code, and its key is the SHA-256 of its access token, as raw bytes. A device can
//! work that out and a gateway already holds it, so nothing new has to reach either of them.
//! Certificates over DTLS are not here.
//!
//! Handshakes run side by side, so one that stalls holds up nobody else, and each has a limit.

use crate::adapters::hub::Hub;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::time::timeout;
use webrtc_dtls::cipher_suite::CipherSuiteId;
use webrtc_dtls::config::Config;
use webrtc_dtls::conn::DTLSConn;
use webrtc_util::conn::Listener;
use webrtc_util::conn::conn_udp_listener::listen;

const HANDSHAKE: Duration = Duration::from_secs(15);
/// A session nothing has crossed for this long is forgotten, and the device shakes hands again.
const IDLE: Duration = Duration::from_secs(3600);
const MAX_SESSIONS: usize = 10_000;
const MAX_HANDSHAKES: usize = 256;
const MAX_DATAGRAM: usize = 2048;
const WAITING: usize = 256;

/// A port as a server uses it, encrypted or not.
#[derive(Clone)]
pub enum Wire {
    Plain(Arc<UdpSocket>),
    Secured(Arc<Secured>),
}

pub struct Secured {
    sessions: Arc<Mutex<HashMap<SocketAddr, Session>>>,
    arrived: tokio::sync::Mutex<mpsc::Receiver<(Vec<u8>, SocketAddr)>>,
    closing: Arc<Notify>,
}

struct Session {
    conn: Arc<DTLSConn>,
    identity: String,
}

impl Drop for Secured {
    fn drop(&mut self) {
        self.closing.notify_one();
    }
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What is offered: the suite RFC 7252 requires of a CoAP device, and two more.
fn config(hub: Arc<Hub>, identity: Arc<Mutex<String>>) -> Config {
    Config {
        psk: Some(Arc::new(move |offered: &[u8]| {
            let name = String::from_utf8_lossy(offered).into_owned();
            let key = hub.key_of(&name).ok_or(webrtc_dtls::Error::ErrIdentityNoPsk)?;
            *locked(&identity) = name;
            Ok(key)
        })),
        psk_identity_hint: Some(b"ferrum".to_vec()),
        cipher_suites: vec![CipherSuiteId::Tls_Psk_With_Aes_128_Ccm_8, CipherSuiteId::Tls_Psk_With_Aes_128_Ccm, CipherSuiteId::Tls_Psk_With_Aes_128_Gcm_Sha256],
        ..Default::default()
    }
}

async fn serve(
    conn: Arc<dyn webrtc_util::Conn + Send + Sync>,
    peer: SocketAddr,
    hub: Arc<Hub>,
    sessions: Arc<Mutex<HashMap<SocketAddr, Session>>>,
    tx: mpsc::Sender<(Vec<u8>, SocketAddr)>,
    turn: OwnedSemaphorePermit,
) {
    let identity = Arc::new(Mutex::new(String::new()));
    let shaken = timeout(HANDSHAKE, DTLSConn::new(conn, config(hub, identity.clone()), false, None)).await;
    drop(turn);
    let conn = match shaken {
        Ok(Ok(conn)) => Arc::new(conn),
        Ok(Err(e)) => {
            tracing::info!(%peer, "a DTLS handshake failed: {e}");
            return;
        }
        Err(_) => return,
    };
    {
        let mut held = locked(&sessions);
        if held.len() >= MAX_SESSIONS {
            return;
        }
        held.insert(peer, Session { conn: conn.clone(), identity: locked(&identity).clone() });
    }
    let mut buffer = vec![0u8; MAX_DATAGRAM];
    while let Ok(Ok(n)) = timeout(IDLE, conn.read(&mut buffer, None)).await {
        if tx.send((buffer[..n].to_vec(), peer)).await.is_err() {
            break;
        }
    }
    locked(&sessions).remove(&peer);
    let _ = conn.close().await;
}

impl Wire {
    pub async fn plain(address: &str) -> Result<Wire, String> {
        Ok(Wire::Plain(Arc::new(UdpSocket::bind(address).await.map_err(|e| format!("{address}: {e}"))?)))
    }

    pub async fn secured(address: &str, hub: Arc<Hub>) -> Result<Wire, String> {
        let bound: SocketAddr = address.parse().map_err(|_| format!("{address} is not an address to listen on"))?;
        let listener = listen(bound).await.map_err(|e| format!("{address}: {e}"))?;
        let (tx, rx) = mpsc::channel(WAITING);
        let sessions: Arc<Mutex<HashMap<SocketAddr, Session>>> = Arc::default();
        let held = sessions.clone();
        let closing = Arc::new(Notify::new());
        let closed = closing.clone();
        let shaking = Arc::new(Semaphore::new(MAX_HANDSHAKES));
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = closed.notified() => break,
                    got = listener.accept() => got,
                };
                let Ok((conn, peer)) = accepted else { break };
                // A datagram costs a sender nothing, so what it can start here is limited.
                let Ok(turn) = shaking.clone().try_acquire_owned() else { continue };
                tokio::spawn(serve(conn, peer, hub.clone(), held.clone(), tx.clone(), turn));
            }
            // The port stays bound until this is said, and a new version of a definition wants it.
            let _ = listener.close().await;
            let open: Vec<Arc<DTLSConn>> = locked(&held).drain().map(|(_, session)| session.conn).collect();
            for conn in open {
                let _ = conn.close().await;
            }
        });
        Ok(Wire::Secured(Arc::new(Secured { sessions, arrived: tokio::sync::Mutex::new(rx), closing })))
    }

    pub async fn recv_from(&self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        match self {
            Wire::Plain(socket) => socket.recv_from(buffer).await,
            Wire::Secured(s) => {
                let (bytes, peer) = s.arrived.lock().await.recv().await.ok_or_else(|| io::Error::other("the port closed"))?;
                let n = bytes.len().min(buffer.len());
                buffer[..n].copy_from_slice(&bytes[..n]);
                Ok((n, peer))
            }
        }
    }

    pub async fn send_to(&self, bytes: &[u8], peer: SocketAddr) -> io::Result<usize> {
        match self {
            Wire::Plain(socket) => socket.send_to(bytes, peer).await,
            Wire::Secured(s) => {
                let conn = locked(&s.sessions).get(&peer).map(|session| session.conn.clone());
                let conn = conn.ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no session with that peer"))?;
                conn.write(bytes, None).await.map_err(io::Error::other)
            }
        }
    }

    /// Who a peer proved it was. A plain port asks nobody, so it has nothing to say.
    pub fn identity(&self, peer: SocketAddr) -> Option<String> {
        match self {
            Wire::Plain(_) => None,
            Wire::Secured(s) => locked(&s.sessions).get(&peer).map(|session| session.identity.clone()),
        }
    }

    pub fn encrypted(&self) -> bool {
        matches!(self, Wire::Secured(_))
    }
}
