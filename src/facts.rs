//! What gateways report. Facts arrive at least once and can be late by days, so each is applied by
//! when it happened: a late attribute does not replace a newer one, and an alarm's clearing that
//! arrives before its raising leaves it cleared.

use crate::notices;
use chrono::{DateTime, Utc};
use ferrum_domain::util::{elevated, sys};
use ferrum_edge_contract::edge::AlarmChange;
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result, Tx, notify};
use ferrum_ops::commands::{self, Issuer, NewCommand};
use ferrum_ops::contract::Lifecycle;
use ferrum_ops::devices;
use serde_json::{Value, json};
use uuid::Uuid;

/// One fact, by what comes after `ops.fact.edge.`.
pub async fn apply(k: &Kernel, ctx: &Ctx, tx: &mut Tx, kind: &str, payload: &Value) -> Result<()> {
    let Some(at) = payload["at"].as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok()) else { return Ok(()) };
    let body = &payload["body"];
    match kind {
        "attribute" => {
            let (Some(device), Some(key)) = (body["device"].as_str().and_then(|d| d.parse::<Uuid>().ok()), body["key"].as_str()) else { return Ok(()) };
            sqlx::query(
                "insert into edge_attribute (tenant_id, device_id, key, value, reported_at, received_at) values ($1, $2, $3, $4, $5, now()) \
                 on conflict (tenant_id, device_id, key) do update set value = excluded.value, reported_at = excluded.reported_at, received_at = now() \
                 where edge_attribute.reported_at is null or edge_attribute.reported_at < excluded.reported_at",
            )
            .bind(ctx.tenant)
            .bind(device)
            .bind(key)
            .bind(&body["value"])
            .bind(at)
            .execute(&mut **tx)
            .await?;
        }
        "event" => {
            let (Some(device), Some(name), Some(id)) = (
                body["device"].as_str().and_then(|d| d.parse::<Uuid>().ok()),
                body["name"].as_str(),
                payload["fact"].as_str().and_then(|f| f.parse::<Uuid>().ok()),
            ) else {
                return Ok(());
            };
            sqlx::query("insert into edge_event (tenant_id, id, device_id, name, value, at) values ($1, $2, $3, $4, $5, $6) on conflict do nothing")
                .bind(ctx.tenant)
                .bind(id)
                .bind(device)
                .bind(name)
                .bind(&body["value"])
                .bind(at)
                .execute(&mut **tx)
                .await?;
        }
        "alarm" => {
            let Ok(change) = serde_json::from_value::<AlarmChange>(body.clone()) else { return Ok(()) };
            let site = payload["site"].as_str().and_then(|s| s.parse::<Uuid>().ok());
            alarm(k, ctx, tx, &change, at, site).await?;
        }
        "notify" => {
            let (Some(title), flow) = (body["title"].as_str(), body["flow"].as_str().and_then(|f| f.parse::<Uuid>().ok())) else { return Ok(()) };
            for user in engineers(ctx, tx).await? {
                notify::send(tx, ctx.tenant, user, title, None, Some("EdgeFlow"), flow).await?;
            }
        }
        "request" => request(ctx, tx, payload, at).await?,
        "joined" => joined(k, ctx, tx, payload).await?,
        "transaction" => crate::charging::heard(tx, ctx.tenant, body).await?,
        "condition" => condition(k, ctx, tx, payload, at).await?,
        _ => {}
    }
    Ok(())
}

