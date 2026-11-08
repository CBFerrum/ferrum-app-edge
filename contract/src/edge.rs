//! What a gateway is told to be: its definitions, its devices, its rules, where else its data goes,
//! and which release it should run. Fetched from the platform when it changes and kept in the
//! journal, so that a gateway restarted with the link down is still what it was.
//!
//! A rule watches a number here, at the edge, so an alarm is raised with the link down. It says only
//! that something is wrong. Rules raise alarms and do not move equipment.

use crate::flow::Trace;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceLink {
    pub code: String,
    /// The definition's name.
    pub definition: String,
    /// Overrides the definition's address.
    #[serde(default)]
    pub connect: Option<String>,
    #[serde(default)]
    pub settings: Value,
    /// Commands it may run at once.
    #[serde(default = "one")]
    pub concurrency: u32,
    /// How it proves who it is when it calls in.
    #[serde(default, skip_serializing_if = "Credentials::is_empty")]
    pub credentials: Credentials,
    /// The device it is reached through, for one behind a gateway of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Values kept here for it to ask for: what it was last asked to be.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shared: BTreeMap<String, Value>,
}

/// What a device can prove itself with. Only hashes come here, so a gateway that is lost gives
/// nothing away, and checking one needs no link to the platform.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// SHA-256 of an access token, sent as the MQTT user name or in an HTTP or CoAP path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// An MQTT client id, user and SHA-256 of a password, any of the first two of which can be empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basic: Option<Basic>,
    /// SHA-256 of the device's certificate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
}

impl Credentials {
    pub fn is_empty(&self) -> bool {
        self.token.is_none() && self.basic.is_none() && self.certificate.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Basic {
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    pub password: String,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Comparison {
    Above,
    Below,
    /// Nothing has been heard for `hold_secs`.
    Silent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: Uuid,
    pub name: String,
    /// Device codes. Empty means every device with this metric.
    #[serde(default)]
    pub devices: Vec<String>,
    pub metric: String,
    pub when: Comparison,
    #[serde(default)]
    pub value: f64,
    /// How long it must hold before anyone is told, so that one spike is not an alarm.
    #[serde(default)]
    pub hold_secs: i64,
    /// Where it stops being true, when that is not `value`. Without a gap, a reading sitting on the
    /// limit would raise and clear for ever.
    #[serde(default)]
    pub clear: Option<f64>,
    #[serde(default = "warning")]
    pub severity: String,
}

fn warning() -> String {
    "Warning".into()
}

/// Where else a site's data goes: a local historian, another broker, someone else's cloud.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Export {
    pub name: String,
    /// One of [`EXPORT_SCHEMES`]. `docs/definitions.md` says what each takes.
    pub to: String,
    /// A topic, subject or routing key. `{device}` and `{metric}` work here.
    #[serde(default)]
    pub topic: Option<String>,
    /// Only these metrics, with `*` for the rest of a name. Empty means all.
    #[serde(default)]
    pub only: Vec<String>,
    /// The name of a secret held at the site: what signs an HTTP batch, or logs in anywhere else.
    #[serde(default)]
    pub sign_with: Option<String>,
}

pub const EXPORT_SCHEMES: &[&str] = &[
    "http",
    "https",
    "mqtt",
    "mqtts",
    "nats",
    "nats+tls",
    "kafka",
    "kafkas",
    "amqp",
    "amqps",
    "influx",
    "influxs",
    "clickhouse",
    "clickhouses",
    "s3",
    "sqs",
    "sns",
    "pubsub",
    "azure",
    "eventhub",
    "sparkplug",
    "sparkplugs",
    "listen+opcua",
    "postgres",
];

/// Whether an export's address can work, for saying so when it is saved. A password has no place
/// in one: it would reach every gateway and every backup. It belongs to the site, as a secret.
/// A table's name as a query can hold it, with its schema if it says one.
pub fn a_table(name: &str) -> bool {
    let plain = |part: &str| part.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() <= 2 && parts.iter().all(|p| plain(p))
}

pub fn check_export(to: &str) -> Result<(), String> {
    let (scheme, rest) = to.split_once("://").ok_or("say where: https://historian/in, nats://bus:4222")?;
    if !EXPORT_SCHEMES.contains(&scheme) {
        return Err(format!("{scheme} is not a place to send to: try {}", EXPORT_SCHEMES.join(", ")));
    }
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err("say which host".into());
    }
    if authority.rsplit_once('@').is_some_and(|(login, _)| login.contains(':')) {
        return Err("a password goes in a secret at the site, not here".into());
    }
    if scheme == "postgres" {
        let table = rest.split_once("table=").map(|(_, t)| t.split('&').next().unwrap_or(t));
        if !table.is_some_and(a_table) {
            return Err("say ?table= with the name of a table".into());
        }
    }
    Ok(())
}

/// A build of the site runtime, and how to know it is the real one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// Of the file, in hexadecimal.
    pub sha256: String,
    /// Ed25519 over [`Release::message`], in hexadecimal, by a key the site was installed with.
    pub signature: String,
    /// Refused after this, so an old release cannot be replayed for ever.
    pub expires_at: DateTime<Utc>,
}

