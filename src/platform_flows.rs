//! Flows that run here and not at a gateway, for what has to see across sites: a total over a whole
//! fleet, or one line's output against another's on the far side of the world. They go through the
//! same review and run the same code. A gateway's flow hears each reading as it happens and keeps
//! working with the link down; one here hears each closed minute and stops when the link does, so
//! anything that has to work through an outage belongs at a gateway.
//!
//! What a flow does is applied as the fact a gateway would have sent, in one transaction with where
//! it stands, so a crash repeats nothing and loses nothing. Commands take the same road to the ledger.
//
// ponytail: a minute's readings are its rollups as they stand when it closes. What a gateway sends
// late has been through that gateway's flows and is not heard here; events and attributes go by when
// they arrived, so those are.

use crate::facts;
use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use ferrum_edge_contract::edge::{AlarmChange, FlowTrace};
use ferrum_edge_contract::flow::{self, Effect, Input, Known, Output, State, Wants};
use ferrum_kernel::{Ctx, Kernel, Result, Tx};
use ferrum_ops::contract::telemetry::{Class, Measurement, Quality};
use ferrum_ops::telemetry;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

/// Minutes one run gets through, so a server that was down catches up without taking all day.
const MINUTES_PER_RUN: i32 = 60;
/// What one flow hears in a minute of each kind of thing. Past this it says so, in the log.
const MAX_HEARD: i64 = 20_000;

#[derive(sqlx::FromRow)]
struct Flow {
    id: Uuid,
    flow_name: String,
    revision: i64,
    published_body: String,
    site: Option<Uuid>,
    watched: bool,
}

/// Each flow in its own transaction: one that fails waits for the next minute, and the others go on.
pub async fn run(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let flows: Vec<Flow> = sqlx::query_as(
        "select id, flow_name, revision, published_body, site, coalesce(debug_until > now(), false) as watched from e_edge_flow \
         where tenant_id = $1 and live and runs_at = 'Platform' and published_body is not null",
    )
    .bind(ctx.tenant)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut first = None;
    for f in flows {
        if let Err(e) = one(k, ctx, &f).await {
            tracing::warn!(flow = %f.flow_name, "{e}");
            first.get_or_insert(e);
        }
    }
    first.map_or(Ok(()), Err)
}