/// A device added itself at a gateway, which may have been some time ago. It becomes a record that
/// reports and takes no command until a person commissions it. If the name has been taken since, the
/// record that exists stands, and this device's token is not added to it.
async fn joined(k: &Kernel, ctx: &Ctx, tx: &mut Tx, payload: &Value) -> Result<()> {
    let body = &payload["body"];
    let (Some(code), Some(definition), Some(hash)) = (body["code"].as_str(), body["definition"].as_str(), body["token_hash"].as_str()) else { return Ok(()) };
    let site = payload["site"].as_str().and_then(|s| s.parse::<Uuid>().ok());
    let taken: Option<i32> =
        sqlx::query_scalar("select 1 from e_device where tenant_id = $1 and device_code = $2").bind(ctx.tenant).bind(code).fetch_optional(&mut **tx).await?;
    let found: Option<Uuid> = sqlx::query_scalar("select id from e_device_definition where tenant_id = $1 and definition_name = $2")
        .bind(ctx.tenant)
        .bind(definition)
        .fetch_optional(&mut **tx)
        .await?;
    let (None, Some(found)) = (taken, found) else { return Ok(()) };
    let doc = json!({"device_name": code, "device_code": code, "kind": "Sensor", "site": site, "gateway": body["gateway"], "edge_definition": found, "edge_token_hash": hash});
    let made = k.insert_tx(&elevated(ctx), tx, "Device", doc.as_object().cloned().unwrap_or_default()).await?;
    let Some(id) = made.get("id").and_then(Value::as_str).and_then(|i| i.parse::<Uuid>().ok()) else { return Ok(()) };
    for to in [Lifecycle::Claimed, Lifecycle::Provisioned] {
        devices::set_lifecycle_tx(k, &elevated(ctx), tx, id, to, "it added itself at a gateway").await?;
    }
    Ok(())
}

async fn engineers(ctx: &Ctx, tx: &mut Tx) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar("select user_id from user_role where tenant_id = $1 and role = 'Edge Engineer'").bind(ctx.tenant).fetch_all(&mut **tx).await?)
}

