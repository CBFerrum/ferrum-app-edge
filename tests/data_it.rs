//! A device's numbers over time, and what a dashboard asks for.

mod support;

use chrono::{Duration, DurationRound, Utc};
use ferrum_ops::contract::telemetry::{Class, Measurement, Quality};
use serde_json::json;
use support::*;
use uuid::Uuid;

#[test]
fn numbers_over_time() {
    run(scenario());
}

async fn scenario() {
    let t = workspace("data").await;
    let viewer = t.person("view@data.test", &["Edge Viewer"]).await;
    let outsider = t.person("none@data.test", &[]).await;
    let site = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    let press = id_of(&t.insert("Device", json!({"device_name": "Press", "device_code": "PRESS-1", "kind": "Machine", "site": site})).await);
    let quiet = id_of(&t.insert("Device", json!({"device_name": "Quiet", "device_code": "QUIET-1", "kind": "Machine", "site": site})).await);

    // Five minutes of readings, one of them marked bad, the last a minute ago.
    let now = Utc::now().duration_trunc(Duration::minutes(1)).unwrap();
    let boot = Uuid::now_v7();
    let reading = |seq: i64, metric: &str, value: f64, quality: Quality| Measurement {
        device_id: press,
        metric: metric.into(),
        value,
        unit: Some("C".into()),
        source_time: now - Duration::minutes(6 - seq) + Duration::seconds(5),
        boot,
        seq,
        quality,
        class: Class::Sample,
    };
    let batch = vec![
        reading(1, "temp.c", 10.0, Quality::Good),
        reading(2, "temp.c", 20.0, Quality::Good),
        reading(3, "temp.c", 9000.0, Quality::Bad),
        reading(4, "temp.c", 30.0, Quality::Good),
        reading(5, "temp.c", 40.0, Quality::Uncertain),
        reading(6, "rpm", 1500.0, Quality::Good),
    ];
    ferrum_ops::telemetry::ensure_partitions(&t.k, now).await.unwrap();
    let mut tx = t.tx().await;
    ferrum_ops::telemetry::ingest(&mut tx, t.tenant, site, batch, now).await.unwrap();
    tx.commit().await.unwrap();

    // ───── a short period is drawn from samples ─────
    let path = format!("/devices/{press}/history");
    let short = t
        .get(&viewer, &path, json!({"metrics": "temp.c, rpm", "from": (now - Duration::minutes(10)).to_rfc3339(), "to": now.to_rfc3339(), "points": "10"}))
        .await
        .unwrap();
    assert_eq!(short["bucket_secs"], json!(60), "{short}");
    assert_eq!(short["series"][0]["metric"], json!("temp.c"));
    assert_eq!(short["series"][0]["unit"], json!("C"), "a unit is kept with the samples");
    let points = short["series"][0]["points"].as_array().unwrap();
    assert_eq!(points.len(), 5, "{short}");
    assert_eq!(
        (&points[2]["avg"], &points[2]["bad"], &points[2]["samples"]),
        (&json!(null), &json!(1), &json!(0)),
        "bad is counted, and is not a value: {short}"
    );
    assert_eq!(points[3]["avg"], json!(30.0));
    assert_eq!(points[4]["avg"], json!(40.0), "a doubt is still a reading");
    assert_eq!(short["series"][1]["metric"], json!("rpm"), "in the order asked for");

    // ───── a long one from rollups, with what was marked bad left out ─────
    let long = t
        .get(
            &viewer,
            &path,
            json!({"metrics": "temp.c", "from": (now - Duration::days(2)).to_rfc3339(), "to": (now + Duration::minutes(1)).to_rfc3339(), "points": "10"}),
        )
        .await
        .unwrap();
    assert!(long["bucket_secs"].as_i64().unwrap() > 3600, "{long}");
    let points = long["series"][0]["points"].as_array().unwrap();
    let (samples, max) =
        (points.iter().map(|p| p["samples"].as_i64().unwrap()).sum::<i64>(), points.iter().filter_map(|p| p["max"].as_f64()).fold(f64::MIN, f64::max));
    assert_eq!((samples, max), (4, 40.0), "{long}");

    // ───── what makes no sense is refused, and who may not read does not ─────
    let (from, to) = (now.to_rfc3339(), (now - Duration::hours(1)).to_rfc3339());
    assert!(t.get(&viewer, &path, json!({"metrics": "rpm", "from": from, "to": to})).await.is_err(), "backwards");
    assert!(t.get(&viewer, &path, json!({"from": to, "to": from})).await.is_err(), "no metric");
    assert!(t.get(&viewer, &path, json!({"metrics": "a,b,c,d,e,f,g,h,i"})).await.is_err(), "more than can be told apart");
    assert!(t.get(&viewer, &path, json!({"metrics": "rpm", "from": (now - Duration::days(500)).to_rfc3339()})).await.is_err(), "longer than anything is kept");
    assert!(t.get(&viewer, &path, json!({"metrics": "rpm", "from": "yesterday"})).await.is_err(), "not a time");
    assert!(t.get(&outsider, &path, json!({"metrics": "rpm"})).await.is_err(), "no role here");

    // ───── a dashboard says what each widget needs, and one bad widget stays one bad widget ─────
    let engineer = t.person("eng@data.test", &["Edge Engineer", "Ops Dispatcher"]).await;
    let layout = json!({"last_secs": 1200, "variables": [{"name": "machine", "label": "Machine", "default": press}], "widgets": [
        {"id": "now", "type": "value", "device": "$machine", "metric": "rpm", "x": 0, "y": 0, "w": 3, "h": 3},
        {"id": "also", "type": "gauge", "device": press, "metric": "rpm", "x": 3, "y": 0, "w": 3, "h": 3},
        {"id": "line", "type": "line", "sources": [{"device": press, "metric": "temp.c"}, {"device": "$machine", "metric": "rpm"}], "points": 10, "x": 0, "y": 3, "w": 6, "h": 6},
        {"id": "gone", "type": "value", "device": Uuid::now_v7(), "metric": "rpm"},
        {"id": "said", "type": "state", "device": press, "attribute": "door"},
        {"id": "wrong", "type": "alarms", "device": press},
        {"id": "log", "type": "events", "device": quiet},
        {"id": "fleet", "type": "table", "site": site},
        {"id": "machines", "type": "table", "kind": "Robot"},
        {"id": "note", "type": "text", "text": "Hello"},
        {"id": "start", "type": "button", "device": press, "operation": "fan.start"},
        {"id": "set", "type": "setpoint", "device": press, "attribute": "limit"},
    ]});
    let body = layout.to_string();
    let data = t.api(&viewer, "POST", "/boards/data", json!({"body": body})).await.unwrap();
    assert_eq!(data["now"]["data"].as_array().unwrap().len(), 2, "a variable's default stands in: {data}");
    assert_eq!(data["now"], data["also"], "asked once");
    assert_eq!(data["line"]["data"]["series"].as_array().unwrap().len(), 2, "{data}");
    assert!(data["line"]["data"]["series"][0]["points"].as_array().is_some_and(|p| !p.is_empty()), "{data}");
    assert_eq!(data["gone"]["data"], json!([]), "a device since removed shows nothing and breaks nothing: {data}");
    assert_eq!(data["said"]["data"], json!([]));
    assert_eq!(data["log"]["data"], json!([]));
    assert!(data.get("note").is_none() && data.get("start").is_none(), "they need nothing: {data}");
    let fleet = data["fleet"]["data"].as_array().unwrap();
    assert_eq!(fleet.len(), 2, "{data}");
    assert_eq!(fleet.iter().find(|d| d["code"] == json!("PRESS-1")).unwrap()["latest"]["rpm"]["value"].as_f64(), Some(1500.0), "{data}");
    assert_eq!(data["machines"]["data"], json!([]), "only that kind");
    let chosen = t.api(&viewer, "POST", "/boards/data", json!({"body": body, "vars": {"machine": quiet}})).await.unwrap();
    assert_eq!(chosen["now"]["data"], json!([]), "what the person chose wins: {chosen}");
    assert_eq!(chosen["also"]["data"].as_array().unwrap().len(), 2, "and only where the variable is used");
    assert!(t.api(&outsider, "POST", "/boards/data", json!({"body": body})).await.is_err());

    // ───── what cannot be a dashboard is not saved ─────
    let save = |name: &str, body: String| {
        let (k, engineer, doc) = (t.k.clone(), engineer.clone(), obj(json!({"dashboard_name": name, "body": body})));
        async move { k.insert(&engineer, "EdgeDashboard", doc).await }
    };
    assert!(save("broken", "{".into()).await.is_err(), "not JSON");
    assert!(save("kinds", json!({"widgets": [{"id": "a", "type": "fireworks"}]}).to_string()).await.is_err(), "no such widget");
    assert!(save("twins", json!({"widgets": [{"id": "a", "type": "text"}, {"id": "a", "type": "text"}]}).to_string()).await.is_err(), "two of one name");
    let many: Vec<_> = (0..101).map(|i| json!({"id": format!("w{i}"), "type": "text"})).collect();
    assert!(save("many", json!({"widgets": many}).to_string()).await.is_err(), "too many");
    assert!(t.k.insert(&viewer, "EdgeDashboard", obj(json!({"dashboard_name": "mine", "body": body}))).await.is_err(), "a viewer looks, and does not lay out");
    let board = id_of(&save("Floor", body.clone()).await.unwrap());

    // ───── a link shows what the dashboard shows, and no more ─────
    assert!(t.api(&viewer, "POST", &format!("/boards/{board}/share"), json!({})).await.is_err(), "not a viewer's to give out");
    let token = t.api(&engineer, "POST", &format!("/boards/{board}/share"), json!({})).await.unwrap()["token"].as_str().unwrap().to_string();
    let public = t.visit(&format!("/shared/{token}")).await;
    assert_eq!((public.0, public.1["name"].as_str()), (200, Some("Floor")), "{public:?}");
    assert_eq!(public.1["data"]["line"]["data"]["series"].as_array().unwrap().len(), 2, "{:?}", public.1);
    let shown: Vec<&str> = public.1["board"]["widgets"].as_array().unwrap().iter().filter_map(|w| w["id"].as_str()).collect();
    assert!(!shown.contains(&"start") && !shown.contains(&"set"), "a visitor changes nothing: {shown:?}");
    assert!(public.1["board"].get("shared_hash").is_none());
    assert_eq!(t.visit("/shared/not-the-token").await.0, 404);
    let second = t.api(&engineer, "POST", &format!("/boards/{board}/share"), json!({})).await.unwrap()["token"].as_str().unwrap().to_string();
    assert_eq!(t.visit(&format!("/shared/{token}")).await.0, 404, "a new link ends the old one");
    assert_eq!(t.visit(&format!("/shared/{second}")).await.0, 200);
    t.api(&engineer, "DELETE", &format!("/boards/{board}/share"), json!({})).await.unwrap();
    assert_eq!(t.visit(&format!("/shared/{second}")).await.0, 404, "and it can be ended");
}