async fn one(k: &Kernel, ctx: &Ctx, f: &Flow) -> Result<()> {
    let compiled = match flow::parse(&f.published_body) {
        Ok(compiled) => compiled,
        Err(why) => {
            tracing::warn!(flow = %f.flow_name, "{why}");
            return Ok(());
        }
    };
    let now = Utc::now();
    let closed = now.duration_trunc(TimeDelta::minutes(1)).unwrap_or(now);
    telemetry::ensure_partitions(k, now).await?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    sqlx::query("insert into edge_flow_state (tenant_id, flow_id, revision, read_to) values ($1, $2, $3, $4) on conflict do nothing")
        .bind(ctx.tenant)
        .bind(f.id)
        .bind(f.revision)
        .bind(closed)
        .execute(&mut *tx)
        .await?;
    // Held to the end, so a slow run and the next cannot both do the same minute.
    let (revision, saved, read_to): (i64, Value, DateTime<Utc>) =
        sqlx::query_as("select revision, state, read_to from edge_flow_state where tenant_id = $1 and flow_id = $2 for update")
            .bind(ctx.tenant)
            .bind(f.id)
            .fetch_one(&mut *tx)
            .await?;
    // A new revision starts afresh and from now: its nodes may mean something else.
    let (mut state, mut from): (State, _) =
        if revision == f.revision { (serde_json::from_value(saved).unwrap_or_default(), read_to) } else { (State::default(), closed) };
    let wants = compiled.wants();
    let mut known = known(&mut tx, ctx, f, &wants).await?;
    let mut places = Places::default();
    let mut measurements = HashMap::new();
    for _ in 0..MINUTES_PER_RUN {
        if from >= closed {
            break;
        }
        let to = from + TimeDelta::minutes(1);
        for (input, at) in heard(&mut tx, ctx, f, &wants, from, to).await? {
            known.note(&input);
            let output = compiled.input_knowing(&mut state, &input, at, &known);
            act(k, ctx, &mut tx, f, output, at, &mut places, &mut measurements).await?;
        }
        let output = compiled.tick_knowing(&mut state, to, &known);
        act(k, ctx, &mut tx, f, output, to, &mut places, &mut measurements).await?;
        from = to;
    }
    for (site, batch) in measurements {
        telemetry::ingest(&mut tx, ctx.tenant, site, batch, now).await?;
    }
    sqlx::query("update edge_flow_state set revision = $3, state = $4, read_to = $5 where tenant_id = $1 and flow_id = $2")
        .bind(ctx.tenant)
        .bind(f.id)
        .bind(f.revision)
        .bind(json!(state))
        .bind(from)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Where a flow starts from: the newest of what it looks up. What it hears keeps this current.
async fn known(tx: &mut Tx, ctx: &Ctx, f: &Flow, wants: &Wants) -> Result<Known> {
    let mut known = Known::default();
    let names = |set: &std::collections::BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>();
    let metrics: Vec<(String, String, f64)> = sqlx::query_as(
        "select distinct on (r.device_id, r.metric) d.device_code, r.metric, r.last_value from op_rollup_minute r \
         join e_device d on d.tenant_id = r.tenant_id and d.id = r.device_id \
         where r.tenant_id = $1 and r.metric = any($2) and r.last_value is not null and ($3::uuid is null or d.site = $3) and r.minute > now() - interval '1 day' \
         order by r.device_id, r.metric, r.minute desc",
    )
    .bind(ctx.tenant)
    .bind(names(&wants.looked_up_metrics))
    .bind(f.site)
    .fetch_all(&mut **tx)
    .await?;
    for (device, metric, value) in metrics {
        known.note(&Input { device, metric: Some(metric), value: json!(value), ..Default::default() });
    }
    let attributes: Vec<(String, String, Option<Value>)> = sqlx::query_as(
        "select d.device_code, a.key, a.value from edge_attribute a join e_device d on d.tenant_id = a.tenant_id and d.id = a.device_id \
         where a.tenant_id = $1 and a.key = any($2) and ($3::uuid is null or d.site = $3)",
    )
    .bind(ctx.tenant)
    .bind(names(&wants.looked_up_attributes))
    .bind(f.site)
    .fetch_all(&mut **tx)
    .await?;
    for (device, key, value) in attributes {
        known.note(&Input { device, attribute: Some(key), value: value.unwrap_or(Value::Null), ..Default::default() });
    }
    Ok(known)
}

/// What a flow hears of one minute, in the order it happened.
async fn heard(tx: &mut Tx, ctx: &Ctx, f: &Flow, wants: &Wants, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<(Input, DateTime<Utc>)>> {
    let names = |set: &std::collections::BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>();
    let mut out = Vec::new();
    let metrics: Vec<(String, String, f64, DateTime<Utc>)> = sqlx::query_as(
        "select d.device_code, r.metric, r.last_value, r.last_time from op_rollup_minute r join e_device d on d.tenant_id = r.tenant_id and d.id = r.device_id \
         where r.tenant_id = $1 and r.minute >= $2 and r.minute < $3 and r.metric = any($4) and r.last_value is not null and ($5::uuid is null or d.site = $5) \
         order by r.last_time limit $6",
    )
    .bind(ctx.tenant)
    .bind(from)
    .bind(to)
    .bind(names(&wants.metrics))
    .bind(f.site)
    .bind(MAX_HEARD)
    .fetch_all(&mut **tx)
    .await?;
    let mut full = metrics.len() as i64 == MAX_HEARD;
    out.extend(metrics.into_iter().map(|(device, metric, value, at)| (Input { device, metric: Some(metric), value: json!(value), ..Default::default() }, at)));
    let events: Vec<(String, String, Option<Value>, DateTime<Utc>)> = sqlx::query_as(
        "select d.device_code, e.name, e.value, e.at from edge_event e join e_device d on d.tenant_id = e.tenant_id and d.id = e.device_id \
         where e.tenant_id = $1 and e.received_at >= $2 and e.received_at < $3 and e.name = any($4) and ($5::uuid is null or d.site = $5) order by e.at limit $6",
    )
    .bind(ctx.tenant)
    .bind(from)
    .bind(to)
    .bind(names(&wants.events))
    .bind(f.site)
    .bind(MAX_HEARD)
    .fetch_all(&mut **tx)
    .await?;
    full |= events.len() as i64 == MAX_HEARD;
    out.extend(
        events
            .into_iter()
            .map(|(device, name, value, at)| (Input { device, event: Some(name), value: value.unwrap_or(Value::Null), ..Default::default() }, at)),
    );
    let attributes: Vec<(String, String, Option<Value>, DateTime<Utc>)> = sqlx::query_as(
        "select d.device_code, a.key, a.value, a.received_at from edge_attribute a join e_device d on d.tenant_id = a.tenant_id and d.id = a.device_id \
         where a.tenant_id = $1 and a.received_at >= $2 and a.received_at < $3 and a.key = any($4) and ($5::uuid is null or d.site = $5) limit $6",
    )
    .bind(ctx.tenant)
    .bind(from)
    .bind(to)
    .bind(names(&wants.attributes))
    .bind(f.site)
    .bind(MAX_HEARD)
    .fetch_all(&mut **tx)
    .await?;
    out.extend(
        attributes
            .into_iter()
            .map(|(device, key, value, at)| (Input { device, attribute: Some(key), value: value.unwrap_or(Value::Null), ..Default::default() }, at)),
    );
    if wants.states {
        let states: Vec<(String, String, DateTime<Utc>)> = sqlx::query_as(
            "select d.device_code, e.state, e.at from op_equipment_event e join e_device d on d.tenant_id = e.tenant_id and d.id = e.device_id \
             where e.tenant_id = $1 and e.at >= $2 and e.at < $3 and ($4::uuid is null or d.site = $4) limit $5",
        )
        .bind(ctx.tenant)
        .bind(from)
        .bind(to)
        .bind(f.site)
        .bind(MAX_HEARD)
        .fetch_all(&mut **tx)
        .await?;
        out.extend(states.into_iter().map(|(device, state, at)| (Input { device, state: Some(state), ..Default::default() }, at)));
    }
    if full {
        tracing::warn!(flow = %f.flow_name, "more happened in the minute from {from} than a flow here hears: {MAX_HEARD} of each");
    }
    out.sort_by_key(|(_, at)| *at);
    Ok(out)
}

/// A device by its code, remembered for the run.
#[derive(Default)]
struct Places(HashMap<String, Option<(Uuid, Option<Uuid>)>>);

impl Places {
    async fn of(&mut self, tx: &mut Tx, tenant: Uuid, code: Option<&String>) -> Result<Option<(Uuid, Option<Uuid>)>> {
        let Some(code) = code else { return Ok(None) };
        if let Some(found) = self.0.get(code) {
            return Ok(*found);
        }
        let found =
            sqlx::query_as("select id, site from e_device where tenant_id = $1 and device_code = $2").bind(tenant).bind(code).fetch_optional(&mut **tx).await?;
        self.0.insert(code.clone(), found);
        Ok(found)
    }
}

#[allow(clippy::too_many_arguments)]
async fn act(
    k: &Kernel,
    ctx: &Ctx,
    tx: &mut Tx,
    f: &Flow,
    output: Output,
    at: DateTime<Utc>,
    places: &mut Places,
    measurements: &mut HashMap<Uuid, Vec<Measurement>>,
) -> Result<()> {
    for p in &output.problems {
        tracing::warn!(flow = %f.flow_name, "{p}");
    }
    if f.watched {
        let traces: Vec<FlowTrace> = output.trace.into_iter().map(|step| FlowTrace::new(f.id, at, step)).collect();
        keep_traces(tx, ctx.tenant, None, &traces).await?;
    }
    for effect in output.effects {
        let device = match &effect {
            Effect::Alarm { device, .. }
            | Effect::Event { device, .. }
            | Effect::Attribute { device, .. }
            | Effect::Metric { device, .. }
            | Effect::Command { device, .. }
            | Effect::Notify { device, .. } => places.of(tx, ctx.tenant, device.as_ref()).await?,
            _ => None,
        };
        let (id, site) = (device.map(|d| d.0), device.and_then(|d| d.1));
        let (kind, body) = match effect {
            Effect::Alarm { node, name, severity, raised, value, .. } => {
                let Some(id) = id else { continue };
                ("alarm", json!(AlarmChange { rule: f.id, device: id, raised, value, node: Some(node), name: Some(name), severity: Some(severity) }))
            }
            Effect::Event { name, value, .. } => ("event", json!({"device": id, "name": name, "value": value})),
            Effect::Attribute { key, value, .. } => ("attribute", json!({"device": id, "key": key, "value": value})),
            Effect::Notify { title, .. } => ("notify", json!({"flow": f.id, "device": id, "title": title})),
            Effect::Command { node, operation, payload, expires_secs, .. } => {
                ("request", json!({"flow": f.id, "node": node, "device": id, "operation": operation, "payload": payload, "expires_secs": expires_secs}))
            }
            Effect::Metric { name, value, unit, .. } => {
                if let (Some(device), Some(site)) = (id, site) {
                    let batch = measurements.entry(site).or_default();
                    let seq = at.timestamp_micros() + batch.len() as i64;
                    batch.push(Measurement {
                        device_id: device,
                        metric: name,
                        value,
                        unit,
                        source_time: at,
                        boot: f.id,
                        seq,
                        quality: Quality::Good,
                        class: Class::Sample,
                    });
                }
                continue;
            }
            Effect::Log { node, device, text } => {
                tracing::info!(flow = %f.flow_name, node = %node, device = device.as_deref().unwrap_or("-"), "{text}");
                continue;
            }
            // Refused when it was run, and only here if a flow changed underneath that.
            Effect::Reply { .. } | Effect::Export { .. } => continue,
        };
        facts::apply(k, ctx, tx, kind, &json!({"at": at, "fact": Uuid::now_v7(), "site": site, "body": body})).await?;
    }
    Ok(())
}

/// Keeps what passed through flows someone is still watching, and nothing about any other.
pub async fn keep_traces(tx: &mut Tx, tenant: Uuid, gateway: Option<Uuid>, traces: &[FlowTrace]) -> Result<()> {
    for t in traces.iter().take(1_000) {
        sqlx::query(
            "insert into edge_flow_trace (tenant_id, id, flow_id, gateway_id, node, device, value, ports, at) \
             select $1, $2, f.id, $3, $4, $5, $6, $7, $8 from e_edge_flow f where f.tenant_id = $1 and f.id = $9 and f.debug_until > now()",
        )
        .bind(tenant)
        .bind(Uuid::now_v7())
        .bind(gateway)
        .bind(&t.step.node)
        .bind(&t.step.device)
        .bind(&t.step.value)
        .bind(&t.step.ports)
        .bind(t.at)
        .bind(t.flow)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// What passed through a flow, newest last. A device's data, so a site's members rule would hide it
/// from someone who could not see that device: a flow's trace is for people who can see its site.
pub async fn traces(k: &Kernel, ctx: &Ctx, flow: Uuid, since: Option<DateTime<Utc>>) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "select jsonb_build_object('id', id, 'node', node, 'device', device, 'value', value, 'ports', ports, 'at', at) from ( \
           select * from edge_flow_trace where tenant_id = $1 and flow_id = $2 and ($3::timestamptz is null or at > $3) order by at desc, id desc limit 500) t order by at, id",
    )
    .bind(ctx.tenant)
    .bind(flow)
    .bind(since)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"traces": rows}))
}

/// Every version of a flow that has run.
pub async fn versions(k: &Kernel, ctx: &Ctx, flow: Uuid) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "select jsonb_build_object('revision', revision, 'body', body, 'by', by_whom, 'at', at) from edge_flow_version \
         where tenant_id = $1 and flow_id = $2 order by revision desc limit 100",
    )
    .bind(ctx.tenant)
    .bind(flow)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"versions": rows}))
}
