//! IoT Edge (Ferrum docs/plan/edge.md): equipment reached by definitions, through gateways that keep
//! working on their own.
//!
//! * [`alarms`]: what people do about an alarm;
//! * [`bundles`]: a solution in one file;
//! * [`config`]: what each gateway should be, worked out from the records;
//! * [`data`]: numbers over time, and what a dashboard shows;
//! * [`facts`]: what gateways report, arriving when the link allows;
//! * [`firmware`]: new firmware for devices, a few at a time;
//! * [`fleet`]: many devices at once;
//! * [`notices`]: telling people, in steps;
//! * [`platform_flows`]: flows that run here, for what has to see across sites;
//! * [`reports`]: availability, alarms and readings on paper;
//! * [`schedules`]: asking devices for something by the clock;
//! * [`site`]: the HTTP endpoints;
//! * [`templates`]: definitions to start from;
//! * [`twins`]: a device as a twin, a Thing Description and an Asset Administration Shell.
//!
//! Commands, identity, telemetry and authority are Operations'. This adds how devices are described
//! and looked after. Rules raise alarms and move nothing: anything physical goes through the ledger.

pub mod alarms;
pub mod bundles;
pub mod config;
pub mod data;
pub mod facts;
pub mod firmware;
pub mod fleet;
pub mod notices;
pub mod platform_flows;
pub mod portal;
pub mod reports;
pub mod schedules;
pub mod site;
pub mod templates;
pub mod twins;

use async_trait::async_trait;
use ferrum_domain::reports::ReportSource;
use ferrum_domain::util::{id, s, sys};
use ferrum_kernel::outbox::Event;
use ferrum_kernel::{Ctx, Doc, DocHook, Error, EventSink, HookCtx, HookRegistry, Kernel, Result};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

include!(concat!(env!("OUT_DIR"), "/packages.rs"));
include!(concat!(env!("OUT_DIR"), "/ui.rs"));

/// This app's manifest (`app.yaml`).
pub const MANIFEST: &str = include_str!("../app.yaml");

/// This app's metadata packages, embedded at build time.
pub fn packages() -> std::result::Result<Vec<ferrum_meta::Package>, ferrum_meta::MetaError> {
    ferrum_meta::catalog::load_embedded(EMBEDDED)
}

/// Registers this app's document hooks and endpoints.
pub fn register(h: &mut HookRegistry) {
    h.register("DeviceDefinition", Arc::new(DefinitionHook));
    h.register("EdgeRule", Arc::new(RuleHook));
    h.register("EdgeFlow", Arc::new(FlowHook));
    h.register("EdgeDashboard", Arc::new(BoardHook));
    h.register("EdgeExport", Arc::new(ExportHook));
    h.register("EdgeFirmware", Arc::new(firmware::FirmwareHook));
    h.register("EdgeRollout", Arc::new(firmware::RolloutHook));
    h.register("EdgeSchedule", Arc::new(schedules::ScheduleHook));
    h.register("EdgeReportSchedule", Arc::new(reports::ReportScheduleHook));
    h.add_event_sink(Arc::new(Facts));
    h.add_site("edge", Arc::new(site::EdgeSite));
    h.add_job_handler(Arc::new(notices::Notices));
}

/// This app's reports.
pub fn reports() -> Vec<Arc<dyn ReportSource>> {
    vec![Arc::new(reports::EdgeReports)]
}

/// A dashboard is checked when it is saved, so that nothing is left for a person looking at it to find.
struct BoardHook;

#[async_trait]
impl DocHook for BoardHook {
    fn name(&self) -> &'static str {
        "edge.dashboard"
    }

    async fn validate_change(&self, _hc: &mut HookCtx<'_>, _old: Option<&Doc>, doc: &Doc) -> Result<()> {
        match s(doc, "body") {
            Some(body) => data::check(body).map(drop).map_err(|why| Error::field("body", why)),
            None => Ok(()),
        }
    }
}

/// Where an export sends is checked when it is saved, and not by a gateway that can only fail quietly.
struct ExportHook;

