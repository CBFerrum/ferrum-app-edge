//! Devices that call a gateway: who gets in, what they can say, and what reaches them. The far end is
//! rumqttc, someone else's MQTT client, in both versions it speaks, because a device that is not ours
//! is what this is for. No vendor's device was involved.

use chrono::{Duration, Utc};
use ferrum_edge_contract::edge::{Basic, Credentials, DeviceLink};
use ferrum_ops_contract::Command;
use ferrum_ops_contract::sync::SiteDevice;
use ferrum_site::adapter::{Adapter, Dispatched, Failure, Observation, Reading};
use ferrum_site::adapters::coap::Coap;
use ferrum_site::adapters::defined::Defined;
use ferrum_site::adapters::hub::hash;
use ferrum_site::adapters::transport::Limits;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration as Wait;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use uuid::Uuid;

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

async fn until(what: &str, mut done: impl AsyncFnMut() -> bool) {
    for _ in 0..400 {
        if done().await {
            return;
        }
        tokio::time::sleep(Wait::from_millis(25)).await;
    }
    panic!("gave up waiting for {what}");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn token(code: &str, secret: &str) -> DeviceLink {
    DeviceLink {
        code: code.into(),
        definition: "devices".into(),
        credentials: Credentials { token: Some(hash(secret.as_bytes())), ..Default::default() },
        ..Default::default()
    }
}

struct Plant {
    d: Defined,
    definitions: BTreeMap<String, String>,
    port: u16,
    seen: Mutex<Vec<Reading>>,
}

impl Plant {
    async fn up(links: &[DeviceLink]) -> Plant {
        let port = free_port();
        let body = format!("name: devices\nconnect: listen+mqtt://127.0.0.1:{port}\napi: device\n");
        let definitions = BTreeMap::from([("devices".to_string(), body)]);
        let d = Defined::new(Limits { listen: vec![format!("127.0.0.1:{port}")], ..Default::default() }, json!({}));
        assert_eq!(d.apply(&definitions, links), Vec::<String>::new());
        until("the port", async || TcpStream::connect(("127.0.0.1", port)).await.is_ok()).await;
        Plant { d, definitions, port, seen: Mutex::default() }
    }

    async fn reading(&self, want: impl Fn(&Reading) -> bool) -> Reading {
        let mut got = None;
        until("a reading", async || {
            let more = self.d.read().await.unwrap();
            let mut seen = self.seen.lock().unwrap();
            seen.extend(more);
            got = seen.iter().position(&want).map(|i| seen.remove(i));
            got.is_some()
        })
        .await;
        got.unwrap()
    }

    async fn saw(&self, want: impl Fn(&Reading) -> bool) -> bool {
        tokio::time::sleep(Wait::from_millis(200)).await;
        let more = self.d.read().await.unwrap();
        let mut seen = self.seen.lock().unwrap();
        seen.extend(more);
        seen.iter().any(&want)
    }
}

/// A device, by someone else's client. What it hears comes down `heard`; the channel ending means
/// it was refused or dropped.
struct Device {
    client: AsyncClient,
    heard: mpsc::Receiver<(String, Value)>,
    state: mpsc::Receiver<bool>,
}

fn run(mut events: EventLoop, heard: mpsc::Sender<(String, Value)>, state: mpsc::Sender<bool>) {
    tokio::spawn(async move {
        loop {
            match events.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    let _ = state.send(true).await;
                }
                Ok(Event::Incoming(Packet::Publish(p))) => {
                    let _ = heard.send((p.topic, serde_json::from_slice(&p.payload).unwrap_or(Value::Null))).await;
                }
                Err(_) => {
                    let _ = state.send(false).await;
                    return;
                }
                _ => {}
            }
        }
    });
}

fn connect(options: MqttOptions) -> Device {
    let (client, events) = AsyncClient::new(options, 32);
    let (heard_tx, heard) = mpsc::channel(32);
    let (state_tx, state) = mpsc::channel(4);
    run(events, heard_tx, state_tx);
    Device { client, heard, state }
}

fn options(port: u16, client: &str) -> MqttOptions {
    let mut o = MqttOptions::new(client, "127.0.0.1", port);
    o.set_keep_alive(Wait::from_secs(30));
    o
}

async fn with_token(port: u16, secret: &str) -> Device {
    let mut o = options(port, &format!("c-{}", Uuid::now_v7()));
    o.set_credentials(secret, "");
    // A token is a user name alone. rumqttc sends an empty password, as many clients do.
    connect(o)
}

impl Device {
    async fn up(&mut self) -> bool {
        tokio::time::timeout(Wait::from_secs(5), self.state.recv()).await.expect("an answer").unwrap_or(false)
    }

