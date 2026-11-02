//! Many devices at once: finding them, adding them, asking them, and how the fleet stands.

mod support;

use chrono::Utc;
use ferrum_ops::contract::telemetry::{Class, Quality};
use ferrum_ops::contract::{Lifecycle, Measurement};
use ferrum_ops::{devices, telemetry};
use serde_json::{Value, json};
use sha2::Digest;
use support::*;
use uuid::Uuid;

#[test]
fn a_fleet_at_once() {
    run(scenario());
}

fn codes(listing: &Value) -> Vec<String> {
    listing["devices"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

async fn scenario() {
    let t = workspace("fleet").await;
    let engineer = t.person("eng@fleet.test", &["Edge Engineer", "Ops Manager"]).await;
    let viewer = t.person("view@fleet.test", &["Edge Viewer"]).await;
    let north = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    gateway(&t, "GW-N", north, 1).await;
    let definition = id_of(&t.insert("DeviceDefinition", json!({"definition_name": "press", "body": "name: press\nconnect: tcp://x:1\n"})).await);
    let hall = id_of(&t.insert("EdgeAsset", json!({"asset_name": "Hall", "asset_type": "Building", "site": north})).await);
    let room = id_of(&t.insert("EdgeAsset", json!({"asset_name": "Cold room", "asset_type": "Room", "parent": hall})).await);

    // ───── many at once, each row on its own ─────
    let csv = "code,name,kind,site,gateway,definition,tags,address\n\
               PRESS-1,\"Press, big\",Machine,north,GW-N,press,line-1;hot,tcp://10.0.0.1:9100\n\
               PRESS-2,Press two,Machine,north,GW-N,press,line-1,\n\
               SENSOR-1,Sensor,,north,,,line-2,\n\
               PRESS-1,Twice,Machine,,,,,\n\
               ,No code,,,,,,\n\
               PRESS-3,Nowhere,Machine,mars,,,,\n";
    let done = t.api(&engineer, "POST", "/devices/import", json!({"csv": csv, "tokens": true})).await.unwrap();
    assert_eq!(done["created"], 3, "{done}");
    let failed = done["failed"].as_array().unwrap();
    assert_eq!(failed.iter().map(|f| f["line"].as_i64().unwrap()).collect::<Vec<_>>(), [5, 6, 7], "{done}");
    assert!(failed[2]["why"].as_str().unwrap().contains("mars"), "it says what it could not find");
    let token = done["tokens"][0]["token"].as_str().unwrap();
    assert_eq!(done["tokens"].as_array().unwrap().len(), 3);
    assert!(t.api(&viewer, "POST", "/devices/import", json!({"csv": csv})).await.is_err(), "adding devices is an engineer's");
    assert!(t.api(&engineer, "POST", "/devices/import", json!({"csv": "name\nx"})).await.is_err(), "it has to say what its columns are");

    // A token given here is a token that works.
    let mut headers = serde_json::Map::new();
    headers.insert("x-ferrum-device-token".into(), json!(token));
    let site = t.k.hooks.site("edge").unwrap().clone();
    let answer = site.handle(&t.k, request(&t, None, "POST", "/ingest", headers, json!({"values": {"temp.c": 21}}).to_string().into_bytes())).await.unwrap();
    assert_eq!(answer.status, 200, "{}", String::from_utf8_lossy(&answer.body));

    // ───── finding them ─────
    let tagged = t.get(&viewer, "/devices", json!({"tag": "line-1"})).await.unwrap();
    assert_eq!(codes(&tagged), ["PRESS-1", "PRESS-2"]);
    assert_eq!(codes(&t.get(&viewer, "/devices", json!({"tag": "HOT"})).await.unwrap()), ["PRESS-1"], "a tag is one of the list, in any case");
    assert_eq!(codes(&t.get(&viewer, "/devices", json!({"tag": "line"})).await.unwrap()), Vec::<String>::new(), "and all of it");
    assert_eq!(codes(&t.get(&viewer, "/devices", json!({"definition": definition.to_string()})).await.unwrap()), ["PRESS-1", "PRESS-2"]);
    assert_eq!(codes(&t.get(&viewer, "/devices", json!({"q": "sensor"})).await.unwrap()), ["SENSOR-1"]);

    let press1 = press(&t, "PRESS-1").await;
    let sensor = press(&t, "SENSOR-1").await;
    t.k.update(&engineer, "Device", sensor, obj(json!({"edge_asset": room})), None).await.unwrap();
    assert_eq!(codes(&t.get(&viewer, "/devices", json!({"asset": hall.to_string()})).await.unwrap()), ["SENSOR-1"], "an asset takes in what is inside it");

    let heard = t.get(&viewer, "/devices", json!({"q": "PRESS-1"})).await.unwrap();
    assert!(heard["devices"][0]["last_seen"].is_string(), "it was just heard: {heard}");
    let silent = t.get(&viewer, "/devices", json!({"q": "PRESS-2"})).await.unwrap();
    assert_eq!(silent["devices"][0]["last_seen"], Value::Null);

    // ───── asking them ─────
    for code in ["PRESS-1", "PRESS-2"] {
        let id = press(&t, code).await;
        devices::set_lifecycle(&t.k, &t.admin, id, Lifecycle::Claimed, "ours").await.unwrap();
        devices::set_lifecycle(&t.k, &t.admin, id, Lifecycle::Provisioned, "reachable").await.unwrap();
    }
    let nothing = t.api(&engineer, "POST", "/fleet/command", json!({"select": {}, "operation": "fan.start"})).await;
    assert!(format!("{nothing:?}").contains("say which devices"), "nothing chosen is not everything: {nothing:?}");
    let refused = t.api(&viewer, "POST", "/fleet/command", json!({"select": {"tag": "line-1"}, "operation": "fan.start"})).await;
    assert!(refused.is_err(), "a viewer asks nothing");
    let asked =
        t.api(&engineer, "POST", "/fleet/command", json!({"select": {"tag": "line-1"}, "operation": "fan.start", "payload": {"speed": 2}})).await.unwrap();
    let outcomes = asked["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 2, "{asked}");
    assert!(outcomes.iter().all(|o| o["refused"].is_string() || o["command"].is_string()), "each says how it went: {asked}");

    let set = t.api(&engineer, "POST", "/fleet/desired", json!({"select": {"ids": [press1]}, "key": "limit", "value": 80})).await.unwrap();
    assert!(set["outcomes"][0]["refused"].as_str().unwrap().contains("takes no work"), "{set}");
    let wanted = "select desired from edge_attribute where tenant_id = $1 and device_id = $2 and key = 'limit'";
    let mut tx = t.tx().await;
    let kept: Option<Value> = sqlx::query_scalar(wanted).bind(t.tenant).bind(press1).fetch_optional(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(kept, None, "what could not be asked for is not left standing");
    t.k.update(&engineer, "Device", press1, obj(json!({"operations": [{"operation": "fan.start"}, {"operation": "set.limit"}]})), None).await.unwrap();
    for to in [Lifecycle::Commissioned, Lifecycle::Active] {
        devices::set_lifecycle(&t.k, &t.admin, press1, to, "in service").await.unwrap();
    }
    let set = t.api(&engineer, "POST", "/fleet/desired", json!({"select": {"ids": [press1]}, "key": "limit", "value": 80})).await.unwrap();
    assert!(set["outcomes"][0]["command"].is_string(), "{set}");
    let asked = t.api(&engineer, "POST", "/fleet/command", json!({"select": {"tag": "line-1"}, "operation": "fan.start"})).await.unwrap();
    let states: Vec<bool> = asked["outcomes"].as_array().unwrap().iter().map(|o| o["command"].is_string()).collect();
    assert_eq!(states, [true, false], "one in service is asked and one that is not says why: {asked}");
    let mut tx = t.tx().await;
    let kept: Option<Value> = sqlx::query_scalar(wanted).bind(t.tenant).bind(press1).fetch_optional(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(kept, Some(json!(80)));

    // ───── by the clock ─────
    let every = json!({"schedule_name": "Wipe", "every_minutes": 5, "operation": "fan.start", "tag": "line-1"});
    let schedule = id_of(&t.k.insert(&engineer, "EdgeSchedule", obj(every.clone())).await.unwrap());
    let both =
        t.k.insert(&engineer, "EdgeSchedule", obj(json!({"schedule_name": "Both", "every_minutes": 5, "at_time": "08:00", "operation": "x", "tag": "a"})))
            .await;
    assert!(format!("{both:?}").contains("not both"), "{both:?}");
    let aimless = t.k.insert(&engineer, "EdgeSchedule", obj(json!({"schedule_name": "Everything", "every_minutes": 5, "operation": "x"}))).await;
    assert!(format!("{aimless:?}").contains("which devices"), "{aimless:?}");
    ferrum_app_edge::schedules::run(&t.k, &t.system()).await.unwrap();
    let doc = t.k.get(&t.admin, "EdgeSchedule", schedule, false).await.unwrap();
    assert!(doc["last_result"].as_str().unwrap().contains("1 asked"), "{doc:?}");
    let mut tx = t.tx().await;
    let (state, kind): (String, String) = sqlx::query_as("select state, issued_kind from op_command where tenant_id = $1 and workflow_id = $2")
        .bind(t.tenant)
        .bind(format!("schedule:{schedule}"))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!((state.as_str(), kind.as_str()), ("Proposed", "automation"), "with no envelope, it waits for a person");
    ferrum_app_edge::schedules::run(&t.k, &t.system()).await.unwrap();
    let mut tx = t.tx().await;
    let times: i64 = sqlx::query_scalar("select count(*) from op_command where tenant_id = $1 and workflow_id = $2")
        .bind(t.tenant)
        .bind(format!("schedule:{schedule}"))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(times, 1, "not again until it is due");

    // ───── a device claimed by whoever holds it ─────
    let claimed = id_of(&t.insert("Device", json!({"device_name": "New", "device_code": "NEW-1", "kind": "Sensor", "site": north})).await);
    let event = json!({"device": claimed, "name": "claim", "value": {"secret_hash": hex::encode(sha2::Sha256::digest(b"under-the-lid")), "for_ms": 60_000}});
    t.fact(north, "edge.event", Utc::now(), event).await;
    let attempt = async |code: &str, secret: &str| t.api(&engineer, "POST", "/devices/claim", json!({"code": code, "secret": secret})).await;
    assert!(attempt("NEW-1", "guess").await.is_err());
    assert!(attempt("NOBODY", "under-the-lid").await.is_err());
    assert!(t.api(&viewer, "POST", "/devices/claim", json!({"code": "NEW-1", "secret": "under-the-lid"})).await.is_err());
    attempt("NEW-1", "under-the-lid").await.expect("claimed");
    assert_eq!(t.k.get(&t.admin, "Device", claimed, false).await.unwrap()["lifecycle"], "Claimed");
    let old = json!({"device": claimed, "name": "claim", "value": {"secret_hash": hex::encode(sha2::Sha256::digest(b"stale")), "for_ms": 1_000}});
    t.fact(north, "edge.event", Utc::now() - chrono::Duration::minutes(5), old).await;
    assert!(attempt("NEW-1", "stale").await.is_err(), "a device says for how long");

    // ───── a solution in one file ─────
    let listed = t.get(&viewer, "/solutions", json!({})).await.unwrap();
    assert_eq!(listed["solutions"].as_array().unwrap().len(), 2);
    assert!(t.api(&viewer, "POST", "/solutions/install", json!({"name": "Cold rooms"})).await.is_err());
    let done = t.api(&engineer, "POST", "/solutions/install", json!({"name": "Cold rooms"})).await.unwrap();
    assert!(done["items"].as_array().unwrap().iter().all(|i| i["added"] == true), "{done}");
    let again = t.api(&engineer, "POST", "/solutions/install", json!({"name": "Cold rooms"})).await.unwrap();
    assert!(again["items"].as_array().unwrap().iter().any(|i| i["added"] == false), "a name already taken is left alone: {again}");
    let mut tx = t.tx().await;
    let (status, live): (String, Option<bool>) =
        sqlx::query_as("select status, live from e_edge_flow where tenant_id = $1 and flow_name = 'Cold room door left open'")
            .bind(t.tenant)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!((status.as_str(), live), ("Draft", Some(false)), "a flow from a file still goes through review");
    let out = t.get(&engineer, "/solutions/export", json!({"name": "Mine"})).await.unwrap();
    assert_eq!(out["name"], "Mine");
    assert!(out["flows"].as_array().unwrap().iter().any(|f| f["name"] == "Cold room door left open"));
    assert!(out["rules"].as_array().unwrap().iter().any(|r| r["rule_name"] == "Cold room too warm"), "{out}");
    let taken = t.api(&engineer, "POST", "/solutions/install", json!({"bundle": out})).await.unwrap();
    assert!(taken["items"].is_array(), "what comes out goes back in");

    // ───── on paper ─────
    let reports = ferrum_app_edge::reports();
    let names: Vec<String> = reports[0].catalog().iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["edge_availability", "edge_alarm_history", "edge_readings"]);
    let run_report = async |name: &str, params: &[(&str, String)]| {
        let params = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        reports[0].run(&t.k, &engineer, name, &params).await
    };
    let alarms = run_report("edge_alarm_history", &[]).await.unwrap();
    assert!(alarms["rows"].is_array());
    assert!(run_report("edge_readings", &[]).await.is_err(), "it has to know which");
    assert!(run_report("nothing", &[]).await.is_err());

    // ───── as others describe a device ─────
    let twin = t.get(&viewer, &format!("/devices/{press1}/twin"), json!({})).await.unwrap();
    assert_eq!(twin["attributes"]["limit"]["desired"], 80);
    assert_eq!(twin["attributes"]["limit"]["in_step"], false, "it has been asked, and has not said so yet");
    assert_eq!(twin["operations"], json!(["fan.start", "set.limit"]));
    let td = t.get(&viewer, &format!("/devices/{press1}/td"), json!({})).await.unwrap();
    assert_eq!(td["@context"][0], "https://www.w3.org/2022/wot/td/v1.1");
    assert!(td["properties"]["limit"]["forms"].is_array(), "{td}");
    assert!(td["actions"]["fan.start"]["forms"][0]["href"].is_string());
    let shell = t.get(&viewer, &format!("/devices/{press1}/aas"), json!({})).await.unwrap();
    assert_eq!(shell["assetAdministrationShells"][0]["idShort"], "PRESS_1");
    assert_eq!(shell["submodels"][0]["idShort"], "OperationalData");

    // ───── how it stands ─────
    let now = Utc::now();
    telemetry::ensure_partitions(&t.k, now).await.unwrap();
    let m = Measurement {
        device_id: press1,
        metric: "temp.c".into(),
        value: 1.0,
        unit: None,
        source_time: now,
        boot: press1,
        seq: 1,
        quality: Quality::Good,
        class: Class::Sample,
    };
    let mut tx = t.tx().await;
    telemetry::ingest(&mut tx, t.tenant, north, vec![m], now).await.unwrap();
    tx.commit().await.unwrap();
    let health = t.get(&viewer, "/fleet", json!({})).await.unwrap();
    let press_row = health["definitions"].as_array().unwrap().iter().find(|d| d["name"] == "press").unwrap().clone();
    assert_eq!((press_row["devices"].as_i64(), press_row["heard"].as_i64()), (Some(2), Some(1)), "{health}");
    let site = health["sites"].as_array().unwrap().iter().find(|d| d["name"] == "North").unwrap().clone();
    assert_eq!(site["devices"], 4, "{health}");

    let today = Utc::now().date_naive().to_string();
    let hours = run_report("edge_readings", &[("device", press1.to_string()), ("metric", "temp.c".to_string()), ("from_date", today)]).await.unwrap();
    assert_eq!(hours["rows"].as_array().unwrap().iter().map(|r| r["samples"].as_i64().unwrap()).sum::<i64>(), 2, "{hours}");
    let present = run_report("edge_availability", &[("site", north.to_string())]).await.unwrap();
    let row = present["rows"].as_array().unwrap().iter().find(|r| r["device"] == "PRESS-1").unwrap().clone();
    assert!(row["minutes"].as_i64().unwrap() >= 1, "{present}");

    let seen = t.get(&viewer, &format!("/devices/{press1}/availability"), json!({"days": "1"})).await.unwrap();
    assert_eq!(seen["days"].as_array().unwrap().len(), 1, "{seen}");
    assert!(seen["percent"].as_f64().unwrap() > 0.0, "{seen}");
}

async fn press(t: &T, code: &str) -> Uuid {
    let mut tx = t.tx().await;
    let id =
        sqlx::query_scalar("select id from e_device where tenant_id = $1 and device_code = $2").bind(t.tenant).bind(code).fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    id
}
