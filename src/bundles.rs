//! A solution in one file: the definitions, rules, flows and dashboards that go together for one kind
//! of job. Installing one adds them as a person would, as drafts where there is such a thing, so a
//! flow from a file still goes through simulation and review before it runs, and a name already
//! taken is left alone and said to be.

use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

macro_rules! solutions {
    ($($file:literal),* $(,)?) => {
        const FILES: &[&str] = &[$(include_str!(concat!("../solutions/", $file))),*];
    };
}

solutions!["cold-rooms.yaml", "energy-meters.yaml"];

/// Most of anything one bundle can hold.
const MAX_ITEMS: usize = 200;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Named {
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bundle {
    pub name: String,
    #[serde(default)]
    pub about: String,
    #[serde(default)]
    pub definitions: Vec<Named>,
    #[serde(default)]
    pub flows: Vec<Named>,
    #[serde(default)]
    pub dashboards: Vec<Named>,
    /// As an Edge Rule's own fields.
    #[serde(default)]
    pub rules: Vec<serde_json::Map<String, Value>>,
}

pub fn shipped() -> Vec<Bundle> {
    FILES.iter().filter_map(|f| serde_yaml::from_str(f).ok()).collect()
}

async fn add(k: &Kernel, ctx: &Ctx, entity: &str, what: &str, name: &str, doc: Doc, out: &mut Vec<Value>) -> Result<()> {
    out.push(match k.insert(ctx, entity, doc).await {
        Ok(_) => json!({"what": what, "name": name, "added": true}),
        Err(e @ (Error::Validation(_) | Error::Invalid(_) | Error::Conflict(_))) => json!({"what": what, "name": name, "added": false, "why": e.to_string()}),
        Err(e) => return Err(e),
    });
    Ok(())
}

/// Adds a bundle under the caller's own permissions, so it can do nothing they could not.
pub async fn install(k: &Kernel, ctx: &Ctx, b: &Bundle) -> Result<Value> {
    if b.definitions.len() + b.flows.len() + b.dashboards.len() + b.rules.len() > MAX_ITEMS {
        return Err(Error::invalid(format!("that is more than {MAX_ITEMS} things in one bundle")));
    }
    let doc = |v: Value| v.as_object().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for d in &b.definitions {
        add(k, ctx, "DeviceDefinition", "definition", &d.name, doc(json!({"definition_name": d.name, "body": d.body})), &mut out).await?;
    }
    for r in &b.rules {
        let name = r.get("rule_name").and_then(Value::as_str).unwrap_or_default().to_string();
        add(k, ctx, "EdgeRule", "rule", &name, r.clone(), &mut out).await?;
    }
    for f in &b.flows {
        add(k, ctx, "EdgeFlow", "flow", &f.name, doc(json!({"flow_name": f.name, "body": f.body})), &mut out).await?;
    }
    for d in &b.dashboards {
        add(k, ctx, "EdgeDashboard", "dashboard", &d.name, doc(json!({"dashboard_name": d.name, "body": d.body})), &mut out).await?;
    }
    Ok(json!({"items": out}))
}

/// What is here now, as a bundle to take somewhere else. Never exports, secrets or tokens: those
/// belong to a site, and a file gets passed around.
pub async fn export(k: &Kernel, ctx: &Ctx, name: &str) -> Result<Bundle> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let named = |rows: Vec<(String, Option<String>)>| rows.into_iter().filter_map(|(name, body)| Some(Named { name, body: body? })).collect::<Vec<_>>();
    let definitions = sqlx::query_as("select definition_name, coalesce(published_body, body) from e_device_definition where tenant_id = $1 order by 1")
        .bind(ctx.tenant)
        .fetch_all(&mut *tx)
        .await?;
    let flows = sqlx::query_as("select flow_name, coalesce(published_body, body) from e_edge_flow where tenant_id = $1 order by 1")
        .bind(ctx.tenant)
        .fetch_all(&mut *tx)
        .await?;
    let dashboards =
        sqlx::query_as("select dashboard_name, body from e_edge_dashboard where tenant_id = $1 order by 1").bind(ctx.tenant).fetch_all(&mut *tx).await?;
    let rules: Vec<Value> = sqlx::query_scalar(
        "select jsonb_strip_nulls(jsonb_build_object('rule_name', rule_name, 'metric', metric, 'comparison', comparison, 'limit_value', limit_value, 'clear_value', clear_value, \
           'hold_secs', hold_secs, 'severity', severity)) from e_edge_rule where tenant_id = $1 order by rule_name",
    )
    .bind(ctx.tenant)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Bundle {
        name: name.to_string(),
        about: String::new(),
        definitions: named(definitions),
        flows: named(flows),
        dashboards: named(dashboards),
        rules: rules.into_iter().filter_map(|r| r.as_object().cloned()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrum_edge_contract::{driver, flow};

    #[test]
    fn what_ships_is_valid() {
        let bundles = shipped();
        assert_eq!(bundles.len(), FILES.len(), "each file reads");
        for b in bundles {
            for d in &b.definitions {
                assert!(driver::parse(&d.body).is_ok(), "{}: {}", b.name, d.name);
            }
            for f in &b.flows {
                flow::parse(&f.body).unwrap_or_else(|e| panic!("{}: {}: {e}", b.name, f.name));
            }
            for d in &b.dashboards {
                crate::data::check(&d.body).unwrap_or_else(|e| panic!("{}: {}: {e}", b.name, d.name));
            }
        }
    }
}
