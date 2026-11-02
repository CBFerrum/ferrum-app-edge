//! Charging sessions, above the commands that start and stop them (Ferrum docs/platform/decisions.md,
//! D-16). A command ends when a charger answers it, and that is all it knows: `Accepted` to a remote start
//! means it will try. What happened is in what the charger and its meter said afterwards, which arrives
//! late, twice and out of order, so a session is worked out again from all of it each time anything
//! arrives. It is `charging` only once a register has moved, and it is money only once the evidence is
//! clean or a person has said why it is good enough.

use chrono::{DateTime, Utc};
use ferrum_edge_contract::ocpp::{self, Evidence, Reading};
use ferrum_kernel::{Ctx, Error, Kernel, Result, Tx, audit};
use serde_json::{Value, json};
use uuid::Uuid;

/// What a charger said, in any version, as one thing that can be applied.
struct Said {
    transaction: String,
    started: Option<DateTime<Utc>>,
    stopped: Option<DateTime<Utc>>,
    stop_reason: Option<String>,
    id_token: Option<String>,
    /// Register readings the frame gave as plain fields, which 1.6 does at each end.
    edges: Vec<Reading>,
}

fn time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()?.parse().ok()
}

fn plain_wh(at: DateTime<Utc>, value: &Value) -> Option<Reading> {
    Some(Reading { at, value: value.as_f64()?, unit: Some("Wh".into()), signed: false })
}

fn read(action: &str, body: &Value) -> Option<Said> {
    let said = &body["said"];
    let id = |v: &Value| v.as_i64().map(|n| n.to_string()).or_else(|| v.as_str().map(str::to_string));
    match action {
        "StartTransaction" => {
            let at = time(&said["timestamp"])?;
            Some(Said {
                transaction: id(&body["transaction"])?,
                started: Some(at),
                stopped: None,
                stop_reason: None,
                id_token: said["idTag"].as_str().map(str::to_string),
                edges: plain_wh(at, &said["meterStart"]).into_iter().collect(),
            })
        }
        "StopTransaction" => {
            let at = time(&said["timestamp"])?;
            Some(Said {
                transaction: id(&said["transactionId"])?,
                started: None,
                stopped: Some(at),
                stop_reason: said["reason"].as_str().map(str::to_string),
                id_token: None,
                edges: plain_wh(at, &said["meterStop"]).into_iter().collect(),
            })
        }
        "MeterValues" => {
            Some(Said { transaction: id(&said["transactionId"])?, started: None, stopped: None, stop_reason: None, id_token: None, edges: vec![] })
        }
        "TransactionEvent" => {
            let at = time(&said["timestamp"])?;
            let kind = said["eventType"].as_str().unwrap_or_default();
            Some(Said {
                transaction: id(&said["transactionInfo"]["transactionId"])?,
                started: (kind == "Started").then_some(at),
                stopped: (kind == "Ended").then_some(at),
                stop_reason: said["transactionInfo"]["stoppedReason"].as_str().map(str::to_string),
                id_token: said["idToken"]["idToken"].as_str().map(str::to_string),
                edges: vec![],
            })
        }
        _ => None,
    }
}

/// Applies what a gateway kept, in whatever order it arrives. Hearing it twice changes nothing.
pub async fn heard(tx: &mut Tx, tenant: Uuid, body: &Value) -> Result<()> {
    let (Some(code), Some(action)) = (body["code"].as_str(), body["action"].as_str()) else { return Ok(()) };
    let device: Option<Uuid> =
        sqlx::query_scalar("select id from e_device where tenant_id = $1 and device_code = $2").bind(tenant).bind(code).fetch_optional(&mut **tx).await?;
    let (Some(device), Some(said)) = (device, read(action, body)) else {
        tracing::warn!(%tenant, code, action, "a charger said something that could not be placed");
        return Ok(());
    };
    let id: Uuid = sqlx::query_scalar(
        "insert into edge_charging_session (tenant_id, id, device_id, transaction, id_token, started_at, stopped_at, stop_reason) values ($1, $2, $3, $4, $5, $6, $7, $8) \
         on conflict (tenant_id, device_id, transaction) do update set id_token = coalesce(edge_charging_session.id_token, excluded.id_token), \
           started_at = coalesce(edge_charging_session.started_at, excluded.started_at), stopped_at = coalesce(edge_charging_session.stopped_at, excluded.stopped_at), \
           stop_reason = coalesce(edge_charging_session.stop_reason, excluded.stop_reason) returning id",
    )
    .bind(tenant)
    .bind(Uuid::now_v7())
    .bind(device)
    .bind(&said.transaction)
    .bind(&said.id_token)
    .bind(said.started)
    .bind(said.stopped)
    .bind(&said.stop_reason)
    .fetch_one(&mut **tx)
    .await?;
    for r in said.edges.iter().cloned().chain(ocpp::registers(&body["said"])) {
        sqlx::query(
            "insert into edge_charging_reading (tenant_id, session_id, at, value, unit, signed) values ($1, $2, $3, $4, $5, $6) on conflict do nothing",
        )
        .bind(tenant)
        .bind(id)
        .bind(r.at)
        .bind(r.value)
        .bind(r.unit.unwrap_or_default())
        .bind(r.signed)
        .execute(&mut **tx)
        .await?;
    }
    judge(tx, tenant, id).await
}

