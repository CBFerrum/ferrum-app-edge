//! CoAP and LwM2M over DTLS, which is what lets them leave a private network. Here the far end uses
//! the same DTLS library, so this shows what a gateway refuses. `lwm2m_it` has Eclipse Leshan, which
//! brings a DTLS stack of its own.

use ferrum_edge_contract::edge::{Credentials, DeviceLink};
use ferrum_site::adapter::{Adapter, Reading};
use ferrum_site::adapters::coap::Coap;
use ferrum_site::adapters::defined::Defined;
use ferrum_site::adapters::hub::hash;
use ferrum_site::adapters::transport::Limits;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::timeout;
use webrtc_dtls::cipher_suite::CipherSuiteId;
use webrtc_dtls::config::Config;
use webrtc_dtls::conn::DTLSConn;

const TOKEN: &str = "press-token";
const POST: u8 = 2;
const CREATED_OR_CHANGED: [u8; 2] = [0x41, 0x44];

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn plant() -> (Defined, u16) {
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let definitions = BTreeMap::from([("devices".to_string(), format!("name: devices\nconnect: listen+coaps://{bind}\napi: device\n"))]);
    let credentials = Credentials { token: Some(hash(TOKEN.as_bytes())), ..Default::default() };
    let link = DeviceLink { code: "PRESS-1".into(), definition: "devices".into(), credentials, ..Default::default() };
    let d = Defined::new(Limits { listen: vec![bind], ..Default::default() }, json!({}));
    assert_eq!(d.apply(&definitions, &[link]), Vec::<String>::new());
    for _ in 0..200 {
        if d.links().iter().all(|l| l.up) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (d, port)
}

async fn shake(port: u16, identity: &str, key: Vec<u8>, suite: CipherSuiteId) -> Result<DTLSConn, String> {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect(("127.0.0.1", port)).await.unwrap();
    let config = Config {
        psk: Some(Arc::new(move |_: &[u8]| Ok(key.clone()))),
        psk_identity_hint: Some(identity.as_bytes().to_vec()),
        cipher_suites: vec![suite],
        ..Default::default()
    };
    match timeout(Duration::from_secs(20), DTLSConn::new(Arc::new(socket), config, true, None)).await {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("it took too long".into()),
    }
}

fn the_key() -> Vec<u8> {
    Sha256::digest(TOKEN.as_bytes()).to_vec()
}

fn telemetry(token: &str, body: &Value) -> Coap {
    let options = ["api", "v1", token, "telemetry"].iter().map(|p| (11u16, p.as_bytes().to_vec())).collect();
    Coap { kind: 0, code: POST, id: 7, token: vec![1], options, payload: body.to_string().into_bytes() }
}

async fn reading(d: &Defined, name: &str, want: f64) {
    for _ in 0..200 {
        for r in d.read().await.unwrap() {
            if matches!(&r, Reading::Value { device, metric, value, .. } if device == "PRESS-1" && metric == name && (*value - want).abs() < 1e-9) {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no {name}");
}

#[tokio::test]
async fn a_device_gets_in_with_the_hash_of_its_token_and_is_heard() {
    let (d, port) = plant().await;
    for suite in [CipherSuiteId::Tls_Psk_With_Aes_128_Ccm_8, CipherSuiteId::Tls_Psk_With_Aes_128_Gcm_Sha256] {
        let conn = shake(port, "PRESS-1", the_key(), suite).await.expect("it shook hands");
        conn.write(&telemetry(TOKEN, &json!({"temp": 21.5})).to_bytes(), None).await.unwrap();
        let mut buffer = [0u8; 1500];
        let n = timeout(Duration::from_secs(5), conn.read(&mut buffer, None)).await.expect("an answer").unwrap();
        let answer = Coap::parse(&buffer[..n]).unwrap();
        assert!(CREATED_OR_CHANGED.contains(&answer.code), "{:#x}", answer.code);
        reading(&d, "temp", 21.5).await;
    }
}

#[tokio::test]
async fn nothing_gets_in_without_the_key() {
    let (_d, port) = plant().await;
    let suite = CipherSuiteId::Tls_Psk_With_Aes_128_Ccm_8;
    assert!(shake(port, "PRESS-1", vec![0; 32], suite).await.is_err(), "a wrong key");
    assert!(shake(port, "NOBODY", the_key(), suite).await.is_err(), "someone it has never heard of");
    assert!(shake(port, "PRESS-1", TOKEN.as_bytes().to_vec(), suite).await.is_err(), "and the token itself is not the key");
}

#[tokio::test]
async fn the_plain_protocol_gets_no_answer_on_an_encrypted_port() {
    let (d, port) = plant().await;
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect(("127.0.0.1", port)).await.unwrap();
    socket.send(&telemetry(TOKEN, &json!({"plain": 1})).to_bytes()).await.unwrap();
    let mut buffer = [0u8; 1500];
    assert!(timeout(Duration::from_millis(500), socket.recv(&mut buffer)).await.is_err(), "not a word");
    assert!(d.read().await.unwrap().iter().all(|r| !matches!(r, Reading::Value { metric, .. } if metric == "plain")));
}

#[tokio::test]
async fn a_new_version_takes_over_the_port() {
    let (d, port) = plant().await;
    shake(port, "PRESS-1", the_key(), CipherSuiteId::Tls_Psk_With_Aes_128_Ccm_8).await.expect("it shook hands");
    let bind = format!("127.0.0.1:{port}");
    let definitions = BTreeMap::from([("devices".to_string(), format!("name: devices\nconnect: listen+coaps://{bind}\napi: device\nheaders: {{x: y}}\n"))]);
    let credentials = Credentials { token: Some(hash(TOKEN.as_bytes())), ..Default::default() };
    let link = DeviceLink { code: "PRESS-1".into(), definition: "devices".into(), credentials, ..Default::default() };
    assert_eq!(d.apply(&definitions, &[link]), Vec::<String>::new());
    for _ in 0..200 {
        if d.links().iter().all(|l| l.up) && shake(port, "PRESS-1", the_key(), CipherSuiteId::Tls_Psk_With_Aes_128_Ccm_8).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the port was not free again: {:?}", d.links());
}
