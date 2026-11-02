//! A fleet at once: finding devices by what they have in common, adding them in numbers, and asking
//! many of them for the same thing. Every command here is issued one device at a time through the
//! ledger, so what would stop a person asking one device stops this for that device, and says so.

use chrono::{DateTime, Duration, Utc};
use ferrum_domain::util::elevated;
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result};
use ferrum_ops::commands::{self, Issuer, NewCommand};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Devices one request may touch, as many as a list can.
pub const MAX_SELECTED: i64 = 500;
/// Rows one import may hold.
const MAX_IMPORTED: usize = 5_000;
/// Days of history an availability figure can look at.
const MAX_AVAILABILITY_DAYS: i64 = 90;

/// Which devices. Everything given has to hold, and at least one thing has to be given, so that a
/// slip does not mean every device there is.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub ids: Vec<Uuid>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub site: Option<Uuid>,
    #[serde(default)]
    pub definition: Option<Uuid>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub asset: Option<Uuid>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.q.is_none() && self.site.is_none() && self.definition.is_none() && self.tag.is_none() && self.asset.is_none()
    }

    pub fn from_query(q: &serde_json::Map<String, Value>) -> Selection {
        let text = |k: &str| q.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_string);
        let id = |k: &str| text(k).and_then(|v| v.parse().ok());
        Selection { ids: vec![], q: text("q"), site: id("site"), definition: id("definition"), tag: text("tag"), asset: id("asset") }
    }
}

/// `$2` to `$7`: what [`bind`] gives. A tag is one of a comma-separated list, and an asset takes in
/// everything under it.
const CHOSEN: &str = "($2::uuid[] = '{}' or d.id = any($2)) \
   and ($3::text is null or d.device_code ilike '%' || $3 || '%' or d.device_name ilike '%' || $3 || '%') \
   and ($4::uuid is null or d.site = $4) \
   and ($5::uuid is null or d.edge_definition = $5) \
   and ($6::text is null or ',' || replace(lower(d.edge_tags), ' ', '') || ',' like '%,' || lower($6) || ',%') \
   and ($7::uuid is null or d.edge_asset in ( \
     with recursive below as (select id from e_edge_asset where tenant_id = $1 and id = $7 \
       union select a.id from e_edge_asset a join below b on a.parent = b.id where a.tenant_id = $1) select id from below))";

macro_rules! bind {
    ($query:expr, $ctx:expr, $s:expr) => {
        $query.bind($ctx.tenant).bind(&$s.ids).bind(&$s.q).bind($s.site).bind($s.definition).bind(&$s.tag).bind($s.asset)
    };
}

pub async fn list(k: &Kernel, ctx: &Ctx, s: &Selection) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let sql = format!(
        "select coalesce(jsonb_agg(r order by r.code), '[]') from ( \
           select d.id, d.device_code as code, d.device_name as name, d.kind, d.lifecycle, d.edge_tags as tags, s.site_name as site, g.device_name as gateway, f.definition_name as definition, \
             (select count(*) from e_edge_alarm a where a.tenant_id = d.tenant_id and a.device = d.id and a.status <> 'Cleared') as alarms, \
             (select max(r.last_time) from op_rollup_minute r where r.tenant_id = d.tenant_id and r.device_id = d.id and r.minute > now() - interval '30 days') as last_seen, \
             (select a.value from edge_attribute a where a.tenant_id = d.tenant_id and a.device_id = d.id and a.key = 'active') as active \
           from e_device d left join e_site s on s.tenant_id = d.tenant_id and s.id = d.site \
           left join e_device g on g.tenant_id = d.tenant_id and g.id = d.gateway \
           left join e_device_definition f on f.tenant_id = d.tenant_id and f.id = d.edge_definition \
           where d.tenant_id = $1 and d.lifecycle <> 'Retired' and d.kind <> 'Gateway' and {CHOSEN} \
           order by d.device_code limit {MAX_SELECTED}) r"
    );
    let rows: Value = bind!(sqlx::query_scalar(sqlx::AssertSqlSafe(sql)), ctx, s).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(json!({"devices": rows}))
}

