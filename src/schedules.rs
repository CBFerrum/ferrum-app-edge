//! Asking devices for something by the clock. Each ask goes through the ledger as automation, so it
//! runs under an envelope someone approved or waits for a person, and a schedule can no more reach
//! past that than a flow can.

use crate::fleet::Selection;
use chrono::{DateTime, Datelike, TimeDelta, Timelike, Utc};
use ferrum_domain::util::{s, sys};
use ferrum_edge_contract::flow::minutes_of;
use ferrum_kernel::{Ctx, Doc, DocHook, Error, HookCtx, Kernel, Result};
use ferrum_ops::commands::{self, Issuer, NewCommand};
use serde_json::{Value, json};
use uuid::Uuid;

/// A time of day missed by more than this has passed: nobody wants 08:00's job at noon.
const LATE_MINUTES: i32 = 30;
const EXPIRES_SECS: i64 = 3600;

fn days_of(text: &str) -> Option<Vec<u32>> {
    text.split(',').map(str::trim).filter(|d| !d.is_empty()).map(|d| d.parse().ok().filter(|n| (1..=7).contains(n))).collect()
}

/// Whether a schedule should run now.
pub fn due(every_minutes: Option<i64>, at_time: Option<&str>, days: &str, offset_minutes: i64, last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    let local = |t: DateTime<Utc>| t + TimeDelta::minutes(offset_minutes);
    if let Some(every) = every_minutes.filter(|e| *e > 0) {
        return last.is_none_or(|l| now - l >= TimeDelta::minutes(every));
    }
    let (Some(at), Some(days)) = (at_time.and_then(minutes_of), days_of(days)) else { return false };
    let here = local(now);
    let minutes = (here.hour() * 60 + here.minute()) as i32;
    (days.is_empty() || days.contains(&here.weekday().number_from_monday()))
        && (at..at + LATE_MINUTES).contains(&minutes)
        && last.is_none_or(|l| local(l).date_naive() != here.date_naive())
}

pub struct ScheduleHook;

#[async_trait::async_trait]
impl DocHook for ScheduleHook {
    fn name(&self) -> &'static str {
        "edge.schedule"
    }

    async fn validate_change(&self, _hc: &mut HookCtx<'_>, _old: Option<&Doc>, doc: &Doc) -> Result<()> {
        let every = doc.get("every_minutes").and_then(Value::as_i64).filter(|e| *e > 0);
        let at = s(doc, "at_time").filter(|t| !t.is_empty());
        if every.is_some() == at.is_some() {
            return Err(Error::field("every_minutes", "say how often, or at what time of day, and not both"));
        }
        if at.is_some_and(|t| minutes_of(t).is_none()) {
            return Err(Error::field("at_time", "times of day are written 08:30"));
        }
        if days_of(s(doc, "days").unwrap_or_default()).is_none() {
            return Err(Error::field("days", "days go from 1, Monday, to 7"));
        }
        if ["site", "definition", "tag"].iter().all(|k| doc.get(*k).is_none_or(|v| v.is_null() || v == "")) {
            return Err(Error::field("definition", "say which devices: a site, a definition or a tag"));
        }
        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct Schedule {
    id: Uuid,
    every_minutes: Option<i64>,
    at_time: Option<String>,
    days: Option<String>,
    offset_minutes: Option<i64>,
    site: Option<Uuid>,
    definition: Option<Uuid>,
    tag: Option<String>,
    operation: String,
    payload: Option<Value>,
    last_run: Option<DateTime<Utc>>,
}

pub async fn run(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let now = Utc::now();
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let schedules: Vec<Schedule> = sqlx::query_as(
        "select id, every_minutes, at_time, days, offset_minutes, site, definition, tag, operation, payload, last_run from e_edge_schedule where tenant_id = $1 and enabled",
    )
    .bind(ctx.tenant)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    for one in schedules {
        if !due(one.every_minutes, one.at_time.as_deref(), one.days.as_deref().unwrap_or_default(), one.offset_minutes.unwrap_or(0), one.last_run, now) {
            continue;
        }
        let select = Selection { site: one.site, definition: one.definition, tag: one.tag.clone().filter(|t| !t.is_empty()), ..Default::default() };
        let devices = crate::fleet::chosen(k, ctx, &select).await;
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        let (mut asked, mut refused) = (0, Vec::new());
        match devices {
            Ok(devices) => {
                for (device, code) in devices {
                    let cmd = NewCommand {
                        target: device,
                        operation: one.operation.clone(),
                        schema_version: 1,
                        payload: one.payload.clone().unwrap_or(Value::Null),
                        expires_in_secs: EXPIRES_SECS,
                        dedupe_key: Some(format!("schedule:{}:{}:{device}", one.id, now.format("%Y%m%d%H%M"))),
                        workflow_id: Some(format!("schedule:{}", one.id)),
                        issuer: Issuer::Automation,
                        ..Default::default()
                    };
                    let mut attempt = k.db.tenant_tx(ctx.tenant).await?;
                    match commands::issue(&mut attempt, ctx, cmd).await {
                        Ok(_) => {
                            attempt.commit().await?;
                            asked += 1;
                        }
                        Err(e @ (Error::Validation(_) | Error::Invalid(_) | Error::Forbidden(_) | Error::NotFound(_))) => refused.push(format!("{code}: {e}")),
                        Err(e) => return Err(e),
                    }
                }
            }
            Err(e) => refused.push(e.to_string()),
        }
        let mut values = Doc::new();
        values.insert("last_run".into(), json!(now.to_rfc3339()));
        values.insert("last_result".into(), json!(format!("{asked} asked. {}", refused.iter().take(5).cloned().collect::<Vec<_>>().join("; "))));
        k.set_values_tx(&sys(ctx), &mut tx, "EdgeSchedule", one.id, values).await?;
        tx.commit().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn every_so_often() {
        let now = at("2026-11-02T10:00:00Z");
        assert!(due(Some(5), None, "", 0, None, now), "never run is due");
        assert!(!due(Some(5), None, "", 0, Some(at("2026-11-02T09:56:00Z")), now));
        assert!(due(Some(5), None, "", 0, Some(at("2026-11-02T09:55:00Z")), now));
    }

    #[test]
    fn at_a_time_of_day_once_a_day() {
        // 2026-11-02 is a Monday.
        let eight = at("2026-11-02T08:00:00Z");
        assert!(due(None, Some("08:00"), "", 0, None, eight));
        assert!(!due(None, Some("08:00"), "", 0, None, at("2026-11-02T07:59:00Z")), "not before");
        assert!(due(None, Some("08:00"), "1", 0, None, at("2026-11-02T08:20:00Z")), "a little late is better than not at all");
        assert!(!due(None, Some("08:00"), "", 0, None, at("2026-11-02T08:30:00Z")), "and much later is not what was asked for");
        assert!(!due(None, Some("08:00"), "", 0, Some(at("2026-11-02T08:00:00Z")), at("2026-11-02T08:05:00Z")), "once");
        assert!(due(None, Some("08:00"), "", 0, Some(at("2026-11-01T08:00:00Z")), eight), "and again the next day");
        assert!(!due(None, Some("08:00"), "6,7", 0, None, eight), "not on a Monday");
        assert!(due(None, Some("08:00"), "", 330, None, at("2026-11-02T02:30:00Z")), "by the site's clock");
    }
}
