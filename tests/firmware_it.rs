//! New firmware reaching devices: only those it may, a few at a time, and stopping when they fail.

mod support;

use ferrum_app_edge::firmware;
use ferrum_ops::contract::Lifecycle;
use ferrum_ops::devices;
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

#[test]
fn firmware_reaches_devices_with_care() {
    run(scenario());
}

const HASH: &str = "ab54d286c0ba8f5cdb7b5e7dd6f9b63b6a6a3b6a6a3b6a6a3b6a6a3b6a6a3b6a";

async fn device(t: &T, code: &str, kind: &str, (site, gateway): (Uuid, Uuid), definition: Uuid, tags: &str) -> Uuid {
    let doc =
        json!({"device_name": code, "device_code": code, "kind": kind, "site": site, "gateway": gateway, "edge_definition": definition, "edge_tags": tags});
    let id = id_of(&t.insert("Device", doc).await);
    t.k.update(&t.admin, "Device", id, obj(json!({"operations": [{"operation": "firmware.update"}]})), None).await.unwrap();
    for to in [Lifecycle::Claimed, Lifecycle::Provisioned, Lifecycle::Commissioned, Lifecycle::Active] {
        devices::set_lifecycle(&t.k, &t.admin, id, to, "in service").await.unwrap();
    }
    id
}

async fn rows(t: &T, rollout: Uuid) -> Vec<(String, String)> {
    let mut tx = t.tx().await;
    let rows = sqlx::query_as(
        "select d.device_code, x.status from edge_rollout_device x join e_device d on d.tenant_id = x.tenant_id and d.id = x.device_id \
         where x.tenant_id = $1 and x.rollout_id = $2 order by 1",
    )
    .bind(t.tenant)
    .bind(rollout)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    rows
}

async fn status(t: &T, rollout: Uuid) -> String {
    let doc = t.k.get(&t.admin, "EdgeRollout", rollout, false).await.unwrap();
    doc["status"].as_str().unwrap().to_string()
}