    async fn hears(&mut self) -> (String, Value) {
        tokio::time::timeout(Wait::from_secs(5), self.heard.recv()).await.expect("something heard").expect("connected")
    }

    async fn says(&self, topic: &str, body: Value) {
        self.client.publish(topic, QoS::AtLeastOnce, false, body.to_string()).await.unwrap();
    }

    async fn listens(&self, filter: &str) {
        self.client.subscribe(filter, QoS::AtLeastOnce).await.unwrap();
        tokio::time::sleep(Wait::from_millis(100)).await;
    }
}

const PRESS: &str = "token-of-the-press";
const OTHER: &str = "token-of-another";

fn is_metric(r: &Reading, code: &str, name: &str, want: f64) -> bool {
    matches!(r, Reading::Value { device, metric, value, .. } if device == code && metric == name && (*value - want).abs() < 1e-9)
}

fn is_attribute(r: &Reading, code: &str, name: &str) -> bool {
    matches!(r, Reading::Attribute { device, key, .. } if device == code && key == name)
}

#[tokio::test]
async fn a_device_with_a_token_says_what_it_likes_as_itself() {
    let plant = Plant::up(&[token("PRESS-1", PRESS)]).await;
    let mut press = with_token(plant.port, PRESS).await;
    assert!(press.up().await, "a good token gets in");
    press.says("v1/devices/me/telemetry", json!({"temp": 21.5, "on": true})).await;
    press.says("v1/devices/me/telemetry", json!({"ts": 1_700_000_000_000i64, "values": {"old": 1}})).await;
    press.says("v1/devices/me/attributes", json!({"firmware": "1.2"})).await;
    plant.reading(|r| is_metric(r, "PRESS-1", "temp", 21.5)).await;
    plant.reading(|r| is_metric(r, "PRESS-1", "on", 1.0)).await;
    let stamped = plant.reading(|r| is_metric(r, "PRESS-1", "old", 1.0)).await;
    assert!(matches!(stamped, Reading::Value { source_time: Some(t), .. } if t.timestamp_millis() == 1_700_000_000_000), "{stamped:?}");
    plant.reading(|r| is_attribute(r, "PRESS-1", "firmware")).await;
    plant.reading(|r| matches!(r, Reading::Attribute { key, value, .. } if key == "active" && value == &json!(true))).await;
    press.client.disconnect().await.unwrap();
    plant.reading(|r| matches!(r, Reading::Attribute { key, value, .. } if key == "active" && value == &json!(false))).await;
}

#[tokio::test]
async fn a_wrong_token_does_not_get_in() {
    let plant = Plant::up(&[token("PRESS-1", PRESS)]).await;
    let mut stranger = with_token(plant.port, "not-a-token").await;
    assert!(!stranger.up().await);
    assert!(!plant.saw(|_| true).await, "nothing it did is heard");
}

#[tokio::test]
async fn a_device_cannot_speak_as_another() {
    let plant = Plant::up(&[token("PRESS-1", PRESS), token("OTHER", OTHER)]).await;
    let mut press = with_token(plant.port, PRESS).await;
    assert!(press.up().await);
    press.says("v1/devices/OTHER/telemetry", json!({"temp": 99})).await;
    assert!(!press.up().await, "trying ends the connection");
    assert!(!plant.saw(|r| matches!(r, Reading::Value { metric, .. } if metric == "temp")).await);
}

#[tokio::test]
async fn nor_can_it_listen_to_another() {
    let plant = Plant::up(&[token("PRESS-1", PRESS), token("OTHER", OTHER)]).await;
    let mut press = with_token(plant.port, PRESS).await;
    assert!(press.up().await);
    let mut other = with_token(plant.port, OTHER).await;
    assert!(other.up().await);
    press.listens("v1/devices/OTHER/rpc/request/+").await;
    press.listens("#").await;
    other.listens("v1/devices/me/rpc/request/+").await;
    let c = command("fan.start", json!({}));
    assert!(matches!(plant.d.dispatch(&c, &site_device("OTHER")).await, Ok(Dispatched::Accepted { .. })));
    other.hears().await;
    assert!(tokio::time::timeout(Wait::from_millis(300), press.heard.recv()).await.is_err(), "it was refused those");
}

