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

/// What money depends on. These are kept before a charger is told they were heard, because a charger
/// that gets no answer says them again and one that gets an answer forgets them.
pub fn billing(action: &str) -> bool {
    matches!(action, "StartTransaction" | "StopTransaction" | "MeterValues" | "TransactionEvent")
}

/// One meter reading of a session, in what it was said in.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub at: DateTime<Utc>,
    pub value: f64,
    pub unit: Option<String>,
    /// Signed by the meter, which nothing here can verify.
    pub signed: bool,
}

fn to_wh(value: f64, unit: Option<&str>) -> Option<f64> {
    match unit {
        None | Some("Wh") => Some(value),
        Some("kWh") => Some(value * 1000.0),
        _ => None,
    }
}

/// The energy registers in what a charger said, whatever shape it said them in.
pub fn registers(said: &Value) -> Vec<Reading> {
    let mut out = Vec::new();
    for list in [&said["meterValue"], &said["transactionData"]] {
        for reading in list.as_array().into_iter().flatten() {
            let Some(at) = reading["timestamp"].as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok()) else { continue };
            for sample in reading["sampledValue"].as_array().into_iter().flatten() {
                let measurand = sample["measurand"].as_str().unwrap_or("Energy.Active.Import.Register");
                if measurand != "Energy.Active.Import.Register" || sample["phase"].is_string() {
                    continue;
                }
                let Some(value) = sample["value"].as_f64().or_else(|| sample["value"].as_str()?.parse().ok()) else { continue };
                let signed = sample["format"].as_str() == Some("SignedData") || sample.get("signedMeterValue").is_some();
                let unit = sample["unit"].as_str().or(sample["unitOfMeasure"]["unit"].as_str()).map(str::to_string);
                out.push(Reading { at, value, unit, signed });
            }
        }
    }
    out
}

/// What can be said about a session's energy, and why it may not be enough to bill on.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    /// Energy between the first and last register, when those can be trusted.
    pub energy_wh: Option<f64>,
    /// Anything a person has to look at before this is money. Empty is the only thing that settles.
    pub flags: Vec<&'static str>,
    /// Whether anything shows energy flowed. A charger saying it started does not.
    pub flowed: bool,
}