pub async fn chosen(k: &Kernel, ctx: &Ctx, s: &Selection) -> Result<Vec<(Uuid, String)>> {
    if s.is_empty() {
        return Err(Error::invalid("say which devices"));
    }
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let sql = format!(
        "select d.id, d.device_code from e_device d where d.tenant_id = $1 and d.lifecycle <> 'Retired' and d.kind <> 'Gateway' and {CHOSEN} order by d.device_code limit {}",
        MAX_SELECTED + 1
    );
    let rows: Vec<(Uuid, String)> = bind!(sqlx::query_as(sqlx::AssertSqlSafe(sql)), ctx, s).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    if rows.len() as i64 > MAX_SELECTED {
        return Err(Error::invalid(format!("that is more than {MAX_SELECTED} devices: narrow it down")));
    }
    Ok(rows)
}

/// Asks each device for the same thing, each in its own transaction. One that cannot says why, and
/// the rest go on. Each is the person's own request, under their authority.
pub async fn command(k: &Kernel, ctx: &Ctx, b: &Value) -> Result<Value> {
    let s: Selection = serde_json::from_value(b["select"].clone()).map_err(|e| Error::invalid(e.to_string()))?;
    let operation = b["operation"].as_str().filter(|o| !o.is_empty()).ok_or_else(|| Error::field("operation", "say what to ask for"))?;
    let expires = b["expires_in_secs"].as_i64().unwrap_or(3600).clamp(10, 86_400);
    let batch = Uuid::now_v7();
    let mut outcomes = Vec::new();
    for (id, code) in chosen(k, ctx, &s).await? {
        let cmd = NewCommand {
            target: id,
            operation: operation.to_string(),
            schema_version: 1,
            payload: b["payload"].clone(),
            expires_in_secs: expires,
            workflow_id: Some(format!("bulk:{batch}")),
            issuer: Issuer::Person,
            ..Default::default()
        };
        outcomes.push(asked(k, ctx, code, cmd).await?);
    }
    Ok(json!({"batch": batch, "outcomes": outcomes}))
}

async fn asked(k: &Kernel, ctx: &Ctx, code: String, cmd: NewCommand) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    match commands::issue(&mut tx, ctx, cmd).await {
        Ok(issued) => {
            tx.commit().await?;
            Ok(json!({"device": code, "command": issued.id, "state": issued.state.as_str()}))
        }
        Err(e @ (Error::Validation(_) | Error::Invalid(_) | Error::Forbidden(_) | Error::NotFound(_))) => Ok(json!({"device": code, "refused": e.to_string()})),
        Err(e) => Err(e),
    }
}

/// What many devices should be set to. It is the same as asking one, for each.
pub async fn desire(k: &Kernel, ctx: &Ctx, b: &Value) -> Result<Value> {
    let s: Selection = serde_json::from_value(b["select"].clone()).map_err(|e| Error::invalid(e.to_string()))?;
    let key = b["key"].as_str().filter(|o| !o.is_empty()).ok_or_else(|| Error::field("key", "say which setting"))?;
    let mut outcomes = Vec::new();
    for (id, code) in chosen(k, ctx, &s).await? {
        outcomes.push(match crate::site::desire(k, ctx, id, key, &b["value"]).await {
            Ok(done) => json!({"device": code, "command": done["command"], "state": done["state"]}),
            Err(e @ (Error::Validation(_) | Error::Invalid(_) | Error::Forbidden(_) | Error::NotFound(_))) => json!({"device": code, "refused": e.to_string()}),
            Err(e) => return Err(e),
        });
    }
    Ok(json!({"outcomes": outcomes}))
}

fn cells(line: &str) -> Vec<String> {
    let (mut out, mut cell, mut quoted, mut chars) = (Vec::new(), String::new(), false, line.chars().peekable());
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cell)),
            (c, _) => cell.push(c),
        }
    }
    out.push(cell);
    out.iter().map(|c| c.trim().to_string()).collect()
}

