//! EV chargers calling a gateway. A charger written here says what the specification says, and one
//! written with the Python `ocpp` library, when `FERRUM_TEST_PYTHON` names a Python that has it,
//! proves that someone else reads it the same way. No charger made by anyone was involved.
//!
//! What a charger says about money is kept before it hears that it was heard, here in a real journal
//! that is closed and opened again, the way a crash and a restart leave it.

use chrono::{Duration, Utc};
use ferrum_edge_contract::edge::{Credentials, DeviceLink};
use ferrum_ops_contract::Command;
use ferrum_ops_contract::sync::{Fact, SiteDevice};
use ferrum_site::adapter::{Adapter, Dispatched, Observation, Reading};
use ferrum_site::adapters::defined::Defined;
use ferrum_site::adapters::hub::{Keeper, hash};
use ferrum_site::adapters::transport::Limits;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as Wait;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

/// How long a dispatch is told it has.
const LEFT: std::time::Duration = std::time::Duration::from_secs(600);

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
        Plant::with_keeper(links, query, None).await
    }

    async fn with_keeper(links: &[DeviceLink], query: &str, keeper: Option<Arc<dyn Keeper>>) -> Plant {
        let port = free_port();
        let body = format!("name: chargers\nconnect: listen+ocpp://127.0.0.1:{port}/ocpp{query}\napi: ocpp\n");
        let definitions = BTreeMap::from([("chargers".to_string(), body)]);
        let d = Defined::new(Limits { listen: vec![format!("127.0.0.1:{port}/ocpp"), format!("127.0.0.1:{port}")], keeper, ..Default::default() }, json!({}));
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
            let found = {
                let mut seen = self.seen.lock().unwrap();
                seen.extend(more);
                seen.iter().position(&want).map(|i| seen.remove(i))
            };
            if let Some(reading) = found {
                return reading;
            }
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

/// A keeper that can be made to fail, in front of a real journal.
struct Flaky {
    journal: Arc<ferrum_site::journal::Journal>,
    down: std::sync::atomic::AtomicBool,
}

impl Keeper for Flaky {
    fn keep(&self, device: &str, _call: &str, action: &str, said: &Value) -> Result<(), String> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the disk is full".into());
        }
        let hash = hash(format!("{device}/{action}/{said}").as_bytes());
        let id = Uuid::parse_str(&hash[..32]).unwrap();
        self.journal
            .add_fact(&Fact { fact_id: id, kind: "edge.transaction".into(), at_source: Utc::now(), actor: None, body: json!({"said": said}) })
            .map_err(|e| e.to_string())
    }

    fn number(&self, device: &str, said: &Value) -> Result<i32, String> {
        self.journal.transaction_number(&format!("{device}/{said}"), Utc::now().timestamp_millis()).map_err(|e| e.to_string())
    }
}

