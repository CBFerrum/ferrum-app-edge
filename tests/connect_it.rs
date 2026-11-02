//! How a device comes to be known: what it is given, what a gateway is told about it, and what is
//! kept. The gateway's side of this is `gateway/tests/devices_it.rs`.

mod support;

use ferrum_edge_contract::edge::Credentials;
use ferrum_ops::contract::Lifecycle;
use ferrum_ops::devices;
use serde_json::json;
use sha2::{Digest, Sha256};
use support::*;

const DEVICES: &str = "name: devices\nconnect: listen+mqtt://0.0.0.0:1883\napi: device\nalso: [listen+http://0.0.0.0:8080]\n";

fn hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[test]
fn a_device_comes_to_be_known() {
    run(scenario());
}

async fn scenario() {
    let t = workspace("connect").await;
    let viewer = t.person("view@connect.test", &["Edge Viewer"]).await;
    let engineer = t.person("eng@connect.test", &["Edge Engineer", "Ops Dispatcher", "Device Administrator"]).await;
    let site = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    let (gw, gateway1) = gateway(&t, "GW-1", site, 1).await;
    let definition =
        id_of(&t.k.insert(&engineer, "DeviceDefinition", obj(json!({"definition_name": "devices", "body": DEVICES}))).await.expect("a definition"));
    t.api(&engineer, "POST", &format!("/definitions/{definition}/publish"), json!({})).await.ok();
    t.k.run_action(&engineer, "DeviceDefinition", definition, "publish", json!({})).await.expect("published");
    let press = id_of(
        &t.insert(
            "Device",
            json!({"device_name": "Press", "device_code": "PRESS-1", "kind": "Machine", "site": site, "gateway": gw, "edge_definition": definition, "operations": [{"operation": "set.limit"}]}),
        )
        .await,
    );
    let other = id_of(
        &t.insert(
            "Device",
            json!({"device_name": "Other", "device_code": "OTHER-1", "kind": "Machine", "site": site, "gateway": gw, "edge_definition": definition}),
        )
        .await,
    );
    let path = format!("/devices/{press}/credentials");
    let of_press = async |config: &ferrum_edge_contract::edge::EdgeConfig| config.devices.iter().find(|d| d.code == "PRESS-1").unwrap().credentials.clone();

    // ───── nothing yet, and a person can see how it connects without seeing a secret ─────
    let seen = t.get(&viewer, &format!("/devices/{press}/connection"), json!({})).await.unwrap();
    assert_eq!((&seen["calls_in"], &seen["token"], &seen["password"]), (&json!(true), &json!(false), &json!(false)), "{seen}");
    assert_eq!(seen["addresses"].as_array().unwrap().len(), 2, "{seen}");
    assert_eq!(of_press(&gateway1.fetch(&t, "").await.unwrap()).await, Credentials::default());

    // ───── a token: shown once, and a gateway hears only its hash ─────
    assert!(t.api(&viewer, "POST", &path, json!({"kind": "token"})).await.is_err(), "not a viewer's to issue");
    let token = t.api(&engineer, "POST", &path, json!({"kind": "token"})).await.unwrap()["token"].as_str().unwrap().to_string();
    assert!(token.len() >= 32);
    let config = gateway1.fetch(&t, "").await.unwrap();
    assert_eq!(of_press(&config).await.token, Some(hash(&token)));
    let told = serde_json::to_string(&config).unwrap();
    assert!(!told.contains(&token), "a gateway never holds a secret");
    let again = t.api(&engineer, "POST", &path, json!({"kind": "token"})).await.unwrap()["token"].as_str().unwrap().to_string();
    assert_ne!(token, again);
    assert_eq!(of_press(&gateway1.fetch(&t, "").await.unwrap()).await.token, Some(hash(&again)), "a new one replaces the old");

    // ───── a password, with a name to go with it ─────
    assert!(t.api(&engineer, "POST", &path, json!({"kind": "password"})).await.is_err(), "a password alone proves too little");
    let basic = t.api(&engineer, "POST", &path, json!({"kind": "password", "client_id": " line-4 ", "username": ""})).await.unwrap();
    let password = basic["password"].as_str().unwrap();
    assert_eq!(basic["client_id"], json!("line-4"));
    let got = of_press(&gateway1.fetch(&t, "").await.unwrap()).await.basic.unwrap();
    assert_eq!((got.client_id.as_deref(), got.username, got.password), (Some("line-4"), None, hash(password)));

    // ───── a certificate, by its fingerprint or its PEM, and never a key ─────
    let fingerprint = "A".repeat(64);
    let said = t.api(&engineer, "POST", &path, json!({"kind": "certificate", "certificate": fingerprint})).await.unwrap();
    assert_eq!(said["fingerprint"], json!("a".repeat(64)));
    let pem = "-----BEGIN CERTIFICATE-----\nAQIDBA==\n-----END CERTIFICATE-----\n";
    let said = t.api(&engineer, "POST", &path, json!({"kind": "certificate", "certificate": pem})).await.unwrap();
    assert_eq!(said["fingerprint"], json!(hex::encode(Sha256::digest([1u8, 2, 3, 4]))), "the hash of what the PEM holds");
    assert!(
        t.api(&engineer, "POST", &path, json!({"kind": "certificate", "certificate": "-----BEGIN PRIVATE KEY-----\nAQID\n-----END PRIVATE KEY-----"}))
            .await
            .is_err()
    );
    assert!(
        t.api(&engineer, "POST", &format!("/devices/{other}/credentials"), json!({"kind": "certificate", "certificate": pem})).await.is_err(),
        "one certificate is one device"
    );
    assert!(t.api(&engineer, "POST", &path, json!({"kind": "nothing"})).await.is_err());

    // ───── what it asked to be goes with it, for when it asks ─────
    for to in [Lifecycle::Claimed, Lifecycle::Provisioned, Lifecycle::Commissioned, Lifecycle::Active] {
        devices::set_lifecycle(&t.k, &t.admin, press, to, "in service").await.unwrap();
    }
    t.api(&engineer, "PUT", &format!("/devices/{press}/desired"), json!({"key": "limit", "value": 40})).await.unwrap();
    let wanted = gateway1.fetch(&t, "").await.unwrap();
    assert_eq!(wanted.devices.iter().find(|d| d.code == "PRESS-1").unwrap().shared["limit"], json!(40));

    // ───── a device behind a gateway of its own ─────
    t.k.update(&engineer, "Device", other, obj(json!({"edge_via": press})), None).await.expect("it can be set");
    assert_eq!(gateway1.fetch(&t, "").await.unwrap().devices.iter().find(|d| d.code == "OTHER-1").unwrap().via.as_deref(), Some("PRESS-1"));

    // ───── taken away, one at a time or all together ─────
    let seen = t.get(&viewer, &format!("/devices/{press}/connection"), json!({})).await.unwrap();
    assert_eq!((&seen["token"], &seen["password"], seen["certificate"].is_string()), (&json!(true), &json!(true), true), "{seen}");
    assert!(seen.to_string().find(&hash(&again)).is_none(), "no hash is shown either");
    assert!(t.api(&viewer, "DELETE", &path, json!({})).await.is_err());
    t.api(&engineer, "DELETE", &path, json!({})).await.ok();
    assert_eq!(of_press(&gateway1.fetch(&t, "").await.unwrap()).await, Credentials::default());
}
