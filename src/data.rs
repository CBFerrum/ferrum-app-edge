//! Numbers over time, and what a dashboard shows. Samples last days and minute rollups a year, so a
//! short period is drawn from samples and a long one from rollups, and a chart asks for as many points
//! as it can draw, not as many as there are.

use chrono::{DateTime, Duration, Utc};
use ferrum_kernel::{Ctx, Error, Kernel, Result};
use ferrum_ops::telemetry;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use uuid::Uuid;

/// The longest period, about as long as rollups last.
const MAX_DAYS: i64 = 400;
/// Metrics on one chart. More cannot be told apart.
const MAX_METRICS: usize = 8;
const MAX_POINTS: i64 = 1000;
/// A period up to this long is drawn from samples, as long as there are any.
const SAMPLES_FOR_SECS: i64 = 6 * 3600;
/// What one dashboard asks for at a time.
pub const MAX_QUERIES: usize = 100;
/// Rows of a table or a list, at most.
const MAX_ROWS: i64 = 200;

fn points_default() -> i64 {
    300
}

fn rows_default() -> i64 {
    20
}

/// One line on a chart, which may come from any device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub device: Uuid,
    pub metric: String,
}

/// One thing a dashboard widget shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Latest {
        device: Uuid,
    },
    Lines {
        sources: Vec<Source>,
        last_secs: i64,
        #[serde(default = "points_default")]
        points: i64,
    },
    Attributes {
        device: Uuid,
    },
    Alarms {
        #[serde(default)]
        device: Option<Uuid>,
        #[serde(default)]
        site: Option<Uuid>,
        #[serde(default)]
        all: bool,
        #[serde(default = "rows_default")]
        limit: i64,
    },
    Events {
        device: Uuid,
        #[serde(default = "rows_default")]
        limit: i64,
    },
    Devices {
        #[serde(default)]
        site: Option<Uuid>,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default = "rows_default")]
        limit: i64,
    },
    Robots,
}

/// Every kind of widget, for checking what is saved.
pub const WIDGETS: &[&str] =
    &["value", "gauge", "level", "state", "symbol", "line", "bars", "table", "alarms", "events", "map", "button", "setpoint", "text", "link"];
/// Those that change something, which a shared dashboard leaves out.
pub const CONTROLS: &[&str] = &["button", "setpoint"];
/// Widgets on one dashboard.
pub const MAX_WIDGETS: usize = 100;

/// What is wrong with a saved dashboard, if anything. A widget whose device has gone is not wrong: it
/// says so where it stands.
pub fn check(body: &str) -> std::result::Result<Value, String> {
    let board: Value = serde_json::from_str(body).map_err(|e| format!("that is not JSON: {e}"))?;
    let widgets = board["widgets"].as_array().ok_or("a dashboard has a list of widgets")?;
    if widgets.len() > MAX_WIDGETS {
        return Err(format!("at most {MAX_WIDGETS} widgets"));
    }
    let mut seen = std::collections::HashSet::new();
    for w in widgets {
        let id = w["id"].as_str().filter(|i| !i.is_empty()).ok_or("every widget has an id")?;
        if !seen.insert(id) {
            return Err(format!("two widgets are called {id}"));
        }
        let kind = w["type"].as_str().unwrap_or_default();
        if !WIDGETS.contains(&kind) {
            return Err(format!("{kind:?} is not a kind of widget"));
        }
    }
    Ok(board)
}

/// A device a widget names, which may be a variable of the dashboard: `$machine`.
fn named(v: &Value, board: &Value, vars: &Map<String, Value>) -> Option<Uuid> {
    let text = v.as_str()?;
    let Some(name) = text.strip_prefix('$') else { return text.parse().ok() };
    let chosen = vars.get(name).or_else(|| board["variables"].as_array()?.iter().find(|x| x["name"] == name)?.get("default"));
    chosen?.as_str()?.parse().ok()
}

