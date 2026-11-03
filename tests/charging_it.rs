//! Charging sessions from what chargers say, in the order a real network delivers it (AT-OPS-009 and 012).
//! Against a real PostgreSQL, through the facts a gateway sends. No charger or meter was involved.

mod support;

use chrono::Utc;
use ferrum_kernel::Error;
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

#[test]
fn a_session_is_what_the_evidence_shows() {
    run(scenario());
}

fn meter(id: &str, at: &str, wh: f64) -> Value {
    json!({"transactionId": id.parse::<i64>().unwrap(), "meterValue": [{"timestamp": at, "sampledValue": [{"value": wh.to_string(), "measurand": "Energy.Active.Import.Register", "unit": "Wh"}]}]})
}

async fn scenario() {
    let t = workspace("charge").await;
    let operator = t.person("op@charge.test", &["Edge Engineer"]).await;
    let viewer = t.person("view@charge.test", &["Edge Viewer"]).await;
    let site = id_of(&t.insert("Site", json!({"site_name": "Depot", "site_code": "depot"})).await);
    t.insert("Device", json!({"device_name": "CP-1", "device_code": "CP-1", "kind": "Machine", "site": site})).await;
    let frame = async |action: &str, said: Value, transaction: Option<i64>| {
        let mut body = json!({"code": "CP-1", "call": "1", "action": action, "said": said});
        if let Some(n) = transaction {
            body["transaction"] = json!(n);
        }
        t.fact(site, "edge.transaction", Utc::now(), body).await;
        t.outbox().await;
    };
    let session = async |transaction: &str| -> Value {
        let all = t.get(&viewer, "/charging", json!({})).await.expect("sessions");
        all["sessions"].as_array().unwrap().iter().find(|s| s["transaction"] == json!(transaction)).cloned().unwrap_or(Value::Null)
    };
    let id_of_session = |s: &Value| s["id"].as_str().unwrap().parse::<Uuid>().unwrap();

    // AT-OPS-009: the charger took the start and said it began. Nothing shows energy flowing.
    let start = |wh: i64| json!({"connectorId": 1, "idTag": "A", "meterStart": wh, "timestamp": "2026-11-02T10:00:00Z"});
    frame("StartTransaction", start(1000), Some(11)).await;
    assert_eq!(session("11").await["state"], json!("started"));
    frame("MeterValues", meter("11", "2026-11-02T10:01:00Z", 1000.0), None).await;
    frame("MeterValues", meter("11", "2026-11-02T10:02:00Z", 1000.0), None).await;
    let flat = session("11").await;
    assert_eq!(flat["state"], json!("started"), "a register that has not moved is not charging: {flat}");
    frame("StopTransaction", json!({"transactionId": 11, "meterStop": 1000, "timestamp": "2026-11-02T10:03:00Z"}), None).await;
    let nothing = session("11").await;
    assert_eq!((&nothing["state"], &nothing["energy_wh"]), (&json!("stopped"), &json!(0)), "{nothing}");

    // Energy, then it is charging.
    frame("StartTransaction", json!({"connectorId": 1, "idTag": "B", "meterStart": 5000, "timestamp": "2026-11-02T11:00:00Z"}), Some(12)).await;
    frame("MeterValues", meter("12", "2026-11-02T11:10:00Z", 5400.0), None).await;
    assert_eq!(session("12").await["state"], json!("charging"));
    let open = id_of_session(&session("12").await);
    let early = t.api(&operator, "POST", &format!("/charging/{open}/settle"), json!({"reference": "INV-1"})).await;
    assert!(matches!(early, Err(Error::Conflict(_))), "still running: {early:?}");

    // Out of order, twice: a late reading, a repeated stop, and the same answer each time.
    let stop = json!({"transactionId": 12, "meterStop": 6000, "timestamp": "2026-11-02T11:30:00Z", "reason": "Local"});
    frame("StopTransaction", stop.clone(), None).await;
    frame("MeterValues", meter("12", "2026-11-02T11:20:00Z", 5700.0), None).await;
    frame("StopTransaction", stop, None).await;
    let done = session("12").await;
    assert_eq!((&done["state"], &done["energy_wh"], &done["flags"]), (&json!("stopped"), &json!(1000), &json!([])), "{done}");
    assert!(matches!(t.api(&viewer, "POST", &format!("/charging/{open}/settle"), json!({"reference": "INV-1"})).await, Err(Error::Forbidden(_))));
    assert!(t.api(&operator, "POST", &format!("/charging/{open}/settle"), json!({"reference": " "})).await.is_err(), "it settles against something");
    let settled = t.api(&operator, "POST", &format!("/charging/{open}/settle"), json!({"reference": "INV-1"})).await.expect("clean evidence settles");
    assert_eq!(settled["energy_wh"], json!(1000.0));
    frame("MeterValues", meter("12", "2026-11-02T11:40:00Z", 9000.0), None).await;
    assert_eq!(session("12").await["energy_wh"], json!(1000), "what is settled stays as it was settled");

    // AT-OPS-012: a reset meter gives no number, and only a person can make one up.
    frame("StartTransaction", json!({"connectorId": 2, "idTag": "C", "meterStart": 9000, "timestamp": "2026-11-02T12:00:00Z"}), Some(13)).await;
    frame("MeterValues", meter("13", "2026-11-02T12:10:00Z", 40.0), None).await;
    frame("StopTransaction", json!({"transactionId": 13, "meterStop": 90, "timestamp": "2026-11-02T12:20:00Z"}), None).await;
    let reset = session("13").await;
    assert_eq!(reset["energy_wh"], Value::Null, "{reset}");
    assert!(reset["flags"].to_string().contains("register_went_backwards"), "{reset}");
    let id = id_of_session(&reset);
    assert!(matches!(t.api(&operator, "POST", &format!("/charging/{id}/settle"), json!({"reference": "INV-2"})).await, Err(Error::Conflict(_))));
    assert!(
        t.api(&operator, "POST", &format!("/charging/{id}/settle"), json!({"reference": "INV-2", "energy_wh": 90})).await.is_err(),
        "a figure of your own comes with why"
    );
    t.api(&operator, "POST", &format!("/charging/{id}/settle"), json!({"reference": "INV-2", "energy_wh": 90, "note": "the meter was replaced mid-session"}))
        .await
        .expect("a person's word");
    let mut tx = t.tx().await;
    let audited: i64 = sqlx::query_scalar("select count(*) from audit_event where action = 'edge.charging.settled'").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(audited, 2);
    drop(tx);

    // Readings that came before any start, because of the order they travelled in.
    frame("MeterValues", meter("14", "2026-11-02T13:10:00Z", 100.0), None).await;
    frame("StopTransaction", json!({"transactionId": 14, "meterStop": 200, "timestamp": "2026-11-02T13:20:00Z"}), None).await;
    let orphan = session("14").await;
    assert!(orphan["flags"].to_string().contains("no_start_heard"), "{orphan}");
    frame("StartTransaction", json!({"connectorId": 1, "idTag": "D", "meterStart": 50, "timestamp": "2026-11-02T13:00:00Z"}), Some(14)).await;
    let whole = session("14").await;
    assert_eq!((&whole["energy_wh"], &whole["flags"], &whole["id_token"]), (&json!(150), &json!([]), &json!("D")), "and it comes out the same: {whole}");

    // 2.0.1 says the same things differently, and its text id is its own.
    let event = |kind: &str, at: &str, wh: i64| {
        json!({"eventType": kind, "timestamp": at, "transactionInfo": {"transactionId": "tx-abc"}, "idToken": {"idToken": "E"},
        "meterValue": [{"timestamp": at, "sampledValue": [{"value": wh, "unitOfMeasure": {"unit": "Wh"}}]}]})
    };
    frame("TransactionEvent", event("Started", "2026-11-02T14:00:00Z", 100), None).await;
    frame("TransactionEvent", event("Updated", "2026-11-02T14:10:00Z", 300), None).await;
    assert_eq!(session("tx-abc").await["state"], json!("charging"));
    frame("TransactionEvent", event("Ended", "2026-11-02T14:20:00Z", 700), None).await;
    let new = session("tx-abc").await;
    assert_eq!((&new["state"], &new["energy_wh"]), (&json!("stopped"), &json!(600)), "{new}");
}
