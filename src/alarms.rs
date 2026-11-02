//! What people do about an alarm. Equipment raises and clears them; a person can say they have seen
//! one, take it on, note what they found, close it themselves when a condition has gone with no one
//! left to say so, or shelve it while they work on something they already know about. Everything
//! done to an alarm goes on its timeline.

use crate::notices;
use chrono::{DateTime, Duration, Utc};
use ferrum_domain::util::sys;
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result, Tx};
use serde_json::{Value, json};
use uuid::Uuid;

/// What a notice needs to say about an alarm.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Alarm {
    pub id: Uuid,
    pub summary: String,
    pub severity: String,
    pub site: Option<Uuid>,
    pub device: Option<Uuid>,
    pub raised_at: DateTime<Utc>,
    pub code: Option<String>,
    pub site_name: Option<String>,
}

impl Alarm {
    pub fn words(&self) -> Vec<(&'static str, String)> {
        vec![
            ("summary", self.summary.clone()),
            ("severity", self.severity.clone()),
            ("device", self.code.clone().unwrap_or_default()),
            ("site", self.site_name.clone().unwrap_or_default()),
            ("raised_at", self.raised_at.to_rfc3339()),
        ]
    }
}

pub async fn load(tx: &mut Tx, tenant: Uuid, id: Uuid) -> Result<Alarm> {
    sqlx::query_as(
        "select a.id, coalesce(a.summary, '') as summary, coalesce(a.severity, 'Warning') as severity, a.site, a.device, a.raised_at, d.device_code as code, s.site_name \
         from e_edge_alarm a left join e_device d on d.tenant_id = a.tenant_id and d.id = a.device left join e_site s on s.tenant_id = a.tenant_id and s.id = a.site \
         where a.tenant_id = $1 and a.id = $2",
    )
    .bind(tenant)
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| Error::NotFound("alarm".into()))
}

/// A line on an alarm's timeline: what happened and who did it, or a person's words.
pub async fn note(tx: &mut Tx, tenant: Uuid, alarm: Uuid, by: &str, kind: &str, text: Option<&str>) -> Result<()> {
    sqlx::query("insert into edge_alarm_note (tenant_id, id, alarm_id, kind, by_whom, text) values ($1, $2, $3, $4, $5, $6)")
        .bind(tenant)
        .bind(Uuid::now_v7())
        .bind(alarm)
        .bind(kind)
        .bind(by)
        .bind(text)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn locked_status(tx: &mut Tx, tenant: Uuid, id: Uuid) -> Result<String> {
    let status: Option<Option<String>> = sqlx::query_scalar("select status from e_edge_alarm where tenant_id = $1 and id = $2 for update")
        .bind(tenant)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(status.ok_or_else(|| Error::NotFound("alarm".into()))?.unwrap_or_default())
}

pub async fn acknowledge(k: &Kernel, ctx: &Ctx, id: Uuid, text: Option<&str>) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let status = locked_status(&mut tx, ctx.tenant, id).await?;
    if status != "Active" {
        return Err(Error::invalid(format!("this alarm is {status}")));
    }
    let mut v = Doc::new();
    v.insert("status".into(), json!("Acknowledged"));
    v.insert("acknowledged_by".into(), json!(ctx.email));
    v.insert("acknowledged_at".into(), json!(Utc::now().to_rfc3339()));
    v.insert("note".into(), json!(text));
    k.set_values_tx(&sys(ctx), &mut tx, "EdgeAlarm", id, v).await?;
    note(&mut tx, ctx.tenant, id, &ctx.email, "Acknowledged", text).await?;
    notices::stand_down(&mut tx, ctx, id).await?;
    let alarm = load(&mut tx, ctx.tenant, id).await?;
    notices::alarm_changed(&mut tx, ctx, &alarm, notices::ACKNOWLEDGED, Utc::now()).await?;
    tx.commit().await?;
    Ok(json!({"status": "Acknowledged"}))
}

/// Closes an alarm by hand, for when nothing will ever report that the condition went. It says who
/// did: the equipment did not.
pub async fn clear(k: &Kernel, ctx: &Ctx, id: Uuid, text: Option<&str>) -> Result<Value> {
    let text = text.map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| Error::field("note", "say why it can be closed"))?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    if locked_status(&mut tx, ctx.tenant, id).await? == "Cleared" {
        return Err(Error::invalid("this alarm has cleared"));
    }
    let mut v = Doc::new();
    v.insert("status".into(), json!("Cleared"));
    v.insert("cleared_at".into(), json!(Utc::now().to_rfc3339()));
    v.insert("cleared_by".into(), json!(ctx.email));
    k.set_values_tx(&sys(ctx), &mut tx, "EdgeAlarm", id, v).await?;
    note(&mut tx, ctx.tenant, id, &ctx.email, "Closed by hand", Some(text)).await?;
    notices::stand_down(&mut tx, ctx, id).await?;
    let alarm = load(&mut tx, ctx.tenant, id).await?;
    notices::alarm_changed(&mut tx, ctx, &alarm, notices::CLEARED, Utc::now()).await?;
    tx.commit().await?;
    Ok(json!({"status": "Cleared"}))
}

