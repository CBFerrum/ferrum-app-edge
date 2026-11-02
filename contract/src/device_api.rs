//! The device API: what a device says to a gateway with no definition of its own, the same over MQTT,
//! HTTP and CoAP. The shapes are the ones ThingsBoard's devices and its gateway already speak, so that
//! their firmware needs no change to come here.
//!
//! A server takes what a device says and hands it on under the device's own name, whoever it claimed
//! to be: `v1/devices/me/telemetry` from `PRESS-1` is `v1/devices/PRESS-1/telemetry` from here on.

use crate::driver::{Decoded, Ending};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

/// Values one message can carry. A device that sends more is at fault, and costs this much.
pub const MAX_KEYS: usize = 1000;

const DEVICES: &str = "v1/devices/";
const GATEWAY: &str = "v1/gateway/";
/// What a server says about a session: `$session/PRESS-1/online`.
const SESSION: &str = "$session/";

/// A topic as the device said it, with its own name where it said `me`.
pub fn named(topic: &str, code: &str) -> String {
    if let Some(rest) = topic.strip_prefix("v1/devices/me/") {
        return format!("{DEVICES}{code}/{rest}");
    }
    if let Some(rest) = topic.strip_prefix(GATEWAY) {
        return format!("{GATEWAY}{code}/{rest}");
    }
    topic.to_string()
}

/// A topic as a device hears it, the other way round.
pub fn as_heard(topic: &str, code: &str) -> String {
    if let Some(rest) = topic.strip_prefix(DEVICES).and_then(|t| t.strip_prefix(code)).and_then(|t| t.strip_prefix('/')) {
        return format!("v1/devices/me/{rest}");
    }
    if let Some(rest) = topic.strip_prefix(GATEWAY).and_then(|t| t.strip_prefix(code)).and_then(|t| t.strip_prefix('/')) {
        return format!("{GATEWAY}{rest}");
    }
    topic.to_string()
}

/// What says a device added itself: `$joined/NEW-1`.
pub fn joined(code: &str) -> String {
    format!("$joined/{code}")
}

/// Who added themselves, if that is what this says.
pub fn joiner(topic: &str) -> Option<&str> {
    topic.strip_prefix("$joined/")
}

pub fn session(code: &str, online: bool) -> String {
    format!("{SESSION}{code}/{}", if online { "online" } else { "offline" })
}

/// Which of a device's topics this is, by what the topic ends with: a code can have almost anything in it.
fn split<'a>(topic: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let rest = topic.strip_prefix(prefix)?;
    ["/telemetry", "/attributes", "/rpc", "/claim", "/connect", "/disconnect"].iter().find_map(|tail| {
        let at = rest.rfind(tail)?;
        let after = &rest[at + 1..];
        (after == &tail[1..] || after.starts_with(&format!("{}/", &tail[1..]))).then(|| (&rest[..at], after))
    })
}

fn time(ms: &Value) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms.as_i64()?)
}

fn value(device: &str, key: &str, v: &Value, at: Option<DateTime<Utc>>, out: &mut Vec<Decoded>) {
    let metric = match v {
        Value::Number(n) => n.as_f64(),
        // A flag is drawn and compared like a number, and a chart of one is useful.
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    };
    out.push(match metric {
        Some(value) => {
            Decoded::Metric { device: Some(device.to_string()), metric: key.to_string(), value, unit: None, class: None, deadband: None, every: None, at }
        }
        None => Decoded::Attribute { device: Some(device.to_string()), key: key.to_string(), value: v.clone() },
    });
}

fn values(device: &str, map: &Map<String, Value>, at: Option<DateTime<Utc>>, out: &mut Vec<Decoded>) {
    for (k, v) in map.iter().take(MAX_KEYS.saturating_sub(out.len())) {
        value(device, k, v, at, out);
    }
}

/// `{"a": 1}`, `{"ts": 1700000000000, "values": {"a": 1}}`, or a list of either.
fn telemetry(device: &str, body: &Value, out: &mut Vec<Decoded>) {
    match body {
        Value::Array(items) => items.iter().for_each(|i| telemetry(device, i, out)),
        Value::Object(m) => match (m.get("ts").and_then(time), m.get("values").and_then(Value::as_object)) {
            (at @ Some(_), Some(inner)) => values(device, inner, at, out),
            _ => values(device, m, None, out),
        },
        _ => {}
    }
}

