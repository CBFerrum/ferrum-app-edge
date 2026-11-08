//! A business-critical flow's episodes on the platform (Ferrum's Logic Platform, LRP-03): one condition
//! for each episode however its facts arrive (twice, late, out of order, reopened), each reading kept
//! once, each turn published once, and a process the workspace runs started once per episode.

mod support;

use chrono::{DateTime, TimeDelta, Utc};
use ferrum_ops::contract::Fact;
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

#[test]
fn an_episode_is_one_condition_and_one_case() {
    run(scenario());
}

async fn send(t: &T, site: Uuid, id: Uuid, at: DateTime<Utc>, body: Value) {
    let mut tx = t.tx().await;
    ferrum_ops::sync::apply_fact(&mut tx, t.tenant, site, &Fact { fact_id: id, kind: "edge.condition".into(), at_source: at, actor: None, body })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    t.outbox().await;
}

fn said(flow: Uuid, device: Uuid, episode: &str, phase: &str, at: DateTime<Utc>, value: f64) -> Value {
    json!({"flow": flow, "node": "hot", "device": device, "episode": episode, "phase": phase, "title": "Oven too hot", "at": at, "value": value, "peak": value, "quality": "good", "raw": (value + 40.0) * 10.0, "unit": "°C"})
}

async fn condition(t: &T, episode: &str) -> Option<(Uuid, String, i64, Option<f64>, bool)> {
    let mut tx = t.tx().await;
    let row = sqlx::query_as("select id, status, evidence_count, last_value::float8, stale from e_edge_condition where tenant_id = $1 and episode = $2")
        .bind(t.tenant)
        .bind(episode)
        .fetch_optional(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    row
}

async fn published(t: &T, topic: &str) -> i64 {
    let mut tx = t.tx().await;
    let n = sqlx::query_scalar("select count(*) from outbox where tenant_id = $1 and topic = $2").bind(t.tenant).bind(topic).fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    n
}

async fn runs(t: &T, key: &str) -> i64 {
    let mut tx = t.tx().await;
    let n = sqlx::query_scalar("select count(*) from logic_run where tenant_id = $1 and business_key = $2")
        .bind(t.tenant)
        .bind(key)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    n
}

/// What the workspace does about a condition: someone looks, then it waits for the condition to clear.
fn respond() -> String {
    json!({
        "schema_version": "ferrum.solution/1", "kind": "Solution", "id": "acme.respond", "version": "1.0.0", "label": "Respond to conditions", "requires": [],
        "processes": [{"id": "respond", "label": "Respond to a condition", "trigger": {"event": "edge.condition_opened", "business_key": "trigger.episode"},
            "entry": "look", "max_steps": 20, "nodes": [
            {"type": "human_task", "id": "look", "label": "Look at the condition", "role": "Edge Engineer", "material": "trigger", "timeout": "P1D",
             "next": "until_clear", "on_reject": "done", "on_timeout": "attention"},
            {"type": "wait_event", "id": "until_clear", "label": "Until it clears", "event": "edge.condition_closed", "correlation": "trigger.episode",
             "timeout": "P7D", "next": "done", "on_timeout": "attention"},
            {"type": "end", "id": "done", "label": "Dealt with", "outcome": "completed"},
            {"type": "suspend", "id": "attention", "label": "Needs attention", "reason": "Nobody looked, or it never cleared."}
        ]}]
    })
    .to_string()
}

async fn scenario() {
    // An installation with a durable process runtime (the Full Process profile).
    let t = workspace_with("conditions", ferrum_kernel::KernelSettings { process_runtime: true, ..Default::default() }).await;
    // The process that answers conditions: kept by one administrator, reviewed and set running by another.
    let second = t.k.create_user(&t.admin, "b@conditions.test", "Second", Some("Correct-Horse-9"), &[], true).await.unwrap();
    let second = t.k.load_ctx(t.tenant, second, None, "it".into()).await.unwrap();
    let saved = t.k.save_release(&t.admin, &respond()).await.expect("release");
    let release: Uuid = saved["id"].as_str().unwrap().parse().unwrap();
    t.k.review_release_consenting(&second, release, saved["artifact_digest"].as_str().unwrap(), &[], true, "").await.unwrap();
    t.k.activate_release(&second, release, None).await.unwrap();

    let site = id_of(&t.insert("Site", json!({"site_name": "Bakery", "site_code": "bakery"})).await);
    let oven = id_of(&t.insert("Device", json!({"device_name": "Oven", "device_code": "OVEN-1", "kind": "Machine", "site": site})).await);
    let flow = id_of(&t.insert("EdgeFlow", json!({"flow_name": "Ovens", "body": "{}"})).await);
    let episode = "7f3c9a2b4d5e6f708192a3b4c5d6e7f8";
    let t0 = Utc::now() - TimeDelta::minutes(30);
    let at = |s: i64| t0 + TimeDelta::seconds(s);

    // Opened, and the same fact delivered again.
    let opened = Uuid::now_v7();
    send(&t, site, opened, at(30), said(flow, oven, episode, "opened", at(30), 85.0)).await;
    send(&t, site, opened, at(30), said(flow, oven, episode, "opened", at(30), 85.0)).await;
    let (id, status, count, last, _) = condition(&t, episode).await.expect("one condition");
    assert_eq!((status.as_str(), count, last), ("Open", 1, Some(85.0)));
    assert_eq!(published(&t, "edge.condition_opened").await, 1);
    assert_eq!(runs(&t, episode).await, 1, "one case for the episode");

    // Evidence: three readings, one delivered twice, one late (seen before the newest).
    send(&t, site, Uuid::now_v7(), at(40), said(flow, oven, episode, "evidence", at(40), 90.0)).await;
    let twice = Uuid::now_v7();
    send(&t, site, twice, at(50), said(flow, oven, episode, "evidence", at(50), 92.0)).await;
    send(&t, site, twice, at(50), said(flow, oven, episode, "evidence", at(50), 92.0)).await;
    send(&t, site, Uuid::now_v7(), at(45), said(flow, oven, episode, "evidence", at(45), 70.0)).await;
    let (_, _, count, last, _) = condition(&t, episode).await.unwrap();
    assert_eq!((count, last), (4, Some(92.0)), "each reading once, and a late one does not replace the newest");
    let mut tx = t.tx().await;
    let kept: i64 = sqlx::query_scalar("select count(*) from edge_condition_evidence where tenant_id = $1 and condition_id = $2")
        .bind(t.tenant)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(kept, 4);

    // Gone quiet: still open.
    let mut quiet = said(flow, oven, episode, "stale", at(55), 0.0);
    (quiet["value"], quiet["quality"]) = (Value::Null, json!("uncertain"));
    send(&t, site, Uuid::now_v7(), at(200), quiet).await;
    let (_, status, _, _, stale) = condition(&t, episode).await.unwrap();
    assert_eq!((status.as_str(), stale), ("Open", true), "silence is not recovery");

    // Cleared, said twice: closed once, published once.
    let closed = Uuid::now_v7();
    send(&t, site, closed, at(300), said(flow, oven, episode, "closed", at(300), 70.0)).await;
    send(&t, site, closed, at(300), said(flow, oven, episode, "closed", at(300), 70.0)).await;
    assert_eq!(condition(&t, episode).await.unwrap().1, "Closed");
    assert_eq!(published(&t, "edge.condition_closed").await, 1);

    // Held again soon after: the same episode, reopened. Still one case.
    send(&t, site, Uuid::now_v7(), at(400), said(flow, oven, episode, "reopened", at(400), 88.0)).await;
    assert_eq!(condition(&t, episode).await.unwrap().1, "Open");
    assert_eq!(published(&t, "edge.condition_reopened").await, 1);
    assert_eq!(runs(&t, episode).await, 1, "a reopened episode is the same case");

    // Facts out of order: another episode heard first by its closing. It is made, says it opened and
    // closed, and gets a case of its own.
    let other = "00112233445566778899aabbccddeeff";
    send(&t, site, Uuid::now_v7(), at(900), said(flow, oven, other, "closed", at(900), 60.0)).await;
    let (_, status, ..) = condition(&t, other).await.expect("a second condition");
    assert_eq!(status, "Closed");
    assert_eq!(published(&t, "edge.condition_opened").await, 2);
    assert_eq!(runs(&t, other).await, 1);

    // The case is about the condition: the condition's page lists it (the runtime that opens its task
    // for a person is Ferrum's, and is not part of this test).
    let about = serde_json::to_value(t.k.logic_runs_about(&t.admin, "EdgeCondition", id).await.unwrap()).unwrap();
    assert_eq!(about.as_array().map(Vec::len), Some(1), "{about}");

    // A gateway of another site cannot report a condition about this oven.
    let elsewhere = id_of(&t.insert("Site", json!({"site_name": "Depot", "site_code": "depot"})).await);
    send(&t, elsewhere, Uuid::now_v7(), at(950), said(flow, oven, "ffeeddccbbaa99887766554433221100", "opened", at(950), 99.0)).await;
    assert!(condition(&t, "ffeeddccbbaa99887766554433221100").await.is_none(), "a device of another site is not that gateway's to speak for");

    // Another workspace sees none of it.
    let mut tx = t.k.db.tenant_tx(Uuid::now_v7()).await.unwrap();
    let seen: i64 = sqlx::query_scalar("select count(*) from e_edge_condition").fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(seen, 0);
}