#[async_trait]
impl DocHook for ExportHook {
    fn name(&self) -> &'static str {
        "edge.export"
    }

    async fn validate_change(&self, _hc: &mut HookCtx<'_>, _old: Option<&Doc>, doc: &Doc) -> Result<()> {
        ferrum_edge_contract::edge::check_export(s(doc, "send_to").unwrap_or_default()).map_err(|why| Error::field("send_to", why))
    }
}

/// A definition is checked when it is saved, with its reason, so a mistake is a person's to fix at a
/// desk and not a gateway's to find. Publishing is what reaches gateways, so a draft breaks nothing.
struct DefinitionHook;

#[async_trait]
impl DocHook for DefinitionHook {
    fn name(&self) -> &'static str {
        "edge.definition"
    }

    async fn validate_change(&self, _hc: &mut HookCtx<'_>, _old: Option<&Doc>, doc: &Doc) -> Result<()> {
        ferrum_edge_contract::driver::parse(s(doc, "body").unwrap_or_default()).map(drop).map_err(|why| Error::field("body", why))
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, _params: &Value) -> Result<Option<Value>> {
        if name != "publish" {
            return Ok(None);
        }
        let record = id(doc, "id").ok_or_else(|| Error::internal("no id"))?;
        let body = s(doc, "body").unwrap_or_default();
        let revision = doc.get("revision").and_then(Value::as_i64).unwrap_or(0) + 1;
        ferrum_edge_contract::driver::parse(body).map_err(|why| Error::field("body", why))?;
        let mut values = Doc::new();
        values.insert("published_body".into(), json!(body));
        values.insert("revision".into(), json!(revision));
        values.insert("status".into(), json!("Published"));
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "DeviceDefinition", record, values).await?;
        ferrum_kernel::audit::record(hc.tx, hc.ctx, "DeviceDefinition", record, "edge.published", None, json!({"revision": revision})).await?;
        Ok(Some(json!({"revision": revision})))
    }
}

/// A rule's limits, which only make sense together.
struct RuleHook;

#[async_trait]
impl DocHook for RuleHook {
    fn name(&self) -> &'static str {
        "edge.rule"
    }

    async fn validate_change(&self, _hc: &mut HookCtx<'_>, _old: Option<&Doc>, doc: &Doc) -> Result<()> {
        let number = |k: &str| doc.get(k).and_then(|v| v.as_str().and_then(|t| t.parse::<f64>().ok()).or(v.as_f64()));
        let comparison = s(doc, "comparison").unwrap_or("Above");
        if comparison == "Silent" {
            if doc.get("hold_secs").and_then(Value::as_i64).unwrap_or(0) <= 0 {
                return Err(Error::field("hold_secs", "say how long silence must last"));
            }
            return Ok(());
        }
        let Some(limit) = number("limit_value") else { return Err(Error::field("limit_value", "say what the limit is")) };
        if let Some(clear) = number("clear_value") {
            let wrong = if comparison == "Above" { clear > limit } else { clear < limit };
            if wrong {
                return Err(Error::field("clear_value", "it clears on the safe side of the limit"));
            }
        }
        Ok(())
    }
}

/// The way a flow goes from a drawing to running at a site: simulated by its author, reviewed by
/// someone else, and then run, one site first if that is wise. What a reviewer saw is what runs, so
/// a flow cannot change once it has been sent for review, and changing a running one leaves that
/// version running until the new one has been through the same.
struct FlowHook;

const REVIEWER: &str = "Edge Reviewer";

fn flow_step(status: &str, wanted: &[&str], what: &str) -> Result<()> {
    if wanted.contains(&status) {
        return Ok(());
    }
    Err(Error::invalid(format!("this is {status}: it cannot be {what} now")))
}

fn reviewer(ctx: &Ctx) -> Result<()> {
    if ctx.has_role(REVIEWER) {
        return Ok(());
    }
    Err(Error::Forbidden(format!("this takes the role {REVIEWER}")))
}