fn attributes(device: &str, body: &Value, out: &mut Vec<Decoded>) {
    for (k, v) in body.as_object().into_iter().flatten().take(MAX_KEYS) {
        out.push(Decoded::Attribute { device: Some(device.to_string()), key: k.clone(), value: v.clone() });
    }
}

fn presence(device: &str, online: bool, out: &mut Vec<Decoded>) {
    out.push(Decoded::Attribute { device: Some(device.to_string()), key: "active".into(), value: json!(online) });
    let key = if online { "last_connected_at" } else { "last_disconnected_at" };
    out.push(Decoded::Attribute { device: Some(device.to_string()), key: key.into(), value: json!(Utc::now().to_rfc3339()) });
}

/// What a message says, by whom.
pub fn decode(topic: &str, body: &[u8]) -> Vec<Decoded> {
    let mut out = Vec::new();
    if let Some(rest) = topic.strip_prefix(SESSION) {
        if let Some((code, state)) = rest.rsplit_once('/') {
            presence(code, state == "online", &mut out);
        }
        return out;
    }
    let Ok(v) = serde_json::from_slice::<Value>(body) else { return out };
    if let Some((code, tail)) = split(topic, DEVICES) {
        match tail {
            "telemetry" => telemetry(code, &v, &mut out),
            "attributes" => attributes(code, &v, &mut out),
            "claim" => out.push(Decoded::Event { device: Some(code.to_string()), name: "claim".into(), value: v }),
            _ => {
                if let Some(id) = tail.strip_prefix("rpc/request/") {
                    out.push(Decoded::Event {
                        device: Some(code.to_string()),
                        name: "rpc.request".into(),
                        value: json!({"id": id, "method": v["method"], "params": v["params"]}),
                    });
                }
            }
        }
        return out;
    }
    if let Some((_, tail)) = split(topic, GATEWAY) {
        match tail {
            "telemetry" => {
                for (device, readings) in v.as_object().into_iter().flatten() {
                    telemetry(device, readings, &mut out);
                }
            }
            "attributes" => {
                for (device, said) in v.as_object().into_iter().flatten() {
                    attributes(device, said, &mut out);
                }
            }
            "connect" | "disconnect" => {
                if let Some(device) = v["device"].as_str() {
                    presence(device, tail == "connect", &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

/// The devices a message speaks for, for checking that it may: a device speaks for itself, and a
/// gateway for what is reached through it.
pub fn gateway_of(topic: &str) -> Option<&str> {
    split(topic, GATEWAY).map(|(code, _)| code)
}

fn ended(answer: &Value) -> Ending {
    let refused = answer.get("error").is_some_and(|e| !e.is_null()) || answer.get("success") == Some(&Value::Bool(false));
    if refused { Ending::Failed } else { Ending::Done }
}

/// Whether a message answers a command, known here by the id it was sent under.
pub fn status(topic: &str, body: &[u8], names: &[&str]) -> Option<Ending> {
    let answer: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if let Some((_, tail)) = split(topic, DEVICES) {
        let id = tail.strip_prefix("rpc/response/")?;
        return names.contains(&id).then(|| ended(&answer));
    }
    let (_, tail) = split(topic, GATEWAY)?;
    if tail != "rpc" {
        return None;
    }
    let id = match &answer["id"] {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return None,
    };
    names.contains(&id.as_str()).then(|| ended(&answer["data"]))
}

/// What is sent to a device, and whether anything will come back to say how it ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Sending {
    pub topic: String,
    pub body: Value,
    pub answers: bool,
}

/// A command for a device. `set.x` changes an attribute, which a device hears and does not answer:
/// its next report says whether it took. Anything else is a call it answers. `via` is the gateway
/// device it is reached through, if any.
pub fn command(code: &str, via: Option<&str>, id: u64, operation: &str, payload: &Value) -> Sending {
    if let Some(key) = operation.strip_prefix("set.") {
        let mut said = Map::new();
        said.insert(key.to_string(), payload.get("value").cloned().unwrap_or(Value::Null));
        return match via {
            Some(gateway) => Sending { topic: format!("{GATEWAY}{gateway}/attributes"), body: json!({"device": code, "data": said}), answers: false },
            None => Sending { topic: format!("{DEVICES}{code}/attributes"), body: Value::Object(said), answers: false },
        };
    }
    match via {
        Some(gateway) => Sending {
            topic: format!("{GATEWAY}{gateway}/rpc"),
            body: json!({"device": code, "data": {"id": id, "method": operation, "params": payload}}),
            answers: true,
        },
        None => Sending { topic: format!("{DEVICES}{code}/rpc/request/{id}"), body: json!({"method": operation, "params": payload}), answers: true },
    }
}

/// Topic filters a device may hold. It hears what is for it and nothing else.
pub fn may_hear(filter: &str) -> bool {
    filter.starts_with("v1/devices/me/") || filter.starts_with(GATEWAY)
}

/// Whether a device may say this: the device API, as itself.
pub fn may_say(topic: &str) -> bool {
    topic.starts_with("v1/devices/me/") || topic.starts_with(GATEWAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(topic: &str, body: &str) -> Vec<(Option<String>, String, f64, Option<DateTime<Utc>>)> {
        decode(topic, body.as_bytes())
            .into_iter()
            .filter_map(|d| match d {
                Decoded::Metric { device, metric, value, at, .. } => Some((device, metric, value, at)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_device_is_itself_wherever_it_said_me() {
        assert_eq!(named("v1/devices/me/telemetry", "PRESS-1"), "v1/devices/PRESS-1/telemetry");
        assert_eq!(named("v1/gateway/telemetry", "GW"), "v1/gateway/GW/telemetry");
        assert_eq!(named("custom/topic", "PRESS-1"), "custom/topic");
        assert_eq!(as_heard("v1/devices/PRESS-1/rpc/request/7", "PRESS-1"), "v1/devices/me/rpc/request/7");
        assert_eq!(as_heard("v1/gateway/GW/rpc", "GW"), "v1/gateway/rpc");
        assert_eq!(as_heard("v1/devices/OTHER/rpc/request/7", "PRESS-1"), "v1/devices/OTHER/rpc/request/7", "someone else's stays someone else's");
    }

    #[test]
    fn telemetry_comes_in_three_shapes() {
        let plain = metrics("v1/devices/A/telemetry", r#"{"temp": 21.5, "rpm": 900}"#);
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[1], (Some("A".into()), "temp".into(), 21.5, None));
        let stamped = metrics("v1/devices/A/telemetry", r#"{"ts": 1700000000000, "values": {"temp": 1}}"#);
        assert_eq!(stamped[0].3.unwrap().timestamp_millis(), 1_700_000_000_000);
        assert_eq!(stamped.len(), 1, "ts is not a metric");
        let many = metrics("v1/devices/A/telemetry", r#"[{"ts": 1700000000000, "values": {"a": 1}}, {"b": 2}]"#);
        assert_eq!(many.len(), 2);
    }

    #[test]
    fn a_ts_with_no_values_is_a_reading_called_ts() {
        assert_eq!(metrics("v1/devices/A/telemetry", r#"{"ts": 5}"#).len(), 1);
    }

    #[test]
    fn what_is_not_a_number_is_kept_as_what_it_is() {
        let said = decode("v1/devices/A/telemetry", br#"{"on": true, "firmware": "1.2", "gps": {"lat": 1}}"#);
        assert_eq!(said.len(), 3);
        assert!(said.iter().any(|d| matches!(d, Decoded::Metric { metric, value, .. } if metric == "on" && *value == 1.0)));
        assert!(said.iter().any(|d| matches!(d, Decoded::Attribute { key, value, .. } if key == "firmware" && value == "1.2")));
        assert!(said.iter().any(|d| matches!(d, Decoded::Attribute { key, .. } if key == "gps")));
    }

    #[test]
    fn nonsense_says_nothing() {
        assert!(decode("v1/devices/A/telemetry", b"not json").is_empty());
        assert!(decode("v1/devices/A/telemetry", b"7").is_empty());
        assert!(decode("v1/devices/A/somewhere", b"{}").is_empty());
        assert!(decode("elsewhere", b"{\"a\": 1}").is_empty());
    }

    #[test]
    fn one_message_costs_at_most_so_much() {
        let many: Map<String, Value> = (0..MAX_KEYS + 50).map(|i| (format!("k{i}"), json!(i))).collect();
        assert_eq!(decode("v1/devices/A/telemetry", Value::Object(many).to_string().as_bytes()).len(), MAX_KEYS);
    }

    #[test]
    fn a_code_can_hold_what_a_topic_would_trip_on() {
        assert_eq!(metrics("v1/devices/LINE/2/telemetry", r#"{"a": 1}"#)[0].0.as_deref(), Some("LINE/2"));
    }

    #[test]
    fn a_gateway_speaks_for_what_is_behind_it() {
        let said = metrics(
            "v1/gateway/GW/telemetry",
            r#"{"Pump 1": [{"ts": 1700000000000, "values": {"flow": 3}}], "Pump 2": [{"ts": 1700000000000, "values": {"flow": 4}}]}"#,
        );
        assert_eq!(said.iter().map(|m| m.0.clone().unwrap()).collect::<Vec<_>>(), vec!["Pump 1", "Pump 2"]);
        assert_eq!(gateway_of("v1/gateway/GW/telemetry"), Some("GW"));
        assert_eq!(gateway_of("v1/devices/GW/telemetry"), None);
        let attrs = decode("v1/gateway/GW/attributes", br#"{"Pump 1": {"model": "X"}}"#);
        assert!(matches!(&attrs[0], Decoded::Attribute { device: Some(d), key, .. } if d == "Pump 1" && key == "model"));
        let up = decode("v1/gateway/GW/connect", br#"{"device": "Pump 1"}"#);
        assert!(matches!(&up[0], Decoded::Attribute { device: Some(d), value, .. } if d == "Pump 1" && value == &json!(true)));
    }

    #[test]
    fn a_session_going_up_and_down_is_news() {
        let up = decode(&session("A", true), b"");
        assert!(matches!(&up[0], Decoded::Attribute { device: Some(d), key, value } if d == "A" && key == "active" && value == &json!(true)));
        let down = decode(&session("A", false), b"");
        assert!(matches!(&down[0], Decoded::Attribute { value, .. } if value == &json!(false)));
    }

    #[test]
    fn a_device_asking_something_is_an_event() {
        let asked = decode("v1/devices/A/rpc/request/4", br#"{"method": "time", "params": {}}"#);
        assert!(matches!(&asked[0], Decoded::Event { name, value, .. } if name == "rpc.request" && value["id"] == "4" && value["method"] == "time"));
    }

    #[test]
    fn a_command_is_a_call_and_a_setting_is_news() {
        let call = command("A", None, 7, "fan.start", &json!({"speed": 3}));
        assert_eq!(call.topic, "v1/devices/A/rpc/request/7");
        assert_eq!(call.body, json!({"method": "fan.start", "params": {"speed": 3}}));
        assert!(call.answers);
        let set = command("A", None, 8, "set.limit", &json!({"key": "limit", "value": 40}));
        assert_eq!((set.topic.as_str(), &set.body, set.answers), ("v1/devices/A/attributes", &json!({"limit": 40}), false));
    }

    #[test]
    fn through_a_gateway_it_names_the_device() {
        let call = command("Pump 1", Some("GW"), 7, "open", &json!({}));
        assert_eq!(call.topic, "v1/gateway/GW/rpc");
        assert_eq!(call.body, json!({"device": "Pump 1", "data": {"id": 7, "method": "open", "params": {}}}));
        let set = command("Pump 1", Some("GW"), 8, "set.limit", &json!({"value": 1}));
        assert_eq!(set.body, json!({"device": "Pump 1", "data": {"limit": 1}}));
    }

    #[test]
    fn an_answer_ends_only_what_it_answers() {
        assert_eq!(status("v1/devices/A/rpc/response/7", b"{\"ok\": true}", &["7"]), Some(Ending::Done));
        assert_eq!(status("v1/devices/A/rpc/response/7", b"not json", &["7"]), Some(Ending::Done), "a device may answer in anything");
        assert_eq!(status("v1/devices/A/rpc/response/7", b"{\"error\": \"no\"}", &["7"]), Some(Ending::Failed));
        assert_eq!(status("v1/devices/A/rpc/response/8", b"{}", &["7"]), None);
        assert_eq!(status("v1/devices/A/telemetry", b"{}", &["7"]), None);
        assert_eq!(status("v1/gateway/GW/rpc", br#"{"device": "P", "id": 7, "data": {"success": true}}"#, &["7"]), Some(Ending::Done));
        assert_eq!(status("v1/gateway/GW/rpc", br#"{"device": "P", "id": 7, "data": {"success": false}}"#, &["7"]), Some(Ending::Failed));
        assert_eq!(status("v1/gateway/GW/rpc", br#"{"device": "P", "id": 9, "data": {}}"#, &["7"]), None);
    }

    #[test]
    fn a_device_keeps_to_its_own() {
        assert!(may_hear("v1/devices/me/rpc/request/+"));
        assert!(may_hear("v1/gateway/rpc"));
        assert!(!may_hear("#"));
        assert!(!may_hear("v1/devices/PRESS-1/rpc/request/+"));
        assert!(may_say("v1/devices/me/telemetry"));
        assert!(!may_say("v1/devices/PRESS-1/telemetry"));
        assert!(!may_say("$session/PRESS-1/offline"));
    }
}
