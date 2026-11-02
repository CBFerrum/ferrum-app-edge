//! What ThingsBoard's own client library does against a gateway, unchanged. It runs when
//! `FERRUM_TEST_PYTHON` names a Python that has `tb-mqtt-client`, as CI's does. Everything else about
//! the device API is checked with other clients in `devices_it`, so this is here for one claim: that
//! something written for ThingsBoard needs nothing but a new address.

use chrono::{Duration, Utc};
use ferrum_edge_contract::edge::{Credentials, DeviceLink};
use ferrum_ops_contract::Command;
use ferrum_ops_contract::sync::SiteDevice;
use ferrum_site::adapter::{Adapter, Dispatched, Observation, Reading};
use ferrum_site::adapters::defined::Defined;
use ferrum_site::adapters::hub::hash;
use ferrum_site::adapters::transport::Limits;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration as Wait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command as Process};
use tokio::time::timeout;
use uuid::Uuid;

/// How long a dispatch is told it has.
const LEFT: std::time::Duration = std::time::Duration::from_secs(600);

const PEER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tb_peer.py");
const TOKEN: &str = "a-device-token";

fn command(operation: &str, payload: Value) -> Command {
    Command {
        id: Uuid::now_v7(),
        tenant_id: Uuid::nil(),
        site_id: Uuid::nil(),
        domain: "north".into(),
        target_id: Uuid::nil(),
        operation: operation.into(),
        schema_version: 1,
        issued_by: None,
        approval_ref: None,
        epoch: 1,
        expected_state: Value::Null,
        issued_at: Utc::now(),
        expires_at: Utc::now() + Duration::minutes(10),
        payload,
        payload_hash: String::new(),
        workflow_id: None,
        step_id: None,
        correlation_id: None,
        config_version: None,
    }
}

fn site_device(code: &str) -> SiteDevice {
    SiteDevice { id: Uuid::nil(), code: code.into(), kind: "Sensor".into(), lifecycle: "Active".into(), operations: vec![], limits: BTreeMap::new() }
}

struct Plant {
    d: Defined,
    port: u16,
    seen: Mutex<Vec<Reading>>,
}

impl Plant {
    async fn up(links: &[DeviceLink]) -> Plant {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let body = format!("name: devices\nconnect: listen+mqtt://127.0.0.1:{port}\napi: device\n");
        let definitions = BTreeMap::from([("devices".to_string(), body)]);
        let firmware_dir = Some(std::env::temp_dir().join(format!("firmware-{}", Uuid::now_v7().simple())));
        let d = Defined::new(Limits { listen: vec![format!("127.0.0.1:{port}")], firmware_dir, ..Default::default() }, json!({}));
        assert_eq!(d.apply(&definitions, links), Vec::<String>::new());
        for _ in 0..200 {
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                break;
            }
            tokio::time::sleep(Wait::from_millis(25)).await;
        }
        Plant { d, port, seen: Mutex::default() }
    }

    async fn reading(&self, want: impl Fn(&Reading) -> bool) -> Reading {
        for _ in 0..400 {
            let more = self.d.read().await.unwrap();
            {
                let mut seen = self.seen.lock().unwrap();
                seen.extend(more);
                if let Some(i) = seen.iter().position(&want) {
                    return seen.remove(i);
                }
            }
            tokio::time::sleep(Wait::from_millis(25)).await;
        }
        panic!("gave up waiting; saw {:?}", self.seen.lock().unwrap());
    }
}

fn link(code: &str) -> DeviceLink {
    DeviceLink {
        code: code.into(),
        definition: "devices".into(),
        credentials: Credentials { token: Some(hash(TOKEN.as_bytes())), ..Default::default() },
        ..Default::default()
    }
}

type Lines = tokio::io::Lines<BufReader<tokio::process::ChildStdout>>;

fn peer(python: &str, kind: &str, port: u16) -> (Child, Lines) {
    let mut child = Process::new(python)
        .arg(PEER)
        .arg(kind)
        .arg(port.to_string())
        .arg(TOKEN)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let lines = BufReader::new(child.stdout.take().unwrap()).lines();
    (child, lines)
}

/// What it says next, past anything that is not one of its own lines: the library talks too.
async fn next(lines: &mut Lines) -> Value {
    loop {
        let line = timeout(Wait::from_secs(30), lines.next_line()).await.expect("it said something").unwrap().expect("a line");
        if let Ok(said) = serde_json::from_str(&line) {
            return said;
        }
    }
}

/// Reads what the library says until `key` comes, ignoring what comes first.
async fn until(lines: &mut Lines, key: &str) -> Value {
    loop {
        let said = next(lines).await;
        if !said[key].is_null() {
            return said[key].clone();
        }
    }
}

fn metric(r: &Reading, code: &str, name: &str, want: f64) -> bool {
    matches!(r, Reading::Value { device, metric, value, .. } if device == code && metric == name && (*value - want).abs() < 1e-9)
}

fn python() -> Option<String> {
    let found = std::env::var("FERRUM_TEST_PYTHON").ok();
    if found.is_none() {
        eprintln!("skipped: FERRUM_TEST_PYTHON is not set");
    }
    found
}

