//! A gateway on both sides of Sparkplug, over a real broker (rumqttd): a primary host that follows
//! nodes and asks them to start again, and an edge node a SCADA system could read.

use ferrum_edge_contract::edge::{DeviceLink, Export};
use ferrum_edge_contract::sparkplug;
use ferrum_site::adapter::{Adapter, Reading};
use ferrum_site::adapters::defined::Defined;
use ferrum_site::adapters::transport::Limits;
use ferrum_site::export::Sender;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::Packet;
use rumqttc::v5::{AsyncClient, Event, MqttOptions};
use rumqttd::{Broker, Config, ConnectionSettings, RouterConfig, ServerSettings};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use tokio::sync::mpsc;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn broker() -> u16 {
    let port = free_port();
    let router = RouterConfig { max_segment_size: 1 << 20, max_segment_count: 10, max_connections: 100, ..Default::default() };
    let connections = ConnectionSettings {
        connection_timeout_ms: 5_000,
        max_payload_size: 1 << 20,
        max_inflight_count: 100,
        auth: None,
        external_auth: None,
        dynamic_filters: true,
    };
    let server =
        ServerSettings { name: "v5".into(), listen: format!("127.0.0.1:{port}").parse().unwrap(), tls: None, next_connection_delay_ms: 0, connections };
    let config = Config { id: 0, router, v5: Some(HashMap::from([("1".to_string(), server)])), ..Default::default() };
    std::thread::spawn(move || {
        let _ = Broker::new(config).start();
    });
    port
}

