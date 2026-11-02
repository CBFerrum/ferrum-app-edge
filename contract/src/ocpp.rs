//! OCPP 1.6-J and 2.0.1: EV chargers call a gateway over a WebSocket, and this turns what they say
//! into metrics, states, attributes and events, and says what a central system says back. The two
//! versions differ in their shapes and hardly in their meaning, so both end up in the same names.
//!
//! What a charger said arrives here as `ocpp/{id}/{Action}`. A call for a charger is `ocpp/{id}/{call
//! id}/{Action}`, and its answer comes back as `ocpp/{id}/answer/{call id}`.

use crate::driver::{Decoded, Ending};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// How often a charger is told to say it is there, in seconds.
const INTERVAL: u32 = 60;

pub const PREFIX: &str = "ocpp/";

/// An operation is `ocpp.Reset`, which is what asks for it.
pub fn action(operation: &str) -> Option<&str> {
    operation.strip_prefix("ocpp.").filter(|a| !a.is_empty())
}

/// `ocpp.ChangeConfiguration` as the topic it goes out on.
pub fn call(code: &str, id: u64, action: &str) -> String {
    format!("{PREFIX}{code}/{id}/{action}")
}

/// Whose it is, and what they did.
pub fn split(topic: &str) -> Option<(&str, &str)> {
    topic.strip_prefix(PREFIX)?.split_once('/')
}

fn metric_name(measurand: &str, phase: Option<&str>) -> String {
    let name = measurand.replace('-', ".").to_lowercase();
    match phase {
        Some(phase) => format!("{name}.{}", phase.to_lowercase().replace('-', ".")),
        None => name,
    }
}

fn meter_values(device: &str, list: &Value, out: &mut Vec<Decoded>) {
    for reading in list.as_array().into_iter().flatten() {
        let at = reading["timestamp"].as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok());
        for sample in reading["sampledValue"].as_array().into_iter().flatten() {
            // 1.6 sends text and 2.0.1 a number, and a signed reading is not one this can read.
            let value = sample["value"].as_f64().or_else(|| sample["value"].as_str()?.parse().ok());
            let Some(value) = value else { continue };
            let measurand = sample["measurand"].as_str().unwrap_or("Energy.Active.Import.Register");
            let unit = sample["unit"].as_str().or(sample["unitOfMeasure"]["unit"].as_str()).map(str::to_string);
            out.push(Decoded::Metric {
                device: Some(device.to_string()),
                metric: metric_name(measurand, sample["phase"].as_str()),
                value,
                unit,
                class: None,
                deadband: None,
                every: None,
                at,
            });
        }
    }
}

pub fn decode(topic: &str, body: &[u8]) -> Vec<Decoded> {
    let Some((device, action)) = split(topic) else { return vec![] };
    let Ok(said) = serde_json::from_slice::<Value>(body) else { return vec![] };
    let code = Some(device.to_string());
    let attribute = |key: &str, value: &Value| Decoded::Attribute { device: code.clone(), key: key.to_string(), value: value.clone() };
    let event = |name: &str, value: Value| Decoded::Event { device: code.clone(), name: name.to_string(), value };
    let mut out = Vec::new();
    match action {
        "BootNotification" => {
            let station = &said["chargingStation"];
            let pairs = [
                ("vendor", said.get("chargePointVendor").or(station.get("vendorName"))),
                ("model", said.get("chargePointModel").or(station.get("model"))),
                ("serial", said.get("chargePointSerialNumber").or(station.get("serialNumber"))),
                ("fw_version", said.get("firmwareVersion").or(station.get("firmwareVersion"))),
            ];
            out.extend(pairs.into_iter().filter_map(|(key, value)| Some(attribute(key, value?))));
            out.push(event("boot", said));
        }
        "StatusNotification" => {
            if let Some(status) = said["status"].as_str().or(said["connectorStatus"].as_str()) {
                out.push(Decoded::State { device: code.clone(), state: status.to_string() });
            }
            out.push(event("connector.status", said));
        }
        "MeterValues" => meter_values(device, &said["meterValue"], &mut out),
        "StartTransaction" => out.push(event("transaction.started", said)),
        "StopTransaction" => {
            meter_values(device, &said["transactionData"], &mut out);
            out.push(event("transaction.stopped", said));
        }
        "TransactionEvent" => {
            meter_values(device, &said["meterValue"], &mut out);
            let kind = said["eventType"].as_str().unwrap_or("Updated").to_lowercase();
            out.push(event(&format!("transaction.{kind}"), said));
        }
        "Heartbeat" => {}
        other => out.push(event(&format!("ocpp.{other}"), said)),
    }
    out
}