#[tokio::test]
async fn thingsboards_device_client_needs_nothing_but_a_new_address() {
    let Some(python) = python() else { return };
    let plant = Plant::up(&[link("PRESS-1")]).await;
    let (_child, mut lines) = peer(&python, "device", plant.port);
    assert_eq!(until(&mut lines, "connected").await, json!(true));
    assert_eq!(until(&mut lines, "telemetry").await, json!(true), "its publish was confirmed");
    assert_eq!(until(&mut lines, "stamped").await, json!(true));
    assert_eq!(until(&mut lines, "attributes").await, json!(true));
    plant.reading(|r| metric(r, "PRESS-1", "temp", 21.5)).await;
    let stamped = plant.reading(|r| metric(r, "PRESS-1", "old", 1.0)).await;
    assert!(matches!(stamped, Reading::Value { source_time: Some(t), .. } if t.timestamp_millis() == 1_700_000_000_000), "{stamped:?}");
    plant.reading(|r| matches!(r, Reading::Attribute { key, .. } if key == "firmware")).await;
    assert_eq!(until(&mut lines, "asked").await["client"]["firmware"], json!("1.2"), "what it said itself, asked for by name");
    until(&mut lines, "ready").await;

    let c = command("fan.start", json!({"speed": 3}));
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("PRESS-1"), LEFT).await else { panic!("accepted") };
    assert_eq!(until(&mut lines, "rpc").await, json!({"method": "fan.start", "params": {"speed": 3}}));
    for _ in 0..400 {
        if matches!(plant.d.observe(&c, "PRESS-1", Some(&external)).await, Ok(Observation::Completed(_))) {
            break;
        }
        tokio::time::sleep(Wait::from_millis(25)).await;
    }
    let ended = plant.d.observe(&c, "PRESS-1", Some(&external)).await;
    assert!(matches!(&ended, Ok(Observation::Completed(e)) if e["done"] == true), "its answer ends the command: {ended:?}");

    let setting = command("set.limit", json!({"key": "limit", "value": 40}));
    assert!(matches!(plant.d.dispatch(&setting, &site_device("PRESS-1"), LEFT).await, Ok(Dispatched::Accepted { .. })));
    assert_eq!(until(&mut lines, "attribute").await, json!({"limit": 40}), "a setting reaches its subscription");
}

#[tokio::test]
async fn thingsboards_device_client_fetches_firmware_and_it_checks_out() {
    let Some(python) = python() else { return };
    let image: Vec<u8> = (0..50_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    let sha256 = hex::encode(Sha256::digest(&image));
    let served = image.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/fw.bin", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, axum::Router::new().fallback(move || std::future::ready(served.clone()))).await });

    let plant = Plant::up(&[link("PRESS-1")]).await;
    let (_child, mut lines) = peer(&python, "firmware", plant.port);
    assert_eq!(until(&mut lines, "connected").await, json!(true));
    until(&mut lines, "ready").await;
    let order = json!({"title": "sensor-fw", "version": "2.0", "url": url, "sha256": sha256, "size": image.len()});
    let c = command("firmware.update", order);
    assert!(matches!(plant.d.dispatch(&c, &site_device("PRESS-1"), LEFT).await, Ok(Dispatched::Accepted { .. })));
    let got = until(&mut lines, "firmware").await;
    assert_eq!(got["bytes"], json!(image.len()), "{got}");
    assert_eq!(got["sha256"], json!(sha256), "the same bytes, by the library's own count");
    assert_eq!(got["version"], json!("2.0"));
}

#[tokio::test]
async fn thingsboards_gateway_client_too() {
    let Some(python) = python() else { return };
    let pump = DeviceLink { code: "Pump 1".into(), definition: "devices".into(), via: Some("GW-1".into()), settings: json!({}), ..Default::default() };
    let plant = Plant::up(&[link("GW-1"), pump]).await;
    let (_child, mut lines) = peer(&python, "gateway", plant.port);
    assert_eq!(until(&mut lines, "connected").await, json!(true));
    assert_eq!(until(&mut lines, "telemetry").await, json!(true));
    assert_eq!(until(&mut lines, "attributes").await, json!(true));
    plant.reading(|r| metric(r, "Pump 1", "flow", 3.0)).await;
    plant.reading(|r| matches!(r, Reading::Attribute { key, .. } if key == "model")).await;
    until(&mut lines, "ready").await;

    let c = command("pump.start", json!({}));
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("Pump 1"), LEFT).await else { panic!("accepted") };
    let said = until(&mut lines, "rpc").await;
    assert_eq!((said["device"].as_str(), said["data"]["method"].as_str()), (Some("Pump 1"), Some("pump.start")), "{said}");
    for _ in 0..400 {
        if matches!(plant.d.observe(&c, "Pump 1", Some(&external)).await, Ok(Observation::Completed(_))) {
            return;
        }
        tokio::time::sleep(Wait::from_millis(25)).await;
    }
    panic!("its answer did not end the command: {:?}", plant.d.observe(&c, "Pump 1", Some(&external)).await);
}