/// Judges a session's registers. `started` and `stopped` bound it: a reading outside is late or wrong,
/// and is not silently used. Nothing is guessed: a register that went backwards is a reset or a fault,
/// not a smaller number, and a signature nothing here can check is not evidence of anything.
pub fn evidence(mut readings: Vec<Reading>, started: DateTime<Utc>, stopped: Option<DateTime<Utc>>, reported: Option<f64>) -> Evidence {
    let mut flags = Vec::new();
    readings.sort_by_key(|r| r.at);
    let mut within = Vec::new();
    for r in readings {
        if r.at < started || stopped.is_some_and(|end| r.at > end) {
            if !flags.contains(&"reading_outside_session") {
                flags.push("reading_outside_session");
            }
            continue;
        }
        if r.signed && !flags.contains(&"signature_not_verified") {
            flags.push("signature_not_verified");
        }
        match to_wh(r.value, r.unit.as_deref()) {
            Some(wh) => within.push(wh),
            None if !flags.contains(&"unit_not_understood") => flags.push("unit_not_understood"),
            None => {}
        }
    }
    if within.windows(2).any(|w| w[1] < w[0]) {
        flags.push("register_went_backwards");
    }
    let flowed = within.windows(2).any(|w| w[1] > w[0]);
    let energy_wh = match (within.first(), within.last()) {
        (Some(first), Some(last)) if within.len() >= 2 && !flags.contains(&"register_went_backwards") => Some(last - first),
        _ => None,
    };
    if energy_wh.is_none() && !flags.contains(&"register_went_backwards") {
        flags.push("missing_readings");
    }
    if let (Some(said), Some(worked)) = (reported, energy_wh)
        && (said - worked).abs() > 1.0
    {
        flags.push("charger_disagrees_with_its_register");
    }
    if stopped.is_none() {
        flags.push("still_open");
    }
    Evidence { energy_wh, flags, flowed }
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

    fn at(minute: u32) -> DateTime<Utc> {
        format!("2026-11-02T10:{minute:02}:00Z").parse().unwrap()
    }

    fn wh(minute: u32, value: f64) -> Reading {
        Reading { at: at(minute), value, unit: Some("Wh".into()), signed: false }
    }

    #[test]
    fn a_session_is_as_much_as_its_register_shows() {
        let good = evidence(vec![wh(1, 1000.0), wh(5, 1400.0), wh(9, 2000.0)], at(0), Some(at(10)), Some(1000.0));
        assert_eq!((good.energy_wh, good.flowed, good.flags.is_empty()), (Some(1000.0), true, true), "{good:?}");
        let kwh = |m, v| Reading { at: at(m), value: v, unit: Some("kWh".into()), signed: false };
        assert_eq!(evidence(vec![kwh(1, 1.0), wh(9, 3000.0)], at(0), Some(at(10)), None).energy_wh, Some(2000.0), "units are converted, not assumed");

        // AT-OPS-012, one fault at a time. None of them produces a number to bill.
        let reset = evidence(vec![wh(1, 5000.0), wh(5, 10.0), wh(9, 400.0)], at(0), Some(at(10)), None);
        assert_eq!((reset.energy_wh, reset.flags.contains(&"register_went_backwards")), (None, true), "{reset:?}");
        let late = evidence(vec![wh(1, 1000.0), wh(9, 2000.0), wh(20, 9000.0)], at(0), Some(at(10)), None);
        assert_eq!(late.energy_wh, Some(1000.0), "a late reading is not used");
        assert!(late.flags.contains(&"reading_outside_session"), "and is said");
        let missing = evidence(vec![wh(1, 1000.0)], at(0), Some(at(10)), None);
        assert_eq!((missing.energy_wh, missing.flags.contains(&"missing_readings")), (None, true));
        assert!(evidence(vec![], at(0), Some(at(10)), None).flags.contains(&"missing_readings"));
        let odd = Reading { at: at(5), value: 3.0, unit: Some("varh".into()), signed: false };
        assert!(evidence(vec![wh(1, 1.0), odd, wh(9, 2.0)], at(0), Some(at(10)), None).flags.contains(&"unit_not_understood"));
        let signed = Reading { at: at(5), value: 1500.0, unit: Some("Wh".into()), signed: true };
        assert!(evidence(vec![wh(1, 1000.0), signed, wh(9, 2000.0)], at(0), Some(at(10)), None).flags.contains(&"signature_not_verified"));
        let disagrees = evidence(vec![wh(1, 1000.0), wh(9, 2000.0)], at(0), Some(at(10)), Some(1500.0));
        assert!(disagrees.flags.contains(&"charger_disagrees_with_its_register"));
        assert!(evidence(vec![wh(1, 1.0), wh(2, 2.0)], at(0), None, None).flags.contains(&"still_open"));
    }

    #[test]
    fn accepting_a_start_is_not_energy_flowing() {
        assert!(!evidence(vec![wh(1, 1000.0), wh(5, 1000.0), wh(9, 1000.0)], at(0), Some(at(10)), None).flowed);
    }

    #[test]
    fn registers_are_found_in_either_version() {
        let old = json!({"meterValue": [{"timestamp": "2026-11-02T10:00:00Z", "sampledValue": [
            {"value": "1500", "measurand": "Energy.Active.Import.Register", "unit": "Wh"},
            {"value": "99", "measurand": "Energy.Active.Import.Register", "phase": "L1"},
            {"value": "230", "measurand": "Voltage"},
            {"value": "AbCd", "format": "SignedData", "measurand": "Energy.Active.Import.Register"},
        ]}]});
        let new = json!({"meterValue": [{"timestamp": "2026-11-02T10:00:00Z", "sampledValue": [
            {"value": 1.5, "unitOfMeasure": {"unit": "kWh"}, "signedMeterValue": {"signedMeterData": "x"}},
        ]}]});
        assert_eq!(registers(&old).len(), 1, "one register, not its phases or a signature's bytes");
        assert_eq!(registers(&new), [Reading { at: at(0), value: 1.5, unit: Some("kWh".into()), signed: true }]);
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