/// As if the device had finished, or not.
async fn ends(t: &T, device: &str, state: &str) {
    let mut tx = t.tx().await;
    sqlx::query(
        "update op_command c set state = $3 from e_device d where c.tenant_id = $1 and d.tenant_id = $1 and d.device_code = $2 and c.target_id = d.id \
         and c.operation = 'firmware.update' and c.state not in ('Completed', 'Failed')",
    )
    .bind(t.tenant)
    .bind(device)
    .bind(state)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn scenario() {
    let t = workspace("updating").await;
    let engineer = t.person("eng@updating.test", &["Edge Engineer"]).await;
    let viewer = t.person("view@updating.test", &["Edge Viewer"]).await;
    let site = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    let (gw, _gateway) = gateway(&t, "GW-N", site, 1).await;
    let place = (site, gw);
    let closed = id_of(
        &t.insert("DeviceDefinition", json!({"definition_name": "closed", "body": "name: closed\nconnect: listen+mqtt://0.0.0.0:1\napi: device\n"})).await,
    );
    let open = id_of(
        &t.insert(
            "DeviceDefinition",
            json!({"definition_name": "open", "body": "name: open\nconnect: listen+mqtt://0.0.0.0:1\napi: device\nfirmware: true\n"}),
        )
        .await,
    );

    let firmware_doc = json!({"firmware_name": "sensor-fw", "firmware_version": "2.0", "definition": closed, "url": "https://files.test/fw.bin", "sha256": HASH, "size": 1024});
    let refused = t.k.insert(&engineer, "EdgeFirmware", obj(firmware_doc.clone())).await;
    assert!(format!("{refused:?}").contains("firmware: true"), "a definition has to allow it: {refused:?}");
    let mut wrong = firmware_doc.clone();
    wrong["definition"] = json!(open);
    wrong["sha256"] = json!("abc");
    assert!(format!("{:?}", t.k.insert(&engineer, "EdgeFirmware", obj(wrong)).await).contains("SHA-256"));
    let mut good = firmware_doc;
    good["definition"] = json!(open);
    let fw = id_of(&t.k.insert(&engineer, "EdgeFirmware", obj(good)).await.unwrap());

    for code in ["S-1", "S-2", "S-3", "S-4", "S-5"] {
        device(&t, code, "Sensor", place, open, "line-1").await;
    }
    device(&t, "S-OTHER", "Sensor", place, open, "line-2").await;
    device(&t, "ROBOT-1", "Robot", place, open, "line-1").await;
    device(&t, "PLC-1", "Controller", place, open, "line-1").await;
    let current = device(&t, "S-CURRENT", "Sensor", place, open, "line-1").await;
    let mut tx = t.tx().await;
    sqlx::query("insert into edge_attribute (tenant_id, device_id, key, value, reported_at) values ($1, $2, 'current_fw_version', '\"2.0\"', now())")
        .bind(t.tenant)
        .bind(current)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let rollout = id_of(
        &t.k.insert(&engineer, "EdgeRollout", obj(json!({"rollout_name": "Line 1 to 2.0", "firmware": fw, "tag": "line-1", "batch": 2, "stop_after": 2})))
            .await
            .unwrap(),
    );
    firmware::advance(&t.k, &t.system()).await.unwrap();
    assert!(rows(&t, rollout).await.is_empty(), "nothing moves until a person says");
    assert!(t.k.run_action(&viewer, "EdgeRollout", rollout, "start", json!({})).await.is_err());
    t.k.run_action(&engineer, "EdgeRollout", rollout, "start", json!({})).await.unwrap();

    // Two at a time, of those it may touch.
    firmware::advance(&t.k, &t.system()).await.unwrap();
    assert_eq!(rows(&t, rollout).await, [("S-1".to_string(), "Sent".to_string()), ("S-2".to_string(), "Sent".to_string())]);
    firmware::advance(&t.k, &t.system()).await.unwrap();
    assert_eq!(rows(&t, rollout).await.len(), 2, "no more until one of those is finished");

    let mut tx = t.tx().await;
    let (payload, issuer): (Value, String) =
        sqlx::query_as("select payload, issued_kind from op_command where tenant_id = $1 and operation = 'firmware.update' limit 1")
            .bind(t.tenant)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        (payload["version"].as_str(), payload["sha256"].as_str(), issuer.as_str()),
        (Some("2.0"), Some(HASH), "engine"),
        "through the ledger, on a person's behalf"
    );

    ends(&t, "S-1", "Completed").await;
    firmware::advance(&t.k, &t.system()).await.unwrap();
    assert_eq!(rows(&t, rollout).await.iter().filter(|(_, s)| s == "Sent").count(), 2, "one done lets one more start");

    ends(&t, "S-2", "Failed").await;
    ends(&t, "S-3", "Failed").await;
    firmware::advance(&t.k, &t.system()).await.unwrap();
    assert_eq!(status(&t, rollout).await, "Paused", "two failures is what it was told to stop at");
    let doc = t.k.get(&t.admin, "EdgeRollout", rollout, false).await.unwrap();
    assert!(doc["note"].as_str().unwrap().contains("2 devices failed"));
    let before = rows(&t, rollout).await.len();
    firmware::advance(&t.k, &t.system()).await.unwrap();
    assert_eq!(rows(&t, rollout).await.len(), before, "paused means paused");

    // A person looks, and carries on: only the two failures it has seen stop it now.
    let mut tx = t.tx().await;
    sqlx::query("delete from edge_rollout_device where tenant_id = $1 and rollout_id = $2 and status = 'Failed'")
        .bind(t.tenant)
        .bind(rollout)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("insert into edge_attribute (tenant_id, device_id, key, value, reported_at) select $1, id, 'current_fw_version', '\"2.0\"', now() from e_device where tenant_id = $1 and device_code in ('S-2', 'S-3')").bind(t.tenant).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    t.k.run_action(&engineer, "EdgeRollout", rollout, "start", json!({})).await.unwrap();
    for code in ["S-4", "S-5"] {
        firmware::advance(&t.k, &t.system()).await.unwrap();
        ends(&t, code, "Completed").await;
    }
    firmware::advance(&t.k, &t.system()).await.unwrap();
    firmware::advance(&t.k, &t.system()).await.unwrap();
    assert_eq!(status(&t, rollout).await, "Done");
    let reached = rows(&t, rollout).await;
    assert!(
        reached.iter().all(|(code, _)| code.starts_with("S-") && code != "S-OTHER" && code != "S-CURRENT"),
        "never a robot or a controller, another tag or what is up to date: {reached:?}"
    );

    let seen = t.get(&viewer, &format!("/rollouts/{rollout}"), json!({})).await.unwrap();
    assert_eq!(seen["devices"].as_array().unwrap().len(), reached.len());

    // A share of them is how a change is tried.
    let tried =
        id_of(&t.k.insert(&engineer, "EdgeRollout", obj(json!({"rollout_name": "A taste", "firmware": fw, "percent": 20, "batch": 50}))).await.unwrap());
    t.k.run_action(&engineer, "EdgeRollout", tried, "start", json!({})).await.unwrap();
    firmware::advance(&t.k, &t.system()).await.unwrap();
    let taste = rows(&t, tried).await.len();
    assert_eq!(taste, 1, "{taste} of 5 still to go: a fifth, rounded up");
}