/// Someone else on the broker: an MQTT client that hears everything.
async fn listener(port: u16, client: &str, filter: &str) -> (AsyncClient, mpsc::Receiver<(String, Vec<u8>)>) {
    let mut options = MqttOptions::new(client, "127.0.0.1", port);
    options.set_keep_alive(Duration::from_secs(30));
    let (mqtt, mut events) = AsyncClient::new(options, 64);
    mqtt.subscribe(filter, QoS::AtLeastOnce).await.unwrap();
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        loop {
            match events.poll().await {
                Ok(Event::Incoming(Packet::Publish(p))) => {
                    let _ = tx.send((String::from_utf8_lossy(&p.topic).into_owned(), p.payload.to_vec())).await;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                _ => {}
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    (mqtt, rx)
}

async fn next(rx: &mut mpsc::Receiver<(String, Vec<u8>)>, topic: &str) -> Vec<u8> {
    loop {
        let (heard, body) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap_or_else(|_| panic!("nothing on {topic}")).unwrap();
        if heard == topic {
            return body;
        }
    }
}

fn payload(seq: u64, metrics: Value) -> Vec<u8> {
    sparkplug::encode(&json!({"timestamp": 1_800_000_000_000u64, "seq": seq, "metrics": metrics})).unwrap()
}

#[tokio::test]
async fn a_primary_host_says_it_is_there_follows_nodes_and_asks_for_what_it_missed() {
    let port = broker();
    let (node, mut heard) = listener(port, "the-node", "spBv1.0/#").await;
    let body = format!(
        "name: plant\nconnect: mqtt://127.0.0.1:{port}\nprimary_host: ferrum\nsubscribe:\n  - spBv1.0/plant/+/+/+\n  - spBv1.0/plant/+/+\nread:\n  - {{topic: spBv1.0/plant/+/+/+, format: sparkplug, device: \"{{topic.3}}\", each: $.metrics, metric: \"{{each.name}}\", json: $.value}}\n"
    );
    let definitions = BTreeMap::from([("plant".to_string(), body)]);
    let links = [
        DeviceLink { code: "NODE".into(), definition: "plant".into(), ..Default::default() },
        DeviceLink { code: "PRESS".into(), definition: "plant".into(), ..Default::default() },
    ];
    let d = Defined::new(Limits::default(), json!({}));
    assert_eq!(d.apply(&definitions, &links), Vec::<String>::new());

    let state = next(&mut heard, "spBv1.0/STATE/ferrum").await;
    let said: Value = serde_json::from_slice(&state).unwrap();
    assert_eq!(said["online"], true, "it says it is there, as Sparkplug 3.0 has it");
    assert!(said["timestamp"].is_u64());
    tokio::time::sleep(Duration::from_millis(300)).await;

    let say = async |topic: &str, seq: u64, metrics: Value| node.publish(topic, QoS::AtLeastOnce, false, payload(seq, metrics)).await.unwrap();
    // It has not met this node, so it asks to be told who it is.
    say("spBv1.0/plant/NDATA/NODE", 7, json!([])).await;
    let asked = sparkplug::decode(&next(&mut heard, "spBv1.0/plant/NCMD/NODE").await).unwrap();
    assert_eq!((asked["metrics"][0]["name"].as_str(), &asked["metrics"][0]["value"]), (Some("Node Control/Rebirth"), &json!(true)));

    say("spBv1.0/plant/NBIRTH/NODE", 0, json!([{"name": "bdSeq", "value": 5, "type": "int64"}])).await;
    say("spBv1.0/plant/DBIRTH/NODE/PRESS", 1, json!([{"name": "temp", "value": 20.5, "type": "double"}])).await;
    say("spBv1.0/plant/DDATA/NODE/PRESS", 2, json!([{"name": "temp", "value": 21.5, "type": "double"}])).await;
    let mut seen = Vec::new();
    for _ in 0..200 {
        seen.extend(d.read().await.unwrap());
        if seen.iter().any(|r| matches!(r, Reading::Value { metric, value, .. } if metric == "temp" && *value == 21.5)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(seen.iter().any(|r| matches!(r, Reading::State { device, state, .. } if device == "PRESS" && state == "Online")), "{seen:?}");
    assert!(seen.iter().any(|r| matches!(r, Reading::Value { device, value, .. } if device == "PRESS" && *value == 21.5)), "{seen:?}");

    // A gap means something was missed, and it is asked again.
    say("spBv1.0/plant/DDATA/NODE/PRESS", 9, json!([{"name": "temp", "value": 1.0, "type": "double"}])).await;
    next(&mut heard, "spBv1.0/plant/NCMD/NODE").await;
    say("spBv1.0/plant/NBIRTH/NODE", 0, json!([{"name": "bdSeq", "value": 6, "type": "int64"}])).await;
    say("spBv1.0/plant/DDEATH/NODE/PRESS", 1, json!([])).await;
    let mut seen = Vec::new();
    for _ in 0..200 {
        seen.extend(d.read().await.unwrap());
        if seen.iter().any(|r| matches!(r, Reading::State { state, .. } if state == "Offline")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        seen.iter().any(|r| matches!(r, Reading::State { device, state, .. } if device == "PRESS" && state == "Offline")),
        "a device that goes away says so: {seen:?}"
    );
}

#[tokio::test]
async fn a_gateway_can_be_an_edge_node_for_someone_elses_scada() {
    let port = broker();
    let (scada, mut heard) = listener(port, "the-scada", "spBv1.0/#").await;
    let sender = Sender::new(Limits::default());
    let export = Export { name: "scada".into(), to: format!("sparkplug://127.0.0.1:{port}/plant/GW-1"), topic: None, only: vec![], sign_with: None };
    let reading = |metric: &str, value: f64| json!({"device": "PRESS", "metric": metric, "value": value, "at": "2026-11-02T10:00:00Z"}).to_string();
    // The first goes while it is still connecting, as a batch would, and stays queued.
    let mut sent = sender.deliver(&export, &[reading("temp.c", 20.0)], &json!({})).await;
    for _ in 0..100 {
        if sent.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        sent = sender.deliver(&export, &[reading("temp.c", 20.0)], &json!({})).await;
    }
    sent.expect("delivered");

    let birth = sparkplug::decode(&next(&mut heard, "spBv1.0/plant/NBIRTH/GW-1").await).unwrap();
    assert_eq!(birth["seq"], 0);
    let names: Vec<&str> = birth["metrics"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["bdSeq", "Node Control/Rebirth"]);
    let device = sparkplug::decode(&next(&mut heard, "spBv1.0/plant/DBIRTH/GW-1/PRESS").await).unwrap();
    assert_eq!((device["seq"].as_u64(), device["metrics"][0]["name"].as_str(), device["metrics"][0]["value"].as_f64()), (Some(1), Some("temp.c"), Some(20.0)));

    sender.deliver(&export, &[reading("temp.c", 21.0)], &json!({})).await.unwrap();
    let data = sparkplug::decode(&next(&mut heard, "spBv1.0/plant/DDATA/GW-1/PRESS").await).unwrap();
    assert_eq!((data["seq"].as_u64(), data["metrics"][0]["value"].as_f64()), (Some(2), Some(21.0)), "in order, as a host checks");

    sender.deliver(&export, &[reading("rpm", 900.0)], &json!({})).await.unwrap();
    let again = sparkplug::decode(&next(&mut heard, "spBv1.0/plant/DBIRTH/GW-1/PRESS").await).unwrap();
    assert_eq!(again["metrics"].as_array().unwrap().len(), 2, "a new metric needs a new certificate, with everything in it");

    // A host that missed something asks, and everything is said again.
    let rebirth = sparkplug::rebirth("plant/GW-1", 1).unwrap();
    scada.publish(rebirth.0, QoS::AtLeastOnce, false, rebirth.1).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    sender.deliver(&export, &[reading("rpm", 901.0)], &json!({})).await.unwrap();
    let born = sparkplug::decode(&next(&mut heard, "spBv1.0/plant/NBIRTH/GW-1").await).unwrap();
    assert_eq!(born["seq"], 0);
    next(&mut heard, "spBv1.0/plant/DBIRTH/GW-1/PRESS").await;
}

/// Eclipse Tahu's own definitions, when `FERRUM_TEST_PYTHON` has mqtt_spb_wrapper, as CI's does.
#[test]
fn tahus_definitions_read_what_is_written_here_and_the_other_way() {
    let Ok(python) = std::env::var("FERRUM_TEST_PYTHON") else {
        eprintln!("skipped: FERRUM_TEST_PYTHON is not set");
        return;
    };
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/sparkplug_peer.py");
    let run = |args: &[&str]| {
        let out = std::process::Command::new(&python).arg("-W").arg("ignore").arg(script).args(args).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let ours = payload(7, json!([{"name": "temp.c", "value": 21.5}, {"name": "on", "value": true}, {"name": "note", "value": "ok"}]));
    let read: Value = serde_json::from_str(&run(&["read", &hex::encode(ours)])).unwrap();
    assert_eq!((read["seq"].as_u64(), read["timestamp"].as_u64()), (Some(7), Some(1_800_000_000_000)), "{read}");
    let by_name = |name: &str| read["metrics"].as_array().unwrap().iter().find(|m| m["name"] == name).unwrap_or_else(|| panic!("{name}: {read}")).clone();
    assert_eq!(by_name("temp.c")["value"], json!(21.5));
    assert_eq!(by_name("temp.c")["datatype"], json!(10), "a Double, as Tahu numbers them");
    assert_eq!(by_name("on")["value"], json!(true));
    assert_eq!(by_name("note")["value"], json!("ok"));

    let theirs = hex::decode(run(&["write"])).unwrap();
    let got = sparkplug::decode(&theirs).expect("it reads");
    let value = |name: &str| got["metrics"].as_array().unwrap().iter().find(|m| m["name"] == name).map(|m| m["value"].clone());
    assert_eq!(value("temp.c"), Some(json!(21.5)), "{got}");
    assert_eq!(value("count"), Some(json!(7)), "{got}");
    assert_eq!(value("on"), Some(json!(true)), "{got}");
    assert_eq!(value("name"), Some(json!("press")), "{got}");
}
