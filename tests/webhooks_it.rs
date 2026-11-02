//! Telling someone else's system. A separate test because it needs what no real site should have:
//! private addresses allowed, so that a receiver on this machine can stand in for Slack or a pager.

mod support;

use chrono::Utc;
use ferrum_app_edge::notices;
use ferrum_edge_contract::edge::AlarmChange;
use ferrum_kernel::KernelSettings;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use support::*;
use uuid::Uuid;

async fn receiver(answer: u16) -> (SocketAddr, Arc<Mutex<Vec<Value>>>) {
    let heard: Arc<Mutex<Vec<Value>>> = Arc::default();
    let keep = heard.clone();
    let app = axum::Router::new().fallback(move |body: axum::body::Bytes| {
        let keep = keep.clone();
        async move {
            keep.lock().unwrap().push(serde_json::from_slice(&body).unwrap_or(Value::Null));
            axum::http::StatusCode::from_u16(answer).unwrap()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address, heard)
}

#[test]
fn someone_elses_system_is_told() {
    run(scenario());
}

async fn status_of(t: &T, alarm: Uuid, channel: &str) -> (String, i32) {
    let mut tx = t.tx().await;
    let row = sqlx::query_as("select status, attempts from edge_notice where tenant_id = $1 and alarm_id = $2 and channel = $3")
        .bind(t.tenant)
        .bind(alarm)
        .bind(channel)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    row
}

async fn scenario() {
    let t = workspace_with("telling", KernelSettings { webhooks_allow_private: true, ..Default::default() }).await;
    let site = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    let (gw, _gateway) = gateway(&t, "GW-1", site, 1).await;
    let press = id_of(&t.insert("Device", json!({"device_name": "Press", "device_code": "PRESS-1", "kind": "Machine", "site": site, "gateway": gw})).await);
    let hot = id_of(
        &t.insert("EdgeRule", json!({"rule_name": "Too hot", "metric": "temp.c", "comparison": "Above", "limit_value": 80, "severity": "Critical"})).await,
    );
    let (slack, heard) = receiver(200).await;
    let (down, tried) = receiver(503).await;
    t.insert(
        "EdgeNotifyRule",
        json!({
            "rule_name": "Everyone",
            "trigger": "Alarm raised",
            "min_severity": "Warning",
            "subject": "{severity} at {site}: {summary}",
            "steps": [
                {"channel": "Slack", "target": format!("http://{slack}/hook")},
                {"channel": "Webhook", "target": format!("http://{down}/hook")},
            ],
        }),
    )
    .await;
    t.insert("EdgeNotifyRule", json!({"rule_name": "All clear", "trigger": "Alarm cleared", "min_severity": "Info", "steps": [{"channel": "Teams", "target": format!("http://{slack}/teams")}]})).await;
    let change = AlarmChange { rule: hot, device: press, raised: true, value: Some(91.0), node: None, name: None, severity: None };
    t.fact(site, "edge.alarm", Utc::now(), json!(change)).await;
    let mut tx = t.tx().await;
    let alarm: Uuid = sqlx::query_scalar("select id from e_edge_alarm where tenant_id = $1").bind(t.tenant).fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let ctx = t.system();

    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 2);
    let said = heard.lock().unwrap()[0]["text"].as_str().unwrap().to_string();
    assert!(said.starts_with("*Critical at North: Too hot on PRESS-1*\nToo hot on PRESS-1\nSeverity: Critical\nRaised: "), "{said}");
    assert_eq!(status_of(&t, alarm, "Slack").await, ("Sent".to_string(), 1));

    // ───── one that is down is tried again, later each time, and then given up on, out loud ─────
    assert_eq!(status_of(&t, alarm, "Webhook").await, ("Waiting".to_string(), 1));
    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 0, "not straight away");
    for attempt in 2..=5 {
        let mut tx = t.tx().await;
        sqlx::query("update edge_notice set due_at = now() - interval '1 second' where tenant_id = $1 and status = 'Waiting'")
            .bind(t.tenant)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 1);
        let want = if attempt == 5 { "Failed" } else { "Waiting" };
        assert_eq!(status_of(&t, alarm, "Webhook").await, (want.to_string(), attempt));
    }
    assert_eq!(tried.lock().unwrap().len(), 5);
    let id = tried.lock().unwrap()[0]["id"].clone();
    assert!(tried.lock().unwrap().iter().all(|m| m["id"] == id), "the same id each time, so a receiver can tell a repeat");

    // ───── the all clear goes to whoever a rule says ─────
    t.fact(site, "edge.alarm", Utc::now(), json!(AlarmChange { raised: false, ..change })).await;
    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 1);
    assert_eq!(heard.lock().unwrap().len(), 2);
    assert_eq!(status_of(&t, alarm, "Teams").await.0, "Sent");

    // ───── the job that sends at once does the same as upkeep ─────
    let change = AlarmChange { rule: hot, device: press, raised: true, value: Some(95.0), node: None, name: None, severity: None };
    t.fact(site, "edge.alarm", Utc::now(), json!(change)).await;
    let mut ran = 0;
    for _ in 0..200 {
        ran = heard.lock().unwrap().len();
        if ran == 3 {
            break;
        }
        t.k.run_jobs_once(4).await.ok();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(ran, 3, "told without waiting for upkeep");
}