#[tokio::test]
async fn a_command_reaches_a_device_and_its_answer_ends_it() {
    let plant = Plant::up(&[token("PRESS-1", PRESS)]).await;
    let c = command("fan.start", json!({"speed": 3}));
    // Not there, so it did not go, and that is safe to try again.
    assert!(matches!(plant.d.dispatch(&c, &site_device("PRESS-1")).await, Err(Failure::NotSent(_))));
    let mut press = with_token(plant.port, PRESS).await;
    assert!(press.up().await);
    assert!(matches!(plant.d.dispatch(&c, &site_device("PRESS-1")).await, Err(Failure::NotSent(_))), "connected and not listening is the same");
    press.listens("v1/devices/me/rpc/request/+").await;
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("PRESS-1")).await else { panic!("accepted") };
    let (topic, body) = press.hears().await;
    assert_eq!(topic, format!("v1/devices/me/rpc/request/{external}"), "it hears it under its own me");
    assert_eq!(body, json!({"method": "fan.start", "params": {"speed": 3}}));
    assert!(matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Waiting)), "sent is not done");
    press.says(&format!("v1/devices/me/rpc/response/{external}"), json!({"result": "started"})).await;
    let mut ended = None;
    until("the answer", async || {
        ended = Some(plant.d.observe(&c, Some(&external)).await.unwrap());
        matches!(ended, Some(Observation::Completed(_)))
    })
    .await;
    assert!(matches!(ended, Some(Observation::Completed(ref e)) if e["result"] == "started"), "{ended:?}");
}

#[tokio::test]
async fn a_device_can_say_no() {
    let plant = Plant::up(&[token("PRESS-1", PRESS)]).await;
    let mut press = with_token(plant.port, PRESS).await;
    assert!(press.up().await);
    press.listens("v1/devices/me/rpc/request/+").await;
    let c = command("fan.start", json!({}));
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("PRESS-1")).await else { panic!("accepted") };
    press.hears().await;
    press.says(&format!("v1/devices/me/rpc/response/{external}"), json!({"error": "interlocked"})).await;
    until("the refusal", async || matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Failed(_)))).await;
}

#[tokio::test]
async fn a_setting_reaches_a_device_and_it_can_ask_again() {
    let mut link = token("PRESS-1", PRESS);
    link.shared = BTreeMap::from([("mode".to_string(), json!("eco"))]);
    let plant = Plant::up(&[link]).await;
    let mut press = with_token(plant.port, PRESS).await;
    assert!(press.up().await);
    press.listens("v1/devices/me/attributes").await;
    press.listens("v1/devices/me/attributes/response/+").await;
    let c = command("set.limit", json!({"key": "limit", "value": 40}));
    assert!(matches!(plant.d.dispatch(&c, &site_device("PRESS-1")).await, Ok(Dispatched::Accepted { .. })));
    assert_eq!(press.hears().await, ("v1/devices/me/attributes".to_string(), json!({"limit": 40})));
    assert!(matches!(plant.d.observe(&c, None).await, Ok(Observation::Completed(_))), "it was told, which is all a setting can say");
    press.says("v1/devices/me/attributes", json!({"firmware": "1.2"})).await;
    plant.reading(|r| is_attribute(r, "PRESS-1", "firmware")).await;
    press.says("v1/devices/me/attributes/request/5", json!({})).await;
    let (topic, body) = press.hears().await;
    assert_eq!(topic, "v1/devices/me/attributes/response/5");
    assert_eq!(body["shared"], json!({"limit": 40, "mode": "eco"}), "what was set and what the platform sent");
    assert_eq!(body["client"]["firmware"], json!("1.2"), "and what it said itself");
    press.says("v1/devices/me/attributes/request/6", json!({"sharedKeys": "mode"})).await;
    assert_eq!(press.hears().await.1["shared"], json!({"mode": "eco"}), "or only what it asked for");
}

#[tokio::test]
async fn a_password_and_a_client_id_are_another_way_in() {
    let basic =
        Credentials { basic: Some(Basic { client_id: Some("line-4".into()), username: Some("pump".into()), password: hash(b"s3cret") }), ..Default::default() };
    let plant = Plant::up(&[DeviceLink { code: "PUMP".into(), definition: "devices".into(), credentials: basic, ..Default::default() }]).await;
    let mut good = options(plant.port, "line-4");
    good.set_credentials("pump", "s3cret");
    let mut pump = connect(good);
    assert!(pump.up().await);
    pump.says("v1/devices/me/telemetry", json!({"flow": 3})).await;
    plant.reading(|r| is_metric(r, "PUMP", "flow", 3.0)).await;
    let mut wrong = options(plant.port, "line-5");
    wrong.set_credentials("pump", "s3cret");
    assert!(!connect(wrong).up().await, "the same password from another client id");
}

