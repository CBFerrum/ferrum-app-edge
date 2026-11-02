//! What IoT Edge can say on paper: which devices were there, what went wrong, and what they read.

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use ferrum_domain::reports::{ReportSource, col};
use ferrum_kernel::{Ctx, Error, Kernel, Result};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

const MAX_ROWS: i64 = 5_000;

pub struct EdgeReports;

fn date(params: &HashMap<String, String>, key: &str) -> Option<DateTime<Utc>> {
    params.get(key)?.parse::<NaiveDate>().ok()?.and_hms_opt(0, 0, 0).map(|d| d.and_utc())
}

fn period(params: &HashMap<String, String>) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let to = date(params, "to_date").map_or_else(Utc::now, |d| d + Duration::days(1));
    let from = date(params, "from_date").unwrap_or(to - Duration::days(7));
    if to <= from {
        return Err(Error::invalid("the period ends before it starts"));
    }
    Ok((from, to))
}

#[async_trait]
impl ReportSource for EdgeReports {
    fn catalog(&self) -> Vec<Value> {
        let from = json!({"name": "from_date", "label": "From Date", "type": "date"});
        let to = json!({"name": "to_date", "label": "To Date", "type": "date", "default": "today"});
        let site = json!({"name": "site", "label": "Site", "type": "link", "to": "Site"});
        let device = json!({"name": "device", "label": "Device", "type": "link", "to": "Device"});
        let metric = json!({"name": "metric", "label": "Metric", "type": "data"});
        vec![
            json!({"name": "edge_availability", "label": "Device Availability", "module": "edge", "entity": "Device", "scoped": false, "params": [from, to, site]}),
            json!({"name": "edge_alarm_history", "label": "Alarm History", "module": "edge", "entity": "EdgeAlarm", "scoped": false, "params": [from, to, site]}),
            json!({"name": "edge_readings", "label": "Readings by the Hour", "module": "edge", "entity": "Device", "scoped": false, "params": [device, metric, from, to]}),
        ]
    }

    async fn run(&self, k: &Kernel, ctx: &Ctx, name: &str, params: &HashMap<String, String>) -> Result<Value> {
        let (from, to) = period(params)?;
        let site = params.get("site").and_then(|s| s.parse::<Uuid>().ok());
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        let out = match name {
            "edge_availability" => {
                let rows: Vec<Value> = sqlx::query_scalar(
                    "select jsonb_build_object('device', d.device_code, 'name', d.device_name, 'site', s.site_name, 'minutes', h.minutes, \
                       'percent', round(h.minutes * 100.0 / greatest(extract(epoch from least($3, now()) - $2) / 60, 1), 1), 'last_seen', h.last_seen, \
                       'alarms', (select count(*) from e_edge_alarm a where a.tenant_id = d.tenant_id and a.device = d.id and a.raised_at >= $2 and a.raised_at < $3)) \
                     from e_device d left join e_site s on s.tenant_id = d.tenant_id and s.id = d.site \
                     left join lateral (select count(distinct r.minute) as minutes, max(r.last_time) as last_seen from op_rollup_minute r \
                       where r.tenant_id = d.tenant_id and r.device_id = d.id and r.minute >= $2 and r.minute < $3) h on true \
                     where d.tenant_id = $1 and d.lifecycle <> 'Retired' and d.kind <> 'Gateway' and ($4::uuid is null or d.site = $4) order by d.device_code limit $5",
                )
                .bind(ctx.tenant)
                .bind(from)
                .bind(to)
                .bind(site)
                .bind(MAX_ROWS)
                .fetch_all(&mut *tx)
                .await?;
                json!({
                    "columns": [col("device", "Device", "data"), col("name", "Name", "data"), col("site", "Site", "data"), col("minutes", "Minutes Heard", "int"),
                        col("percent", "Availability %", "float"), col("last_seen", "Last Heard", "datetime"), col("alarms", "Alarms", "int")],
                    "totals": {"count": rows.len()},
                    "rows": rows,
                    "meta": {"from": from, "to": to},
                })
            }
            "edge_alarm_history" => {
                let rows: Vec<Value> = sqlx::query_scalar(
                    "select jsonb_build_object('raised_at', a.raised_at, 'severity', a.severity, 'summary', a.summary, 'status', a.status, 'cleared_at', a.cleared_at, \
                       'acknowledged_by', a.acknowledged_by, 'assignee', a.assignee_email, \
                       'minutes_open', round(extract(epoch from coalesce(a.cleared_at, now()) - a.raised_at) / 60)) \
                     from e_edge_alarm a where a.tenant_id = $1 and a.raised_at >= $2 and a.raised_at < $3 and ($4::uuid is null or a.site = $4) order by a.raised_at desc limit $5",
                )
                .bind(ctx.tenant)
                .bind(from)
                .bind(to)
                .bind(site)
                .bind(MAX_ROWS)
                .fetch_all(&mut *tx)
                .await?;
                let open = rows.iter().filter(|r| r["status"] != "Cleared").count();
                json!({
                    "columns": [col("raised_at", "Raised", "datetime"), col("severity", "Severity", "data"), col("summary", "Alarm", "data"), col("status", "Status", "data"),
                        col("cleared_at", "Cleared", "datetime"), col("minutes_open", "Minutes Open", "int"), col("acknowledged_by", "Acknowledged By", "data"), col("assignee", "Assigned To", "data")],
                    "totals": {"count": rows.len(), "open": open},
                    "rows": rows,
                    "meta": {"from": from, "to": to},
                })
            }
            "edge_readings" => {
                let device = params.get("device").and_then(|d| d.parse::<Uuid>().ok()).ok_or_else(|| Error::invalid("say which device"))?;
                let metric = params.get("metric").filter(|m| !m.is_empty()).ok_or_else(|| Error::invalid("say which number"))?;
                ferrum_ops::site::device_site(k, ctx, device).await?;
                let rows: Vec<Value> = sqlx::query_scalar(
                    "select jsonb_build_object('at', date_trunc('hour', minute), 'samples', sum(samples), 'bad', sum(bad), 'min', min(min_value), 'max', max(max_value), \
                       'avg', sum(sum_value) / nullif(sum(samples), 0)) \
                     from op_rollup_minute where tenant_id = $1 and device_id = $2 and metric = $3 and minute >= $4 and minute < $5 \
                     group by date_trunc('hour', minute) order by date_trunc('hour', minute) limit $6",
                )
                .bind(ctx.tenant)
                .bind(device)
                .bind(metric)
                .bind(from)
                .bind(to)
                .bind(MAX_ROWS)
                .fetch_all(&mut *tx)
                .await?;
                json!({
                    "columns": [col("at", "Hour", "datetime"), col("samples", "Samples", "int"), col("bad", "Bad", "int"), col("min", "Lowest", "float"), col("max", "Highest", "float"), col("avg", "Average", "float")],
                    "totals": {"count": rows.len()},
                    "rows": rows,
                    "meta": {"from": from, "to": to},
                })
            }
            _ => return Err(Error::NotFound(format!("report {name}"))),
        };
        tx.rollback().await?;
        Ok(out)
    }
}
