//! Flows on the platform, and watching one run. A flow here sees across sites, a minute at a time,
//! and what it does counts the way a gateway's does.

mod support;

use chrono::{DurationRound, TimeDelta, Utc};
use ferrum_app_edge::{config, platform_flows};
use ferrum_edge_contract::edge::FlowTrace;
use ferrum_edge_contract::flow::Trace;
use ferrum_kernel::Ctx;
use ferrum_ops::contract::telemetry::{Class, Measurement, Quality};
use ferrum_ops::telemetry;
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

#[test]
fn flows_on_the_platform() {
    run(scenario());
}

async fn reading(t: &T, site: Uuid, device: Uuid, metric: &str, value: f64, ago_minutes: i64, seq: i64) {
    let now = Utc::now();
    let at = now.duration_trunc(TimeDelta::minutes(1)).unwrap() - TimeDelta::minutes(ago_minutes) + TimeDelta::seconds(seq);
    let m = Measurement {
        device_id: device,
        metric: metric.into(),
        value,
        unit: None,
        source_time: at,
        boot: device,
        seq,
        quality: Quality::Good,
        class: Class::Sample,
    };
    telemetry::ensure_partitions(&t.k, now).await.unwrap();
    let mut tx = t.tx().await;
    telemetry::ingest(&mut tx, t.tenant, site, vec![m], now).await.unwrap();
    tx.commit().await.unwrap();
}