#[tokio::test]
async fn what_a_charger_says_about_money_is_kept_before_it_is_answered() {
    let dir = std::env::temp_dir().join(format!("ferrum-ocpp-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("journal.db");
    let journal = Arc::new(ferrum_site::journal::Journal::open(&path).unwrap());
    let keeper = Arc::new(Flaky { journal: journal.clone(), down: Default::default() });
    let plant = Plant::with_keeper(&[charger("CP-1", Some("sesame"), json!(null))], "", Some(keeper.clone())).await;
    let mut socket = connect(plant.port, "CP-1", "ocpp1.6", Some("sesame")).await.unwrap();
    let start = json!({"connectorId": 1, "idTag": "A", "meterStart": 1000, "timestamp": "2026-11-02T10:00:00Z"});

    // AT-OPS-008: it cannot be kept, so the charger is not told it was, and says it again.
    keeper.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let refused = call(&mut socket, "1", "StartTransaction", start.clone()).await;
    assert_eq!((&refused[0], &refused[2]), (&json!(4), &json!("InternalError")), "{refused}");
    assert!(journal.facts(10).unwrap().is_empty());
    keeper.down.store(false, std::sync::atomic::Ordering::SeqCst);
    let first = call(&mut socket, "1", "StartTransaction", start.clone()).await;
    let number = first[2]["transactionId"].as_i64().expect("a number");

    // AT-OPS-010: the same start over a new connection, after a restart, is the same transaction.
    drop(socket);
    let mut socket = connect(plant.port, "CP-1", "ocpp1.6", Some("sesame")).await.unwrap();
    let again = call(&mut socket, "2", "StartTransaction", start.clone()).await;
    assert_eq!(again[2]["transactionId"].as_i64(), Some(number));
    let other = call(&mut socket, "3", "StartTransaction", json!({"connectorId": 2, "idTag": "B", "meterStart": 5, "timestamp": "2026-11-02T10:05:00Z"})).await;
    let other = other[2]["transactionId"].as_i64().unwrap();
    assert_ne!(other, number, "and a new one is a new number");
    let stop = json!({"transactionId": number, "meterStop": 2000, "timestamp": "2026-11-02T10:30:00Z"});
    assert_eq!(call(&mut socket, "4", "StopTransaction", stop.clone()).await[0], json!(3));
    assert_eq!(call(&mut socket, "5", "StopTransaction", stop).await[0], json!(3), "a stop it did not hear answered, said again");
    assert_eq!(journal.facts(10).unwrap().len(), 3, "one start, another and one stop, however often they were said");

    // A crash here loses nothing the charger was told was heard, and starts no number again.
    drop(socket);
    let reopened = ferrum_site::journal::Journal::open(&path).unwrap();
    assert_eq!(reopened.facts(10).unwrap().len(), 3);
    let later = reopened.transaction_number("a later one", Utc::now().timestamp_millis()).unwrap();
    assert!(i64::from(later) > other, "{later} after {other}");
    let _ = std::fs::remove_dir_all(&dir);
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
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("CP-1"), LEFT).await else { panic!("not accepted") };
    let asked = hear(&mut old).await;
    assert_eq!((asked[0].as_i64(), asked[2].as_str(), asked[3]["idTag"].as_str()), (Some(2), Some("RemoteStartTransaction"), Some("A1")));
    assert!(matches!(plant.d.observe(&c, "CP-1", Some(&external)).await, Ok(Observation::Waiting)));
    old.send(Message::text(json!([3, asked[1], {"status": "Accepted"}]).to_string())).await.unwrap();
    for _ in 0..200 {
        let _ = plant.d.read().await;
        if matches!(plant.d.observe(&c, "CP-1", Some(&external)).await, Ok(Observation::Completed(_))) {
            break;
        }
        tokio::time::sleep(Wait::from_millis(10)).await;
    }
    assert!(matches!(plant.d.observe(&c, "CP-1", Some(&external)).await, Ok(Observation::Completed(_))));

    let c = command("ocpp.Reset", json!({"type": "Hard"}));
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("CP-1"), LEFT).await else { panic!("not accepted") };
    let asked = hear(&mut old).await;
    old.send(Message::text(json!([3, asked[1], {"status": "Rejected"}]).to_string())).await.unwrap();
    for _ in 0..200 {
        let _ = plant.d.read().await;
        if matches!(plant.d.observe(&c, "CP-1", Some(&external)).await, Ok(Observation::Failed(_))) {
            break;
        }
        tokio::time::sleep(Wait::from_millis(10)).await;
    }
    assert!(matches!(plant.d.observe(&c, "CP-1", Some(&external)).await, Ok(Observation::Failed(_))), "a charger that says no has said no");
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
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("CP_1"), LEFT).await else { panic!("not accepted") };
    for _ in 0..200 {
        let _ = plant.d.read().await;
        if matches!(plant.d.observe(&c, "CP_1", Some(&external)).await, Ok(Observation::Completed(_))) {
            break;
        }
        tokio::time::sleep(Wait::from_millis(10)).await;
    }
    assert!(matches!(plant.d.observe(&c, "CP_1", Some(&external)).await, Ok(Observation::Completed(_))), "it said it would");
    // Smart charging is a command like any other, and the library holds it to OCPP's own schema.
    let profile = json!({"connectorId": 1, "csChargingProfiles": {"chargingProfileId": 1, "stackLevel": 0, "chargingProfilePurpose": "TxDefaultProfile", "chargingProfileKind": "Relative",
        "chargingSchedule": {"chargingRateUnit": "A", "chargingSchedulePeriod": [{"startPeriod": 0, "limit": 16.0}]}}});
    let c = command("ocpp.SetChargingProfile", profile);
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("CP_1"), LEFT).await else { panic!("not accepted") };
    for _ in 0..200 {
        let _ = plant.d.read().await;
        if matches!(plant.d.observe(&c, "CP_1", Some(&external)).await, Ok(Observation::Completed(_))) {
            break;
        }
        tokio::time::sleep(Wait::from_millis(10)).await;
    }
    assert!(matches!(plant.d.observe(&c, "CP_1", Some(&external)).await, Ok(Observation::Completed(_))), "it took the limit");
    let _ = run.wait().await;
    let out = run.wait_with_output().await.unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("boot RegistrationStatus.accepted") || said.contains("boot Accepted"), "{said}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(said.contains("remote start A1"), "{said}");
    assert!(said.contains("limit 16"), "{said}");
}

