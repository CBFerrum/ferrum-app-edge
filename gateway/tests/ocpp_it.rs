//! EV chargers calling a gateway. A charger written here says what the specification says, and one
//! written with the Python `ocpp` library, when `FERRUM_TEST_PYTHON` names a Python that has it,
//! proves that someone else reads it the same way. No charger made by anyone was involved.

use chrono::{Duration, Utc};
use ferrum_edge_contract::edge::{Credentials, DeviceLink};
use ferrum_ops_contract::Command;
use ferrum_ops_contract::sync::SiteDevice;
use ferrum_site::adapter::{Adapter, Dispatched, Observation, Reading};
use ferrum_site::adapters::defined::Defined;
use ferrum_site::adapters::hub::hash;
use ferrum_site::adapters::transport::Limits;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration as Wait;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Plant {
    d: Defined,
    port: u16,
    seen: Mutex<Vec<Reading>>,
}

fn charger(code: &str, token: Option<&str>, id_tags: Value) -> DeviceLink {
    DeviceLink {
        code: code.into(),
        definition: "chargers".into(),
        settings: json!({"id_tags": id_tags}),
        credentials: Credentials { token: token.map(|t| hash(t.as_bytes())), ..Default::default() },
        ..Default::default()
    }
}

impl Plant {
    async fn up(links: &[DeviceLink], query: &str) -> Plant {
        let port = free_port();
        let body = format!("name: chargers\nconnect: listen+ocpp://127.0.0.1:{port}/ocpp{query}\napi: ocpp\n");
        let definitions = BTreeMap::from([("chargers".to_string(), body)]);
        let d = Defined::new(Limits { listen: vec![format!("127.0.0.1:{port}/ocpp"), format!("127.0.0.1:{port}")], ..Default::default() }, json!({}));
        assert_eq!(d.apply(&definitions, links), Vec::<String>::new());
        for _ in 0..200 {
            if d.links().iter().all(|l| l.up) {
                break;
            }
            tokio::time::sleep(Wait::from_millis(10)).await;
        }
        Plant { d, port, seen: Mutex::default() }
    }

