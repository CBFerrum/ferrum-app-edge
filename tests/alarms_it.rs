//! What happens around an alarm: who is told, in what order, and what a person can do about it.

mod support;

use chrono::Utc;
use ferrum_app_edge::notices;
use ferrum_edge_contract::edge::AlarmChange;
use serde_json::json;
use support::*;
use uuid::Uuid;

#[test]
fn around_an_alarm() {
    run(scenario());
}

async fn notices_of(t: &T, alarm: Uuid) -> Vec<(String, String, String)> {
    let mut tx = t.tx().await;
    let rows = sqlx::query_as("select channel, target, status from edge_notice where tenant_id = $1 and alarm_id = $2 order by due_at, target")
        .bind(t.tenant)
        .bind(alarm)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    rows
}

async fn alarm_id(t: &T, device: Uuid, status: &str) -> Uuid {
    let mut tx = t.tx().await;
    let id = sqlx::query_scalar("select id from e_edge_alarm where tenant_id = $1 and device = $2 and status = $3 order by raised_at desc limit 1")
        .bind(t.tenant)
        .bind(device)
        .bind(status)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

/// Brings a waiting notice's time forward, as if the minutes had passed.
async fn time_passes(t: &T, alarm: Uuid) {
    let mut tx = t.tx().await;
    sqlx::query("update edge_notice set due_at = now() - interval '1 second' where tenant_id = $1 and alarm_id = $2 and status = 'Waiting'")
        .bind(t.tenant)
        .bind(alarm)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn raise(t: &T, site: Uuid, rule: Uuid, device: Uuid, raised: bool) {
    let change = AlarmChange { rule, device, raised, value: Some(91.0), node: None, name: None, severity: None };
    t.fact(site, "edge.alarm", Utc::now(), json!(change)).await;
}

async fn scenario() {
    let t = workspace("alarms").await;
    let engineer = t.person("eng@alarms.test", &["Edge Engineer"]).await;
    let manager = t.person("boss@alarms.test", &["Edge Engineer", "Ops Manager"]).await;
    let viewer = t.person("view@alarms.test", &["Edge Viewer"]).await;
    let site = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    let (gw, _gateway) = gateway(&t, "GW-1", site, 1).await;
    let press = id_of(&t.insert("Device", json!({"device_name": "Press", "device_code": "PRESS-1", "kind": "Machine", "site": site, "gateway": gw})).await);
    let hot = id_of(
        &t.insert("EdgeRule", json!({"rule_name": "Too hot", "metric": "temp.c", "comparison": "Above", "limit_value": 80, "severity": "Critical"})).await,
    );
    let mild = id_of(
        &t.insert("EdgeRule", json!({"rule_name": "A bit warm", "metric": "temp.c", "comparison": "Above", "limit_value": 60, "severity": "Warning"})).await,
    );
    t.k.update(
        &t.admin,
        "EdgeNotifyRule",
        id_of(
            &t.insert(
                "EdgeNotifyRule",
                json!({
                    "rule_name": "Serious ones",
                    "trigger": "Alarm raised",
                    "min_severity": "Major",
                    "subject": "{severity}: {summary}",
                    "steps": [
                        {"after_minutes": 0, "channel": "Bell", "target": "Edge Engineer"},
                        {"after_minutes": 0, "channel": "Email", "target": "oncall@alarms.test"},
                        {"after_minutes": 30, "channel": "Bell", "target": "Ops Manager"},
                    ],
                }),
            )
            .await,
        ),
        obj(json!({"enabled": true})),
        None,
    )
    .await
    .unwrap();

    // ───── raised: the first step goes at once, the next waits, and a mild one tells nobody ─────
    raise(&t, site, mild, press, true).await;
    let warm = alarm_id(&t, press, "Active").await;
    assert_eq!(notices_of(&t, warm).await, vec![], "not serious enough for that rule");
    raise(&t, site, hot, press, true).await;
    let alarm = alarm_id(&t, press, "Active").await;
    assert_ne!(alarm, warm);
    assert_eq!(notices_of(&t, alarm).await.len(), 3);
    let ctx = t.system();
    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 2, "the two with no wait");
    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 0, "and nothing goes twice");
    let told = notices_of(&t, alarm).await;
    assert_eq!(told.iter().filter(|n| n.2 == "Sent").count(), 2, "{told:?}");
    assert_eq!(told.iter().find(|n| n.1 == "Ops Manager").unwrap().2, "Waiting");
    let mut tx = t.tx().await;
    let bell: Option<String> = sqlx::query_scalar("select title from notification where tenant_id = $1 and user_id = $2 and title like 'Critical:%' limit 1")
        .bind(t.tenant)
        .bind(engineer.user)
        .fetch_optional(&mut *tx)
        .await
        .unwrap();
    assert_eq!(bell.as_deref(), Some("Critical: Too hot on PRESS-1"), "the template was filled in");
    tx.commit().await.unwrap();

    // ───── nobody came, so it goes up a step ─────
    time_passes(&t, alarm).await;
    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 1);
    let path = format!("/alarms/{alarm}");
    assert_eq!(t.get(&viewer, &path, json!({})).await.unwrap()["notices"].as_array().unwrap().iter().filter(|n| n["status"] == json!("Sent")).count(), 3);

    // ───── what a person does goes on its timeline ─────
    assert!(t.api(&viewer, "POST", &format!("{path}/acknowledge"), json!({})).await.is_err(), "a viewer looks");
    t.api(&engineer, "POST", &format!("{path}/assign"), json!({"user": manager.user})).await.unwrap();
    t.api(&engineer, "POST", &format!("{path}/note"), json!({"text": "Coolant low, topping up"})).await.unwrap();
    assert!(t.api(&engineer, "POST", &format!("{path}/note"), json!({"text": "  "})).await.is_err(), "a note says something");
    assert!(t.api(&engineer, "POST", &format!("{path}/assign"), json!({"user": Uuid::now_v7()})).await.is_err(), "no such person");
    t.api(&engineer, "POST", &format!("{path}/acknowledge"), json!({"note": "On it"})).await.unwrap();
    assert!(t.api(&engineer, "POST", &format!("{path}/acknowledge"), json!({})).await.is_err(), "once");
    let seen = t.get(&viewer, &path, json!({})).await.unwrap();
    let kinds: Vec<&str> = seen["notes"].as_array().unwrap().iter().map(|n| n["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["Assigned", "Note", "Acknowledged"], "{seen}");
    assert_eq!((seen["assignee"].as_str().map(|u| u == manager.user.to_string()), seen["assignee_email"].as_str()), (Some(true), Some("boss@alarms.test")));

    // ───── dealing with it stops what was still to come ─────
    raise(&t, site, hot, press, false).await;
    assert!(seen["status"] == json!("Acknowledged"));
    raise(&t, site, hot, press, true).await;
    let second = alarm_id(&t, press, "Active").await;
    assert_ne!(second, warm);
    let second = if second == alarm { panic!("a new alarm") } else { second };
    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 2);
    t.api(&engineer, "POST", &format!("/alarms/{second}/acknowledge"), json!({})).await.unwrap();
    time_passes(&t, second).await;
    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 0, "nobody needs waking now");
    assert_eq!(notices_of(&t, second).await.iter().find(|n| n.1 == "Ops Manager").unwrap().2, "Dropped");

    // ───── shelved, it stays on every screen and tells nobody until it ends ─────
    raise(&t, site, hot, press, false).await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    raise(&t, site, hot, press, true).await;
    let third = alarm_id(&t, press, "Active").await;
    t.api(&engineer, "POST", &format!("/alarms/{third}/shelve"), json!({"minutes": 60})).await.unwrap();
    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 0, "silent");
    assert!(
        t.api(&viewer, "GET", "/alarms", json!({})).await.unwrap()["alarms"].as_array().unwrap().iter().any(|a| a["id"] == json!(third)),
        "and in plain sight"
    );
    assert!(t.api(&engineer, "POST", &format!("/alarms/{third}/shelve"), json!({"minutes": 100_000})).await.is_err(), "for a week at most");
    t.api(&engineer, "POST", &format!("/alarms/{third}/shelve"), json!({"minutes": 0})).await.unwrap();
    assert_eq!(notices::send_due(&t.k, &ctx).await.unwrap(), 2, "and everyone hears when that ends");

    // ───── closing by hand says why, and who ─────
    assert!(t.api(&engineer, "POST", &format!("/alarms/{third}/clear"), json!({})).await.is_err(), "why?");
    t.api(&engineer, "POST", &format!("/alarms/{third}/clear"), json!({"note": "Sensor replaced, it will not report again"})).await.unwrap();
    let closed = t.get(&viewer, &format!("/alarms/{third}"), json!({})).await.unwrap();
    assert_eq!((closed["status"].as_str(), closed["cleared_by"].as_str()), (Some("Cleared"), Some("eng@alarms.test")), "{closed}");
    assert!(t.api(&engineer, "POST", &format!("/alarms/{third}/clear"), json!({"note": "again"})).await.is_err());
    assert!(t.api(&engineer, "POST", &format!("/alarms/{third}/shelve"), json!({"minutes": 10})).await.is_err(), "there is nothing left to shelve");

    // ───── a rule cannot be made to call into a private network ─────
    let site2 = id_of(&t.insert("Site", json!({"site_name": "South", "site_code": "south"})).await);
    let (gw2, _g2) = gateway(&t, "GW-2", site2, 2).await;
    let pump = id_of(&t.insert("Device", json!({"device_name": "Pump", "device_code": "PUMP-1", "kind": "Machine", "site": site2, "gateway": gw2})).await);
    t.insert("EdgeNotifyRule", json!({"rule_name": "Elsewhere", "trigger": "Alarm raised", "min_severity": "Warning", "site": site2, "steps": [{"channel": "Webhook", "target": "http://169.254.169.254/latest"}]})).await;
    raise(&t, site2, hot, pump, true).await;
    let far = alarm_id(&t, pump, "Active").await;
    notices::send_due(&t.k, &ctx).await.unwrap();
    let far_told = notices_of(&t, far).await;
    assert_eq!(far_told.iter().find(|n| n.0 == "Webhook").unwrap().2, "Failed", "{far_told:?}");
    let errors = t.get(&viewer, &format!("/alarms/{far}"), json!({})).await.unwrap();
    assert!(errors["notices"].as_array().unwrap().iter().any(|n| n["error"].as_str().is_some_and(|e| !e.is_empty())), "it says why: {errors}");

    // ───── a gateway that stops talking cannot say so itself, so this does ─────
    let quiet = t.system();
    let mut tx = t.tx().await;
    sqlx::query("update edge_gateway set seen_at = now() - interval '1 hour'").execute(&mut *tx).await.ok();
    sqlx::query("insert into edge_gateway (tenant_id, device_id, seen_at) values ($1, $2, now() - interval '1 hour') on conflict (tenant_id, device_id) do update set seen_at = excluded.seen_at").bind(t.tenant).bind(gw).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    ferrum_app_edge::facts::watch_gateways(&t.k, &quiet).await.unwrap();
    ferrum_app_edge::facts::watch_gateways(&t.k, &quiet).await.unwrap();
    let mut tx = t.tx().await;
    let open: Vec<(String, String)> = sqlx::query_as("select summary, severity from e_edge_alarm where tenant_id = $1 and device = $2 and status <> 'Cleared'")
        .bind(t.tenant)
        .bind(gw)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(open, vec![("Gateway GW-1 has gone quiet".to_string(), "Major".to_string())], "once");
    sqlx::query("update edge_gateway set seen_at = now() where tenant_id = $1 and device_id = $2").bind(t.tenant).bind(gw).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    ferrum_app_edge::facts::watch_gateways(&t.k, &quiet).await.unwrap();
    let mut tx = t.tx().await;
    let still: i64 = sqlx::query_scalar("select count(*) from e_edge_alarm where tenant_id = $1 and device = $2 and status <> 'Cleared'")
        .bind(t.tenant)
        .bind(gw)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(still, 0, "and it clears when it is back");
    tx.commit().await.unwrap();
}