/// What everything said so far comes to. A settled session stays as it was settled.
async fn judge(tx: &mut Tx, tenant: Uuid, id: Uuid) -> Result<()> {
    let (started, stopped, state): (Option<DateTime<Utc>>, Option<DateTime<Utc>>, String) =
        sqlx::query_as("select started_at, stopped_at, state from edge_charging_session where tenant_id = $1 and id = $2 for update")
            .bind(tenant)
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    if state == "settled" {
        return Ok(());
    }
    let rows: Vec<(DateTime<Utc>, f64, String, bool)> =
        sqlx::query_as("select at, value, unit, signed from edge_charging_reading where tenant_id = $1 and session_id = $2 order by at")
            .bind(tenant)
            .bind(id)
            .fetch_all(&mut **tx)
            .await?;
    let readings: Vec<Reading> =
        rows.into_iter().map(|(at, value, unit, signed)| Reading { at, value, unit: (!unit.is_empty()).then_some(unit), signed }).collect();
    let (evidence, flags) = match started {
        Some(started) => {
            let e = ocpp::evidence(readings, started, stopped, None);
            let flags = json!(e.flags);
            (e, flags)
        }
        // Readings came before the start did, or the start never came.
        None => (Evidence { energy_wh: None, flags: vec!["no_start_heard"], flowed: false }, json!(["no_start_heard"])),
    };
    let state = match (stopped, evidence.flowed) {
        (Some(_), _) => "stopped",
        (None, true) => "charging",
        (None, false) => "started",
    };
    sqlx::query("update edge_charging_session set state = $3, energy_wh = $4, flags = $5 where tenant_id = $1 and id = $2")
        .bind(tenant)
        .bind(id)
        .bind(state)
        .bind(evidence.energy_wh)
        .bind(flags)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn list(k: &Kernel, ctx: &Ctx, device: Option<Uuid>) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "select to_jsonb(s) - 'tenant_id' || jsonb_build_object('device_code', d.device_code) from edge_charging_session s \
         join e_device d on d.tenant_id = s.tenant_id and d.id = s.device_id where s.tenant_id = $1 and ($2::uuid is null or s.device_id = $2) \
         order by s.started_at desc nulls last limit 200",
    )
    .bind(ctx.tenant)
    .bind(device)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"sessions": rows}))
}

pub struct Settle {
    pub reference: String,
    /// A figure a person stands behind, instead of what the evidence gave, and why.
    pub energy_wh: Option<f64>,
    pub note: Option<String>,
}

/// Makes a stopped session money. Clean evidence settles itself on a person's word. Anything else takes a
/// figure and a reason from them, which stays on the session and on the record.
pub async fn settle(k: &Kernel, ctx: &Ctx, id: Uuid, s: Settle) -> Result<Value> {
    if s.reference.trim().is_empty() {
        return Err(Error::field("reference", "what this settles against"));
    }
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let row: Option<(String, Option<f64>, Value)> =
        sqlx::query_as("select state, energy_wh::float8, flags from edge_charging_session where tenant_id = $1 and id = $2 for update")
            .bind(ctx.tenant)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (state, energy, flags) = row.ok_or_else(|| Error::NotFound("session".into()))?;
    if state != "stopped" {
        return Err(Error::Conflict(format!("this session is {state}: only a stopped one can be settled")));
    }
    let clean = flags.as_array().is_some_and(Vec::is_empty) && energy.is_some();
    let overridden = s.energy_wh.is_some() || s.note.is_some();
    if overridden && !(s.energy_wh.is_some_and(f64::is_finite) && s.energy_wh >= Some(0.0) && s.note.as_deref().is_some_and(|n| !n.trim().is_empty())) {
        return Err(Error::field("note", "a figure of your own needs the figure, and why"));
    }
    if !clean && !overridden {
        return Err(Error::Conflict(format!("the evidence does not settle this by itself: {flags}")));
    }
    let settled = s.energy_wh.or(energy);
    sqlx::query(
        "update edge_charging_session set state = 'settled', energy_wh = $3, settled_by = $4, settled_at = now(), settlement_ref = $5, settlement_note = $6 where tenant_id = $1 and id = $2",
    )
    .bind(ctx.tenant)
    .bind(id)
    .bind(settled)
    .bind(ctx.user)
    .bind(s.reference.trim())
    .bind(&s.note)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        ctx,
        "ChargingSession",
        id,
        "edge.charging.settled",
        None,
        json!({"energy_wh": settled, "flags": flags, "note": s.note, "reference": s.reference}),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({"state": "settled", "energy_wh": settled}))
}
