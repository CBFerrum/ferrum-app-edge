//! New firmware for devices, a few at a time. Each device gets a command through the ledger, so what
//! stops a person asking stops this, and it stops by itself when devices start to fail: firmware that
//! bricks devices should reach as few of them as possible.
//!
//! It never reaches a robot, a controller, a machine or a fleet. Their firmware belongs to their
//! makers and to safety assessments that nothing here is part of, so those kinds cannot be selected
//! whatever a definition says.

use chrono::Utc;
use ferrum_domain::util::{id, s, sys};
use ferrum_edge_contract::{device_api, driver};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, HookCtx, Kernel, Result, audit};
use ferrum_ops::commands::{self, Issuer, NewCommand};
use serde_json::{Value, json};
use uuid::Uuid;

/// The only kinds of device this can touch.
const UPDATABLE: &[&str] = &["Sensor", "Meter", "Scanner", "Station"];
/// How long a device has to finish before its command runs out.
const PATIENCE_SECS: i64 = 24 * 3600;

/// Firmware is checked when it is saved, against what its definition allows.
pub struct FirmwareHook;

#[async_trait::async_trait]
impl DocHook for FirmwareHook {
    fn name(&self) -> &'static str {
        "edge.firmware"
    }

    async fn validate_change(&self, hc: &mut HookCtx<'_>, _old: Option<&Doc>, doc: &Doc) -> Result<()> {
        let hash = s(doc, "sha256").unwrap_or_default();
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::field("sha256", "that is not a SHA-256: 64 hex digits"));
        }
        if !s(doc, "url").is_some_and(|u| u.starts_with("https://") || u.starts_with("http://")) {
            return Err(Error::field("url", "say where a gateway can fetch it: https://…"));
        }
        let body: Option<String> = sqlx::query_scalar("select coalesce(published_body, body) from e_device_definition where tenant_id = $1 and id = $2")
            .bind(hc.ctx.tenant)
            .bind(id(doc, "definition"))
            .fetch_optional(&mut **hc.tx)
            .await?;
        if !body.and_then(|b| driver::parse(&b).ok()).is_some_and(|c| c.definition.firmware) {
            return Err(Error::field("definition", "its definition has to say firmware: true"));
        }
        Ok(())
    }
}

/// Starting and pausing. It is a person who starts one, and their name stays on it.
pub struct RolloutHook;

#[async_trait::async_trait]
impl DocHook for RolloutHook {
    fn name(&self) -> &'static str {
        "edge.rollout"
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, _params: &Value) -> Result<Option<Value>> {
        let record = id(doc, "id").ok_or_else(|| Error::internal("no id"))?;
        let status = s(doc, "status").unwrap_or("Draft");
        let mut values = Doc::new();
        match (name, status) {
            ("start", "Draft" | "Paused") => {
                values.insert("status".into(), json!("Running"));
                values.insert("started_by".into(), json!(hc.ctx.email));
                values.insert("note".into(), Value::Null);
            }
            ("pause", "Running") => {
                values.insert("status".into(), json!("Paused"));
            }
            ("start" | "pause", _) => return Err(Error::invalid(format!("this is {status}: it cannot {name} now"))),
            _ => return Ok(None),
        }
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "EdgeRollout", record, values).await?;
        audit::record(hc.tx, hc.ctx, "EdgeRollout", record, &format!("edge.rollout.{name}"), None, json!({"by": hc.ctx.email})).await?;
        Ok(Some(json!({"done": name})))
    }
}

#[derive(sqlx::FromRow)]
struct Rollout {
    id: Uuid,
    site: Option<Uuid>,
    tag: Option<String>,
    percent: Option<i64>,
    batch: Option<i64>,
    stop_after: Option<i64>,
    started_by: Option<String>,
    title: String,
    version: String,
    url: String,
    sha256: String,
    size: i64,
    definition: Uuid,
}

/// Moves every running rollout on by whatever has happened since.
pub async fn advance(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rollouts: Vec<Rollout> = sqlx::query_as(
        "select r.id, r.site, r.tag, r.percent, r.batch, r.stop_after, r.started_by, f.firmware_name as title, f.firmware_version as version, f.url, f.sha256, f.size, f.definition \
         from e_edge_rollout r join e_edge_firmware f on f.tenant_id = r.tenant_id and f.id = r.firmware where r.tenant_id = $1 and r.status = 'Running'",
    )
    .bind(ctx.tenant)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    for r in rollouts {
        one(k, ctx, &r).await?;
    }
    Ok(())
}