pub async fn assign(k: &Kernel, ctx: &Ctx, id: Uuid, user: Option<Uuid>) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    locked_status(&mut tx, ctx.tenant, id).await?;
    let email: Option<String> = match user {
        Some(user) => Some(
            sqlx::query_scalar("select email from app_user where tenant_id = $1 and id = $2")
                .bind(ctx.tenant)
                .bind(user)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| Error::field("user", "no such person"))?,
        ),
        None => None,
    };
    let mut v = Doc::new();
    v.insert("assignee".into(), json!(user));
    v.insert("assignee_email".into(), json!(email));
    k.set_values_tx(&sys(ctx), &mut tx, "EdgeAlarm", id, v).await?;
    note(&mut tx, ctx.tenant, id, &ctx.email, if user.is_some() { "Assigned" } else { "Unassigned" }, email.as_deref()).await?;
    if let Some(user) = user.filter(|u| *u != ctx.user) {
        let alarm = load(&mut tx, ctx.tenant, id).await?;
        ferrum_kernel::notify::send_kind(
            &mut tx,
            ctx.tenant,
            user,
            "edge.alarm",
            "inapp",
            &format!("Yours now: {}", alarm.summary),
            None,
            Some("EdgeAlarm"),
            Some(id),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(json!({"assignee": user}))
}

/// Puts off telling anyone about an alarm for a while, while it is known about. It stays raised and
/// on every screen: shelving silences, it does not hide.
pub async fn shelve(k: &Kernel, ctx: &Ctx, id: Uuid, minutes: i64) -> Result<Value> {
    if !(0..=7 * 24 * 60).contains(&minutes) {
        return Err(Error::field("minutes", "at most a week"));
    }
    let until = (minutes > 0).then(|| Utc::now() + Duration::minutes(minutes));
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    if locked_status(&mut tx, ctx.tenant, id).await? == "Cleared" {
        return Err(Error::invalid("this alarm has cleared"));
    }
    let mut v = Doc::new();
    v.insert("shelved_until".into(), json!(until.map(|u| u.to_rfc3339())));
    k.set_values_tx(&sys(ctx), &mut tx, "EdgeAlarm", id, v).await?;
    note(&mut tx, ctx.tenant, id, &ctx.email, if until.is_some() { "Shelved" } else { "Unshelved" }, until.map(|u| u.to_rfc3339()).as_deref()).await?;
    tx.commit().await?;
    Ok(json!({"shelved_until": until}))
}

pub async fn comment(k: &Kernel, ctx: &Ctx, id: Uuid, text: Option<&str>) -> Result<Value> {
    let text =
        text.map(str::trim).filter(|t| !t.is_empty() && t.len() <= 4000).ok_or_else(|| Error::field("text", "say something, in at most 4,000 characters"))?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    locked_status(&mut tx, ctx.tenant, id).await?;
    note(&mut tx, ctx.tenant, id, &ctx.email, "Note", Some(text)).await?;
    tx.commit().await?;
    Ok(json!({"noted": true}))
}

/// An alarm with everything said and done about it, and who was told.
pub async fn detail(k: &Kernel, ctx: &Ctx, id: Uuid) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let alarm: Option<Value> = sqlx::query_scalar(
        "select jsonb_build_object('id', a.id, 'summary', a.summary, 'severity', a.severity, 'status', a.status, 'raised_at', a.raised_at, 'cleared_at', a.cleared_at, \
                'cleared_by', a.cleared_by, 'acknowledged_by', a.acknowledged_by, 'acknowledged_at', a.acknowledged_at, 'value', a.value, 'device', a.device, 'code', d.device_code, \
                'assignee', a.assignee, 'assignee_email', a.assignee_email, 'shelved_until', a.shelved_until, 'rule', a.rule, 'flow', a.flow) \
         from e_edge_alarm a left join e_device d on d.tenant_id = a.tenant_id and d.id = a.device \
         where a.tenant_id = $1 and a.id = $2",
    )
    .bind(ctx.tenant)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let mut alarm = alarm.ok_or_else(|| Error::NotFound("alarm".into()))?;
    let notes: Vec<Value> = sqlx::query_scalar("select jsonb_build_object('kind', kind, 'by', by_whom, 'text', text, 'at', at) from edge_alarm_note where tenant_id = $1 and alarm_id = $2 order by at")
        .bind(ctx.tenant)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    let told: Vec<Value> = sqlx::query_scalar(
        "select jsonb_build_object('channel', channel, 'target', target, 'status', status, 'due_at', due_at, 'sent_at', sent_at, 'error', error) from edge_notice where tenant_id = $1 and alarm_id = $2 order by due_at",
    )
    .bind(ctx.tenant)
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    alarm["notes"] = json!(notes);
    alarm["notices"] = json!(told);
    Ok(alarm)
}