/// A CSV with a heading: `code` and `name` at least, and `kind`, `site`, `gateway`, `definition`,
/// `tags` and `address` if wanted, by code and name as people know them. Each row stands alone, and
/// every row's outcome comes back. With `tokens`, each new device gets an access token, shown once.
pub async fn import(k: &Kernel, ctx: &Ctx, b: &Value) -> Result<Value> {
    let text = b["csv"].as_str().ok_or_else(|| Error::field("csv", "there is nothing to import"))?;
    let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
    let heading: Vec<String> = lines.next().map(|(_, l)| cells(l)).unwrap_or_default().iter().map(|c| c.to_lowercase()).collect();
    let column = |name: &str| heading.iter().position(|c| c == name);
    let (Some(code_at), Some(name_at)) = (column("code"), column("name")) else {
        return Err(Error::field("csv", "the first line says what each column is: code and name at least"));
    };
    let rows: Vec<_> = lines.collect();
    if rows.len() > MAX_IMPORTED {
        return Err(Error::field("csv", format!("that is more than {MAX_IMPORTED} devices at once")));
    }
    let want_tokens = b["tokens"].as_bool().unwrap_or(false);
    let (mut made, mut failed, mut tokens) = (0, Vec::new(), Vec::new());
    for (number, line) in rows {
        let row = cells(line);
        let cell = |name: &str| column(name).and_then(|i| row.get(i)).filter(|c| !c.is_empty()).cloned();
        let (Some(code), Some(name)) = (row.get(code_at).filter(|c| !c.is_empty()), row.get(name_at).filter(|c| !c.is_empty())) else {
            failed.push(json!({"line": number + 1, "why": "it needs a code and a name"}));
            continue;
        };
        let mut doc = Doc::new();
        doc.insert("device_code".into(), json!(code));
        doc.insert("device_name".into(), json!(name));
        doc.insert("kind".into(), json!(cell("kind").unwrap_or_else(|| "Sensor".into())));
        if let Some(tags) = cell("tags") {
            doc.insert("edge_tags".into(), json!(tags.replace(';', ",")));
        }
        if let Some(address) = cell("address") {
            doc.insert("edge_connect".into(), json!(address));
        }
        let token = want_tokens.then(|| {
            let mut bytes = [0u8; 24];
            rand::rng().fill_bytes(&mut bytes);
            hex::encode(bytes)
        });
        if let Some(token) = &token {
            doc.insert("edge_token_hash".into(), json!(hex::encode(Sha256::digest(token.as_bytes()))));
        }
        let outcome = async {
            let mut tx = k.db.tenant_tx(ctx.tenant).await?;
            for (field, sql, value) in [
                ("site", "select id from e_site where tenant_id = $1 and (site_code = $2 or site_name = $2)", cell("site")),
                ("gateway", "select id from e_device where tenant_id = $1 and device_code = $2", cell("gateway")),
                ("edge_definition", "select id from e_device_definition where tenant_id = $1 and definition_name = $2", cell("definition")),
            ] {
                let Some(value) = value else { continue };
                let found: Option<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).bind(ctx.tenant).bind(&value).fetch_optional(&mut *tx).await?;
                doc.insert(
                    field.into(),
                    json!(found.ok_or_else(|| Error::invalid(format!("there is no {} called {value}", field.trim_start_matches("edge_"))))?),
                );
            }
            // What was read and checked here is what is stored. Only hashes are read-only, and those
            // this makes itself.
            k.insert_tx(&elevated(ctx), &mut tx, "Device", doc).await?;
            tx.commit().await?;
            Ok::<_, Error>(())
        };
        match outcome.await {
            Ok(()) => {
                made += 1;
                if let Some(token) = token {
                    tokens.push(json!({"code": code, "token": token}));
                }
            }
            Err(e) => failed.push(json!({"line": number + 1, "code": code, "why": e.to_string()})),
        }
    }
    Ok(json!({"created": made, "failed": failed, "tokens": tokens}))
}

/// A person showing they hold a device: the secret it said it could be claimed with, in time. It
/// says who holds it and nothing more: it still has to be commissioned before it takes work.
pub async fn claim(k: &Kernel, ctx: &Ctx, code: &str, secret: &str) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let found: Option<(Uuid, String)> = sqlx::query_as("select id, lifecycle from e_device where tenant_id = $1 and device_code = $2")
        .bind(ctx.tenant)
        .bind(code)
        .fetch_optional(&mut *tx)
        .await?;
    let hash = hex::encode(Sha256::digest(secret.as_bytes()));
    let offered: Option<Value> = sqlx::query_scalar(
        "select value from edge_event where tenant_id = $1 and device_id = $2 and name = 'claim' and value ->> 'secret_hash' = $3 \
         and at + make_interval(secs => (value ->> 'for_ms')::float8 / 1000) > now() order by at desc limit 1",
    )
    .bind(ctx.tenant)
    .bind(found.as_ref().map(|f| f.0))
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    // One answer for every way of being wrong, so that this says nothing about which devices exist.
    let (Some((device, lifecycle)), Some(_)) = (found, offered) else { return Err(Error::Forbidden("that device cannot be claimed with that".into())) };
    tx.commit().await?;
    if lifecycle == "Imported" {
        ferrum_ops::devices::set_lifecycle(k, ctx, device, ferrum_ops::contract::Lifecycle::Claimed, "claimed with its own secret").await?;
    }
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    ferrum_kernel::audit::record(&mut tx, ctx, "Device", device, "edge.claimed", None, json!({"by": ctx.email})).await?;
    tx.commit().await?;
    Ok(json!({"device": device}))
}