/// What a flow asked for, which becomes a command only by the ledger's rules: as automation, it runs
/// under an envelope someone approved, or waits for a person. It may have waited out an outage on the
/// way here, so what is too old, or from a flow that has since been stopped, is not issued: a request
/// is a thought at a moment, and that moment has passed. Whatever is refused says why on the device,
/// and is not tried again, because nothing about it will change.
async fn request(ctx: &Ctx, tx: &mut Tx, payload: &Value, at: DateTime<Utc>) -> Result<()> {
    let body = &payload["body"];
    let (Some(device), Some(flow), Some(operation), Some(fact)) = (
        body["device"].as_str().and_then(|d| d.parse::<Uuid>().ok()),
        body["flow"].as_str().and_then(|f| f.parse::<Uuid>().ok()),
        body["operation"].as_str(),
        payload["fact"].as_str().and_then(|f| f.parse::<Uuid>().ok()),
    ) else {
        return Ok(());
    };
    let live: Option<bool> =
        sqlx::query_scalar("select live from e_edge_flow where tenant_id = $1 and id = $2").bind(ctx.tenant).bind(flow).fetch_optional(&mut **tx).await?;
    let left = body["expires_secs"].as_i64().unwrap_or(300) - (Utc::now() - at).num_seconds();
    let refused = if live != Some(true) {
        Some("the flow that asked for this has been stopped".to_string())
    } else if left <= 0 {
        Some("it took too long to get here".to_string())
    } else {
        let cmd = NewCommand {
            target: device,
            operation: operation.to_string(),
            schema_version: 1,
            payload: body["payload"].clone(),
            expected_state: Value::Null,
            expires_in_secs: left,
            dedupe_key: Some(format!("flow:{fact}")),
            workflow_id: Some(format!("flow:{flow}")),
            step_id: body["node"].as_str().map(str::to_string),
            correlation_id: None,
            issuer: Issuer::Automation,
        };
        match commands::issue(tx, ctx, cmd).await {
            Ok(_) => None,
            Err(e @ (Error::Validation(_) | Error::Invalid(_) | Error::Forbidden(_) | Error::NotFound(_))) => Some(e.to_string()),
            Err(e) => return Err(e),
        }
    };
    if let Some(why) = refused {
        sqlx::query("insert into edge_event (tenant_id, id, device_id, name, value, at) values ($1, $2, $3, $4, $5, $6) on conflict do nothing")
            .bind(ctx.tenant)
            .bind(fact)
            .bind(device)
            .bind("flow.request.refused")
            .bind(json!({"operation": operation, "why": why}))
            .bind(at)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// Most readings a condition keeps as evidence.
const MAX_EVIDENCE: i64 = 1_000;

fn uuid_of(v: &Value) -> Option<Uuid> {
    v.as_str().and_then(|s| s.parse().ok())
}

/// An episode from a business-critical flow (FLC-EDGE-007). One condition for each episode, however often
/// its gateway says so; each reading kept once, by the fact that carried it; and each turn it takes
/// published once, with the episode as its subject, for the processes the workspace runs. Facts can
/// come late and out of order: whichever arrives first makes the condition, and it says it opened.
async fn condition(k: &Kernel, ctx: &Ctx, tx: &mut Tx, payload: &Value, reported: DateTime<Utc>) -> Result<()> {
    let body = &payload["body"];
    let (Some(episode), Some(phase), Some(device), Some(fact)) =
        (body["episode"].as_str().filter(|e| !e.is_empty() && e.len() <= 64), body["phase"].as_str(), uuid_of(&body["device"]), uuid_of(&payload["fact"]))
    else {
        return Ok(());
    };
    let flow = uuid_of(&body["flow"]);
    // A gateway speaks for its own site: a condition about a device somewhere else is not taken from it
    // (FLC-EDGE-001). The site is the one the gateway signed for, never anything its payload says.
    if let Some(site) = uuid_of(&payload["site"]) {
        let here: Option<i32> = sqlx::query_scalar("select 1 from e_device where tenant_id = $1 and id = $2 and site = $3")
            .bind(ctx.tenant)
            .bind(device)
            .bind(site)
            .fetch_optional(&mut **tx)
            .await?;
        if here.is_none() {
            tracing::warn!(%device, %site, "a condition about a device of another site was not taken");
            return Ok(());
        }
    }
    let at = body["at"].as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok()).unwrap_or(reported);
    let value = body["value"].as_f64();
    // The flow's row is locked, so two reports of one episode cannot both make its condition.
    if let Some(flow) = flow {
        sqlx::query("select 1 from e_edge_flow where tenant_id = $1 and id = $2 for update").bind(ctx.tenant).bind(flow).execute(&mut **tx).await?;
    }
    let found: Option<(Uuid, String)> = sqlx::query_as("select id, status from e_edge_condition where tenant_id = $1 and episode = $2 for update")
        .bind(ctx.tenant)
        .bind(episode)
        .fetch_optional(&mut **tx)
        .await?;
    let title = body["title"].as_str().unwrap_or("A condition at a site").chars().take(200).collect::<String>();
    let mut turns: Vec<&str> = Vec::new();
    let id = match found {
        Some((id, status)) => {
            let mut v = Doc::new();
            match (phase, status.as_str()) {
                ("reopened", s) if s != "Open" => {
                    v.insert("status".into(), json!("Open"));
                    v.insert("closed_at".into(), Value::Null);
                    turns.push("condition_reopened");
                }
                ("closed", "Open") => {
                    v.insert("status".into(), json!("Closed"));
                    v.insert("closed_at".into(), json!(at.to_rfc3339()));
                    turns.push("condition_closed");
                }
                ("interrupted", "Open") => {
                    v.insert("status".into(), json!("Interrupted"));
                    turns.push("condition_interrupted");
                }
                ("stale", _) => drop(v.insert("stale".into(), json!(true))),
                _ => {}
            }
            if !v.is_empty() {
                k.set_values_tx(&sys(ctx), tx, "EdgeCondition", id, v).await?;
            }
            id
        }
        None => {
            let status = match phase {
                "closed" => "Closed",
                "interrupted" => "Interrupted",
                _ => "Open",
            };
            let doc = json!({
                "condition_title": title, "episode": episode, "device": device, "flow": flow, "flow_node": body["node"],
                "status": status, "opened_at": body["opened"].as_str().unwrap_or(&at.to_rfc3339()), "last_value": value,
                "peak": body["peak"], "unit": body["unit"], "evidence_count": 0, "stale": phase == "stale",
                "closed_at": (status == "Closed").then(|| at.to_rfc3339()),
            });
            let made = k.insert_tx(&elevated(ctx), tx, "EdgeCondition", doc.as_object().cloned().unwrap_or_default()).await?;
            let Some(id) = made.get("id").and_then(Value::as_str).and_then(|i| i.parse().ok()) else { return Ok(()) };
            turns.push("condition_opened");
            match status {
                "Closed" => turns.push("condition_closed"),
                "Interrupted" => turns.push("condition_interrupted"),
                _ => {}
            }
            id
        }
    };
    // The reading itself, once, by the fact that carried it; the condition counts it only then.
    let kept: i64 = sqlx::query_scalar("select count(*) from edge_condition_evidence where tenant_id = $1 and condition_id = $2")
        .bind(ctx.tenant)
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    // Past what it keeps a reading is neither kept nor counted: the count says what was kept.
    let new = kept < MAX_EVIDENCE
        && sqlx::query(
            "insert into edge_condition_evidence (tenant_id, fact_id, condition_id, phase, at, value, quality, raw) values ($1, $2, $3, $4, $5, $6, $7, $8) \
             on conflict do nothing",
        )
        .bind(ctx.tenant)
        .bind(fact)
        .bind(id)
        .bind(phase)
        .bind(at)
        .bind(value)
        .bind(body["quality"].as_str().unwrap_or("good"))
        .bind(&body["raw"])
        .execute(&mut **tx)
        .await?
        .rows_affected()
            == 1;
    // A reading moves the condition's figures; a notice that it went quiet carries none.
    if new && value.is_some() && phase != "stale" {
        sqlx::query(
            "update e_edge_condition set evidence_count = coalesce(evidence_count, 0) + 1, \
               last_value = case when last_seen_at is null or last_seen_at <= $3 then $4::float8::numeric else last_value end, \
               last_seen_at = greatest(last_seen_at, $3), peak = coalesce($5::float8::numeric, peak), \
               stale = case when $6 = 'good' then false else stale end \
             where tenant_id = $1 and id = $2",
        )
        .bind(ctx.tenant)
        .bind(id)
        .bind(at)
        .bind(value)
        .bind(body["peak"].as_f64())
        .bind(body["quality"].as_str().unwrap_or("good"))
        .execute(&mut **tx)
        .await?;
    }
    for name in turns {
        ferrum_kernel::outbox::enqueue(
            tx,
            ctx.tenant,
            &format!("edge.{name}"),
            Some("EdgeCondition"),
            Some(id),
            json!({"subject": episode, "episode": episode, "condition": id, "device": device, "flow": flow, "title": title, "at": at, "value": value}),
        )
        .await?;
    }
    Ok(())
}

async fn alarm(k: &Kernel, ctx: &Ctx, tx: &mut Tx, change: &AlarmChange, at: DateTime<Utc>, site: Option<Uuid>) -> Result<()> {
    // One open alarm per rule or flow node, and device. The lock is on the rule or flow, so two
    // reports cannot both open it.
    let node = change.node.as_deref();
    let origin: Option<(String, Option<String>)> = match node {
        None => sqlx::query_as("select rule_name, severity from e_edge_rule where tenant_id = $1 and id = $2 for update"),
        Some(_) => sqlx::query_as("select flow_name, null::text from e_edge_flow where tenant_id = $1 and id = $2 for update"),
    }
    .bind(ctx.tenant)
    .bind(change.rule)
    .fetch_optional(&mut **tx)
    .await?;
    const SAME: &str = "tenant_id = $1 and device = $3 and case when $4::text is null then rule = $2 else flow = $2 and flow_node = $4 end";
    let open: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select id, raised_at from e_edge_alarm where {SAME} and status <> 'Cleared' order by raised_at desc limit 1"
    )))
    .bind(ctx.tenant)
    .bind(change.rule)
    .bind(change.device)
    .bind(node)
    .fetch_optional(&mut **tx)
    .await?;
    if !change.raised {
        // Only the alarm this clears: one raised after it is another alarm.
        if let Some((id, raised_at)) = open
            && raised_at <= at
        {
            let mut v = Doc::new();
            v.insert("status".into(), json!("Cleared"));
            v.insert("cleared_at".into(), json!(at.to_rfc3339()));
            k.set_values_tx(&sys(ctx), tx, "EdgeAlarm", id, v).await?;
            crate::alarms::note(tx, ctx.tenant, id, "the equipment", "Cleared", None).await?;
            notices::stand_down(tx, ctx, id).await?;
            let alarm = crate::alarms::load(tx, ctx.tenant, id).await?;
            notices::alarm_changed(tx, ctx, &alarm, notices::CLEARED, at).await?;
        }
        return Ok(());
    }
    if open.is_some() {
        return Ok(());
    }
    let cleared_since: Option<i32> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select 1 from e_edge_alarm where {SAME} and raised_at = $5")))
        .bind(ctx.tenant)
        .bind(change.rule)
        .bind(change.device)
        .bind(node)
        .bind(at)
        .fetch_optional(&mut **tx)
        .await?;
    if cleared_since.is_some() {
        return Ok(());
    }
    let (origin_name, origin_severity) = origin.unwrap_or_else(|| ("A rule since removed".to_string(), None));
    let name = change.name.clone().unwrap_or(origin_name);
    let severity = change.severity.clone().or(origin_severity).unwrap_or_else(|| "Warning".into());
    let code: Option<String> = sqlx::query_scalar("select device_code from e_device where tenant_id = $1 and id = $2")
        .bind(ctx.tenant)
        .bind(change.device)
        .fetch_optional(&mut **tx)
        .await?;
    let summary = format!("{name} on {}", code.unwrap_or_else(|| "a device".into()));
    let mut doc = json!({"summary": summary, "device": change.device, "site": site, "severity": severity, "status": "Active", "value": change.value, "raised_at": at.to_rfc3339()});
    match node {
        None => doc["rule"] = json!(change.rule),
        Some(node) => (doc["flow"], doc["flow_node"]) = (json!(change.rule), json!(node)),
    }
    let made = k.insert_tx(&elevated(ctx), tx, "EdgeAlarm", doc.as_object().cloned().unwrap_or_default()).await?;
    let id = made.get("id").and_then(Value::as_str).and_then(|i| i.parse().ok());
    if severity == "Critical" {
        for user in engineers(ctx, tx).await? {
            notify::send(tx, ctx.tenant, user, &summary, None, Some("EdgeAlarm"), id).await?;
        }
    }
    if let Some(id) = id {
        let alarm = crate::alarms::load(tx, ctx.tenant, id).await?;
        notices::alarm_changed(tx, ctx, &alarm, notices::RAISED, at).await?;
    }
    Ok(())
}

