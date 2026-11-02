//! A device as others describe one: its twin as it stands, what was asked of it and what it reported;
//! a W3C Thing Description, for what speaks Web of Things; and an Asset Administration Shell, for
//! Industrie 4.0 tools. All three are written from the records, so they cannot disagree.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat, Utc};
use ferrum_kernel::{Ctx, Error, Kernel, Result};
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

/// The ids AAS Part 2 puts in a path, which are base64url without padding.
fn device_of(encoded: &str, prefix: &str, suffix: &str) -> Result<Uuid> {
    let missing = || Error::NotFound("that identifier".into());
    let id = URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('=')).ok().and_then(|b| String::from_utf8(b).ok()).ok_or_else(missing)?;
    id.strip_prefix(prefix).and_then(|i| i.strip_suffix(suffix)).and_then(|i| i.parse().ok()).ok_or_else(missing)
}

/// AAS Part 2's repository, reading only, for a tool that wants shells by asking rather than a file.
/// Its shells are the devices this person can see, the first hundred of them, and they say what they
/// report now, so there is nothing here to write. Submodels and shells stay in step with `aas`.
pub async fn repository(k: &Kernel, ctx: &Ctx, path: &[&str]) -> Result<Value> {
    const SHELL: &str = "urn:ferrum:aas:";
    const SUBMODEL: &str = "urn:ferrum:submodel:";
    const OPERATIONAL: &str = ":operational";
    match path {
        ["shells"] => {
            let found = crate::fleet::list(k, ctx, &crate::fleet::Selection::default()).await?;
            let ids: Vec<Uuid> = found["devices"].as_array().into_iter().flatten().filter_map(|d| d["id"].as_str()?.parse().ok()).take(100).collect();
            let mut shells = Vec::with_capacity(ids.len());
            for id in ids {
                shells.push(administration_shell(k, ctx, id).await?["assetAdministrationShells"][0].clone());
            }
            Ok(json!({"paging_metadata": {}, "result": shells}))
        }
        ["shells", id] => Ok(administration_shell(k, ctx, device_of(id, SHELL, "")?).await?["assetAdministrationShells"][0].clone()),
        ["submodels", id] => Ok(administration_shell(k, ctx, device_of(id, SUBMODEL, OPERATIONAL)?).await?["submodels"][0].clone()),
        _ => Err(Error::NotFound("route".into())),
    }
}

/// What scanners saw, as a GS1 EPCIS 2.0 document: every event from a device whose value names an
/// `epc`, or a list of them as `epcs`, with `action`, `bizStep`, `disposition` and `readPoint` if it
/// says them. Reading only: Ferrum holds no repository of its own, and capture is someone else's to
/// do with this or with an export.
pub async fn epcis(k: &Kernel, ctx: &Ctx, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let rows: Vec<(String, Value, DateTime<Utc>)> = sqlx::query_as(
        "select name, value, at from edge_event where tenant_id = $1 and at >= $2 and at < $3 and (value ? 'epc' or value ? 'epcs') order by at limit 10000",
    )
    .bind(ctx.tenant)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let events: Vec<Value> = rows.iter().map(|(_, value, at)| object_event(value, *at)).collect();
    Ok(json!({
        "@context": "https://ref.gs1.org/standards/epcis/2.0.0/epcis-context.jsonld",
        "type": "EPCISDocument",
        "schemaVersion": "2.0",
        "creationDate": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "epcisBody": {"eventList": events},
    }))
}

fn object_event(said: &Value, at: DateTime<Utc>) -> Value {
    let epcs = said.get("epcs").cloned().unwrap_or_else(|| json!([said["epc"]]));
    let mut event = json!({
        "type": "ObjectEvent",
        "eventTime": at.to_rfc3339_opts(SecondsFormat::Millis, true),
        "eventTimeZoneOffset": "+00:00",
        "epcList": epcs,
        "action": said["action"].as_str().unwrap_or("OBSERVE"),
    });
    for key in ["bizStep", "disposition"] {
        if let Some(value) = said.get(key) {
            event[key] = value.clone();
        }
    }
    if let Some(place) = said.get("readPoint") {
        event["readPoint"] = json!({"id": place});
    }
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_is_an_object_event() {
        let at = "2026-11-02T10:00:00Z".parse().unwrap();
        let said = json!({"epc": "urn:epc:id:sgtin:0614141.107346.2018", "bizStep": "shipping", "readPoint": "urn:epc:id:sgln:0614141.00777.0"});
        let e = object_event(&said, at);
        assert_eq!(e["epcList"], json!(["urn:epc:id:sgtin:0614141.107346.2018"]));
        assert_eq!((e["action"].as_str(), e["bizStep"].as_str()), (Some("OBSERVE"), Some("shipping")));
        assert_eq!(e["readPoint"]["id"], "urn:epc:id:sgln:0614141.00777.0");
        assert_eq!(object_event(&json!({"epcs": ["a", "b"], "action": "ADD"}), at)["epcList"], json!(["a", "b"]));
    }

    #[test]
    fn identifiers_come_in_a_path_as_part_2_has_them() {
        let id = Uuid::now_v7();
        let shell = URL_SAFE_NO_PAD.encode(format!("urn:ferrum:aas:{id}"));
        assert_eq!(device_of(&shell, "urn:ferrum:aas:", "").unwrap(), id);
        assert!(device_of(&shell, "urn:ferrum:submodel:", ":operational").is_err(), "a shell's id is not a submodel's");
        assert!(device_of("not base64 !", "urn:ferrum:aas:", "").is_err());
    }

    #[test]
    fn a_short_id_is_what_aas_allows() {
        assert_eq!(short_id("temp.c"), "temp_c");
        assert_eq!(short_id("1st"), "x1st");
        assert_eq!(short_id("PRESS-1"), "PRESS_1");
    }
}
