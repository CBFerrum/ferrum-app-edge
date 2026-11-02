//! What a customer sees in the workspace's portal (Ferrum docs/plan/apps-ui.md §9.1). Portal people
//! hold no roles and reach nothing through the records, so all of this is answered by the address
//! they signed in with: the devices of the customers that name them, those devices' alarms, and
//! the dashboards given to those customers. A device of someone else's is not found, rather than
//! forbidden, so that asking says nothing about whether it exists.
//!
//! | Route | Answers |
//! |---|---|
//! | `GET /portal/devices` | `{customers, devices, dashboards}` |
//! | `GET /portal/devices/{id}` | One device: `{device, latest, alarms}` |
//! | `GET /portal/devices/{id}/history?metrics=&from=&to=&points=` | Its readings |
//! | `GET /portal/dashboards/{id}` | A dashboard, without its controls |
//! | `POST /portal/alarms/{id}/acknowledge` | `{note?}`: it says who did |

use crate::site;
use ferrum_domain::util::sys;
use ferrum_kernel::{Ctx, Error, Kernel, Result, SiteRequest};
use serde_json::{Value, json};
use uuid::Uuid;

/// The customers that name this person.
async fn customers(k: &Kernel, ctx: &Ctx) -> Result<Vec<(Uuid, String)>> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let found = sqlx::query_as(
        "select c.id, c.customer_name from e_edge_customer c where c.tenant_id = $1 and exists ( \
           select 1 from e_edge_customer__people p where p.tenant_id = c.tenant_id and p.parent_id = c.id and lower(p.user_email) = lower($2)) \
         order by c.customer_name",
    )
    .bind(ctx.tenant)
    .bind(&ctx.email)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(found)
}

async fn ids(k: &Kernel, ctx: &Ctx) -> Result<Vec<Uuid>> {
    Ok(customers(k, ctx).await?.into_iter().map(|(id, _)| id).collect())
}

async fn mine(k: &Kernel, ctx: &Ctx) -> Result<Value> {
    let theirs = customers(k, ctx).await?;
    let ids: Vec<Uuid> = theirs.iter().map(|(id, _)| *id).collect();
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let devices: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(r order by r.code), '[]') from ( \
           select d.id, d.device_code as code, d.device_name as name, d.lifecycle, s.site_name as site, \
             (select count(*) from e_edge_alarm a where a.tenant_id = d.tenant_id and a.device = d.id and a.status <> 'Cleared') as alarms, \
             (select max(r.last_time) from op_rollup_minute r where r.tenant_id = d.tenant_id and r.device_id = d.id and r.minute > now() - interval '1 day') as last_seen \
           from e_device d left join e_site s on s.tenant_id = d.tenant_id and s.id = d.site \
           where d.tenant_id = $1 and d.edge_customer = any($2) and d.lifecycle <> 'Retired') r",
    )
    .bind(ctx.tenant)
    .bind(&ids)
    .fetch_one(&mut *tx)
    .await?;
    let dashboards: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(jsonb_build_object('id', b.id, 'name', b.dashboard_name, 'description', b.description) order by b.dashboard_name), '[]') \
         from e_edge_dashboard b where b.tenant_id = $1 and b.customer = any($2)",
    )
    .bind(ctx.tenant)
    .bind(&ids)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"customers": theirs.iter().map(|(_, name)| name).collect::<Vec<_>>(), "devices": devices, "dashboards": dashboards}))
}

/// That a device is theirs, or that as far as they can tell it does not exist.
async fn device_of(k: &Kernel, ctx: &Ctx, id: Uuid) -> Result<Value> {
    let theirs = ids(k, ctx).await?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let found: Option<Value> = sqlx::query_scalar(
        "select jsonb_build_object('id', d.id, 'code', d.device_code, 'name', d.device_name, 'lifecycle', d.lifecycle, 'site', s.site_name) \
         from e_device d left join e_site s on s.tenant_id = d.tenant_id and s.id = d.site \
         where d.tenant_id = $1 and d.id = $2 and d.edge_customer = any($3) and d.lifecycle <> 'Retired'",
    )
    .bind(ctx.tenant)
    .bind(id)
    .bind(&theirs)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    found.ok_or_else(|| Error::NotFound("device".into()))
}

async fn device(k: &Kernel, ctx: &Ctx, id: Uuid) -> Result<Value> {
    let device = device_of(k, ctx, id).await?;
    let latest = ferrum_ops::telemetry::latest(k, &sys(ctx), id).await?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let alarms: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(r order by r.raised_at desc), '[]') from ( \
           select a.id, a.summary, a.severity, a.status, a.raised_at, a.cleared_at, a.acknowledged_by from e_edge_alarm a \
           where a.tenant_id = $1 and a.device = $2 order by a.raised_at desc limit 50) r",
    )
    .bind(ctx.tenant)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"device": device, "latest": latest, "alarms": alarms}))
}

async fn dashboard(k: &Kernel, ctx: &Ctx, id: Uuid) -> Result<Value> {
    let theirs = ids(k, ctx).await?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("select dashboard_name, body from e_edge_dashboard where tenant_id = $1 and id = $2 and customer = any($3)")
            .bind(ctx.tenant)
            .bind(id)
            .bind(&theirs)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    let Some((name, Some(body))) = row else { return Err(Error::NotFound("dashboard".into())) };
    site::laid_out(k, &sys(ctx), &name, &body).await
}

async fn acknowledge(k: &Kernel, ctx: &Ctx, id: Uuid, note: Option<&str>) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let device: Option<Option<Uuid>> =
        sqlx::query_scalar("select device from e_edge_alarm where tenant_id = $1 and id = $2").bind(ctx.tenant).bind(id).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    let device = device.flatten().ok_or_else(|| Error::NotFound("alarm".into()))?;
    device_of(k, ctx, device).await.map_err(|_| Error::NotFound("alarm".into()))?;
    crate::alarms::acknowledge(k, ctx, id, note).await
}

fn uuid(seg: &str) -> Result<Uuid> {
    seg.parse().map_err(|_| Error::NotFound("record".into()))
}

pub async fn answer(k: &Kernel, ctx: &Ctx, req: &SiteRequest, path: &[&str]) -> Result<Value> {
    match (req.method.as_str(), path) {
        ("GET", ["devices"]) => mine(k, ctx).await,
        ("GET", ["devices", id]) => device(k, ctx, uuid(id)?).await,
        ("GET", ["devices", id, "history"]) => {
            let id = uuid(id)?;
            device_of(k, ctx, id).await?;
            site::history(k, &sys(ctx), id, req).await
        }
        ("GET", ["dashboards", id]) => dashboard(k, ctx, uuid(id)?).await,
        ("POST", ["alarms", id, "acknowledge"]) => {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            acknowledge(k, ctx, uuid(id)?, body["note"].as_str()).await
        }
        _ => Err(Error::NotFound("route".into())),
    }
}