/// Minutes a gateway can stay silent before someone should look.
const GATEWAY_SILENT_MINUTES: i32 = 5;

/// IoT Edge's own part of a workspace in its support report (Ferrum's `support::Health`): its
/// gateways in service that have spoken, those quiet for longer than a gateway's alarm waits, and
/// those whose last word named problems. Counts only, so a partner reading a customer's health
/// learns no gateway's name or address.
pub struct GatewayHealth;

#[async_trait::async_trait]
impl ferrum_kernel::support::Health for GatewayHealth {
    async fn health(&self, _k: &Kernel, tx: &mut Tx, tenant: Uuid) -> Result<Value> {
        let (total, quiet, troubled): (i64, i64, i64) = sqlx::query_as(
            "select count(*), count(*) filter (where g.seen_at < now() - make_interval(mins => $2)), \
                    count(*) filter (where jsonb_array_length(coalesce(g.problems, '[]')) > 0) \
             from edge_gateway g join e_device d on d.tenant_id = g.tenant_id and d.id = g.device_id \
             where g.tenant_id = $1 and d.lifecycle in ('Commissioned', 'Active')",
        )
        .bind(tenant)
        .bind(GATEWAY_SILENT_MINUTES)
        .fetch_one(&mut **tx)
        .await?;
        Ok(json!({"gateways": total, "quiet": quiet, "with_problems": troubled, "problems": gateway_problems(total, quiet, troubled)}))
    }
}