#[async_trait]
impl DocHook for FlowHook {
    fn name(&self) -> &'static str {
        "edge.flow"
    }

    async fn validate_change(&self, _hc: &mut HookCtx<'_>, old: Option<&Doc>, doc: &Doc) -> Result<()> {
        if let Some(old) = old
            && s(old, "status").is_some_and(|status| status != "Draft")
            && (s(old, "body") != s(doc, "body") || s(old, "runs_at") != s(doc, "runs_at"))
        {
            return Err(Error::field("body", "what is under review cannot change: choose Change It to draft it again"));
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        let record = id(doc, "id").ok_or_else(|| Error::internal("no id"))?;
        let status = s(doc, "status").unwrap_or("Draft").to_string();
        let body = s(doc, "body").unwrap_or_default().to_string();
        let me = hc.ctx.email.clone();
        let now = chrono::Utc::now().to_rfc3339();
        let platform = s(doc, "runs_at") == Some("Platform");
        let used = usable_flows(hc.tx, hc.ctx.tenant, record).await?;
        let whole = whole_flow(&body, &used).map_err(|why| Error::field("body", why))?;
        // A step that needs a gateway is found here and not by a flow that quietly does nothing.
        let parsed = |body: &str| {
            let flow = ferrum_edge_contract::flow::parse(body).map_err(|why| Error::field("body", why))?;
            match flow.needs_a_gateway() {
                Some(node) if platform => Err(Error::field("runs_at", format!("{node} needs a gateway to run on"))),
                _ => Ok(flow),
            }
        };
        let mut values = Doc::new();
        match name {
            "submit_for_review" => {
                flow_step(&status, &["Draft"], "sent for review")?;
                let flow = parsed(&whole)?;
                if s(doc, "simulated_digest") != Some(flow.digest.as_str()) {
                    return Err(Error::invalid("simulate this version first: a reviewer should see what it does, not only what it says"));
                }
                values.insert("status".into(), json!("In Review"));
                values.insert("submitted_by".into(), json!(me));
                values.insert("review_note".into(), Value::Null);
            }
            "send_back" => {
                reviewer(hc.ctx)?;
                flow_step(&status, &["In Review"], "sent back")?;
                values.insert("status".into(), json!("Draft"));
                values.insert("review_note".into(), params.get("note").cloned().unwrap_or(Value::Null));
            }
            "approve" => {
                reviewer(hc.ctx)?;
                flow_step(&status, &["In Review"], "approved")?;
                // A review is someone else looking, whatever roles the author also holds.
                if s(doc, "submitted_by") == Some(me.as_str()) {
                    return Err(Error::Forbidden("someone else has to review what you sent".into()));
                }
                parsed(&whole)?;
                values.insert("status".into(), json!("Approved"));
                values.insert("approved_by".into(), json!(me));
                values.insert("approved_at".into(), json!(now));
            }
            "activate" => {
                reviewer(hc.ctx)?;
                flow_step(&status, &["Approved"], "run")?;
                // What it uses may have changed since it was seen. What runs has to be what was seen.
                if s(doc, "simulated_digest") != Some(parsed(&whole)?.digest.as_str()) {
                    return Err(Error::invalid("a flow this uses has changed: draft it again, and simulate it"));
                }
                let revision = doc.get("revision").and_then(Value::as_i64).unwrap_or(0) + 1;
                sqlx::query("insert into edge_flow_version (tenant_id, flow_id, revision, body, by_whom) values ($1, $2, $3, $4, $5)")
                    .bind(hc.ctx.tenant)
                    .bind(record)
                    .bind(revision)
                    .bind(&whole)
                    .bind(&me)
                    .execute(&mut **hc.tx)
                    .await?;
                values.insert("published_body".into(), json!(whole));
                values.insert("revision".into(), json!(revision));
                values.insert("live".into(), json!(true));
                values.insert("activated_by".into(), json!(me));
                values.insert("activated_at".into(), json!(now));
            }
            // Looking changes nothing that runs, so it needs no review. It has an end, because what
            // passes through costs something and a person forgets.
            "watch" => {
                let minutes = params.get("minutes").and_then(Value::as_i64).unwrap_or(10).clamp(0, 60);
                let until = (minutes > 0).then(|| (chrono::Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339());
                values.insert("debug_until".into(), json!(until));
            }
            // Stopping is never a question of review: whoever may work on flows may stop one.
            "stop" => {
                values.insert("live".into(), json!(false));
            }
            "revise" => {
                flow_step(&status, &["In Review", "Approved"], "changed")?;
                values.insert("status".into(), json!("Draft"));
                values.insert("approved_by".into(), Value::Null);
                values.insert("approved_at".into(), Value::Null);
            }
            _ => return Ok(None),
        }
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "EdgeFlow", record, values).await?;
        ferrum_kernel::audit::record(hc.tx, hc.ctx, "EdgeFlow", record, &format!("edge.flow.{name}"), None, json!({"by": me})).await?;
        Ok(Some(json!({"done": name})))
    }
}