/// As if the flow had last read this long ago.
async fn rewind(t: &T, flow: Uuid, minutes: i32) {
    let mut tx = t.tx().await;
    sqlx::query("update edge_flow_state set read_to = date_trunc('minute', now()) - make_interval(mins => $3) where tenant_id = $1 and flow_id = $2")
        .bind(t.tenant)
        .bind(flow)
        .bind(minutes)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn one_value(t: &T, sql: &str, id: Uuid) -> Option<String> {
    let mut tx = t.tx().await;
    let v = sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).bind(t.tenant).bind(id).fetch_optional(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    v
}

async fn draft(t: &T, engineer: &Ctx, reviewer: &Ctx, name: &str, body: Value, runs_at: &str) -> Uuid {
    let id = id_of(&t.k.insert(engineer, "EdgeFlow", obj(json!({"flow_name": name, "body": body.to_string(), "runs_at": runs_at}))).await.unwrap());
    t.api(engineer, "POST", &format!("/flows/{id}/simulate"), json!({"script": [{"at": 0, "device": "METER-A", "metric": "power.kw", "value": 1}]}))
        .await
        .unwrap();
    t.k.run_action(engineer, "EdgeFlow", id, "submit_for_review", json!({})).await.expect("sent");
    t.k.run_action(reviewer, "EdgeFlow", id, "approve", json!({})).await.expect("approved");
    t.k.run_action(reviewer, "EdgeFlow", id, "activate", json!({})).await.expect("running");
    id
}

async fn scenario() {
    let t = workspace("flowing").await;
    let engineer = t.person("eng@flowing.test", &["Edge Engineer", "Ops Manager"]).await;
    let reviewer = t.person("rev@flowing.test", &["Edge Reviewer"]).await;
    let viewer = t.person("view@flowing.test", &["Edge Viewer"]).await;
    let north = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    let south = id_of(&t.insert("Site", json!({"site_name": "South", "site_code": "south"})).await);
    let (gw, _gateway) = gateway(&t, "GW-N", north, 1).await;
    let (gs, _gateway) = gateway(&t, "GW-S", south, 2).await;
    let a = id_of(&t.insert("Device", json!({"device_name": "A", "device_code": "METER-A", "kind": "Meter", "site": north, "gateway": gw})).await);
    let b = id_of(&t.insert("Device", json!({"device_name": "B", "device_code": "METER-B", "kind": "Meter", "site": south, "gateway": gs})).await);

    // The whole fleet's power, across two sites, which no gateway can see.
    let body = json!({
        "nodes": [
            {"id": "power", "kind": "metric", "device": "METER-*", "metric": "power.kw"},
            {"id": "total", "kind": "window", "over": "5m", "stat": "sum", "across": true},
            {"id": "too_much", "kind": "compare", "op": ">", "value": 100},
            {"id": "alarm", "kind": "alarm", "name": "Fleet over its limit", "severity": "Major", "device": "METER-A"},
            {"id": "onto", "kind": "reassign", "to": "METER-A"},
            {"id": "record", "kind": "record_metric", "name": "fleet.kw"},
        ],
        "edges": [
            {"from": "power", "to": "total"},
            {"from": "total", "to": "too_much"},
            {"from": "too_much", "port": "yes", "to": "alarm"},
            {"from": "too_much", "port": "no", "to": "alarm", "input": "clear"},
            {"from": "total", "to": "onto"},
            {"from": "onto", "to": "record"},
        ],
    });
    // A step that needs a gateway is not found out by a flow that quietly does nothing.
    let id = id_of(
        &t.k.insert(
            &engineer,
            "EdgeFlow",
            obj(json!({"flow_name": "Answering", "runs_at": "Platform", "body": json!({"nodes": [{"id": "ask", "kind": "event", "name": "rpc.request"}, {"id": "say", "kind": "reply"}], "edges": [{"from": "ask", "to": "say"}]}).to_string()})),
        )
        .await
        .unwrap(),
    );
    t.api(
        &engineer,
        "POST",
        &format!("/flows/{id}/simulate"),
        json!({"script": [{"at": 0, "device": "METER-A", "event": "rpc.request", "value": {"id": "1"}}]}),
    )
    .await
    .unwrap();
    let refused = t.k.run_action(&engineer, "EdgeFlow", id, "submit_for_review", json!({})).await;
    assert!(format!("{refused:?}").contains("needs a gateway"), "{refused:?}");

    let fleet = draft(&t, &engineer, &reviewer, "Fleet power", body, "Platform").await;
    let c = config::for_gateway(&mut t.tx().await, t.tenant, gw, Some(north)).await.unwrap();
    assert!(c.flows.iter().all(|f| f.id != fleet), "a gateway does not run what runs here");

    // Its first run only says where it starts: what came before is not news.
    reading(&t, north, a, "power.kw", 500.0, 3, 1).await;
    platform_flows::run(&t.k, &t.system()).await.unwrap();
    let raised = "select status from e_edge_alarm where tenant_id = $1 and flow = $2";
    assert_eq!(one_value(&t, raised, fleet).await, None);

    reading(&t, north, a, "power.kw", 60.0, 2, 2).await;
    reading(&t, south, b, "power.kw", 50.0, 2, 3).await;
    rewind(&t, fleet, 2).await;
    platform_flows::run(&t.k, &t.system()).await.unwrap();
    assert_eq!(one_value(&t, raised, fleet).await.as_deref(), Some("Active"), "60 and 50 from two sites is over 100");
    let alarms = t.get(&engineer, "/alarms", json!({})).await.unwrap();
    assert!(alarms["alarms"].to_string().contains("Fleet over its limit"), "{alarms}");
    let total = "select last_value::text from op_rollup_minute where tenant_id = $1 and device_id = $2 and metric = 'fleet.kw' order by minute desc limit 1";
    assert_eq!(one_value(&t, total, a).await.as_deref(), Some("110"));

    // Run again, nothing has happened: the same minute is not read twice.
    platform_flows::run(&t.k, &t.system()).await.unwrap();
    let count = "select count(*)::text from e_edge_alarm where tenant_id = $1 and flow = $2";
    assert_eq!(one_value(&t, count, fleet).await.as_deref(), Some("1"));

    // A low reading clears it.
    reading(&t, north, a, "power.kw", -400.0, 1, 4).await;
    rewind(&t, fleet, 1).await;
    platform_flows::run(&t.k, &t.system()).await.unwrap();
    assert_eq!(one_value(&t, raised, fleet).await.as_deref(), Some("Cleared"));

    // A new revision starts afresh, from now.
    t.k.run_action(&engineer, "EdgeFlow", fleet, "revise", json!({})).await.unwrap();
    t.api(&engineer, "POST", &format!("/flows/{fleet}/simulate"), json!({"script": []})).await.unwrap();
    t.api(&engineer, "POST", &format!("/flows/{fleet}/simulate"), json!({"script": [{"at": 0, "device": "METER-A", "metric": "power.kw", "value": 1}]}))
        .await
        .unwrap();
    t.k.run_action(&engineer, "EdgeFlow", fleet, "submit_for_review", json!({})).await.unwrap();
    let changed = t.k.update(&engineer, "EdgeFlow", fleet, obj(json!({"runs_at": "Gateway"})), None).await;
    assert!(changed.is_err(), "where it runs is part of what a reviewer saw");
    t.k.run_action(&reviewer, "EdgeFlow", fleet, "approve", json!({})).await.unwrap();
    t.k.run_action(&reviewer, "EdgeFlow", fleet, "activate", json!({})).await.unwrap();
    reading(&t, north, a, "power.kw", 900.0, 1, 5).await;
    rewind(&t, fleet, 3).await;
    platform_flows::run(&t.k, &t.system()).await.unwrap();
    assert_eq!(one_value(&t, raised, fleet).await.as_deref(), Some("Cleared"), "a new revision starts from now, so it has heard none of that");

    // ───── watching a flow run ─────
    let seen = |flow: Uuid, node: &str| {
        let step = Trace { node: node.into(), device: Some("METER-A".into()), value: json!(1), ports: vec!["out".into()] };
        FlowTrace::new(flow, Utc::now(), step)
    };
    let gateway_flow = draft(
        &t,
        &engineer,
        &reviewer,
        "At the gateway",
        json!({"nodes": [{"id": "in", "kind": "metric", "metric": "power.kw"}, {"id": "say", "kind": "log"}], "edges": [{"from": "in", "to": "say"}]}),
        "Gateway",
    )
    .await;
    let c = config::for_gateway(&mut t.tx().await, t.tenant, gw, Some(north)).await.unwrap();
    let spec = c.flows.iter().find(|f| f.id == gateway_flow).unwrap();
    assert!(!spec.debug, "nobody is watching");

    let keep = |traces: Vec<FlowTrace>| {
        let t = &t;
        async move {
            let mut tx = t.tx().await;
            platform_flows::keep_traces(&mut tx, t.tenant, Some(gw), &traces).await.unwrap();
            tx.commit().await.unwrap();
        }
    };
    keep(vec![seen(gateway_flow, "unasked")]).await;
    let path = format!("/flows/{gateway_flow}/traces");
    let got = t.get(&viewer, &path, json!({})).await.unwrap();
    assert_eq!(got["traces"], json!([]), "nothing is kept that nobody asked to see");

    assert!(t.k.run_action(&viewer, "EdgeFlow", gateway_flow, "watch", json!({"minutes": 5})).await.is_err(), "a viewer looks at what others are watching");
    t.k.run_action(&engineer, "EdgeFlow", gateway_flow, "watch", json!({"minutes": 5000})).await.unwrap();
    let until = "select (debug_until - now() < interval '61 minutes')::text from e_edge_flow where tenant_id = $1 and id = $2";
    assert_eq!(one_value(&t, until, gateway_flow).await.as_deref(), Some("true"), "an hour is the most: a person forgets");
    let c = config::for_gateway(&mut t.tx().await, t.tenant, gw, Some(north)).await.unwrap();
    assert!(c.flows.iter().find(|f| f.id == gateway_flow).unwrap().debug, "its gateway is told");

    keep(vec![seen(gateway_flow, "in"), seen(gateway_flow, "say")]).await;
    let got = t.get(&viewer, &path, json!({})).await.unwrap();
    let nodes: Vec<&str> = got["traces"].as_array().unwrap().iter().map(|t| t["node"].as_str().unwrap()).collect();
    assert_eq!(nodes, ["in", "say"]);
    let since = got["traces"][1]["at"].clone();
    assert_eq!(t.get(&viewer, &path, json!({"since": since})).await.unwrap()["traces"], json!([]), "and a page asks only for what is new");

    t.k.run_action(&engineer, "EdgeFlow", gateway_flow, "watch", json!({"minutes": 0})).await.unwrap();
    let c = config::for_gateway(&mut t.tx().await, t.tenant, gw, Some(north)).await.unwrap();
    assert!(!c.flows.iter().find(|f| f.id == gateway_flow).unwrap().debug);

    // What ran is kept, to read and go back to.
    let versions = t.get(&viewer, &format!("/flows/{fleet}/versions"), json!({})).await.unwrap();
    let kept = versions["versions"].as_array().unwrap();
    assert_eq!(kept.iter().map(|v| v["revision"].as_i64().unwrap()).collect::<Vec<_>>(), [2, 1]);
    assert_eq!(kept[0]["by"], "rev@flowing.test");
    assert!(kept[1]["body"].as_str().unwrap().contains("Fleet over its limit"));

    // A long value is not carried about, and it is said that it was long.
    let long = FlowTrace::new(gateway_flow, Utc::now(), Trace { node: "in".into(), device: None, value: json!("x".repeat(5_000)), ports: vec![] });
    assert_eq!(long.step.value, json!({"too_long_to_show": 5002}));
}