/// Security profile 3: TLS with a certificate of the charger's own, and no password.
#[tokio::test]
async fn a_charger_can_be_known_by_its_certificate() {
    use tokio_rustls::rustls;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    use tokio_tungstenite::Connector;

    let dir = std::env::temp_dir().join(format!("ferrum-ocpp-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let server = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
    std::fs::write(dir.join("server.pem"), server.cert.pem()).unwrap();
    std::fs::write(dir.join("server.key"), server.key_pair.serialize_pem()).unwrap();
    let ours = rcgen::generate_simple_self_signed(vec!["cp-1.local".to_string()]).unwrap();
    let stranger = rcgen::generate_simple_self_signed(vec!["cp-9.local".to_string()]).unwrap();

    let port = free_port();
    let body = format!("name: chargers\nconnect: listen+ocpps://127.0.0.1:{port}/ocpp\napi: ocpp\ntls: {{cert: server.pem, key: server.key}}\n");
    let definitions = BTreeMap::from([("chargers".to_string(), body)]);
    let mut link = charger("CP_1", None, json!([]));
    link.credentials.certificate = Some(hash(ours.cert.der()));
    let limits = Limits { listen: vec![format!("127.0.0.1:{port}/ocpp"), format!("127.0.0.1:{port}")], tls_dir: Some(dir.clone()), ..Default::default() };
    let d = Defined::new(limits, json!({}));
    assert_eq!(d.apply(&definitions, &[link]), Vec::<String>::new());
    for _ in 0..200 {
        if d.links().iter().all(|l| l.up) {
            break;
        }
        tokio::time::sleep(Wait::from_millis(10)).await;
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let call = async |who: Option<&rcgen::CertifiedKey>, station: &str| {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(server.cert.der().clone()).unwrap();
        let builder =
            rustls::ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions().unwrap().with_root_certificates(roots);
        let config = match who {
            Some(c) => {
                let chain = vec![CertificateDer::from_pem_slice(c.cert.pem().as_bytes()).unwrap()];
                builder.with_client_auth_cert(chain, PrivateKeyDer::from_pem_slice(c.key_pair.serialize_pem().as_bytes()).unwrap()).unwrap()
            }
            None => builder.with_no_client_auth(),
        };
        let mut request = format!("wss://127.0.0.1:{port}/ocpp/{station}").into_client_request().unwrap();
        request.headers_mut().insert("sec-websocket-protocol", "ocpp1.6".parse().unwrap());
        tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(Connector::Rustls(Arc::new(config))))
            .await
            .map(drop)
            .map_err(|e| e.to_string())
    };
    call(Some(&ours), "CP_1").await.expect("its own certificate is all it needs");
    assert!(call(Some(&ours), "CP_2").await.is_err(), "it proves one charger, and not another");
    assert!(call(Some(&stranger), "CP_1").await.is_err(), "a certificate proves a key, and only a known one proves a charger");
    assert!(call(None, "CP_1").await.is_err(), "and without one, nothing");
    let _ = std::fs::remove_dir_all(dir);
}