/// What flows may use: those that have been reviewed. A flow does not use itself.
pub(crate) async fn usable_flows(tx: &mut ferrum_kernel::Tx, tenant: Uuid, not: Uuid) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as("select flow_name, body from e_edge_flow where tenant_id = $1 and id <> $2 and status = 'Approved' and body is not null")
        .bind(tenant)
        .bind(not)
        .fetch_all(&mut **tx)
        .await?)
}

/// A flow with what it uses copied in, which is what is checked, reviewed and run.
pub(crate) fn whole_flow(body: &str, used: &[(String, String)]) -> std::result::Result<String, String> {
    if !body.contains("\"use\"") {
        return Ok(body.to_string());
    }
    let parsed: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let find = |name: &str| used.iter().find(|(n, _)| n == name).and_then(|(_, b)| serde_json::from_str(b).ok());
    ferrum_edge_contract::subflow::expand(&parsed, &find).map(|whole| whole.to_string())
}

/// What gateways report, in any order and at least once, which [`facts`] is written for.
struct Facts;

#[async_trait]
impl EventSink for Facts {
    async fn on_event(&self, k: &Kernel, ev: &Event) -> Result<()> {
        let Some(kind) = ev.topic.strip_prefix("ops.fact.edge.") else { return Ok(()) };
        let ctx = Ctx::system(ev.tenant_id, k.registry.schema(ev.tenant_id).await?);
        let mut tx = k.db.tenant_tx(ev.tenant_id).await?;
        facts::apply(k, &ctx, &mut tx, kind, &ev.payload).await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Periodic upkeep: events older than this go, and with them whatever only they recorded.
pub async fn maintenance(k: &Kernel) -> Result<()> {
    let mut tx = k.db.system_tx().await?;
    sqlx::query("delete from edge_event where at < now() - interval '90 days'").execute(&mut *tx).await?;
    sqlx::query("delete from edge_notice where status in ('Sent', 'Dropped', 'Failed') and due_at < now() - interval '90 days'").execute(&mut *tx).await?;
    sqlx::query("delete from edge_flow_trace where at < now() - interval '1 hour'").execute(&mut *tx).await?;
    let tenants: Vec<Uuid> = sqlx::query_scalar("select id from tenant where status = 'active' order by id").fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let mut first_error = None;
    for tenant in tenants {
        let schema = match k.registry.schema(tenant).await {
            Ok(schema) if schema.packages.iter().any(|p| p == "edge") => schema,
            Ok(_) => continue,
            Err(e) => {
                first_error.get_or_insert(e);
                continue;
            }
        };
        let ctx = Ctx::system(tenant, schema);
        for outcome in [
            facts::watch_gateways(k, &ctx).await,
            notices::send_due(k, &ctx).await.map(drop),
            platform_flows::run(k, &ctx).await,
            firmware::advance(k, &ctx).await,
            reports::deliver(k, &ctx).await,
        ] {
            if let Err(e) = outcome {
                tracing::warn!(%tenant, "edge maintenance failed: {e}");
                first_error.get_or_insert(e);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}