/// What each widget needs, worked out here from what was laid out, so that a shared dashboard shows
/// what it shows and nothing a visitor might ask for instead.
pub fn plan(board: &Value, vars: &Map<String, Value>, last_secs: i64) -> Vec<(String, Query)> {
    let mut out = Vec::new();
    for w in board["widgets"].as_array().into_iter().flatten() {
        let Some(id) = w["id"].as_str() else { continue };
        let device = named(&w["device"], board, vars);
        let site = w["site"].as_str().and_then(|s| s.parse().ok());
        let limit = w["limit"].as_i64().unwrap_or_else(rows_default);
        let query = match w["type"].as_str().unwrap_or_default() {
            "value" | "gauge" | "level" | "state" | "symbol" => match (device, w["attribute"].is_string()) {
                (Some(device), true) => Some(Query::Attributes { device }),
                (Some(device), false) => Some(Query::Latest { device }),
                _ => None,
            },
            "setpoint" => device.map(|device| Query::Attributes { device }),
            "line" | "bars" => {
                let sources: Vec<Source> = w["sources"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| Some(Source { device: named(&s["device"], board, vars)?, metric: s["metric"].as_str()?.to_string() }))
                    .collect();
                (!sources.is_empty()).then(|| Query::Lines {
                    sources,
                    last_secs: w["last_secs"].as_i64().unwrap_or(last_secs),
                    points: w["points"].as_i64().unwrap_or_else(points_default),
                })
            }
            "table" => Some(Query::Devices { site, kind: w["kind"].as_str().filter(|k| !k.is_empty()).map(str::to_string), limit }),
            "alarms" => Some(Query::Alarms { device, site, all: false, limit }),
            "events" => device.map(|device| Query::Events { device, limit }),
            "map" => Some(Query::Robots),
            _ => None,
        };
        if let Some(query) = query {
            out.push((id.to_string(), query));
        }
    }
    out
}

/// A device's numbers over a period.
pub async fn history(k: &Kernel, ctx: &Ctx, device: Uuid, metrics: &[String], from: DateTime<Utc>, to: DateTime<Utc>, points: i64) -> Result<Value> {
    if to <= from {
        return Err(Error::invalid("the period ends before it starts"));
    }
    if to - from > Duration::days(MAX_DAYS) {
        return Err(Error::invalid(format!("a period is at most {MAX_DAYS} days")));
    }
    if metrics.is_empty() || metrics.len() > MAX_METRICS {
        return Err(Error::invalid(format!("between one and {MAX_METRICS} metrics")));
    }
    let span = (to - from).num_seconds();
    let bucket = (span / points.clamp(10, MAX_POINTS)).max(1);
    let units = units_of(k, ctx, device, metrics).await?;
    let mut by_metric: HashMap<String, Vec<Value>> = HashMap::new();
    if span <= SAMPLES_FOR_SECS {
        by_metric = samples(k, ctx, device, metrics, from, to, bucket).await?;
    }
    let mut series = Vec::with_capacity(metrics.len());
    for metric in metrics {
        // Samples may be gone, or have been sent on with nothing kept, where rollups are still there.
        let points = match by_metric.remove(metric).filter(|p| !p.is_empty()) {
            Some(points) => points,
            None => telemetry::series(k, ctx, device, metric, from, to, (bucket + 59) / 60).await?,
        };
        series.push(json!({"metric": metric, "unit": units.get(metric), "points": points}));
    }
    Ok(json!({"from": from, "to": to, "bucket_secs": bucket, "series": series}))
}

/// Samples of several metrics, averaged into equal buckets. What was marked bad is counted, and is
/// not a value.
async fn samples(
    k: &Kernel,
    ctx: &Ctx,
    device: Uuid,
    metrics: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    bucket: i64,
) -> Result<HashMap<String, Vec<Value>>> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<(String, DateTime<Utc>, i64, i64, Option<f64>, Option<f64>, Option<f64>)> = sqlx::query_as(
        "select metric, to_timestamp(floor(extract(epoch from source_time) / $6) * $6) as at, (count(*) filter (where quality <> 2))::int8, (count(*) filter (where quality = 2))::int8, \
                min(value) filter (where quality <> 2), max(value) filter (where quality <> 2), avg(value) filter (where quality <> 2) \
         from op_measurement where tenant_id = $1 and device_id = $2 and metric = any($3) and source_time >= $4 and source_time < $5 and day between $7 and $8 \
         group by 1, 2 order by 1, 2",
    )
    .bind(ctx.tenant)
    .bind(device)
    .bind(metrics)
    .bind(from)
    .bind(to)
    .bind(bucket as f64)
    .bind(from.date_naive())
    .bind(to.date_naive())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut out: HashMap<String, Vec<Value>> = HashMap::new();
    for (metric, at, samples, bad, min, max, avg) in rows {
        out.entry(metric).or_default().push(json!({"at": at, "samples": samples, "bad": bad, "min": min, "max": max, "avg": avg}));
    }
    Ok(out)
}

