//! IoT Edge against a real PostgreSQL: what a gateway is told to be, what comes back from it, and what
//! a person does with that.

mod support;

use chrono::{Duration, Utc};
use ferrum_edge_contract::edge::{AlarmChange, ConfigRequest};
use ferrum_kernel::Error;
use ferrum_ops::contract::Lifecycle;
use ferrum_ops::devices;
use serde_json::{Map, Value, json};
use sha2::Digest;
use support::*;

const SCALE: &str = r#"
name: acme-scale
connect: tcp://10.0.0.7:9100
frame: {line: "\r\n"}
read:
  - {match: '^(?P<kg>[0-9.]+) kg$', metric: weight.kg, group: kg}
commands:
  set.tare:
    send: "T {payload.value}\r\n"
    once: true
"#;

#[test]
fn iot_edge() {
    run(scenario());
}

async fn scenario() {
    let t = workspace("edge").await;
    let k = t.k.clone();
    let viewer = t.person("view@edge.test", &["Edge Viewer"]).await;
    let engineer = t.person("eng@edge.test", &["Edge Engineer", "Ops Dispatcher", "Device Administrator"]).await;

    // ───── a definition is checked when it is saved, and tried with no device attached ─────
    let bad = k.insert(&engineer, "DeviceDefinition", obj(json!({"definition_name": "bad", "body": "name: x\nconnect: foo://y\n"}))).await;
    assert!(matches!(bad, Err(Error::Validation(ref v)) if v[0].message.contains("not an address")), "{bad:?}");
    let tried = t.api(&engineer, "POST", "/definitions/try", json!({"body": SCALE, "sample": "12.5 kg"})).await.unwrap();
    assert_eq!(tried["ok"], json!(true), "{tried}");
    assert_eq!(tried["decoded"][0]["value"], json!(12.5));
    let wrong = t.api(&engineer, "POST", "/definitions/try", json!({"body": "name: x\nread:\n  - {metirc: a}\n"})).await.unwrap();
    assert!(wrong["error"].as_str().unwrap().contains("metirc"), "{wrong}");
    assert!(t.api(&viewer, "POST", "/definitions/try", json!({"body": SCALE})).await.is_err(), "a viewer does not write definitions");
    let definition = id_of(&k.insert(&engineer, "DeviceDefinition", obj(json!({"definition_name": "acme-scale", "body": SCALE}))).await.expect("a definition"));

    // ───── a gateway gets what it reaches, once it is published ─────
    let site = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    let other_site = id_of(&t.insert("Site", json!({"site_name": "South", "site_code": "south"})).await);
    let (gw, gateway1) = gateway(&t, "GW-1", site, 1).await;
    let scale = id_of(&t.insert("Device", json!({"device_name": "Scale", "device_code": "SCALE-1", "kind": "Machine", "site": site, "gateway": gw, "edge_definition": definition, "edge_connect": "tcp://10.0.0.8:9100", "edge_settings": {"unit": 3}, "edge_concurrency": 2, "operations": [{"operation": "set.tare"}]})).await);
    t.insert("Device", json!({"device_name": "Elsewhere", "device_code": "OTHER-1", "kind": "Machine", "site": other_site, "edge_definition": definition}))
        .await;
    let draft = gateway1.fetch(&t, "").await.expect("a first configuration");
    assert!(draft.devices.is_empty(), "a draft reaches nothing");
    k.run_action(&engineer, "DeviceDefinition", definition, "publish", json!({})).await.expect("published");
    let config = gateway1.fetch(&t, &draft.version).await.expect("it changed");
    assert_eq!(config.devices.len(), 1, "only what this gateway reaches: {config:?}");
    assert_eq!(config.devices[0].code, "SCALE-1");
    assert_eq!(config.devices[0].connect.as_deref(), Some("tcp://10.0.0.8:9100"));
    assert_eq!(config.devices[0].settings, json!({"unit": 3}));
    assert_eq!(config.devices[0].concurrency, 2);
    assert_eq!(config.definitions["acme-scale"], SCALE);
    assert!(gateway1.fetch(&t, &config.version).await.is_none(), "an unchanged configuration costs one short answer");

    // A saved draft does not move what gateways run. Publishing does.
    k.update(&engineer, "DeviceDefinition", definition, obj(json!({"body": format!("{SCALE}\n# better\n")})), None).await.unwrap();
    assert!(gateway1.fetch(&t, &config.version).await.is_none());
    k.run_action(&engineer, "DeviceDefinition", definition, "publish", json!({})).await.unwrap();
    let config = gateway1.fetch(&t, &config.version).await.expect("published again");
    assert!(config.definitions["acme-scale"].contains("better"));

    // ───── a rule reaches the sites it is for ─────
    assert!(k.insert(&engineer, "EdgeRule", obj(json!({"rule_name": "x", "metric": "m", "comparison": "Silent"}))).await.is_err(), "silence needs a length");
    assert!(k.insert(&engineer, "EdgeRule", obj(json!({"rule_name": "x", "metric": "m", "comparison": "Above"}))).await.is_err(), "a limit needs a value");
    assert!(
        k.insert(&engineer, "EdgeRule", obj(json!({"rule_name": "x", "metric": "m", "comparison": "Above", "limit_value": 80, "clear_value": 90})))
            .await
            .is_err(),
        "it clears on the safe side"
    );
    let rule = id_of(&k.insert(&engineer, "EdgeRule", obj(json!({"rule_name": "Overweight", "metric": "weight.kg", "comparison": "Above", "limit_value": 100, "clear_value": 90, "hold_secs": 5, "severity": "Critical", "definition": definition}))).await.expect("a rule"));
    k.insert(
        &engineer,
        "EdgeRule",
        obj(json!({"rule_name": "South only", "metric": "weight.kg", "comparison": "Above", "limit_value": 1, "site": other_site})),
    )
    .await
    .unwrap();
    k.insert(
        &engineer,
        "EdgeExport",
        obj(json!({"export_name": "Historian", "send_to": "https://historian.local/in", "metrics": "weight.*, temp.c", "sign_with": "historian"})),
    )
    .await
    .unwrap();
    let config = gateway1.fetch(&t, &config.version).await.expect("rules changed it");
    assert_eq!(config.rules.len(), 1, "{:?}", config.rules);
    assert_eq!((config.rules[0].value, config.rules[0].clear, config.rules[0].hold_secs), (100.0, Some(90.0), 5));
    assert_eq!(config.rules[0].devices, vec!["SCALE-1".to_string()], "a rule for a definition is for its devices");
    assert_eq!(config.exports[0].only, vec!["weight.*".to_string(), "temp.c".to_string()]);

    // ───── what comes back, in any order ─────
    let base = Utc::now();
    let raised = AlarmChange { rule, device: scale, raised: true, value: Some(120.0), node: None, name: None, severity: None };
    let cleared = AlarmChange { rule, device: scale, raised: false, value: Some(80.0), node: None, name: None, severity: None };
    t.fact(site, "edge.alarm", base, json!(raised)).await;
    t.fact(site, "edge.alarm", base + Duration::seconds(1), json!(raised)).await;
    let overview = t.api(&viewer, "GET", "/overview", Value::Null).await.unwrap();
    assert_eq!(overview["alarms"]["Critical"], json!(1), "raised once, however often it is said: {overview}");
    let device = t.api(&viewer, "GET", &format!("/devices/{scale}"), Value::Null).await.unwrap();
    let alarm = device["alarms"][0]["id"].as_str().unwrap().to_string();
    assert!(t.api(&viewer, "POST", &format!("/alarms/{alarm}/acknowledge"), json!({})).await.is_err());
    t.api(&engineer, "POST", &format!("/alarms/{alarm}/acknowledge"), json!({"note": "Going to look"})).await.expect("acknowledged");
    assert!(t.api(&engineer, "POST", &format!("/alarms/{alarm}/acknowledge"), json!({})).await.is_err(), "once");
    t.fact(site, "edge.alarm", base + Duration::seconds(30), json!(cleared)).await;
    let device = t.api(&viewer, "GET", &format!("/devices/{scale}"), Value::Null).await.unwrap();
    assert_eq!(device["alarms"], json!([]), "the edge cleared it");

    t.fact(site, "edge.attribute", base, json!({"device": scale, "key": "firmware", "value": "1.2"})).await;
    t.fact(site, "edge.attribute", base - Duration::hours(1), json!({"device": scale, "key": "firmware", "value": "1.1"})).await;
    t.fact(site, "edge.event", base, json!({"device": scale, "name": "door.open", "value": true})).await;
    let device = t.api(&viewer, "GET", &format!("/devices/{scale}"), Value::Null).await.unwrap();
    assert_eq!(device["attributes"][0]["value"], json!("1.2"), "a late report does not replace a newer one");
    assert_eq!(device["events"][0]["name"], json!("door.open"));

    // ───── asking for a value goes through the ledger, and stays out of step until it is true ─────
    devices::set_lifecycle(&k, &t.admin, scale, Lifecycle::Claimed, "ours").await.unwrap();
    devices::set_lifecycle(&k, &t.admin, scale, Lifecycle::Provisioned, "behind its gateway").await.unwrap();
    devices::set_lifecycle(&k, &t.admin, scale, Lifecycle::Commissioned, "checked").await.unwrap();
    devices::set_lifecycle(&k, &t.admin, scale, Lifecycle::Active, "in service").await.unwrap();
    assert!(t.api(&viewer, "PUT", &format!("/devices/{scale}/desired"), json!({"key": "tare", "value": 5})).await.is_err());
    let asked = t.api(&engineer, "PUT", &format!("/devices/{scale}/desired"), json!({"key": "tare", "value": 5})).await.expect("asked");
    assert!(asked["command"].is_string(), "{asked}");
    let device = t.api(&viewer, "GET", &format!("/devices/{scale}"), Value::Null).await.unwrap();
    let tare = device["attributes"].as_array().unwrap().iter().find(|a| a["key"] == json!("tare")).unwrap().clone();
    assert_eq!(tare["in_step"], json!(false), "{tare}");
    t.fact(site, "edge.attribute", Utc::now(), json!({"device": scale, "key": "tare", "value": 5})).await;
    let device = t.api(&viewer, "GET", &format!("/devices/{scale}"), Value::Null).await.unwrap();
    let tare = device["attributes"].as_array().unwrap().iter().find(|a| a["key"] == json!("tare")).unwrap().clone();
    assert_eq!(tare["in_step"], json!(true), "{tare}");

    // ───── what a gateway heard and nobody claimed ─────
    gateway1
        .config(
            &t,
            &ConfigRequest {
                have: "x".into(),
                strangers: vec!["V-9".into()],
                problems: vec!["bad: broken".into()],
                links: json!([{"address": "tcp://a", "up": true}]),
                runtime: "0.1.0".into(),
                ..Default::default()
            },
        )
        .await;
    let overview = t.api(&viewer, "GET", "/overview", Value::Null).await.unwrap();
    assert_eq!(overview["strangers"][0]["code"], json!("V-9"), "{overview}");
    assert_eq!(overview["gateways"][0]["problems"], json!(["bad: broken"]));
    assert_eq!(overview["gateways"][0]["links"][0]["up"], json!(true));
    let claimed = t.api(&engineer, "POST", "/strangers/claim", json!({"gateway": gw, "code": "V-9", "definition": definition})).await.expect("claimed");
    assert!(claimed["id"].is_string());
    let overview = t.api(&viewer, "GET", "/overview", Value::Null).await.unwrap();
    assert_eq!(overview["strangers"], json!([]));

    // ───── a device with no gateway ─────
    let probe = id_of(&t.insert("Device", json!({"device_name": "Probe", "device_code": "PROBE-1", "kind": "Sensor", "site": site})).await);
    let token = t.api(&engineer, "POST", &format!("/devices/{probe}/token"), json!({})).await.unwrap()["token"].as_str().unwrap().to_string();
    let site_api = k.hooks.site("edge").unwrap().clone();
    let ingest = |tok: String, body: Value| {
        let mut headers = Map::new();
        headers.insert("x-ferrum-device-token".into(), json!(tok));
        request(&t, None, "POST", "/ingest", headers, body.to_string().into_bytes())
    };
    let refused = site_api.handle(&k, ingest("0".repeat(48), json!({"values": {"temp.c": 1}}))).await.unwrap();
    assert_eq!(refused.status, 401);
    let sent = site_api
        .handle(&k, ingest(token.clone(), json!({"values": {"temp.c": 21.5, "text": "skip"}, "state": "Running", "attributes": {"model": "P1"}})))
        .await
        .unwrap();
    assert_eq!(sent.status, 200, "{}", String::from_utf8_lossy(&sent.body));
    t.outbox().await;
    let device = t.api(&viewer, "GET", &format!("/devices/{probe}"), Value::Null).await.unwrap();
    assert_eq!(device["attributes"][0]["key"], json!("model"), "{device}");
    assert_eq!(device["latest"][0]["metric"], json!("temp.c"));
    devices::set_lifecycle(&k, &t.admin, probe, Lifecycle::Retired, "stolen").await.unwrap();
    let stopped = site_api.handle(&k, ingest(token, json!({"values": {"temp.c": 1}}))).await.unwrap();
    assert_eq!(stopped.status, 403, "its token ends with it");

    // ───── a release goes to a share of gateways, the same share each time ─────
    let release = json!({"release_version": "0.2.0", "url": "https://r/x", "sha256": "ab", "signature": "cd", "expires_at": (Utc::now() + Duration::days(7)).to_rfc3339(), "status": "Rolling Out", "percent": 100});
    let r = id_of(&t.insert("EdgeRelease", release).await);
    let config = gateway1.fetch(&t, "").await.unwrap();
    assert_eq!(config.release.as_ref().map(|r| r.version.as_str()), Some("0.2.0"));
    k.update(&t.admin, "EdgeRelease", r, obj(json!({"percent": 0})), None).await.unwrap();
    assert!(gateway1.fetch(&t, "").await.unwrap().release.is_none(), "not this gateway's turn");
    let _ = sha2::Sha256::digest(b"");

    // ───── a flow is drawn, simulated, reviewed by someone else, and only then runs ─────
    let reviewer = t.person("rev@edge.test", &["Edge Reviewer"]).await;
    let both = t.person("both@edge.test", &["Edge Engineer", "Edge Reviewer"]).await;
    let hot = json!({
        "nodes": [
            {"id": "t", "kind": "metric", "device": "SCALE-*", "metric": "weight.kg", "x": 10, "y": 10},
            {"id": "c", "kind": "compare", "op": ">", "value": 100},
            {"id": "a", "kind": "alarm", "name": "Overloaded", "severity": "Critical"},
            {"id": "ask", "kind": "command", "operation": "tare", "payload": {"key": "tare", "value": 0}, "expires": "2m"},
        ],
        "edges": [
            {"from": "t", "to": "c"},
            {"from": "c", "port": "yes", "to": "a"},
            {"from": "c", "port": "yes", "to": "ask"},
            {"from": "c", "port": "no", "to": "a", "input": "clear"},
        ],
    });
    let draft =
        id_of(&k.insert(&engineer, "EdgeFlow", obj(json!({"flow_name": "Overload", "body": "not finished yet"}))).await.expect("a draft can have faults"));
    assert!(k.insert(&viewer, "EdgeFlow", obj(json!({"flow_name": "Mine", "body": "{}"}))).await.is_err(), "a viewer does not draw flows");
    let too_soon = k.run_action(&engineer, "EdgeFlow", draft, "submit_for_review", json!({})).await;
    assert!(too_soon.is_err(), "not with faults");
    let wrong = t.api(&engineer, "POST", "/flows/try", json!({"body": "{\"nodes\": []}"})).await.unwrap();
    assert_eq!(wrong["ok"], json!(false), "{wrong}");
    assert!(t.api(&viewer, "POST", "/flows/try", json!({"body": hot.to_string()})).await.is_err(), "nor try them");
    k.update(&engineer, "EdgeFlow", draft, obj(json!({"body": hot.to_string()})), None).await.expect("finished");

    let not_seen = k.run_action(&engineer, "EdgeFlow", draft, "submit_for_review", json!({})).await;
    assert!(format!("{not_seen:?}").contains("simulate"), "{not_seen:?}");
    let script = json!({"script": [{"at": 0, "device": "SCALE-1", "metric": "weight.kg", "value": 150}, {"at": 5, "device": "SCALE-1", "metric": "weight.kg", "value": 20}], "until": 6});
    let idle = t.api(&engineer, "POST", &format!("/flows/{draft}/simulate"), json!({"script": []})).await.unwrap();
    assert!(idle["error"].as_str().is_some_and(|e| e.contains("nothing happened")), "a run that showed nothing shows nothing: {idle}");
    assert!(k.run_action(&engineer, "EdgeFlow", draft, "submit_for_review", json!({})).await.is_err());
    let run = t.api(&engineer, "POST", &format!("/flows/{draft}/simulate"), script.clone()).await.unwrap();
    assert_eq!(run["moments"][0]["effects"][0]["effect"], json!("alarm"), "{run}");
    assert_eq!(run["moments"][0]["effects"][1]["effect"], json!("command"), "{run}");

    // A change after the simulation means that was not this version.
    let changed = {
        let mut h = hot.clone();
        h["nodes"][1]["value"] = json!(90);
        h
    };
    k.update(&engineer, "EdgeFlow", draft, obj(json!({"body": changed.to_string()})), None).await.unwrap();
    assert!(k.run_action(&engineer, "EdgeFlow", draft, "submit_for_review", json!({})).await.is_err(), "that simulation was of something else");
    k.update(&engineer, "EdgeFlow", draft, obj(json!({"body": hot.to_string()})), None).await.unwrap();
    t.api(&engineer, "POST", &format!("/flows/{draft}/simulate"), script).await.unwrap();
    k.run_action(&engineer, "EdgeFlow", draft, "submit_for_review", json!({})).await.expect("sent");

    let locked = k.update(&engineer, "EdgeFlow", draft, obj(json!({"body": "{}"})), None).await;
    assert!(locked.is_err(), "what a reviewer is reading cannot change under them");
    assert!(k.run_action(&engineer, "EdgeFlow", draft, "approve", json!({})).await.is_err(), "nor does its author");
    let second = t.person("eng2@edge.test", &["Edge Engineer"]).await;
    assert!(k.run_action(&second, "EdgeFlow", draft, "approve", json!({})).await.is_err(), "and another engineer is not a reviewer");
    assert!(k.run_action(&reviewer, "EdgeFlow", draft, "activate", json!({})).await.is_err(), "nothing runs before it is approved");
    assert!(gateway1.fetch(&t, "").await.unwrap().flows.is_empty(), "and no gateway hears of it");
    k.run_action(&reviewer, "EdgeFlow", draft, "send_back", json!({"note": "Say why 100"})).await.expect("sent back");
    k.update(&both, "EdgeFlow", draft, obj(json!({"notes": "100 kg is the rating of the platform"})), None).await.unwrap();
    k.run_action(&both, "EdgeFlow", draft, "submit_for_review", json!({})).await.expect("sent again");
    let own = k.run_action(&both, "EdgeFlow", draft, "approve", json!({})).await;
    assert!(format!("{own:?}").contains("someone else"), "holding both roles is not two people: {own:?}");
    k.run_action(&reviewer, "EdgeFlow", draft, "approve", json!({})).await.expect("approved");
    k.run_action(&reviewer, "EdgeFlow", draft, "activate", json!({})).await.expect("running");
    let flows = gateway1.fetch(&t, "").await.unwrap().flows;
    assert_eq!((flows.len(), flows[0].revision), (1, 1), "{flows:?}");
    assert!(ferrum_edge_contract::flow::parse(&flows[0].body).is_ok(), "a gateway gets what passed");

    // Changing it leaves what runs alone until the new one has been through the same.
    k.run_action(&engineer, "EdgeFlow", draft, "revise", json!({})).await.expect("a new draft");
    k.update(&engineer, "EdgeFlow", draft, obj(json!({"body": changed.to_string()})), None).await.unwrap();
    assert_eq!(gateway1.fetch(&t, "").await.unwrap().flows[0].body, hot.to_string());

    // ───── what a flow does comes back ─────
    let flow_alarm = |raised: bool, value: f64| json!({"rule": draft, "device": scale, "raised": raised, "value": value, "node": "a", "name": "Overloaded", "severity": "Critical"});
    let base = Utc::now() - Duration::seconds(30);
    t.fact(site, "edge.alarm", base, flow_alarm(true, 150.0)).await;
    t.fact(site, "edge.alarm", base + Duration::seconds(1), flow_alarm(true, 151.0)).await;
    let alarms: Vec<(String, Option<String>)> = sqlx::query_as("select summary, severity from e_edge_alarm where tenant_id = $1 and flow = $2")
        .bind(t.tenant)
        .bind(draft)
        .fetch_all(&mut *t.tx().await)
        .await
        .unwrap();
    assert_eq!(alarms, [("Overloaded on SCALE-1".to_string(), Some("Critical".to_string()))], "raised once, under its own name");
    t.fact(site, "edge.alarm", base + Duration::seconds(2), flow_alarm(false, 20.0)).await;
    let open: i64 = sqlx::query_scalar("select count(*) from e_edge_alarm where tenant_id = $1 and flow = $2 and status <> 'Cleared'")
        .bind(t.tenant)
        .bind(draft)
        .fetch_one(&mut *t.tx().await)
        .await
        .unwrap();
    assert_eq!(open, 0, "the flow cleared it");

    t.fact(site, "edge.notify", Utc::now(), json!({"flow": draft, "device": scale, "title": "Look at the scale"})).await;
    let told: i64 = sqlx::query_scalar("select count(*) from notification where tenant_id = $1 and title = 'Look at the scale'")
        .bind(t.tenant)
        .fetch_one(&mut *t.tx().await)
        .await
        .unwrap();
    assert!(told >= 1, "the people who look after it heard");

    // A request is only a request: it waits for a person, there being no envelope.
    let ask = json!({"flow": draft, "node": "ask", "device": scale, "operation": "set.tare", "payload": {"key": "tare", "value": 0}, "expires_secs": 120});
    t.fact(site, "edge.request", Utc::now(), ask.clone()).await;
    let asked: Vec<(String, String)> = sqlx::query_as("select state, issued_kind from op_command where tenant_id = $1 and workflow_id = $2")
        .bind(t.tenant)
        .bind(format!("flow:{draft}"))
        .fetch_all(&mut *t.tx().await)
        .await
        .unwrap();
    assert_eq!(asked, [("Proposed".to_string(), "automation".to_string())], "nothing moves until someone says so");

    // What waited out an outage has passed, and what its flow no longer wants is not issued.
    t.fact(site, "edge.request", Utc::now() - Duration::minutes(5), ask.clone()).await;
    k.run_action(&engineer, "EdgeFlow", draft, "stop", json!({})).await.expect("stopped");
    assert!(gateway1.fetch(&t, "").await.unwrap().flows.is_empty(), "a stopped flow leaves its gateways");
    t.fact(site, "edge.request", Utc::now(), ask).await;
    let count: i64 = sqlx::query_scalar("select count(*) from op_command where tenant_id = $1 and workflow_id = $2")
        .bind(t.tenant)
        .bind(format!("flow:{draft}"))
        .fetch_one(&mut *t.tx().await)
        .await
        .unwrap();
    assert_eq!(count, 1, "those two became nothing");
    let refused: Vec<String> = sqlx::query_scalar("select value->>'why' from edge_event where tenant_id = $1 and name = 'flow.request.refused' order by at")
        .bind(t.tenant)
        .fetch_all(&mut *t.tx().await)
        .await
        .unwrap();
    assert_eq!(refused.len(), 2, "and they say so");
    assert!(refused[0].contains("too long") && refused[1].contains("stopped"), "{refused:?}");

    // ───── robots ─────
    assert_eq!(t.api(&viewer, "GET", "/robots", Value::Null).await.unwrap()["robots"], json!([]));
}