#[tokio::test]
async fn mqtt_five_comes_in_the_same_way() {
    use rumqttc::v5::mqttbytes::QoS as Qos5;
    use rumqttc::v5::{AsyncClient, Event as Event5, MqttOptions};
    let plant = Plant::up(&[token("PRESS-1", PRESS)]).await;
    let mut o = MqttOptions::new("five", "127.0.0.1", plant.port);
    o.set_credentials(PRESS, "");
    let (client, mut events) = AsyncClient::new(o, 16);
    let (tx, mut rx) = mpsc::channel(8);
    tokio::spawn(async move {
        while let Ok(e) = events.poll().await {
            if let Event5::Incoming(rumqttc::v5::mqttbytes::v5::Packet::Publish(p)) = e {
                let _ = tx.send((String::from_utf8_lossy(&p.topic).to_string(), p.payload.to_vec())).await;
            }
        }
    });
    client.subscribe("v1/devices/me/rpc/request/+", Qos5::AtLeastOnce).await.unwrap();
    client.publish("v1/devices/me/telemetry", Qos5::AtLeastOnce, false, r#"{"temp": 7}"#).await.unwrap();
    plant.reading(|r| is_metric(r, "PRESS-1", "temp", 7.0)).await;
    let c = command("fan.start", json!({}));
    until("it to be listening", async || matches!(plant.d.dispatch(&c, &site_device("PRESS-1")).await, Ok(Dispatched::Accepted { .. }))).await;
    let (topic, _) = tokio::time::timeout(Wait::from_secs(5), rx.recv()).await.unwrap().unwrap();
    assert!(topic.starts_with("v1/devices/me/rpc/request/"), "{topic}");
}

#[tokio::test]
async fn a_gateway_of_its_own_speaks_for_what_is_behind_it() {
    let mut pump = DeviceLink { code: "Pump 1".into(), definition: "devices".into(), via: Some("TB-GW".into()), ..Default::default() };
    pump.settings = json!({});
    let elsewhere = DeviceLink { code: "Pump 2".into(), definition: "devices".into(), ..Default::default() };
    let plant = Plant::up(&[token("TB-GW", "gw-token"), pump, elsewhere]).await;
    let mut gateway = with_token(plant.port, "gw-token").await;
    assert!(gateway.up().await);
    gateway.listens("v1/gateway/rpc").await;
    gateway.says("v1/gateway/connect", json!({"device": "Pump 1"})).await;
    gateway.says("v1/gateway/telemetry", json!({"Pump 1": [{"ts": 1_700_000_000_000i64, "values": {"flow": 3}}], "Pump 2": [{"ts": 1_700_000_000_000i64, "values": {"flow": 4}}], "Pump 3": [{"ts": 1_700_000_000_000i64, "values": {"flow": 5}}]})).await;
    plant.reading(|r| is_metric(r, "Pump 1", "flow", 3.0)).await;
    assert!(!plant.saw(|r| is_metric(r, "Pump 2", "flow", 4.0)).await, "that one is not behind it");
    assert_eq!(plant.d.strangers(), vec!["Pump 3".to_string()], "one nobody knows is for a person to claim");

    let c = command("open", json!({"by": 2}));
    let Ok(Dispatched::Accepted { external: Some(external) }) = plant.d.dispatch(&c, &site_device("Pump 1")).await else { panic!("accepted") };
    let (topic, body) = gateway.hears().await;
    assert_eq!(topic, "v1/gateway/rpc");
    assert_eq!(body["device"], json!("Pump 1"));
    assert_eq!(body["data"]["method"], json!("open"));
    gateway.says("v1/gateway/rpc", json!({"device": "Pump 1", "id": external.parse::<u64>().unwrap(), "data": {"success": true}})).await;
    until("the answer", async || matches!(plant.d.observe(&c, Some(&external)).await, Ok(Observation::Completed(_)))).await;
}

#[tokio::test]
async fn taking_a_device_away_ends_its_connection() {
    let links = [token("PRESS-1", PRESS), token("OTHER", OTHER)];
    let plant = Plant::up(&links).await;
    let mut press = with_token(plant.port, PRESS).await;
    assert!(press.up().await);
    assert_eq!(plant.d.apply(&plant.definitions, &links[1..]), Vec::<String>::new());
    assert!(!press.up().await, "dropped, not left to find out");
    let mut again = with_token(plant.port, PRESS).await;
    assert!(!again.up().await, "and it cannot come back");
    let mut other = with_token(plant.port, OTHER).await;
    assert!(other.up().await, "and the rest are as they were");
}

#[tokio::test]
async fn nonsense_on_the_port_harms_nothing() {
    let plant = Plant::up(&[token("PRESS-1", PRESS)]).await;
    let junk: [&[u8]; 4] = [b"GET / HTTP/1.1\r\n\r\n", &[0x10, 0xff, 0xff, 0xff, 0xff, 0x01], &[0x10, 0x02, 0x00], &[0x30, 0x7f, 0x00, 0xff, 0xff]];
    for bytes in junk {
        let mut s = TcpStream::connect(("127.0.0.1", plant.port)).await.unwrap();
        s.write_all(bytes).await.unwrap();
        s.flush().await.unwrap();
    }
    let mut press = with_token(plant.port, PRESS).await;
    assert!(press.up().await, "it is still there for those who belong");
    press.says("v1/devices/me/telemetry", json!({"temp": 1})).await;
    plant.reading(|r| is_metric(r, "PRESS-1", "temp", 1.0)).await;
}

struct Pki {
    ca_pem: Vec<u8>,
    dir: std::path::PathBuf,
    device_pem: String,
    device_key: String,
    device_hash: String,
}

/// A store with a server's certificate, and a certificate for a device that no authority signed.
fn pki() -> Pki {
    let dir = std::env::temp_dir().join(format!("ferrum-devices-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let server = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string(), "localhost".to_string()]).unwrap();
    std::fs::write(dir.join("server.pem"), server.cert.pem()).unwrap();
    std::fs::write(dir.join("server.key"), server.key_pair.serialize_pem()).unwrap();
    let device = rcgen::generate_simple_self_signed(vec!["press.local".to_string()]).unwrap();
    Pki {
        ca_pem: server.cert.pem().into_bytes(),
        device_hash: hash(device.cert.der()),
        device_pem: device.cert.pem(),
        device_key: device.key_pair.serialize_pem(),
        dir,
    }
}

async fn secured(pki: &Pki, links: &[DeviceLink]) -> (Defined, u16) {
    let port = free_port();
    let body = format!("name: devices\nconnect: listen+mqtts://127.0.0.1:{port}\napi: device\ntls: {{cert: server.pem, key: server.key}}\n");
    let definitions = BTreeMap::from([("devices".to_string(), body)]);
    let d = Defined::new(Limits { listen: vec![format!("127.0.0.1:{port}")], tls_dir: Some(pki.dir.clone()), ..Default::default() }, json!({}));
    assert_eq!(d.apply(&definitions, links), Vec::<String>::new());
    until("the port", async || TcpStream::connect(("127.0.0.1", port)).await.is_ok()).await;
    (d, port)
}

#[tokio::test]
async fn over_tls_a_device_can_be_known_by_its_certificate() {
    let pki = pki();
    let by_cert = DeviceLink {
        code: "CERT".into(),
        definition: "devices".into(),
        credentials: Credentials { certificate: Some(pki.device_hash.clone()), ..Default::default() },
        ..Default::default()
    };
    let (d, port) = secured(&pki, &[by_cert, token("PRESS-1", PRESS)]).await;
    let tls = |client_auth| Transport::Tls(TlsConfiguration::Simple { ca: pki.ca_pem.clone(), alpn: None, client_auth });

    let mut o = options(port, "with-cert");
    o.set_transport(tls(Some((pki.device_pem.clone().into_bytes(), pki.device_key.clone().into_bytes()))));
    let mut device = connect(o);
    assert!(device.up().await, "its certificate is all it needs");
    device.says("v1/devices/me/telemetry", json!({"temp": 5})).await;
    let plant = Plant { d, definitions: BTreeMap::new(), port, seen: Mutex::default() };
    plant.reading(|r| is_metric(r, "CERT", "temp", 5.0)).await;

    let mut o = options(port, "with-token");
    o.set_transport(tls(None));
    o.set_credentials(PRESS, "");
    let mut press = connect(o);
    assert!(press.up().await, "a token over TLS still works");

    let stranger = rcgen::generate_simple_self_signed(vec!["nobody.local".to_string()]).unwrap();
    let mut o = options(port, "unknown-cert");
    o.set_transport(tls(Some((stranger.cert.pem().into_bytes(), stranger.key_pair.serialize_pem().into_bytes()))));
    assert!(!connect(o).up().await, "any certificate proves a key, and only a known one proves a device");
    std::fs::remove_dir_all(&pki.dir).ok();
}

#[tokio::test]
async fn opening_a_port_for_devices_is_the_sites_decision() {
    let definitions = BTreeMap::from([("devices".to_string(), "name: devices\nconnect: listen+mqtt://0.0.0.0:1883\napi: device\n".to_string())]);
    let d = Defined::new(Limits::default(), json!({}));
    let problems = d.apply(&definitions, &[token("PRESS-1", PRESS)]);
    assert!(problems.iter().any(|p| p.contains("listen list")), "{problems:?}");
}

struct Doors {
    plant: Plant,
    http: String,
    coap: std::net::SocketAddr,
}

/// One definition with every way in, and one device that can use any of them.
async fn every_way_in(link: DeviceLink) -> Doors {
    let (mqtt, http, coap) = (free_port(), free_port(), free_port());
    let body = format!(
        "name: devices\nconnect: listen+mqtt://127.0.0.1:{mqtt}\napi: device\nalso:\n  - listen+http://127.0.0.1:{http}\n  - listen+coap://127.0.0.1:{coap}\n"
    );
    let definitions = BTreeMap::from([("devices".to_string(), body)]);
    let listen = [mqtt, http, coap].map(|p| format!("127.0.0.1:{p}")).to_vec();
    let d = Defined::new(Limits { listen, ..Default::default() }, json!({}));
    assert_eq!(d.apply(&definitions, &[link]), Vec::<String>::new());
    until("the ports", async || d.links().iter().all(|l| l.up)).await;
    Doors {
        plant: Plant { d, definitions, port: mqtt, seen: Mutex::default() },
        http: format!("http://127.0.0.1:{http}/api/v1"),
        coap: format!("127.0.0.1:{coap}").parse().unwrap(),
    }
}

#[tokio::test]
async fn over_http_a_device_says_and_hears_the_same_things() {
    let mut link = token("PRESS-1", PRESS);
    link.shared = BTreeMap::from([("mode".to_string(), json!("eco"))]);
    let Doors { plant, http, .. } = every_way_in(link).await;
    let web = reqwest::Client::new();
    let post = |path: String, body: &'static str| web.post(path).header("content-type", "application/json").body(body).send();

    assert_eq!(post(format!("{http}/{PRESS}/telemetry"), r#"{"temp": 3}"#).await.unwrap().status(), 200);
    plant.reading(|r| is_metric(r, "PRESS-1", "temp", 3.0)).await;
    assert_eq!(post(format!("{http}/{PRESS}/attributes"), r#"{"firmware": "1.2"}"#).await.unwrap().status(), 200);
    plant.reading(|r| is_attribute(r, "PRESS-1", "firmware")).await;
    assert_eq!(post(format!("{http}/wrong/telemetry"), r#"{"temp": 3}"#).await.unwrap().status(), 401, "a token that is nobody's");
    assert_eq!(post(format!("{http}/{PRESS}/telemetry"), "not json").await.unwrap().status(), 400);
    assert_eq!(post(format!("{http}/{PRESS}/nothing"), "{}").await.unwrap().status(), 404);
    assert_eq!(web.get(format!("{http}/{PRESS}/telemetry")).send().await.unwrap().status(), 405);

    let said: Value = web.get(format!("{http}/{PRESS}/attributes")).send().await.unwrap().json().await.unwrap();
    assert_eq!(said, json!({"client": {"firmware": "1.2"}, "shared": {"mode": "eco"}}));
    let only: Value = web.get(format!("{http}/{PRESS}/attributes?sharedKeys=mode&clientKeys=none")).send().await.unwrap().json().await.unwrap();
    assert_eq!(only, json!({"client": {}, "shared": {"mode": "eco"}}));

    assert_eq!(web.get(format!("{http}/{PRESS}/rpc?timeout=50")).send().await.unwrap().status(), 408, "nothing for it");
    let waiting = tokio::spawn({
        let (web, url) = (web.clone(), format!("{http}/{PRESS}/rpc?timeout=5000"));
        async move { web.get(url).send().await.unwrap().json::<Value>().await.unwrap() }
    });
    let c = command("fan.start", json!({"speed": 3}));
    let mut external = None;
    until("it to be waiting", async || {
        if let Ok(Dispatched::Accepted { external: e }) = plant.d.dispatch(&c, &site_device("PRESS-1")).await {
            external = e;
        }
        external.is_some()
    })
    .await;
    let call = waiting.await.unwrap();
    let id = external.unwrap();
    assert_eq!((call["method"].as_str(), call["params"]["speed"].as_i64(), call["id"].to_string()), (Some("fan.start"), Some(3), id.clone()));
    assert_eq!(post(format!("{http}/{PRESS}/rpc/{id}"), r#"{"result": "ok"}"#).await.unwrap().status(), 200);
    until("the answer", async || matches!(plant.d.observe(&c, Some(&id)).await, Ok(Observation::Completed(_)))).await;

    let updates = tokio::spawn({
        let (web, url) = (web.clone(), format!("{http}/{PRESS}/attributes/updates?timeout=5000"));
        async move { web.get(url).send().await.unwrap().json::<Value>().await.unwrap() }
    });
    let set = command("set.limit", json!({"value": 9}));
    until("it to be waiting", async || matches!(plant.d.dispatch(&set, &site_device("PRESS-1")).await, Ok(Dispatched::Accepted { .. }))).await;
    assert_eq!(updates.await.unwrap(), json!({"limit": 9}));
    assert_eq!(web.post(format!("{http}/{PRESS}/telemetry")).body(vec![b'1'; 2 << 20]).send().await.unwrap().status(), 413, "there is a limit");
}

mod coap {
    use super::*;
    use tokio::net::UdpSocket;

    pub struct Client {
        socket: UdpSocket,
        next: std::sync::atomic::AtomicU16,
    }

    pub fn path(token: &str, rest: &[&str]) -> Vec<(u16, Vec<u8>)> {
        ["api", "v1", token].iter().chain(rest).map(|p| (11, p.as_bytes().to_vec())).collect()
    }

    impl Client {
        pub async fn to(server: std::net::SocketAddr) -> Client {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            socket.connect(server).await.unwrap();
            Client { socket, next: 1.into() }
        }

        pub fn request(&self, code: u8, options: Vec<(u16, Vec<u8>)>, payload: &[u8]) -> Coap {
            let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Coap { kind: 0, code, id, token: id.to_be_bytes().to_vec(), options, payload: payload.to_vec() }
        }

        pub async fn say(&self, m: &Coap) {
            self.socket.send(&m.to_bytes()).await.unwrap();
        }

        pub async fn hear(&self) -> Coap {
            let mut buffer = [0u8; 1500];
            let n = tokio::time::timeout(Wait::from_secs(5), self.socket.recv(&mut buffer)).await.expect("an answer").unwrap();
            Coap::parse(&buffer[..n]).unwrap()
        }

        pub async fn ask(&self, m: &Coap) -> Coap {
            self.say(m).await;
            self.hear().await
        }

        pub async fn silent(&self) -> bool {
            let mut buffer = [0u8; 1500];
            tokio::time::timeout(Wait::from_millis(150), self.socket.recv(&mut buffer)).await.is_err()
        }
    }
}

#[tokio::test]
async fn over_coap_a_device_says_things_once_however_often_it_repeats_them() {
    let Doors { plant, coap: server, .. } = every_way_in(token("PRESS-1", PRESS)).await;
    let device = coap::Client::to(server).await;
    let post = device.request(2, coap::path(PRESS, &["telemetry"]), br#"{"temp": 4}"#);
    let first = device.ask(&post).await;
    assert_eq!((first.kind, first.code, first.id, &first.token), (2, 0x44, post.id, &post.token), "acknowledged, changed, in the same exchange");
    plant.reading(|r| is_metric(r, "PRESS-1", "temp", 4.0)).await;

    // Its answer was lost, as far as it knows, so it says it again.
    assert_eq!(device.ask(&post).await, first, "the same answer");
    assert!(!plant.saw(|r| is_metric(r, "PRESS-1", "temp", 4.0)).await, "and one reading");

    assert_eq!(device.ask(&device.request(2, coap::path("wrong", &["telemetry"]), b"{}")).await.code, 0x81, "4.01 for a token that is nobody's");
    assert_eq!(device.ask(&device.request(2, coap::path(PRESS, &["telemetry"]), b"not json")).await.code, 0x80, "4.00");
    assert_eq!(device.ask(&device.request(1, coap::path(PRESS, &["telemetry"]), b"")).await.code, 0x85, "4.05");
    assert_eq!(device.ask(&device.request(2, coap::path(PRESS, &["nothing"]), b"{}")).await.code, 0x84, "4.04");

    // A ping, which is an empty confirmable message, is answered with a reset.
    let ping = Coap { kind: 0, code: 0, id: 900, ..Default::default() };
    assert_eq!(device.ask(&ping).await.kind, 3);

    // Non-confirmable: it asked for no acknowledgement and gets a plain answer.
    let mut non = device.request(2, coap::path(PRESS, &["attributes"]), br#"{"firmware": "2"}"#);
    non.kind = 1;
    assert_eq!(device.ask(&non).await.kind, 1);
    plant.reading(|r| is_attribute(r, "PRESS-1", "firmware")).await;
    let read = device.ask(&device.request(1, coap::path(PRESS, &["attributes"]), b"")).await;
    assert_eq!(serde_json::from_slice::<Value>(&read.payload).unwrap()["client"]["firmware"], json!("2"));
}

#[tokio::test]
async fn over_coap_a_body_can_come_in_pieces() {
    let Doors { plant, coap: server, .. } = every_way_in(token("PRESS-1", PRESS)).await;
    let device = coap::Client::to(server).await;
    let body = br#"{"temp": 12, "note": "long enough to need two datagrams on a small device"}"#;
    let (first, second) = body.split_at(32);
    let mut a = device.request(2, coap::path(PRESS, &["telemetry"]), first);
    a.options.push((27, vec![0b0000_1001])); // Block 0, more, 32 bytes.
    let said = device.ask(&a).await;
    assert_eq!(said.code, 0x5f, "2.31 Continue");
    // Each block may have a token of its own, and aiocoap's do.
    let mut b = device.request(2, coap::path(PRESS, &["telemetry"]), second);
    assert_ne!(a.token, b.token);
    b.options.push((27, vec![0b0001_0001])); // Block 1, the last.
    assert_eq!(device.ask(&b).await.code, 0x44);
    plant.reading(|r| is_metric(r, "PRESS-1", "temp", 12.0)).await;
}

#[tokio::test]
async fn over_coap_a_device_can_watch_for_what_is_for_it() {
    let Doors { plant, coap: server, .. } = every_way_in(token("PRESS-1", PRESS)).await;
    let device = coap::Client::to(server).await;
    let mut options = coap::path(PRESS, &["rpc"]);
    options.push((6, vec![]));
    let watch = device.request(1, options, b"");
    assert_eq!(device.ask(&watch).await.code, 0x45, "2.05, and it is registered");
    let c = command("fan.start", json!({"speed": 2}));
    let Ok(Dispatched::Accepted { external: Some(id) }) = plant.d.dispatch(&c, &site_device("PRESS-1")).await else { panic!("accepted") };
    let note = device.hear().await;
    assert_eq!((note.code, &note.token), (0x45, &watch.token));
    let call: Value = serde_json::from_slice(&note.payload).unwrap();
    assert_eq!((call["method"].as_str(), call["id"].to_string()), (Some("fan.start"), id.clone()));
    let answer = device.request(2, coap::path(PRESS, &["rpc", &id]), br#"{"result": "ok"}"#);
    assert_eq!(device.ask(&answer).await.code, 0x44);
    until("the answer", async || matches!(plant.d.observe(&c, Some(&id)).await, Ok(Observation::Completed(_)))).await;

    // A reset says it has stopped caring, and it goes the same way as a device that left.
    device.say(&Coap { kind: 3, code: 0, id: 1, ..Default::default() }).await;
    let again = command("fan.stop", json!({}));
    tokio::time::sleep(Wait::from_millis(100)).await;
    assert!(matches!(plant.d.dispatch(&again, &site_device("PRESS-1")).await, Err(Failure::NotSent(_))));
    assert!(device.silent().await);
}

#[tokio::test]
async fn nonsense_over_coap_harms_nothing() {
    let Doors { plant, coap: server, .. } = every_way_in(token("PRESS-1", PRESS)).await;
    let device = coap::Client::to(server).await;
    for junk in [&b""[..], b"\x00", b"\xff\xff\xff\xff", b"\x40\x01", b"\x48\x01\x00\x00", b"\x40\x01\x00\x01\xff"] {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket.send_to(junk, server).await.unwrap();
    }
    device.ask(&device.request(2, coap::path(PRESS, &["telemetry"]), br#"{"temp": 6}"#)).await;
    plant.reading(|r| is_metric(r, "PRESS-1", "temp", 6.0)).await;
}

#[tokio::test]
async fn one_device_can_use_any_way_in() {
    let Doors { plant, http, coap: server } = every_way_in(token("PRESS-1", PRESS)).await;
    let mut press = with_token(plant.port, PRESS).await;
    assert!(press.up().await);
    press.says("v1/devices/me/attributes", json!({"via": "mqtt"})).await;
    plant.reading(|r| is_attribute(r, "PRESS-1", "via")).await;
    let said: Value = reqwest::get(format!("{http}/{PRESS}/attributes")).await.unwrap().json().await.unwrap();
    assert_eq!(said["client"]["via"], json!("mqtt"), "what it said one way is there the other");
    press.listens("v1/devices/me/rpc/request/+").await;
    let device = coap::Client::to(server).await;
    let mut options = coap::path(PRESS, &["rpc"]);
    options.push((6, vec![]));
    device.ask(&device.request(1, options, b"")).await;
    let c = command("fan.start", json!({}));
    assert!(matches!(plant.d.dispatch(&c, &site_device("PRESS-1")).await, Ok(Dispatched::Accepted { .. })));
    press.hears().await;
    assert_eq!(device.hear().await.code, 0x45, "both hear it");
}

#[tokio::test]
async fn a_definition_only_opens_what_devices_can_call() {
    let bad = [
        ("name: a\nconnect: tcp://x:1\napi: device\n", "listen+mqtt"),
        ("name: a\nconnect: listen+coap://0.0.0.0:5683\n", "api: device"),
        ("name: a\nconnect: listen+http://0.0.0.0:8080\n", "api: device"),
        ("name: a\nconnect: listen+mqtt://0.0.0.0:1883\nalso: [listen+http://0.0.0.0:8080]\n", "with an api"),
        ("name: a\nconnect: listen+mqtt://0.0.0.0:1883\napi: device\nalso: [tcp://x:1]\n", "not a way"),
    ];
    for (body, why) in bad {
        let e = ferrum_edge_contract::driver::parse(body).err().unwrap_or_else(|| panic!("{body} passed"));
        assert!(e.contains(why), "{body}: {e}");
    }
    assert!(
        ferrum_edge_contract::driver::parse(
            "name: a\nconnect: listen+mqtt://0.0.0.0:1883\napi: device\nalso: [listen+http://0.0.0.0:8080, listen+coap://0.0.0.0:5683]\n"
        )
        .is_ok()
    );
}