impl Release {
    /// What is signed: everything that decides what runs.
    pub fn message(&self) -> Vec<u8> {
        format!("ferrum-site-release\n{}\n{}\n{}\n{}", self.version, self.url, self.sha256, self.expires_at.to_rfc3339()).into_bytes()
    }
}

/// Dotted numbers compared as numbers. Anything else is older than everything: a build cannot be
/// made newer by naming it oddly.
pub fn newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| v.split('.').map(|p| p.parse::<u64>()).collect::<Result<Vec<_>, _>>();
    match (parts(candidate), parts(current)) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

/// A flow that has been through review and is meant to run here (`crate::flow`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowSpec {
    pub id: Uuid,
    pub name: String,
    pub revision: i64,
    pub body: String,
    /// Someone is watching it, so what passes through is reported.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub debug: bool,
    /// What this revision does with what the one before it at the site remembered, as the person who set
    /// it running chose: start with nothing, drain the old one first, or keep what unchanged steps knew.
    #[serde(default, skip_serializing_if = "is_reset")]
    pub transition: crate::flow::Transition,
}

fn is_reset(t: &crate::flow::Transition) -> bool {
    *t == crate::flow::Transition::Reset
}

/// A message passing through a step of a flow someone is watching.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowTrace {
    pub flow: Uuid,
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub step: Trace,
}

/// Longest value kept in a trace. Past that it is only said how long it was.
const MAX_TRACED: usize = 2_000;

impl FlowTrace {
    pub fn new(flow: Uuid, at: DateTime<Utc>, mut step: Trace) -> FlowTrace {
        let length = step.value.to_string().len();
        if length > MAX_TRACED {
            step.value = serde_json::json!({"too_long_to_show": length});
        }
        FlowTrace { flow, at, step }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EdgeConfig {
    pub version: String,
    #[serde(default)]
    pub definitions: BTreeMap<String, String>,
    #[serde(default)]
    pub devices: Vec<DeviceLink>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub flows: Vec<FlowSpec>,
    #[serde(default)]
    pub exports: Vec<Export>,
    #[serde(default)]
    pub release: Option<Release>,
    /// Definitions a device can add itself to, with a key and secret every device of the kind holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provisioning: Vec<Provisioning>,
    /// Who this gateway is, for what it reports about devices the platform has not met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<Uuid>,
}

/// A way for a device of one definition to add itself. The key says which, and the secret, kept here
/// as a hash, is what lets it. It adds a device and nothing more: it reports, and takes no command
/// until a person commissions it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provisioning {
    pub definition: String,
    pub key: String,
    pub secret: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigRequest {
    /// The version held, so an unchanged configuration costs one short answer.
    #[serde(default)]
    pub have: String,
    /// What this gateway heard and does not know, for a person to claim.
    #[serde(default)]
    pub strangers: Vec<String>,
    /// Why what it was given could not all start.
    #[serde(default)]
    pub problems: Vec<String>,
    /// Each link's state, as it is.
    #[serde(default)]
    pub links: Value,
    /// The release it is running.
    #[serde(default)]
    pub runtime: String,
    /// What passed through the flows someone is watching, since the last exchange.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traces: Vec<FlowTrace>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigResponse {
    /// Absent when `have` is current.
    #[serde(default)]
    pub config: Option<EdgeConfig>,
}

/// A change in an alarm, in a fact of kind `edge.alarm`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlarmChange {
    pub rule: Uuid,
    pub device: Uuid,
    pub raised: bool,
    #[serde(default)]
    pub value: Option<f64>,
    /// When a flow raised it, and not a rule: which flow and node. `rule` is then the flow's id.
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
}

/// Where one rule stands on one device.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Watch {
    since: Option<DateTime<Utc>>,
    raised: bool,
    last_heard: Option<DateTime<Utc>>,
}

impl Watch {
    fn breached(&self, rule: &Rule, value: f64) -> bool {
        // Once raised, it clears at `clear`: the gap stops a reading on the limit from chattering.
        let limit = if self.raised { rule.clear.unwrap_or(rule.value) } else { rule.value };
        match rule.when {
            Comparison::Above => value > limit,
            Comparison::Below => value < limit,
            Comparison::Silent => false,
        }
    }

    /// Takes a reading. `Some` when the alarm should change.
    pub fn feed(&mut self, rule: &Rule, value: f64, at: DateTime<Utc>) -> Option<bool> {
        self.last_heard = Some(at);
        if rule.when == Comparison::Silent {
            self.since = None;
            return self.raised.then(|| {
                self.raised = false;
                false
            });
        }
        if !self.breached(rule, value) {
            self.since = None;
            return self.raised.then(|| {
                self.raised = false;
                false
            });
        }
        let since = *self.since.get_or_insert(at);
        if !self.raised && at - since >= Duration::seconds(rule.hold_secs) {
            self.raised = true;
            return Some(true);
        }
        None
    }

