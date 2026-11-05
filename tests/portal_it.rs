//! Customers who own devices, and reports that go out by themselves.

mod support;

use chrono::{Timelike, Utc};
use ferrum_app_edge::reports;
use ferrum_kernel::Error;
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

#[test]
fn a_customer_sees_their_own_and_nothing_else() {
    run(owners());
}

#[test]
fn a_report_goes_out_once_by_itself() {
    run(reporting());
}

fn is_missing<T: std::fmt::Debug>(got: Result<T, Error>, what: &str) {
    assert!(matches!(got, Err(Error::NotFound(_))), "{what}: {got:?}");
}

async fn alarm(t: &T, device: Uuid, summary: &str) -> Uuid {
    let id = Uuid::now_v7();
    let mut tx = t.tx().await;
    sqlx::query("insert into e_edge_alarm (tenant_id, id, summary, device, severity, status, raised_at) values ($1, $2, $3, $4, 'Major', 'Active', now())")
        .bind(t.tenant)
        .bind(id)
        .bind(summary)
        .bind(device)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn owners() {
    let t = workspace("owners").await;
    let acme_person = t.person("Ana@Acme.test", &[]).await;
    let beta_person = t.person("bo@beta.test", &[]).await;
    let stranger = t.person("who@else.test", &[]).await;
    let site = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    gateway(&t, "GW-1", site, 1).await;
    let acme = id_of(&t.insert("EdgeCustomer", json!({"customer_name": "Acme", "people": [{"user_email": "ana@acme.test"}]})).await);
    let beta = id_of(&t.insert("EdgeCustomer", json!({"customer_name": "Beta", "people": [{"user_email": "bo@beta.test"}]})).await);
    let device = async |code: &str, owner: Option<Uuid>| {
        id_of(&t.insert("Device", json!({"device_name": code, "device_code": code, "kind": "Sensor", "site": site, "edge_customer": owner})).await)
    };
    let (theirs, others, nobodys) = (device("A-1", Some(acme)).await, device("B-1", Some(beta)).await, device("N-1", None).await);

    let layout = |metric: &str| {
        json!({"widgets": [
            {"id": "w", "type": "value", "title": "T", "x": 0, "y": 0, "w": 3, "h": 2, "device": theirs.to_string(), "metric": metric},
            {"id": "c", "type": "button", "title": "Stop", "x": 3, "y": 0, "w": 3, "h": 2, "device": theirs.to_string(), "operation": "press.stop"},
        ]})
        .to_string()
    };
    let board = id_of(&t.insert("EdgeDashboard", json!({"dashboard_name": "Acme's", "customer": acme, "body": layout("temp.c")})).await);
    let foreign = id_of(&t.insert("EdgeDashboard", json!({"dashboard_name": "Beta's", "customer": beta, "body": layout("temp.c")})).await);

    // Each sees theirs, whatever the case of the address they signed in with.
    let mine = t.api(&acme_person, "GET", "/portal/devices", json!({})).await.unwrap();
    assert_eq!(mine["customers"], json!(["Acme"]));
    let codes: Vec<&str> = mine["devices"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["A-1"], "{mine}");
    assert_eq!(mine["dashboards"].as_array().unwrap().len(), 1);

    let none = t.api(&stranger, "GET", "/portal/devices", json!({})).await.unwrap();
    assert_eq!((none["customers"].as_array().unwrap().len(), none["devices"].as_array().unwrap().len()), (0, 0), "being signed in is not owning anything");

    // Someone else's is not there at all, by any way of asking.
    t.api(&acme_person, "GET", &format!("/portal/devices/{theirs}"), json!({})).await.expect("theirs");
    is_missing(t.api(&acme_person, "GET", &format!("/portal/devices/{others}"), json!({})).await, "another customer's device");
    is_missing(t.api(&acme_person, "GET", &format!("/portal/devices/{nobodys}"), json!({})).await, "a device with no customer");
    is_missing(t.get(&acme_person, &format!("/portal/devices/{others}/history"), json!({})).await, "another's history");
    t.get(&acme_person, &format!("/portal/devices/{theirs}/history"), json!({"metrics": "temp.c"})).await.expect("its own history");

    // A device can show its customer part of what it reports, which is ThingsBoard's entity view.
    let mut tx = t.tx().await;
    sqlx::query("insert into op_rollup_minute (tenant_id, device_id, metric, minute, samples, bad, last_value, last_time) values ($1, $2, 'temp.c', now(), 1, 0, 21.5, now()), ($1, $2, 'secret.margin', now(), 1, 0, 9.0, now())")
        .bind(t.tenant)
        .bind(theirs)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("update e_device set edge_shows = 'temp.*' where tenant_id = $1 and id = $2").bind(t.tenant).bind(theirs).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let seen = t.api(&acme_person, "GET", &format!("/portal/devices/{theirs}"), json!({})).await.unwrap();
    let metrics: Vec<&str> = seen["latest"].as_array().unwrap().iter().map(|m| m["metric"].as_str().unwrap()).collect();
    assert_eq!(metrics, ["temp.c"], "{seen}");
    is_missing(
        t.get(&acme_person, &format!("/portal/devices/{theirs}/history"), json!({"metrics": "temp.c,secret.margin"})).await,
        "what it does not show, by asking for it",
    );

    // A dashboard is what was laid out, and a control takes someone who may give commands.
    let shown = t.api(&acme_person, "GET", &format!("/portal/dashboards/{board}"), json!({})).await.unwrap();
    let kinds: Vec<&str> = shown["board"]["widgets"].as_array().unwrap().iter().map(|w| w["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["value"], "{shown}");
    is_missing(t.api(&acme_person, "GET", &format!("/portal/dashboards/{foreign}"), json!({})).await, "another customer's dashboard");

    // An alarm can be acknowledged by whoever owns what raised it, and it says who.
    let (own, foreign_alarm) = (alarm(&t, theirs, "too hot").await, alarm(&t, others, "too cold").await);
    is_missing(t.api(&acme_person, "POST", &format!("/portal/alarms/{foreign_alarm}/acknowledge"), json!({})).await, "another's alarm");
    t.api(&acme_person, "POST", &format!("/portal/alarms/{own}/acknowledge"), json!({"note": "on our way"})).await.expect("theirs");
    let seen = t.api(&acme_person, "GET", &format!("/portal/devices/{theirs}"), json!({})).await.unwrap();
    assert_eq!(seen["alarms"][0]["status"], "Acknowledged");
    assert_eq!(seen["alarms"][0]["acknowledged_by"].as_str().map(str::to_lowercase).as_deref(), Some("ana@acme.test"));

    // No role means none of the rest, and owning a device is not a role.
    assert!(matches!(t.api(&acme_person, "GET", "/overview", json!({})).await, Err(Error::Forbidden(_))));
    assert!(matches!(t.get(&acme_person, "/devices", json!({})).await, Err(Error::Forbidden(_))));
    assert!(t.api(&beta_person, "GET", "/portal/devices", json!({})).await.is_ok());

    // Devices related to one another are found from either end.
    let engineer = t.person("eng@owners.test", &["Edge Engineer"]).await;
    t.insert("EdgeRelation", json!({"from_device": theirs, "relation": "feeds", "to_device": others})).await;
    let related = |id: Uuid| {
        let (t, engineer) = (&t, &engineer);
        async move {
            let found = t.get(engineer, "/devices", json!({"related": id.to_string()})).await.unwrap();
            found["devices"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        }
    };
    assert_eq!(related(theirs).await, ["B-1"]);
    assert_eq!(related(others).await, ["A-1"]);
    assert_eq!(related(nobodys).await, Vec::<String>::new());
    let detail = t.api(&engineer, "GET", &format!("/devices/{theirs}"), json!({})).await.unwrap();
    assert_eq!(
        (detail["related"][0]["relation"].as_str(), detail["related"][0]["code"].as_str(), detail["related"][0]["out"].as_bool()),
        (Some("feeds"), Some("B-1"), Some(true))
    );
}

async fn mails(t: &T) -> Vec<Value> {
    let mut tx = t.tx().await;
    let found: Vec<Value> = sqlx::query_scalar("select payload from outbox where tenant_id = $1 and topic = 'ferrum.mail' order by created_at")
        .bind(t.tenant)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    found
}

fn this_minute() -> String {
    let now = Utc::now();
    format!("{:02}:{:02}", now.hour(), now.minute())
}

async fn reporting() {
    let t = workspace("reporting").await;
    let site = id_of(&t.insert("Site", json!({"site_name": "North", "site_code": "north"})).await);
    t.insert("Device", json!({"device_name": "<Press>", "device_code": "P-1", "kind": "Sensor", "site": site})).await;

    // What cannot work is said when it is saved.
    let refused = async |v: Value| t.k.insert(&t.admin, "EdgeReportSchedule", obj(v)).await.is_err();
    let base = json!({"schedule_name": "x", "report": "edge_availability", "at_time": "07:00", "recipients": [{"email": "a@b.test"}]});
    let with = |k: &str, v: Value| {
        let mut o = base.clone();
        o[k] = v;
        o
    };
    assert!(refused(with("at_time", json!("seven"))).await, "a time of day");
    assert!(refused(with("days", json!("8"))).await, "a day of the week");
    assert!(refused(with("recipients", json!([]))).await, "nobody to send it to");
    assert!(refused(with("report", json!("edge_readings"))).await, "readings are of one device");

    let at = this_minute();
    t.insert(
        "EdgeReportSchedule",
        json!({"schedule_name": "Weekly", "report": "edge_availability", "at_time": at, "site": site, "recipients": [{"email": "ops@x.test"}, {"email": "boss@x.test"}]}),
    )
    .await;
    t.insert(
        "EdgeReportSchedule",
        json!({"schedule_name": "Later", "report": "edge_alarm_history", "at_time": at, "enabled": false, "recipients": [{"email": "no@x.test"}]}),
    )
    .await;
    t.insert(
        "EdgeReportSchedule",
        json!({"schedule_name": "Printed", "report": "edge_availability", "format": "PDF", "at_time": at, "recipients": [{"email": "pdf@x.test"}]}),
    )
    .await;

    let ctx = t.system();
    reports::deliver(&t.k, &ctx).await.unwrap();
    let sent = mails(&t).await;
    let mut to: Vec<&str> = sent.iter().map(|m| m["to"].as_str().unwrap()).collect();
    to.sort_unstable();
    assert_eq!(to, ["boss@x.test", "ops@x.test"], "each person named, and nobody on what is turned off");
    let html = sent[0]["html"].as_str().unwrap();
    assert!(html.contains("&lt;Press&gt;") && !html.contains("<Press>"), "a name from outside does nothing in a mail: {html}");

    let mut tx = t.tx().await;
    let notes: Vec<(String, Option<String>)> =
        sqlx::query_as("select schedule_name, last_result from e_edge_report_schedule where tenant_id = $1 and last_run is not null order by schedule_name")
            .bind(t.tenant)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes[0].1.as_deref().unwrap().starts_with("It did not go out"), "no renderer here, and it says so on the schedule: {notes:?}");
    assert!(notes[1].1.as_deref().unwrap().starts_with("Sent to 2"), "{notes:?}");

    // Every minute comes round, and a day's report goes out once.
    reports::deliver(&t.k, &ctx).await.unwrap();
    assert_eq!(mails(&t).await.len(), 2);

    // Asked for by the boss, their row of the weekly schedule is theirs alone: erased, it goes, and
    // the schedule goes on to ops.
    let asked = t.k.personal_data(&t.admin, "boss@x.test").await.unwrap();
    let theirs: Vec<&Value> = asked["records"].as_array().unwrap().iter().filter(|r| r["entity"] == "EdgeReportSchedule").collect();
    assert_eq!(theirs.len(), 1, "{asked}");
    assert_eq!(theirs[0]["data"]["recipients"], json!([theirs[0]["data"]["recipients"][0].clone()]));
    assert_eq!(theirs[0]["data"]["recipients"][0]["email"], "boss@x.test");
    let erased = t.k.erase_personal_data(&t.admin, "boss@x.test", "Request 1").await.unwrap();
    assert_eq!(erased["erased"][0]["rows"], json!(1), "{erased}");
    let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
    let left: Vec<String> = sqlx::query_scalar(
        "select r.email from e_edge_report_schedule__recipients r join e_edge_report_schedule s on s.id = r.parent_id where s.schedule_name = 'Weekly' order by r.email",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(left, ["ops@x.test"]);
}
