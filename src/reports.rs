//! What IoT Edge can say on paper: which devices were there, what went wrong, and what they read.

use crate::schedules::{days_of, due};
use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use ferrum_domain::reports::{ReportSource, col};
use ferrum_domain::util::s;
use ferrum_edge_contract::flow::minutes_of;
use ferrum_kernel::{Ctx, Doc, DocHook, Error, HookCtx, Kernel, Result};
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

/// How many rows a mail shows of a report. The whole of it is attached.
const IN_THE_MAIL: usize = 20;

fn cell(v: &Value) -> String {
    let text = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    // A device's name came from outside: a spreadsheet must not run it as a formula.
    let text = if v.is_string() && text.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{text}") } else { text };
    if text.contains(['"', ',', '\n', '\r']) { format!("\"{}\"", text.replace('"', "\"\"")) } else { text }
}

/// A report as CSV: titled, in the order of its columns, with a line ending a spreadsheet expects.
pub fn csv(report: &Value) -> String {
    let columns = report["columns"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut lines = vec![columns.iter().map(|c| cell(&c["label"])).collect::<Vec<_>>().join(",")];
    for row in report["rows"].as_array().into_iter().flatten() {
        lines.push(columns.iter().map(|c| cell(&row[c["name"].as_str().unwrap_or_default()])).collect::<Vec<_>>().join(","));
    }
    lines.join("\r\n") + "\r\n"
}

fn escaped(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The first rows of a report as a table, with what a reader should know about the rest.
pub fn html(title: &str, report: &Value, limit: usize) -> String {
    let columns = report["columns"].as_array().map(Vec::as_slice).unwrap_or_default();
    let shown = |v: &Value| escaped(&v.as_str().map_or_else(|| if v.is_null() { String::new() } else { v.to_string() }, str::to_string));
    let head: String = columns.iter().map(|c| format!("<th align=\"left\">{}</th>", shown(&c["label"]))).collect();
    let rows = report["rows"].as_array().map(Vec::as_slice).unwrap_or_default();
    let body: String = rows
        .iter()
        .take(limit)
        .map(|r| format!("<tr>{}</tr>", columns.iter().map(|c| format!("<td>{}</td>", shown(&r[c["name"].as_str().unwrap_or_default()]))).collect::<String>()))
        .collect();
    let more = if rows.len() > limit { format!("<p>The first {limit} of {} rows.</p>", rows.len()) } else { String::new() };
    format!("<h1>{}</h1><table cellpadding=\"4\"><tr>{head}</tr>{body}</table>{more}", escaped(title))
}

#[derive(sqlx::FromRow)]
struct Scheduled {
    id: Uuid,
    schedule_name: String,
    report: String,
    format: Option<String>,
    days_back: Option<i64>,
    at_time: Option<String>,
    days: Option<String>,
    offset_minutes: Option<i64>,
    site: Option<Uuid>,
    device: Option<Uuid>,
    metric: Option<String>,
    last_run: Option<DateTime<Utc>>,
}

/// Runs what is due. The mail goes through the platform's outbox, in one transaction with the note
/// that it ran, so a report is neither lost nor sent twice by a crash here, and a mailer that is down
/// is retried there. A report that cannot be made is said on the schedule and not tried again until
/// its next day: it is someone's to fix, and trying every minute changes nothing.
pub async fn deliver(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let now = Utc::now();
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let schedules: Vec<Scheduled> = sqlx::query_as(
        "select id, schedule_name, report, format, days_back, at_time, days, offset_minutes, site, device, metric, last_run \
         from e_edge_report_schedule where tenant_id = $1 and enabled",
    )
    .bind(ctx.tenant)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    for s in schedules {
        if !due(None, s.at_time.as_deref(), s.days.as_deref().unwrap_or_default(), s.offset_minutes.unwrap_or(0), s.last_run, now) {
            continue;
        }
        if let Err(e) = send(k, ctx, &s, now).await {
            tracing::warn!(tenant = %ctx.tenant, schedule = %s.id, "a report did not go out: {e}");
            let mut tx = k.db.tenant_tx(ctx.tenant).await?;
            noted(&mut tx, ctx, s.id, now, &format!("It did not go out: {e}")).await?;
            tx.commit().await?;
        }
    }
    Ok(())
}

async fn noted(tx: &mut ferrum_kernel::Tx, ctx: &Ctx, id: Uuid, now: DateTime<Utc>, result: &str) -> Result<()> {
    sqlx::query("update e_edge_report_schedule set last_run = $3, last_result = $4 where tenant_id = $1 and id = $2")
        .bind(ctx.tenant)
        .bind(id)
        .bind(now)
        .bind(result)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn send(k: &Kernel, ctx: &Ctx, s: &Scheduled, now: DateTime<Utc>) -> Result<()> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let to: Vec<String> = sqlx::query_scalar("select email from e_edge_report_schedule__recipients where tenant_id = $1 and parent_id = $2 order by idx")
        .bind(ctx.tenant)
        .bind(s.id)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    if to.is_empty() {
        return Err(Error::invalid("it names nobody to send it to"));
    }
    let mut params = HashMap::from([
        ("from_date".to_string(), (now - Duration::days(s.days_back.unwrap_or(7))).date_naive().to_string()),
        ("to_date".to_string(), now.date_naive().to_string()),
    ]);
    params.extend(s.site.map(|v| ("site".to_string(), v.to_string())));
    params.extend(s.device.map(|v| ("device".to_string(), v.to_string())));
    params.extend(s.metric.clone().map(|v| ("metric".to_string(), v)));
    let report = EdgeReports.run(k, ctx, &s.report, &params).await?;
    let stem = format!("{} {}", s.schedule_name, now.date_naive());
    let (name, kind, data) = if s.format.as_deref() == Some("PDF") {
        let page = format!("<!doctype html><meta charset=\"utf-8\">{}", html(&stem, &report, MAX_ROWS as usize));
        (format!("{stem}.pdf"), "application/pdf", k.html_to_pdf(&page).await?)
    } else {
        (format!("{stem}.csv"), "text/csv", csv(&report).into_bytes())
    };
    // ponytail: each run's file stays on the schedule as the record of what went out, and counts
    // against the workspace's storage. Delete old ones from there if that ever matters.
    let kept = k.store_file(ctx, Some(("EdgeReportSchedule", s.id)), &name, kind, &data).await;
    let can_attach = k.mailer.as_ref().is_some_and(|m| m.can_attach());
    let files: Vec<Uuid> = match &kept {
        Ok(file) if can_attach => file["id"].as_str().and_then(|i| i.parse().ok()).into_iter().collect(),
        Ok(_) => vec![],
        Err(e) => {
            tracing::warn!(tenant = %ctx.tenant, "a report's file was not kept, so it goes in the mail alone: {e}");
            vec![]
        }
    };
    let body = html(&s.schedule_name, &report, IN_THE_MAIL);
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    for person in &to {
        k.queue_mail_with(&mut tx, ctx.tenant, person, &stem, &body, &files).await?;
    }
    noted(&mut tx, ctx, s.id, now, &format!("Sent to {} people, {} rows", to.len(), report["rows"].as_array().map_or(0, Vec::len))).await?;
    tx.commit().await?;
    Ok(())
}

pub struct ReportScheduleHook;

#[async_trait]
impl DocHook for ReportScheduleHook {
    fn name(&self) -> &'static str {
        "edge.report_schedule"
    }

    async fn validate_change(&self, _hc: &mut HookCtx<'_>, _old: Option<&Doc>, doc: &Doc) -> Result<()> {
        if !s(doc, "at_time").is_some_and(|t| minutes_of(t).is_some()) {
            return Err(Error::field("at_time", "times of day are written 08:30"));
        }
        if days_of(s(doc, "days").unwrap_or_default()).is_none() {
            return Err(Error::field("days", "days go from 1, Monday, to 7"));
        }
        if !doc.get("recipients").and_then(Value::as_array).is_some_and(|r| !r.is_empty()) {
            return Err(Error::field("recipients", "say who it goes to"));
        }
        if s(doc, "report") == Some("edge_readings") && (s(doc, "device").is_none() || s(doc, "metric").is_none()) {
            return Err(Error::field("device", "readings by the hour are of one device and one metric"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_as_csv_cannot_run_anything() {
        let report = json!({
            "columns": [{"name": "device", "label": "Device"}, {"name": "note", "label": "Note, or not"}, {"name": "n", "label": "N"}],
            "rows": [{"device": "=HYPERLINK(1)", "note": "a,\"b\"", "n": -3}, {"device": "P1"}],
        });
        assert_eq!(csv(&report), "Device,\"Note, or not\",N\r\n'=HYPERLINK(1),\"a,\"\"b\"\"\",-3\r\nP1,,\r\n");
    }

    #[test]
    fn a_mail_shows_the_first_rows_and_says_so() {
        let report = json!({"columns": [{"name": "d", "label": "D"}], "rows": [{"d": "<b>"}, {"d": 2}, {"d": 3}]});
        let page = html("Week & day", &report, 2);
        assert!(page.contains("<h1>Week &amp; day</h1>"));
        assert!(page.contains("<td>&lt;b&gt;</td><"), "a name from outside is shown, and does nothing: {page}");
        assert!(page.contains("The first 2 of 3 rows"));
        assert!(!page.contains("<td>3</td>"));
    }
}