    /// Time passing, for what silence means. Only after something has been heard: a device nobody
    /// has seen yet is not a device that went quiet.
    pub fn tick(&mut self, rule: &Rule, now: DateTime<Utc>) -> Option<bool> {
        if rule.when != Comparison::Silent || self.raised {
            return None;
        }
        let heard = self.last_heard?;
        (now - heard >= Duration::seconds(rule.hold_secs)).then(|| {
            self.raised = true;
            true
        })
    }

    pub fn raised(&self) -> bool {
        self.raised
    }

    /// One that was raised before a restart: forgetting would leave it raised for ever on the
    /// platform, with nothing here left to clear it.
    pub fn still_raised() -> Watch {
        Watch { raised: true, ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_is_named_and_nothing_else() {
        assert!(check_export("postgres://writer@historian/plant?table=readings").is_ok());
        assert!(check_export("postgres://writer@historian/plant?table=plant.readings").is_ok());
        assert!(check_export("postgres://writer@historian/plant").is_err(), "which table");
        for bad in ["readings;drop table x", "a.b.c", "1x", "a b", "\"x\""] {
            assert!(!a_table(bad), "{bad}");
        }
    }

    #[test]
    fn an_export_says_where_and_keeps_its_password_to_itself() {
        assert!(check_export("https://historian/in").is_ok());
        assert!(check_export("nats://bus:4222/x").is_ok());
        assert!(check_export("amqp://user@broker:5672/plant?exchange=readings").is_ok());
        assert!(check_export("amqp://user:hunter2@broker:5672/").unwrap_err().contains("secret"));
        assert!(check_export("ftp://x").unwrap_err().contains("not a place"));
        assert!(check_export("kafka://").unwrap_err().contains("host"));
        assert!(check_export("historian").is_err());
    }

    fn rule(when: Comparison, value: f64, hold: i64, clear: Option<f64>) -> Rule {
        Rule { id: Uuid::nil(), name: "r".into(), devices: vec![], metric: "m".into(), when, value, hold_secs: hold, clear, severity: "Warning".into() }
    }

    fn at(s: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_000_000 + s, 0).unwrap()
    }

    #[test]
    fn one_spike_is_not_an_alarm() {
        let r = rule(Comparison::Above, 80.0, 30, None);
        let mut w = Watch::default();
        assert_eq!(w.feed(&r, 90.0, at(0)), None);
        assert_eq!(w.feed(&r, 70.0, at(10)), None, "it came back in time");
        assert_eq!(w.feed(&r, 90.0, at(20)), None);
        assert_eq!(w.feed(&r, 90.0, at(49)), None);
        assert_eq!(w.feed(&r, 90.0, at(50)), Some(true), "it held for thirty seconds");
        assert_eq!(w.feed(&r, 95.0, at(60)), None, "raised once");
        assert_eq!(w.feed(&r, 70.0, at(70)), Some(false));
        assert_eq!(w.feed(&r, 70.0, at(80)), None, "cleared once");
    }

    #[test]
    fn a_reading_on_the_limit_does_not_chatter() {
        let r = rule(Comparison::Above, 80.0, 0, Some(75.0));
        let mut w = Watch::default();
        assert_eq!(w.feed(&r, 81.0, at(0)), Some(true));
        assert_eq!(w.feed(&r, 79.0, at(1)), None, "still above where it clears");
        assert_eq!(w.feed(&r, 81.0, at(2)), None);
        assert_eq!(w.feed(&r, 74.0, at(3)), Some(false));
        assert_eq!(w.feed(&r, 79.0, at(4)), None, "and below where it raises");
    }

    #[test]
    fn below_is_the_same_the_other_way() {
        let r = rule(Comparison::Below, 20.0, 0, Some(25.0));
        let mut w = Watch::default();
        assert_eq!(w.feed(&r, 15.0, at(0)), Some(true));
        assert_eq!(w.feed(&r, 22.0, at(1)), None);
        assert_eq!(w.feed(&r, 26.0, at(2)), Some(false));
    }

    #[test]
    fn silence_counts_only_after_something_was_heard() {
        let r = rule(Comparison::Silent, 0.0, 60, None);
        let mut w = Watch::default();
        assert_eq!(w.tick(&r, at(1000)), None, "never heard is not gone quiet");
        assert_eq!(w.feed(&r, 1.0, at(0)), None);
        assert_eq!(w.tick(&r, at(59)), None);
        assert_eq!(w.tick(&r, at(60)), Some(true));
        assert_eq!(w.tick(&r, at(120)), None, "raised once");
        assert_eq!(w.feed(&r, 1.0, at(121)), Some(false), "it is back");
    }

    #[test]
    fn a_release_is_newer_by_its_numbers() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.0"), "as numbers, not as text");
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
        assert!(!newer("9.9.9-evil", "0.1.0"), "a name that is not numbers is not newer");
    }

    #[test]
    fn what_is_signed_covers_what_runs() {
        let r = Release { version: "0.2.0".into(), url: "https://x".into(), sha256: "ab".into(), signature: String::new(), expires_at: at(0) };
        let mut other = r.clone();
        other.sha256 = "cd".into();
        assert_ne!(r.message(), other.message());
    }
}