    async fn reading(&self, want: impl Fn(&Reading) -> bool) -> Reading {
        for _ in 0..400 {
            let more = self.d.read().await.unwrap();
            let mut seen = self.seen.lock().unwrap();
            seen.extend(more);
            if let Some(i) = seen.iter().position(&want) {
                return seen.remove(i);
            }
            drop(seen);
            tokio::time::sleep(Wait::from_millis(25)).await;
        }
        panic!("gave up waiting; have {:?}", self.seen.lock().unwrap());
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(port: u16, station: &str, protocol: &str, password: Option<&str>) -> Result<Socket, String> {
    let mut request = format!("ws://127.0.0.1:{port}/ocpp/{station}").into_client_request().unwrap();
    request.headers_mut().insert("sec-websocket-protocol", protocol.parse().unwrap());
    if let Some(password) = password {
        let pair = format!("{station}:{password}");
        let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut encoded = String::new();
        for chunk in pair.as_bytes().chunks(3) {
            let n = u32::from(chunk[0]) << 16 | u32::from(*chunk.get(1).unwrap_or(&0)) << 8 | u32::from(*chunk.get(2).unwrap_or(&0));
            for i in 0..4 {
                encoded.push(if i <= chunk.len() { table[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
            }
        }
        request.headers_mut().insert("authorization", format!("Basic {encoded}").parse().unwrap());
    }
    tokio_tungstenite::connect_async(request).await.map(|(s, _)| s).map_err(|e| e.to_string())
}

async fn call(socket: &mut Socket, id: &str, action: &str, payload: Value) -> Value {
    socket.send(Message::text(json!([2, id, action, payload]).to_string())).await.unwrap();
    hear(socket).await
}

async fn hear(socket: &mut Socket) -> Value {
    let frame = tokio::time::timeout(Wait::from_secs(5), socket.next()).await.expect("an answer").unwrap().unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}

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
    SiteDevice { id: Uuid::nil(), code: code.into(), kind: "Station".into(), lifecycle: "Active".into(), operations: vec![], limits: Default::default() }
}

fn is_metric(r: &Reading, name: &str, want: f64) -> bool {
    matches!(r, Reading::Value { metric, value, .. } if metric == name && (*value - want).abs() < 1e-6)
}

#[tokio::test]
async fn a_charger_is_known_by_its_address_and_its_password() {
    let plant = Plant::up(&[charger("CP-1", Some("secret"), json!(null)), charger("CP-2", None, json!(null))], "").await;
    assert!(connect(plant.port, "CP-1", "ocpp1.6", Some("wrong")).await.is_err());
    assert!(connect(plant.port, "CP-1", "ocpp1.6", None).await.is_err());
    assert!(connect(plant.port, "NOBODY", "ocpp1.6", Some("secret")).await.is_err());
    assert!(connect(plant.port, "CP-2", "ocpp1.6", None).await.is_err(), "no token is no way in, unless a site says so");
    assert!(connect(plant.port, "CP-1", "ocpp9", Some("secret")).await.is_err(), "and it has to speak something known");
    assert!(connect(plant.port, "CP-1", "ocpp1.6", Some("secret")).await.is_ok());
    let open = Plant::up(&[charger("CP-2", None, json!(null))], "?open=1").await;
    assert!(connect(open.port, "CP-2", "ocpp1.6", None).await.is_ok(), "a site can choose that");
    assert!(connect(open.port, "NOBODY", "ocpp1.6", None).await.is_err(), "even so, only what is on its list");
}

#[tokio::test]
async fn a_charger_speaks_and_is_answered_in_either_version() {
    let plant = Plant::up(&[charger("CP-1", Some("secret"), json!(["A1"]))], "").await;
    let mut old = connect(plant.port, "CP-1", "ocpp1.6", Some("secret")).await.unwrap();
    let boot = call(&mut old, "1", "BootNotification", json!({"chargePointVendor": "V", "chargePointModel": "M", "firmwareVersion": "1.2"})).await;
    assert_eq!((boot[0].as_i64(), boot[1].as_str(), boot[2]["status"].as_str()), (Some(3), Some("1"), Some("Accepted")));
    assert!(boot[2]["interval"].as_i64().unwrap() > 0);
    plant.reading(|r| matches!(r, Reading::Attribute { device, key, value } if device == "CP-1" && key == "fw_version" && value == "1.2")).await;

    let refused = call(&mut old, "2", "Authorize", json!({"idTag": "NOPE"})).await;
    assert_eq!(refused[2]["idTagInfo"]["status"], "Invalid");
    let meter = json!({"connectorId": 1, "meterValue": [{"timestamp": "2026-11-02T10:00:00Z", "sampledValue": [{"value": "1500", "unit": "Wh"}]}]});
    call(&mut old, "3", "MeterValues", meter).await;
    plant.reading(|r| is_metric(r, "energy.active.import.register", 1500.0)).await;
    let started = call(&mut old, "4", "StartTransaction", json!({"connectorId": 1, "idTag": "A1", "meterStart": 0, "timestamp": "2026-11-02T10:00:00Z"})).await;
    let first = started[2]["transactionId"].as_i64().unwrap();
    let again = call(&mut old, "5", "StartTransaction", json!({"connectorId": 2, "idTag": "A1", "meterStart": 0, "timestamp": "2026-11-02T10:00:00Z"})).await;
    assert_ne!(first, again[2]["transactionId"].as_i64().unwrap(), "each is its own");
    plant.reading(|r| matches!(r, Reading::Event { name, .. } if name == "transaction.started")).await;

    let c = command("ocpp.RemoteStartTransaction", json!({"idTag": "A1"}));
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("CP-1")).await else { panic!("not accepted") };
    let asked = hear(&mut old).await;
    assert_eq!((asked[0].as_i64(), asked[2].as_str(), asked[3]["idTag"].as_str()), (Some(2), Some("RemoteStartTransaction"), Some("A1")));
    assert!(matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Waiting)));
    old.send(Message::text(json!([3, asked[1], {"status": "Accepted"}]).to_string())).await.unwrap();
    for _ in 0..200 {
        let _ = plant.d.read().await;
        if matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Completed(_))) {
            break;
        }
        tokio::time::sleep(Wait::from_millis(10)).await;
    }
    assert!(matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Completed(_))));

    let c = command("ocpp.Reset", json!({"type": "Hard"}));
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("CP-1")).await else { panic!("not accepted") };
    let asked = hear(&mut old).await;
    old.send(Message::text(json!([3, asked[1], {"status": "Rejected"}]).to_string())).await.unwrap();
    for _ in 0..200 {
        let _ = plant.d.read().await;
        if matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Failed(_))) {
            break;
        }
        tokio::time::sleep(Wait::from_millis(10)).await;
    }
    assert!(matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Failed(_))), "a charger that says no has said no");
    drop(old);

    let mut new = connect(plant.port, "CP-1", "ocpp2.0.1", Some("secret")).await.unwrap();
    let boot = call(&mut new, "1", "BootNotification", json!({"reason": "PowerUp", "chargingStation": {"model": "M2", "vendorName": "V2"}})).await;
    assert_eq!(boot[2]["status"], "Accepted");
    plant.reading(|r| matches!(r, Reading::Attribute { key, value, .. } if key == "model" && value == "M2")).await;
    call(&mut new, "2", "StatusNotification", json!({"timestamp": "2026-11-02T10:00:00Z", "connectorStatus": "Occupied", "evseId": 1, "connectorId": 1})).await;
    plant.reading(|r| matches!(r, Reading::State { state, .. } if state == "Occupied")).await;
    let event = json!({"eventType": "Started", "timestamp": "2026-11-02T10:00:00Z", "triggerReason": "Authorized", "seqNo": 0, "transactionInfo": {"transactionId": "t1"},
        "idToken": {"idToken": "A1", "type": "ISO14443"}, "meterValue": [{"timestamp": "2026-11-02T10:00:00Z", "sampledValue": [{"value": 7.5, "measurand": "Power.Active.Import"}]}]});
    let answered = call(&mut new, "3", "TransactionEvent", event).await;
    assert_eq!(answered[2]["idTokenInfo"]["status"], "Accepted");
    plant.reading(|r| is_metric(r, "power.active.import", 7.5)).await;
    plant.reading(|r| matches!(r, Reading::Event { name, .. } if name == "transaction.started")).await;

    new.send(Message::text("not json")).await.unwrap();
    assert!(!call(&mut new, "4", "Heartbeat", json!({})).await[2]["currentTime"].is_null(), "and it took no notice of nonsense");
}

