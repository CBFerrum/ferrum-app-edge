//! Constrained devices registering over LwM2M. A device written here shows what is refused, and
//! Eclipse Leshan's demo client, when `FERRUM_TEST_LESHAN` names its jar, proves someone else's device
//! can register and be read.

use ferrum_edge_contract::edge::DeviceLink;
use ferrum_site::adapter::{Adapter, Reading};
use ferrum_site::adapters::coap::Coap;
use ferrum_site::adapters::defined::Defined;
use ferrum_site::adapters::transport::Limits;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::net::UdpSocket;

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn plant(code: &str) -> (Defined, u16) {
    let port = free_port();
    let body = format!(
        "name: meters\nconnect: listen+lwm2m://127.0.0.1:{port}\nsubscribe: [/3/0/0]\nread:\n  - {{topic: +/3/0/0, device: \"{{topic.0}}\", attribute: manufacturer, json: $.value}}\n  - {{topic: +/3/0/9, device: \"{{topic.0}}\", metric: battery.pct, json: $.value}}\n"
    );
    let definitions = BTreeMap::from([("meters".to_string(), body)]);
    let link = DeviceLink { code: code.into(), definition: "meters".into(), ..Default::default() };
    let d = Defined::new(Limits { listen: vec![format!("127.0.0.1:{port}")], ..Default::default() }, json!({}));
    assert_eq!(d.apply(&definitions, &[link]), Vec::<String>::new());
    for _ in 0..200 {
        if d.links().iter().all(|l| l.up) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (d, port)
}

fn register(endpoint: &str) -> Coap {
    let options = vec![(11, b"rd".to_vec()), (15, format!("ep={endpoint}").into_bytes()), (15, b"lt=60".to_vec())];
    Coap { kind: 0, code: 2, id: 1, token: vec![1], options, payload: b"</3/0>".to_vec() }
}

#[tokio::test]
async fn only_a_device_with_a_record_registers_and_is_then_watched() {
    let (d, port) = plant("METER-1").await;
    let device = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    device.connect(("127.0.0.1", port)).await.unwrap();
    let mut buffer = [0u8; 1500];

    device.send(&register("STRANGER").to_bytes()).await.unwrap();
    let n = tokio::time::timeout(Duration::from_secs(3), device.recv(&mut buffer)).await.unwrap().unwrap();
    assert_eq!(Coap::parse(&buffer[..n]).unwrap().code, 0x83, "forbidden");

    device.send(&register("METER-1").to_bytes()).await.unwrap();
    let n = tokio::time::timeout(Duration::from_secs(3), device.recv(&mut buffer)).await.unwrap().unwrap();
    let created = Coap::parse(&buffer[..n]).unwrap();
    assert_eq!(created.code, 0x41);
    assert_eq!(created.options[0].1, b"rd", "it says where the registration lives");

    // It is asked to be watched for what the definition says, and its answer is a reading.
    let n = tokio::time::timeout(Duration::from_secs(3), device.recv(&mut buffer)).await.unwrap().unwrap();
    let asked = Coap::parse(&buffer[..n]).unwrap();
    assert_eq!((asked.code, asked.path()), (1, vec!["3", "0", "0"]));
    assert!(asked.option(6).is_some(), "observed");
    let answer = Coap { kind: 2, code: 0x45, id: asked.id, token: asked.token.clone(), options: vec![(6, vec![1])], payload: b"Acme".to_vec() };
    device.send(&answer.to_bytes()).await.unwrap();
    let mut found = false;
    for _ in 0..200 {
        found |= d
            .read()
            .await
            .unwrap()
            .iter()
            .any(|r| matches!(r, Reading::Attribute { device, key, value } if device == "METER-1" && key == "manufacturer" && value == "Acme"));
        if found {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(found, "the reading arrived as the device's");
}

#[tokio::test]
async fn a_device_written_by_someone_else() {
    let Ok(jar) = std::env::var("FERRUM_TEST_LESHAN") else {
        eprintln!("skipped: FERRUM_TEST_LESHAN is not set");
        return;
    };
    let (d, port) = plant("meter-1").await;
    let mut client = tokio::process::Command::new("java")
        .args(["-jar", &jar, "-n", "meter-1", "-u", &format!("coap://127.0.0.1:{port}")])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut found = false;
    for _ in 0..1200 {
        found |= d.read().await.unwrap().iter().any(|r| matches!(r, Reading::Attribute { device, key, .. } if device == "meter-1" && key == "manufacturer"));
        if found {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = client.kill().await;
    assert!(found, "Leshan's client registered and said who made it");
}
