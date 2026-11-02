//! A device as others describe one: its twin as it stands, what was asked of it and what it reported;
//! a W3C Thing Description, for what speaks Web of Things; and an Asset Administration Shell, for
//! Industrie 4.0 tools. All three are written from the records, so they cannot disagree.

use ferrum_kernel::{Ctx, Kernel, Result};
use ferrum_ops::telemetry;
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// A name as AAS allows it: a letter, then letters, digits and underscores.
pub fn short_id(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if clean.starts_with(|c: char| c.is_ascii_alphabetic()) { clean } else { format!("x{clean}") }
}

struct Device {
    code: String,
    name: String,
    kind: String,
    latest: Vec<Value>,
    attributes: Vec<(String, Option<Value>, Option<Value>)>,
    operations: Vec<String>,
}

async fn gather(k: &Kernel, ctx: &Ctx, id: Uuid) -> Result<Device> {
    ferrum_ops::site::device_site(k, ctx, id).await?;
    let latest = telemetry::latest(k, ctx, id).await?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let (code, name, kind): (String, String, String) = sqlx::query_as("select device_code, device_name, kind from e_device where tenant_id = $1 and id = $2")
        .bind(ctx.tenant)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let attributes = sqlx::query_as("select key, value, desired from edge_attribute where tenant_id = $1 and device_id = $2 order by key")
        .bind(ctx.tenant)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    let operations = sqlx::query_scalar("select operation from e_device__operations where tenant_id = $1 and parent_id = $2 order by idx")
        .bind(ctx.tenant)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Device { code, name, kind, latest, attributes, operations })
}

/// What was asked of a device and what it says, side by side.
pub async fn twin(k: &Kernel, ctx: &Ctx, id: Uuid) -> Result<Value> {
    let d = gather(k, ctx, id).await?;
    let attributes: Map<String, Value> = d
        .attributes
        .into_iter()
        .map(|(key, reported, desired)| {
            let in_step = desired.as_ref().map(|wanted| Some(wanted) == reported.as_ref());
            (key, json!({"reported": reported, "desired": desired, "in_step": in_step}))
        })
        .collect();
    Ok(
        json!({"thing": format!("urn:ferrum:device:{id}"), "code": d.code, "name": d.name, "kind": d.kind, "attributes": attributes, "telemetry": d.latest, "operations": d.operations}),
    )
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        _ => "string",
    }
}

/// A Thing Description, version 1.1. Reading and acting go through this app's own endpoints, under
/// whoever is signed in.
pub async fn thing_description(k: &Kernel, ctx: &Ctx, id: Uuid) -> Result<Value> {
    let d = gather(k, ctx, id).await?;
    let base = format!("/api/app/edge/devices/{id}");
    let mut properties = Map::new();
    for m in &d.latest {
        let Some(metric) = m["metric"].as_str() else { continue };
        properties.insert(metric.to_string(), json!({"type": "number", "readOnly": true, "observable": false, "forms": [{"href": format!("{base}/history?metrics={metric}"), "op": "readproperty"}]}));
    }
    for (key, reported, desired) in &d.attributes {
        let sample = reported.as_ref().or(desired.as_ref()).unwrap_or(&Value::Null);
        properties.insert(key.clone(), json!({"type": json_type(sample), "readOnly": false, "forms": [{"href": base.clone(), "op": "readproperty"}, {"href": format!("{base}/desired"), "op": "writeproperty", "htv:methodName": "PUT"}]}));
    }
    let actions: Map<String, Value> = d
        .operations
        .iter()
        .map(|op| (op.clone(), json!({"forms": [{"href": "/api/app/ops/commands", "op": "invokeaction", "htv:methodName": "POST"}]})))
        .collect();
    Ok(json!({
        "@context": ["https://www.w3.org/2022/wot/td/v1.1", {"htv": "http://www.w3.org/2011/http#"}],
        "id": format!("urn:ferrum:device:{id}"),
        "title": d.name,
        "description": format!("{} {}", d.kind, d.code),
        "securityDefinitions": {"bearer": {"scheme": "bearer"}},
        "security": "bearer",
        "properties": properties,
        "actions": actions,
        "events": {},
    }))
}

/// An environment holding one shell and one submodel of what a device reports now, as AAS Part 2 has
/// it in JSON.
pub async fn administration_shell(k: &Kernel, ctx: &Ctx, id: Uuid) -> Result<Value> {
    let d = gather(k, ctx, id).await?;
    let submodel = format!("urn:ferrum:submodel:{id}:operational");
    let mut elements = Vec::new();
    for m in &d.latest {
        let (Some(metric), Some(value)) = (m["metric"].as_str(), m["value"].as_f64()) else { continue };
        elements.push(json!({"modelType": "Property", "idShort": short_id(metric), "valueType": "xs:double", "value": value.to_string()}));
    }
    for (key, reported, _) in &d.attributes {
        let Some(reported) = reported else { continue };
        let value = reported.as_str().map_or_else(|| reported.to_string(), str::to_string);
        elements.push(json!({"modelType": "Property", "idShort": short_id(key), "valueType": "xs:string", "value": value}));
    }
    Ok(json!({
        "assetAdministrationShells": [{
            "modelType": "AssetAdministrationShell",
            "id": format!("urn:ferrum:aas:{id}"),
            "idShort": short_id(&d.code),
            "assetInformation": {"assetKind": "Instance", "globalAssetId": format!("urn:ferrum:device:{id}")},
            "submodels": [{"type": "ModelReference", "keys": [{"type": "Submodel", "value": submodel}]}],
        }],
        "submodels": [{"modelType": "Submodel", "id": submodel, "idShort": "OperationalData", "submodelElements": elements}],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_id_is_what_aas_allows() {
        assert_eq!(short_id("temp.c"), "temp_c");
        assert_eq!(short_id("1st"), "x1st");
        assert_eq!(short_id("PRESS-1"), "PRESS_1");
    }
}