#[tokio::test]
async fn a_charger_written_by_someone_else() {
    let Ok(python) = std::env::var("FERRUM_TEST_PYTHON") else {
        eprintln!("skipped: FERRUM_TEST_PYTHON is not set");
        return;
    };
    let plant = Plant::up(&[charger("CP_1", None, json!(["A1"]))], "?open=1").await;
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ocpp_station.py");
    let url = format!("ws://127.0.0.1:{}/ocpp/CP_1", plant.port);
    let mut run =
        tokio::process::Command::new(python).arg(script).arg(url).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    plant.reading(|r| matches!(r, Reading::Attribute { key, value, .. } if key == "vendor" && value == "V1")).await;
    plant.reading(|r| is_metric(r, "energy.active.import.register", 1500.0)).await;
    plant.reading(|r| matches!(r, Reading::State { state, .. } if state == "Charging")).await;
    plant.reading(|r| matches!(r, Reading::Event { name, .. } if name == "transaction.started")).await;
    let c = command("ocpp.RemoteStartTransaction", json!({"idTag": "A1"}));
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("CP_1")).await else { panic!("not accepted") };
    for _ in 0..200 {
        let _ = plant.d.read().await;
        if matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Completed(_))) {
            break;
        }
        tokio::time::sleep(Wait::from_millis(10)).await;
    }
    assert!(matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Completed(_))), "it said it would");
    let _ = run.wait().await;
    let out = run.wait_with_output().await.unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("boot RegistrationStatus.accepted") || said.contains("boot Accepted"), "{said}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(said.contains("remote start A1"), "{said}");
}