/// What someone looking after the workspace should hear of its gateways.
fn gateway_problems(total: i64, quiet: i64, troubled: i64) -> Vec<String> {
    let mut said = Vec::new();
    if quiet > 0 {
        said.push(format!("{quiet} of {total} gateways not heard from for {GATEWAY_SILENT_MINUTES} minutes"));
    }
    if troubled > 0 {
        said.push(format!("{troubled} of {total} gateways report problems"));
    }
    said
}

/// A gateway that stopped talking cannot raise an alarm about itself, so this does, and clears it
/// when it is back. One that was never heard from has not gone anywhere.
pub async fn watch_gateways(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<(Uuid, String, Option<Uuid>, bool, Option<Uuid>)> = sqlx::query_as(
        "select g.device_id, d.device_code, d.site, g.seen_at < now() - make_interval(mins => $2), \
                (select a.id from e_edge_alarm a where a.tenant_id = g.tenant_id and a.device = g.device_id and a.source = 'gateway' and a.status <> 'Cleared' limit 1) \
         from edge_gateway g join e_device d on d.tenant_id = g.tenant_id and d.id = g.device_id \
         where g.tenant_id = $1 and d.lifecycle in ('Commissioned', 'Active')",
    )
    .bind(ctx.tenant)
    .bind(GATEWAY_SILENT_MINUTES)
    .fetch_all(&mut *tx)
    .await?;
    let now = Utc::now();
    for (device, code, site, silent, open) in rows {
        match (silent, open) {
            (true, None) => {
                let doc = json!({"summary": format!("Gateway {code} has gone quiet"), "device": device, "site": site, "severity": "Major", "status": "Active", "source": "gateway", "raised_at": now.to_rfc3339()});
                let made = k.insert_tx(&elevated(ctx), &mut tx, "EdgeAlarm", doc.as_object().cloned().unwrap_or_default()).await?;
                if let Some(id) = made.get("id").and_then(Value::as_str).and_then(|i| i.parse().ok()) {
                    let alarm = crate::alarms::load(&mut tx, ctx.tenant, id).await?;
                    notices::alarm_changed(&mut tx, ctx, &alarm, notices::RAISED, now).await?;
                }
            }
            (false, Some(id)) => {
                let mut v = Doc::new();
                v.insert("status".into(), json!("Cleared"));
                v.insert("cleared_at".into(), json!(now.to_rfc3339()));
                k.set_values_tx(&sys(ctx), &mut tx, "EdgeAlarm", id, v).await?;
                crate::alarms::note(&mut tx, ctx.tenant, id, "the gateway", "Cleared", Some("It is talking again")).await?;
                notices::stand_down(&mut tx, ctx, id).await?;
                let alarm = crate::alarms::load(&mut tx, ctx.tenant, id).await?;
                notices::alarm_changed(&mut tx, ctx, &alarm, notices::CLEARED, now).await?;
            }
            _ => {}
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::gateway_problems;

    #[test]
    fn gateways_said_plainly() {
        assert_eq!(gateway_problems(3, 1, 0), ["1 of 3 gateways not heard from for 5 minutes"]);
        assert_eq!(gateway_problems(3, 0, 2), ["2 of 3 gateways report problems"]);
        assert!(gateway_problems(3, 0, 0).is_empty() && gateway_problems(0, 0, 0).is_empty());
    }
}