async fn units_of(k: &Kernel, ctx: &Ctx, device: Uuid, metrics: &[String]) -> Result<HashMap<String, String>> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select distinct on (metric) metric, unit from op_measurement where tenant_id = $1 and device_id = $2 and metric = any($3) and unit is not null order by metric, source_time desc",
    )
    .bind(ctx.tenant)
    .bind(device)
    .bind(metrics)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows.into_iter().collect())
}

async fn attributes(k: &Kernel, ctx: &Ctx, device: Uuid) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "select jsonb_build_object('key', key, 'value', value, 'reported_at', reported_at, 'desired', desired) from edge_attribute where tenant_id = $1 and device_id = $2 order by key limit 500",
    )
    .bind(ctx.tenant)
    .bind(device)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!(rows))
}

async fn alarms(k: &Kernel, ctx: &Ctx, device: Option<Uuid>, site: Option<Uuid>, all: bool, limit: i64) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "select jsonb_build_object('id', a.id, 'summary', a.summary, 'severity', a.severity, 'status', a.status, 'raised_at', a.raised_at, 'device', a.device, 'code', d.device_code) \
         from e_edge_alarm a left join e_device d on d.tenant_id = a.tenant_id and d.id = a.device \
         where a.tenant_id = $1 and ($2::uuid is null or a.device = $2) and ($3::uuid is null or a.site = $3) and ($4 or a.status <> 'Cleared') \
         order by a.raised_at desc limit $5",
    )
    .bind(ctx.tenant)
    .bind(device)
    .bind(site)
    .bind(all)
    .bind(limit.clamp(1, MAX_ROWS))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!(rows))
}

async fn events(k: &Kernel, ctx: &Ctx, device: Uuid, limit: i64) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "select jsonb_build_object('name', name, 'value', value, 'at', at) from edge_event where tenant_id = $1 and device_id = $2 order by at desc limit $3",
    )
    .bind(ctx.tenant)
    .bind(device)
    .bind(limit.clamp(1, MAX_ROWS))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!(rows))
}

/// Devices with each one's newest values and open alarms, for a table.
async fn devices(k: &Kernel, ctx: &Ctx, site: Option<Uuid>, kind: Option<&str>, limit: i64) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "select jsonb_build_object('id', d.id, 'code', d.device_code, 'name', d.device_name, 'kind', d.kind, 'lifecycle', d.lifecycle, \
                'alarms', (select count(*) from e_edge_alarm a where a.tenant_id = d.tenant_id and a.device = d.id and a.status <> 'Cleared'), \
                'latest', coalesce((select jsonb_object_agg(l.metric, jsonb_build_object('value', l.last_value, 'at', l.last_time)) from \
                    (select distinct on (metric) metric, last_value, last_time from op_rollup_minute r where r.tenant_id = d.tenant_id and r.device_id = d.id and r.last_time is not null order by metric, minute desc) l), '{}'::jsonb)) \
         from e_device d where d.tenant_id = $1 and ($2::uuid is null or d.site = $2) and ($3::text is null or d.kind = $3) and d.kind <> 'Gateway' order by d.device_name limit $4",
    )
    .bind(ctx.tenant)
    .bind(site)
    .bind(kind)
    .bind(limit.clamp(1, MAX_ROWS))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!(rows))
}