/// Whether a card or a token may charge. No list means anyone may: a charger on private ground.
fn allowed(said: &Value, list: Option<&Value>) -> &'static str {
    let tag = said["idTag"].as_str().or(said["idToken"]["idToken"].as_str()).unwrap_or_default();
    match list.and_then(Value::as_array) {
        Some(list) if !list.iter().any(|t| t.as_str() == Some(tag)) => "Invalid",
        _ => "Accepted",
    }
}

/// What a central system says back. `transaction` is a new number, for a charger that asks for one.
pub fn answer(action: &str, said: &Value, id_tags: Option<&Value>, transaction: u32, now: DateTime<Utc>) -> Value {
    let time = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    match action {
        "BootNotification" => json!({"status": "Accepted", "currentTime": time, "interval": INTERVAL}),
        "Heartbeat" => json!({"currentTime": time}),
        "Authorize" if said.get("idToken").is_some() => json!({"idTokenInfo": {"status": allowed(said, id_tags)}}),
        "Authorize" => json!({"idTagInfo": {"status": allowed(said, id_tags)}}),
        "StartTransaction" => json!({"transactionId": transaction, "idTagInfo": {"status": allowed(said, id_tags)}}),
        "TransactionEvent" if said.get("idToken").is_some() => json!({"idTokenInfo": {"status": allowed(said, id_tags)}}),
        _ => json!({}),
    }
}