/// How much of each day a device was heard, as a share of the minutes in it.
pub async fn availability(k: &Kernel, ctx: &Ctx, device: Uuid, days: i64) -> Result<Value> {
    ferrum_ops::site::device_site(k, ctx, device).await?;
    let days = days.clamp(1, MAX_AVAILABILITY_DAYS);
    let from = (Utc::now() - Duration::days(days - 1)).date_naive().and_hms_opt(0, 0, 0).map(|d| d.and_utc()).unwrap_or_else(Utc::now);
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<(DateTime<Utc>, i64)> = sqlx::query_as(
        "select date_trunc('day', minute) as day, count(distinct minute) from op_rollup_minute \
         where tenant_id = $1 and device_id = $2 and minute >= $3 group by 1 order by 1",
    )
    .bind(ctx.tenant)
    .bind(device)
    .bind(from)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let total: i64 = rows.iter().map(|(_, minutes)| minutes).sum();
    let so_far = ((Utc::now() - from).num_minutes()).max(1);
    let by_day: Vec<Value> = rows.into_iter().map(|(day, minutes)| json!({"day": day, "minutes": minutes})).collect();
    Ok(json!({"from": from, "percent": (total as f64 / so_far as f64 * 1000.0).round() / 10.0, "days": by_day}))
}

/// The whole fleet at a glance: how each definition and site stands, and what is running on it.
pub async fn health(k: &Kernel, ctx: &Ctx) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let group = |by: &str, join: &str| {
        format!(
            "select coalesce(jsonb_agg(r order by r.name), '[]') from ( \
               select coalesce({by}, 'None') as name, count(*) as devices, count(*) filter (where h.heard) as heard, \
                 count(*) filter (where d.lifecycle in ('Commissioned', 'Active')) as in_service, coalesce(sum(a.open), 0) as alarms \
               from e_device d {join} \
               left join lateral (select exists (select 1 from op_rollup_minute r where r.tenant_id = d.tenant_id and r.device_id = d.id and r.minute > now() - interval '15 minutes') as heard) h on true \
               left join lateral (select count(*) as open from e_edge_alarm a where a.tenant_id = d.tenant_id and a.device = d.id and a.status <> 'Cleared') a on true \
               where d.tenant_id = $1 and d.lifecycle <> 'Retired' and d.kind <> 'Gateway' group by 1) r"
        )
    };
    let by_definition: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(group(
        "f.definition_name",
        "left join e_device_definition f on f.tenant_id = d.tenant_id and f.id = d.edge_definition",
    )))
    .bind(ctx.tenant)
    .fetch_one(&mut *tx)
    .await?;
    let by_site: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(group("s.site_name", "left join e_site s on s.tenant_id = d.tenant_id and s.id = d.site")))
        .bind(ctx.tenant)
        .fetch_one(&mut *tx)
        .await?;
    let versions: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(r order by r.devices desc), '[]') from ( \
           select a.value #>> '{}' as version, count(*) as devices from edge_attribute a \
           where a.tenant_id = $1 and a.key in ('fw_version', 'firmware') and a.value is not null group by 1) r",
    )
    .bind(ctx.tenant)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"definitions": by_definition, "sites": by_site, "versions": versions}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_csv_line_can_hold_a_comma() {
        assert_eq!(cells(r#"PRESS-1, "Press, big" ,"say ""hi""",,"#), ["PRESS-1", "Press, big", r#"say "hi""#, "", ""]);
    }

    #[test]
    fn nothing_is_not_everything() {
        assert!(Selection::default().is_empty());
        assert!(!Selection { tag: Some("line-1".into()), ..Default::default() }.is_empty());
        let mut q = serde_json::Map::new();
        q.insert("q".into(), json!(""));
        q.insert("tag".into(), json!("cold"));
        q.insert("site".into(), json!("not a uuid"));
        let s = Selection::from_query(&q);
        assert_eq!((s.q, s.tag.as_deref(), s.site), (None, Some("cold"), None));
    }
}