async fn one(k: &Kernel, ctx: &Ctx, r: &Rollout) -> Result<()> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    sqlx::query("select 1 from e_edge_rollout where tenant_id = $1 and id = $2 for update").bind(ctx.tenant).bind(r.id).execute(&mut *tx).await?;
    sqlx::query(
        "update edge_rollout_device d set status = case when c.state = 'Completed' then 'Done' else 'Failed' end, \
           error = case when c.state = 'Completed' then null else c.state end, at = now() \
         from op_command c where c.tenant_id = d.tenant_id and c.id = d.command_id and d.tenant_id = $1 and d.rollout_id = $2 and d.status = 'Sent' \
           and c.state in ('Completed', 'Failed', 'Rejected', 'Refused', 'Expired', 'Aborted', 'OutcomeUnknown')",
    )
    .bind(ctx.tenant)
    .bind(r.id)
    .execute(&mut *tx)
    .await?;
    let (sent, failed, reached): (i64, i64, i64) = sqlx::query_as(
        "select count(*) filter (where status = 'Sent'), count(*) filter (where status = 'Failed'), count(*) from edge_rollout_device where tenant_id = $1 and rollout_id = $2",
    )
    .bind(ctx.tenant)
    .bind(r.id)
    .fetch_one(&mut *tx)
    .await?;
    let stop_after = r.stop_after.unwrap_or(3);
    if failed >= stop_after {
        stop(k, ctx, &mut tx, r.id, "Paused", Some(format!("{failed} devices failed, so it stopped itself. Whatever is under way carries on."))).await?;
        tx.commit().await?;
        return Ok(());
    }
    let candidates: Vec<Uuid> = sqlx::query_scalar(
        "select d.id from e_device d where d.tenant_id = $1 and d.edge_definition = $2 and d.kind = any($3) and d.lifecycle in ('Commissioned', 'Active') \
           and ($4::uuid is null or d.site = $4) \
           and ($5::text is null or ',' || replace(lower(d.edge_tags), ' ', '') || ',' like '%,' || lower($5) || ',%') \
           and not exists (select 1 from edge_rollout_device x where x.tenant_id = d.tenant_id and x.rollout_id = $6 and x.device_id = d.id) \
           and not exists (select 1 from edge_attribute a where a.tenant_id = d.tenant_id and a.device_id = d.id and a.key in ('current_fw_version', 'fw_version') and a.value #>> '{}' = $7) \
         order by d.device_code",
    )
    .bind(ctx.tenant)
    .bind(r.definition)
    .bind(UPDATABLE.iter().map(|k| k.to_string()).collect::<Vec<_>>())
    .bind(r.site)
    .bind(r.tag.as_deref().filter(|t| !t.is_empty()))
    .bind(r.id)
    .bind(&r.version)
    .fetch_all(&mut *tx)
    .await?;
    let planned = (((reached as usize + candidates.len()) as f64) * r.percent.unwrap_or(100).clamp(1, 100) as f64 / 100.0).ceil() as usize;
    let room = (r.batch.unwrap_or(10).max(1) - sent).max(0) as usize;
    let take = room.min(planned.saturating_sub(reached as usize)).min(candidates.len());
    for device in &candidates[..take] {
        let cmd = NewCommand {
            target: *device,
            operation: device_api::FIRMWARE.to_string(),
            schema_version: 1,
            payload: json!({"title": r.title, "version": r.version, "url": r.url, "sha256": r.sha256, "size": r.size}),
            expires_in_secs: PATIENCE_SECS,
            dedupe_key: Some(format!("rollout:{}:{device}", r.id)),
            workflow_id: Some(format!("rollout:{}", r.id)),
            correlation_id: r.started_by.clone(),
            issuer: Issuer::Engine,
            ..Default::default()
        };
        let (command, status, error) = match commands::issue(&mut tx, ctx, cmd).await {
            Ok(issued) => (Some(issued.id), "Sent", None),
            Err(e @ (Error::Validation(_) | Error::Invalid(_) | Error::Forbidden(_) | Error::NotFound(_))) => (None, "Failed", Some(e.to_string())),
            Err(e) => return Err(e),
        };
        sqlx::query("insert into edge_rollout_device (tenant_id, rollout_id, device_id, command_id, status, error) values ($1, $2, $3, $4, $5, $6) on conflict do nothing")
            .bind(ctx.tenant)
            .bind(r.id)
            .bind(device)
            .bind(command)
            .bind(status)
            .bind(error)
            .execute(&mut *tx)
            .await?;
    }
    if take == 0 && sent == 0 && reached as usize >= planned {
        stop(k, ctx, &mut tx, r.id, "Done", None).await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn stop(k: &Kernel, ctx: &Ctx, tx: &mut ferrum_kernel::Tx, id: Uuid, status: &str, note: Option<String>) -> Result<()> {
    let mut values = Doc::new();
    values.insert("status".into(), json!(status));
    values.insert("note".into(), json!(note));
    k.set_values_tx(&sys(ctx), tx, "EdgeRollout", id, values).await
}

/// How a rollout stands, device by device.
pub async fn progress(k: &Kernel, ctx: &Ctx, rollout: Uuid) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(r order by r.at desc), '[]') from ( \
           select d.device_code as code, x.status, x.error, x.at from edge_rollout_device x join e_device d on d.tenant_id = x.tenant_id and d.id = x.device_id \
           where x.tenant_id = $1 and x.rollout_id = $2 order by x.at desc limit 500) r",
    )
    .bind(ctx.tenant)
    .bind(rollout)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"devices": rows, "at": Utc::now()}))
}