/// How a call ended. A charger that accepts has only said it will try, which is as much as OCPP says.
pub fn status(topic: &str, body: &[u8], names: &[&str]) -> Option<Ending> {
    let (_, rest) = split(topic)?;
    let id = rest.strip_prefix("answer/")?;
    if !names.contains(&id) {
        return None;
    }
    let said: Value = serde_json::from_slice(body).ok()?;
    match said["status"].as_str() {
        None if said.get("error").is_none() => Some(Ending::Done),
        Some("Accepted" | "Scheduled" | "Unlocked" | "AcceptedCanceled") => Some(Ending::Done),
        _ => Some(Ending::Failed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(topic: &str, body: Value) -> Vec<(String, f64, Option<String>)> {
        decode(topic, body.to_string().as_bytes())
            .into_iter()
            .filter_map(|d| match d {
                Decoded::Metric { metric, value, unit, .. } => Some((metric, value, unit)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn both_versions_end_up_the_same() {
        let old = json!({"connectorId": 1, "meterValue": [{"timestamp": "2026-11-02T10:00:00Z", "sampledValue": [
            {"value": "1500", "measurand": "Energy.Active.Import.Register", "unit": "Wh"},
            {"value": "230.1", "measurand": "Voltage", "phase": "L1-N", "unit": "V"},
        ]}]});
        let new = json!({"evseId": 1, "meterValue": [{"timestamp": "2026-11-02T10:00:00Z", "sampledValue": [
            {"value": 1500, "measurand": "Energy.Active.Import.Register", "unitOfMeasure": {"unit": "Wh"}},
        ]}]});
        let got = metrics("ocpp/CP/MeterValues", old);
        assert_eq!(got[0], ("energy.active.import.register".into(), 1500.0, Some("Wh".into())));
        assert_eq!(got[1].0, "voltage.l1.n");
        assert_eq!(metrics("ocpp/CP/MeterValues", new)[0], got[0]);
    }

    #[test]
    fn a_reading_with_no_measurand_is_the_energy_register() {
        let got = metrics("ocpp/CP/MeterValues", json!({"meterValue": [{"timestamp": "2026-11-02T10:00:00Z", "sampledValue": [{"value": "9"}]}]}));
        assert_eq!(got[0].0, "energy.active.import.register");
    }

    #[test]
    fn a_boot_says_what_it_is() {
        let boot = decode("ocpp/CP/BootNotification", br#"{"chargePointVendor": "V", "chargePointModel": "M", "firmwareVersion": "1.2"}"#);
        assert!(boot.iter().any(|d| matches!(d, Decoded::Attribute { key, value, .. } if key == "fw_version" && value == "1.2")));
        let boot = decode("ocpp/CP/BootNotification", br#"{"reason": "PowerUp", "chargingStation": {"model": "M", "vendorName": "V"}}"#);
        assert!(boot.iter().any(|d| matches!(d, Decoded::Attribute { key, value, .. } if key == "vendor" && value == "V")));
    }

    #[test]
    fn a_status_is_a_state() {
        for body in [
            &br#"{"connectorId": 1, "status": "Charging", "errorCode": "NoError"}"#[..],
            &br#"{"connectorStatus": "Charging", "evseId": 1, "connectorId": 1}"#[..],
        ] {
            assert!(matches!(&decode("ocpp/CP/StatusNotification", body)[0], Decoded::State { state, .. } if state == "Charging"));
        }
    }

    #[test]
    fn what_is_said_back() {
        let now = "2026-11-02T10:00:00Z".parse().unwrap();
        assert_eq!(answer("BootNotification", &json!({}), None, 0, now)["status"], "Accepted");
        assert_eq!(answer("Heartbeat", &json!({}), None, 0, now)["currentTime"], "2026-11-02T10:00:00Z");
        let list = json!(["A1"]);
        assert_eq!(answer("Authorize", &json!({"idTag": "A1"}), Some(&list), 0, now)["idTagInfo"]["status"], "Accepted");
        assert_eq!(answer("Authorize", &json!({"idTag": "B2"}), Some(&list), 0, now)["idTagInfo"]["status"], "Invalid");
        assert_eq!(answer("Authorize", &json!({"idToken": {"idToken": "B2", "type": "ISO14443"}}), Some(&list), 0, now)["idTokenInfo"]["status"], "Invalid");
        assert_eq!(answer("Authorize", &json!({"idTag": "B2"}), None, 0, now)["idTagInfo"]["status"], "Accepted", "no list, no gate");
        let started = answer("StartTransaction", &json!({"idTag": "A1"}), Some(&list), 7, now);
        assert_eq!((started["transactionId"].as_u64(), started["idTagInfo"]["status"].as_str()), (Some(7), Some("Accepted")));
        assert_eq!(answer("StatusNotification", &json!({}), None, 0, now), json!({}));
    }

    #[test]
    fn how_a_call_ended() {
        let topic = "ocpp/CP/answer/7";
        assert_eq!(status(topic, br#"{"status": "Accepted"}"#, &["7"]), Some(Ending::Done));
        assert_eq!(status(topic, br#"{"status": "Rejected"}"#, &["7"]), Some(Ending::Failed));
        assert_eq!(status(topic, br#"{"error": "NotImplemented"}"#, &["7"]), Some(Ending::Failed));
        assert_eq!(status(topic, b"{}", &["7"]), Some(Ending::Done), "some calls have nothing to say");
        assert_eq!(status(topic, b"{}", &["8"]), None);
        assert_eq!(action("ocpp.Reset"), Some("Reset"));
        assert_eq!(action("fan.start"), None);
    }
}