/// Lines from any devices, on one time scale.
async fn lines(k: &Kernel, ctx: &Ctx, sources: &[Source], last_secs: i64, points: i64) -> Result<Value> {
    if sources.is_empty() || sources.len() > MAX_METRICS {
        return Err(Error::invalid(format!("between one and {MAX_METRICS} lines")));
    }
    let to = Utc::now();
    let from = to - Duration::seconds(last_secs.clamp(60, MAX_DAYS * 86_400));
    let mut series = Vec::with_capacity(sources.len());
    let mut bucket = Value::Null;
    for s in sources {
        let mut one = history(k, ctx, s.device, std::slice::from_ref(&s.metric), from, to, points).await?;
        bucket = one["bucket_secs"].take();
        let mut line = one["series"][0].take();
        line["device"] = json!(s.device);
        series.push(line);
    }
    Ok(json!({"from": from, "to": to, "bucket_secs": bucket, "series": series}))
}

/// Robots, where each says it is and what it is doing.
pub async fn robots(k: &Kernel, ctx: &Ctx) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(jsonb_build_object('id', d.id, 'code', d.device_code, 'name', d.device_name, 'lifecycle', d.lifecycle, 'site', s.site_name, \
           'metrics', (select coalesce(jsonb_object_agg(l.metric, jsonb_build_object('value', l.last_value, 'at', l.last_time)), '{}') from ( \
              select distinct on (r.metric) r.metric, r.last_value, r.last_time from op_rollup_minute r where r.tenant_id = d.tenant_id and r.device_id = d.id and r.last_time is not null order by r.metric, r.minute desc) l), \
           'attributes', (select coalesce(jsonb_object_agg(a.key, a.value), '{}') from edge_attribute a where a.tenant_id = d.tenant_id and a.device_id = d.id), \
           'state', (select e.state from op_equipment_event e where e.tenant_id = d.tenant_id and e.device_id = d.id order by e.at desc limit 1), \
           'running', (select count(*) from op_command c where c.tenant_id = d.tenant_id and c.target_id = d.id and c.state in ('Accepted', 'Executing'))) order by d.device_code), '[]'::jsonb) \
         from e_device d left join e_site s on s.tenant_id = d.tenant_id and s.id = d.site \
         where d.tenant_id = $1 and d.kind = 'Robot' and d.lifecycle <> 'Retired'",
    )
    .bind(ctx.tenant)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// One query. One that fails says so in its own place, so that a widget pointing at something since
/// removed does not take the dashboard down with it.
pub async fn answer(k: &Kernel, ctx: &Ctx, query: &Query) -> Value {
    let got = match query {
        Query::Latest { device } => telemetry::latest(k, ctx, *device).await.map(|l| json!(l)),
        Query::Lines { sources, last_secs, points } => lines(k, ctx, sources, *last_secs, *points).await,
        Query::Attributes { device } => attributes(k, ctx, *device).await,
        Query::Alarms { device, site, all, limit } => alarms(k, ctx, *device, *site, *all, *limit).await,
        Query::Events { device, limit } => events(k, ctx, *device, *limit).await,
        Query::Devices { site, kind, limit } => devices(k, ctx, *site, kind.as_deref(), *limit).await,
        Query::Robots => robots(k, ctx).await,
    };
    match got {
        Ok(data) => json!({"data": data}),
        Err(e) => json!({"error": e.to_string()}),
    }
}

/// Everything a dashboard's widgets need, by widget. Two widgets on one device ask once.
pub async fn board(k: &Kernel, ctx: &Ctx, queries: &[(String, Query)]) -> Result<Value> {
    if queries.len() > MAX_QUERIES {
        return Err(Error::invalid(format!("a dashboard asks for at most {MAX_QUERIES} things at once")));
    }
    let mut asked: HashMap<String, Value> = HashMap::new();
    let mut out = Map::new();
    for (name, query) in queries {
        let key = serde_json::to_string(query).map_err(|e| Error::internal(e.to_string()))?;
        if !asked.contains_key(&key) {
            asked.insert(key.clone(), answer(k, ctx, query).await);
        }
        out.insert(name.clone(), asked[&key].clone());
    }
    Ok(Value::Object(out))
}
