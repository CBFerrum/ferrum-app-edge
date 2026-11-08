//! Flows: logic a person draws, run at the gateway so that it keeps working with the link down.
//! Messages leave sources, pass through logic and reach actions, as in Node-RED or a ThingsBoard
//! rule chain, with three differences that matter on a plant floor.
//!
//! * There is no code. Every node is one of the kinds below, so a flow can be read, reviewed and
//!   simulated, and nothing in one can do what its nodes do not say.
//! * It cannot loop. A flow is checked to have no cycles, and each message's work is bounded, so a
//!   bad one costs a little time and cannot hang a gateway.
//! * It cannot move anything. The strongest thing a flow can do is ask for a command, which goes
//!   through the same ledger as a person's: commissioned operations, authority, approval or an
//!   automation envelope. Safety functions do not belong here (`docs/flows.md`).
//!
//! Pure: this checks and evaluates. The same code simulates a flow on the platform and runs it at
//! the site, so what a reviewer saw is what runs.

use crate::driver::{at_path, parse_duration, render_json};
use crate::pipeline::{self, Episode, EpisodeRule, Happened, Quality, Said, WindowRule, Windows};
use chrono::{DateTime, TimeDelta, Utc};
use ferrum_calc::formula::Formula;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::Duration;

pub const MAX_NODES: usize = 500;
pub const MAX_EDGES: usize = 2_000;
/// Steps one message may cost. No cycle can exist, so this bounds fan-out, not a loop.
const MAX_STEPS: usize = 5_000;
/// Devices a node keeps memory for.
const MAX_KEYS: usize = 5_000;
/// What a window or a delay holds per device.
const MAX_HELD: usize = 10_000;
/// What one list can become.
const MAX_SPLIT: usize = 1_000;
/// What a flow can be told about devices, by metric and attribute.
const MAX_KNOWN: usize = 100_000;
/// Readings before an anomaly can mean anything.
const ANOMALY_MIN: usize = 10;
/// Keys a whole flow keeps memory for, every node together: untrusted device codes cannot grow a
/// gateway's memory without bound, and what is left out is said (FLC-EDGE-004).
const MAX_FLOW_KEYS: usize = 50_000;
/// Times of readings a business-critical flow remembers per source, to know one it has had.
const MAX_SEEN: usize = 256;
/// The longest stretch of time a simulation will walk through, second by second.
pub const MAX_SIMULATED_SECS: i64 = 6 * 3600;

fn period<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    let v = Value::deserialize(d)?;
    let text = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
    parse_duration(&text).ok_or_else(|| serde::de::Error::custom(format!("{text} is not a duration: try 500ms, 5s or 2m")))
}

fn some_period<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    period(d).map(Some)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Comparison {
    #[serde(rename = ">")]
    Above,
    #[serde(rename = ">=")]
    AtLeast,
    #[serde(rename = "<")]
    Below,
    #[serde(rename = "<=")]
    AtMost,
    #[serde(rename = "==")]
    Is,
    #[serde(rename = "!=")]
    IsNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stat {
    Avg,
    Min,
    Max,
    Sum,
    Count,
    /// The newest less the oldest.
    Delta,
}

/// What a node is. Sources first, then logic, then actions.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Kind {
    /// A number a device reported. `device` is a code, `PRESS-*`, or empty for any.
    Metric {
        #[serde(default)]
        device: String,
        metric: String,
    },
    /// A device's state changing.
    State {
        #[serde(default)]
        device: String,
    },
    Event {
        #[serde(default)]
        device: String,
        name: String,
    },
    Attribute {
        #[serde(default)]
        device: String,
        key: String,
    },
    /// A way into a flow other flows use. On its own, a number of this name comes in here, so that it
    /// can be simulated and reviewed by itself.
    Input {
        name: String,
    },
    /// Fires on its own, with the time as its value.
    Timer {
        #[serde(deserialize_with = "period")]
        every: Duration,
    },

    /// Sends the message out of `yes` or `no`.
    Compare {
        op: Comparison,
        value: Value,
    },
    Range {
        min: f64,
        max: f64,
    },
    /// Only what is different from last time, or by at least `by`.
    Changed {
        #[serde(default)]
        by: Option<f64>,
    },
    /// Passes one message once `in` has held this long with nothing arriving at `reset`.
    Hold {
        #[serde(rename = "for", deserialize_with = "period")]
        hold: Duration,
    },
    /// Fires when nothing has arrived for this long.
    Silence {
        #[serde(rename = "for", deserialize_with = "period")]
        quiet: Duration,
    },
    /// At most one message this often.
    Limit {
        #[serde(deserialize_with = "period")]
        every: Duration,
    },
    Delay {
        #[serde(rename = "for", deserialize_with = "period")]
        wait: Duration,
    },
    /// Passes the count when `of` messages have arrived within this time.
    Count {
        of: usize,
        #[serde(deserialize_with = "period")]
        within: Duration,
    },
    /// Replaces a number with a statistic of the last stretch of them. `across: true` takes every
    /// device's numbers together, for a total or an average over a line.
    Window {
        #[serde(deserialize_with = "period")]
        over: Duration,
        stat: Stat,
        #[serde(default)]
        across: bool,
    },
    /// `[value] * 1.8 + 32`. The fields of a value that has them are there by name too.
    Math {
        formula: String,
    },
    /// Waits for every wired input, then passes them together, by input name, or what `formula`
    /// makes of them. Values older than `fresh` do not count: a stale reading is not a reading.
    Combine {
        #[serde(default)]
        formula: Option<String>,
        #[serde(default, deserialize_with = "some_period")]
        fresh: Option<Duration>,
        /// Across devices, instead of for each device on its own.
        #[serde(default)]
        across: bool,
    },
    /// `yes` when every wired input was last true, `no` when that stops being so. It says each
    /// once. `any: true` asks for one instead of all.
    All {
        #[serde(default)]
        any: bool,
        #[serde(default, deserialize_with = "some_period")]
        fresh: Option<Duration>,
        #[serde(default)]
        across: bool,
    },
    /// Remembers: `set` turns it on and `reset` off. Says `yes` or `no` when that changes.
    Latch,
    /// What a raw value means: `{0: Idle, 1: Running}`.
    Map {
        map: BTreeMap<String, Value>,
        #[serde(default)]
        other: Option<Value>,
    },
    /// Sends the message out of the port named by its value, or by the part of it at `on`, or
    /// `other`.
    Switch {
        cases: Vec<String>,
        #[serde(default)]
        on: Option<String>,
    },
    /// Replaces the value. `{value}` and `{device}` work inside.
    Set {
        value: Value,
    },
    /// Takes one thing out of a value: `$.a.b`.
    Field {
        path: String,
    },
    /// `yes` when a value has every one of these parts.
    Has {
        paths: Vec<String>,
    },
    /// How far a counter has gone since last time, or that as a rate `per` so long. One that went
    /// backwards was reset, and that says nothing about how much was counted.
    Delta {
        #[serde(default, deserialize_with = "some_period")]
        per: Option<Duration>,
    },
    /// Whether a position is inside a place: `inside` or `outside` each time, and `entered` or
    /// `left` when that changes. Corners of a polygon are `[lat, lon]`.
    Geofence {
        #[serde(default = "lat_path")]
        lat: String,
        #[serde(default = "lon_path")]
        lon: String,
        #[serde(default)]
        polygon: Vec<[f64; 2]>,
        #[serde(default)]
        circle: Option<Circle>,
    },
    /// The first of what is the same within this time, and none of the rest. `key` says which part
    /// makes two the same.
    Dedupe {
        #[serde(default)]
        key: Option<String>,
        #[serde(deserialize_with = "period")]
        within: Duration,
    },
    /// One message for each thing in a list.
    Split {
        #[serde(default)]
        path: Option<String>,
    },
    /// Renames the parts of a value, and leaves some out. With `keep`, only those stay.
    Keys {
        #[serde(default)]
        rename: BTreeMap<String, String>,
        #[serde(default)]
        drop: Vec<String>,
        #[serde(default)]
        keep: Vec<String>,
    },
    /// Adds what is known about a device to a value: its newest numbers and what it said about
    /// itself. A value that is not an object is kept as `value`.
    Enrich {
        #[serde(default)]
        metrics: Vec<String>,
        #[serde(default)]
        attributes: Vec<String>,
        /// Another device, instead of the one this came from.
        #[serde(default)]
        device: Option<String>,
    },
    /// `yes` between two times of day, on these days. `from` after `to` runs over midnight. Days are
    /// 1, Monday, to 7, and none means every day. `offset_minutes` is the site's distance from UTC.
    Schedule {
        from: String,
        to: String,
        #[serde(default)]
        days: Vec<u8>,
        #[serde(default)]
        offset_minutes: i32,
    },
    /// A number unlike the last `window` of them, by more than `sigma` standard deviations. It says
    /// `normal` until it has seen enough to know.
    Anomaly {
        #[serde(default = "a_hundred")]
        window: usize,
        #[serde(default = "three")]
        sigma: f64,
    },
    /// Makes the message another device's: what follows treats it as having come from there.
    /// `{value}` and `{device}` work inside.
    Reassign {
        to: String,
    },
    /// Raw counts to engineering units, exactly: `value × scale + offset`, with `unit`. The raw value
    /// stays with the reading as its provenance. What is not a number, or lands outside `min`..`max`,
    /// leaves by `quarantine` and is never a normal reading (FLC-EDGE-002).
    Normalize {
        #[serde(default = "decimal_one")]
        scale: Decimal,
        #[serde(default)]
        offset: Decimal,
        #[serde(default)]
        unit: Option<String>,
        #[serde(default)]
        min: Option<Decimal>,
        #[serde(default)]
        max: Option<Decimal>,
    },
    /// Sends a reading on by how far it can be trusted: `good`, `uncertain` or `bad`, or `stale` when it
    /// was seen longer ago than `stale` (FLC-EDGE-003).
    #[serde(rename = "quality")]
    QualityGate {
        #[serde(default, deserialize_with = "some_period")]
        stale: Option<Duration>,
    },
    /// A statistic for each stretch of the time readings were *seen*: `out` once the watermark (the
    /// latest time seen, less `wait`) passes a window's end, `revised` for a late reading within
    /// `lateness` after that, `late` for one later still. A device quiet for `idle` closes what it has
    /// open, marked partial (FLC-EDGE-006).
    EventWindow {
        #[serde(deserialize_with = "period")]
        size: Duration,
        stat: Stat,
        #[serde(default = "no_time", deserialize_with = "period")]
        wait: Duration,
        #[serde(default = "no_time", deserialize_with = "period")]
        lateness: Duration,
        #[serde(default, deserialize_with = "some_period")]
        idle: Option<Duration>,
        #[serde(default)]
        across: bool,
    },
    /// A sustained condition with one identity: `opened` once it has held above `above` (or below
    /// `below`) for `on_delay`, `closed` once it has stayed past `close` for `off_delay`, the band
    /// between a deadband; `reopened` if it holds again within `reopen` of closing; `evidence` for each
    /// reading meanwhile, and `stale` once when good readings stop. Only a good reading moves it
    /// (FLC-EDGE-007).
    Episode {
        #[serde(default)]
        above: Option<f64>,
        #[serde(default)]
        below: Option<f64>,
        close: f64,
        #[serde(default = "no_time", deserialize_with = "period")]
        on_delay: Duration,
        #[serde(default = "no_time", deserialize_with = "period")]
        off_delay: Duration,
        #[serde(default = "no_time", deserialize_with = "period")]
        reopen: Duration,
        #[serde(default, deserialize_with = "some_period")]
        stale: Option<Duration>,
    },

    /// Hands an episode to the platform as a condition: one case for each episode, whatever its gateway
    /// says twice, with its evidence and how it ended. What happens next is a process the workspace
    /// runs, reviewed like any other.
    Case {
        title: String,
    },
    /// A way out of a flow other flows use. On its own, it only says what reached it.
    Output {
        name: String,
    },
    /// Raises at `in`, clears at `clear`, once each, for each device.
    Alarm {
        name: String,
        #[serde(default = "warning")]
        severity: String,
        #[serde(default)]
        device: Option<String>,
    },
    RecordEvent {
        name: String,
    },
    RecordMetric {
        name: String,
        #[serde(default)]
        unit: Option<String>,
    },
    RecordAttribute {
        key: String,
    },
    /// Tells the people who look after this. `{value}` and `{device}` work inside.
    Notify {
        title: String,
        #[serde(default = "a_minute", deserialize_with = "period")]
        every: Duration,
    },
    /// Hands the message to one of the site's exports.
    Export {
        to: String,
    },
    /// Answers a device that asked something, by the id its question came with.
    Reply {
        #[serde(default = "id_path")]
        id: String,
        #[serde(default)]
        with: Value,
    },
    /// Writes to the gateway's log, and to whoever is watching this flow.
    Log {
        #[serde(default)]
        text: Option<String>,
    },
    /// Asks for a command. It goes through the ledger, and goes nowhere until someone or something
    /// entitled has allowed it. At most once per `every`, however often this is reached.
    Command {
        operation: String,
        #[serde(default)]
        device: Option<String>,
        #[serde(default)]
        payload: Value,
        #[serde(default = "a_minute", deserialize_with = "period")]
        every: Duration,
        #[serde(default = "five_minutes", deserialize_with = "period")]
        expires: Duration,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Circle {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
}

fn lat_path() -> String {
    "$.lat".into()
}

fn lon_path() -> String {
    "$.lon".into()
}

fn id_path() -> String {
    "$.id".into()
}

fn a_hundred() -> usize {
    100
}

fn three() -> f64 {
    3.0
}

fn warning() -> String {
    "Warning".into()
}

fn a_minute() -> Duration {
    Duration::from_secs(60)
}

fn five_minutes() -> Duration {
    Duration::from_secs(300)
}

fn no_time() -> Duration {
    Duration::ZERO
}

fn decimal_one() -> Decimal {
    Decimal::ONE
}

impl Kind {
    fn is_source(&self) -> bool {
        matches!(self, Kind::Input { .. } | Kind::Metric { .. } | Kind::State { .. } | Kind::Event { .. } | Kind::Attribute { .. } | Kind::Timer { .. })
    }

    fn is_action(&self) -> bool {
        matches!(
            self,
            Kind::Alarm { .. }
                | Kind::RecordEvent { .. }
                | Kind::RecordMetric { .. }
                | Kind::RecordAttribute { .. }
                | Kind::Notify { .. }
                | Kind::Export { .. }
                | Kind::Command { .. }
                | Kind::Reply { .. }
                | Kind::Log { .. }
                | Kind::Output { .. }
                | Kind::Case { .. }
        )
    }

    /// What it can send out of.
    fn ports(&self) -> Vec<String> {
        match self {
            Kind::Compare { .. } | Kind::Range { .. } | Kind::All { .. } | Kind::Latch | Kind::Has { .. } | Kind::Schedule { .. } => {
                vec!["yes".into(), "no".into()]
            }
            Kind::Switch { cases, .. } => cases.iter().cloned().chain(["other".to_string()]).collect(),
            Kind::Geofence { .. } => ["inside", "outside", "entered", "left"].map(String::from).to_vec(),
            Kind::Anomaly { .. } => vec!["normal".into(), "odd".into()],
            Kind::Normalize { .. } => vec!["out".into(), "quarantine".into()],
            Kind::QualityGate { .. } => ["good", "uncertain", "bad", "stale"].map(String::from).to_vec(),
            Kind::EventWindow { .. } => ["out", "revised", "late"].map(String::from).to_vec(),
            Kind::Episode { .. } => ["opened", "reopened", "evidence", "stale", "closed", "interrupted"].map(String::from).to_vec(),
            k if k.is_action() => vec![],
            _ => vec!["out".into()],
        }
    }

    /// What it takes, or `None` for inputs named by whoever wires them.
    fn takes(&self) -> Option<&'static [&'static str]> {
        match self {
            Kind::Combine { .. } | Kind::All { .. } => None,
            Kind::Hold { .. } => Some(&["in", "reset"]),
            Kind::Latch => Some(&["set", "reset"]),
            Kind::Alarm { .. } => Some(&["in", "clear"]),
            k if k.is_source() => Some(&[]),
            _ => Some(&["in"]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edge {
    from: String,
    #[serde(default = "out")]
    port: String,
    to: String,
    #[serde(default = "into")]
    input: String,
}

fn out() -> String {
    "out".into()
}

fn into() -> String {
    "in".into()
}

/// One thing a device did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub device: String,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub attribute: Option<String>,
    #[serde(default)]
    pub value: Value,
    /// When the device saw it, if it says: what event-time steps go by. Otherwise when it arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    /// How far it can be trusted: good unless the device or its gateway said otherwise.
    #[serde(default, skip_serializing_if = "Quality::is_good")]
    pub quality: Quality,
}

/// What a flow did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    Alarm {
        node: String,
        name: String,
        severity: String,
        device: Option<String>,
        raised: bool,
        value: Option<f64>,
    },
    Event {
        device: Option<String>,
        name: String,
        value: Value,
    },
    Metric {
        device: Option<String>,
        name: String,
        value: f64,
        unit: Option<String>,
    },
    Attribute {
        device: Option<String>,
        key: String,
        value: Value,
    },
    Notify {
        device: Option<String>,
        title: String,
    },
    Export {
        to: String,
        body: Value,
    },
    Command {
        node: String,
        device: Option<String>,
        operation: String,
        payload: Value,
        expires_secs: i64,
    },
    Reply {
        device: Option<String>,
        id: String,
        value: Value,
    },
    Log {
        node: String,
        device: Option<String>,
        text: String,
    },
    /// An episode for the platform: `phase` is `opened`, `reopened`, `evidence`, `stale`, `closed` or
    /// `interrupted`. `episode` names it however often it is said, so the platform keeps one condition and
    /// starts one case for it; `at` is when it was seen, and `raw` the reading before it was normalized.
    Condition {
        node: String,
        device: Option<String>,
        episode: String,
        phase: String,
        title: String,
        at: DateTime<Utc>,
        value: Option<f64>,
        /// The furthest past its limit the episode has gone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peak: Option<f64>,
        quality: Quality,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
}

/// What a flow can look up about devices: their newest numbers and what they said about themselves.
/// It is kept up by whatever feeds the flow, the same at a desk, at a gateway and on the platform.
#[derive(Debug, Clone, Default)]
pub struct Known {
    metrics: HashMap<(String, String), Value>,
    attributes: HashMap<(String, String), Value>,
}

impl Known {
    /// Takes note of something a device did. Past the limit, what is new is not kept and what is
    /// already known stays up to date.
    pub fn note(&mut self, input: &Input) {
        let (key, map) = match (&input.metric, &input.attribute) {
            (Some(metric), _) => (metric, &mut self.metrics),
            (_, Some(key)) => (key, &mut self.attributes),
            _ => return,
        };
        let entry = (input.device.clone(), key.clone());
        if map.len() < MAX_KNOWN || map.contains_key(&entry) {
            map.insert(entry, input.value.clone());
        }
    }

    pub fn metric(&self, device: &str, name: &str) -> Option<&Value> {
        self.metrics.get(&(device.to_string(), name.to_string()))
    }

    pub fn attribute(&self, device: &str, key: &str) -> Option<&Value> {
        self.attributes.get(&(device.to_string(), key.to_string()))
    }
}

/// A message passing through one node, for a simulation or someone watching a flow to see.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub node: String,
    pub device: Option<String>,
    pub value: Value,
    /// Where it left by. Empty if it stopped here.
    pub ports: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Output {
    pub effects: Vec<Effect>,
    pub trace: Vec<Trace>,
    /// Why something did not happen, for the person who drew it.
    pub problems: Vec<String>,
    /// A business-critical flow had this reading already: nothing came of it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub duplicate: bool,
}

/// What a flow needs to hear and to look up, for whatever feeds it.
#[derive(Debug, Default, PartialEq)]
pub struct Wants {
    pub metrics: BTreeSet<String>,
    pub states: bool,
    pub events: BTreeSet<String>,
    pub attributes: BTreeSet<String>,
    pub looked_up_metrics: BTreeSet<String>,
    pub looked_up_attributes: BTreeSet<String>,
}

/// What a node remembers for one device.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Memory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    since: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    on: bool,
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    held: VecDeque<(DateTime<Utc>, Value)>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    inputs: BTreeMap<String, (DateTime<Utc>, Value)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    windows: Option<Windows>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    episode: Option<Episode>,
}

/// Everything a running flow remembers, by node and device. It survives a restart in the journal,
/// so a hold half way through keeps counting and a raised alarm is not raised twice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    nodes: BTreeMap<String, BTreeMap<String, Memory>>,
    /// For a business-critical flow: the times of the latest readings had from each source, so the
    /// same reading delivered again changes nothing (FLC-EDGE-005).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    seen: BTreeMap<String, VecDeque<i64>>,
}

impl State {
    /// How many keys every node holds together.
    pub fn keys(&self) -> usize {
        self.nodes.values().map(BTreeMap::len).sum()
    }
}

#[derive(Debug)]
struct Wire {
    port: String,
    to: usize,
    input: String,
}

/// A flow that passed [`parse`].
#[derive(Debug)]
pub struct Compiled {
    pub nodes: Vec<Node>,
    pub digest: String,
    wires: Vec<Vec<Wire>>,
    /// By node, the inputs something is wired to.
    wired: Vec<BTreeSet<String>>,
    formulas: Vec<Option<Formula>>,
    /// `profile: critical`: a reading had already is known and changes nothing, and its gateway writes
    /// where the flow stands with what it did, in one transaction, after each reading.
    pub critical: bool,
    /// What episode identities are scoped by: the flow's id where it runs, so two flows never name the
    /// same episode.
    pub scope: String,
}

fn number(v: &Value) -> Option<f64> {
    match v {
        // What an event window or an episode sends on: its number is its value.
        Value::Object(m) if m.contains_key("end") || m.contains_key("episode") => m.get("value").and_then(number),
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A value as a name. Readings are floating point, and 1.0 from a device is the 1 of a person's map.
fn word(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 1e15).map_or_else(|| n.to_string(), |f| (f as i64).to_string()),
        other => other.to_string(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty() && s != "false",
        _ => true,
    }
}

fn compares(op: Comparison, left: &Value, right: &Value) -> Option<bool> {
    if let (Some(a), Some(b)) = (number(left), number(right)) {
        return Some(match op {
            Comparison::Above => a > b,
            Comparison::AtLeast => a >= b,
            Comparison::Below => a < b,
            Comparison::AtMost => a <= b,
            Comparison::Is => a == b,
            Comparison::IsNot => a != b,
        });
    }
    match op {
        Comparison::Is => Some(word(left) == word(right)),
        Comparison::IsNot => Some(word(left) != word(right)),
        // Text has no order worth acting on, so this says nothing rather than something wrong.
        _ => None,
    }
}

fn device_matches(pattern: &str, device: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => device.starts_with(prefix),
        None => pattern.is_empty() || pattern == device,
    }
}

fn span(d: Duration) -> TimeDelta {
    TimeDelta::from_std(d).unwrap_or(TimeDelta::MAX)
}

/// Reads and checks a flow, `{nodes: [...], edges: [...]}`, as JSON or YAML. Every error names the
/// node, so a person can fix it from the message. A draft can be saved with faults: this is what
/// stands between a draft and a review.
pub fn parse(text: &str) -> Result<Compiled, String> {
    let body: Value = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    let critical = match body.get("profile") {
        None | Some(Value::Null) => false,
        Some(p) if p == "standard" => false,
        Some(p) if p == "critical" => true,
        Some(other) => return Err(format!("profile is standard or critical, not {other}")),
    };
    let raw = body["nodes"].as_array().ok_or("a flow needs nodes")?;
    if raw.len() > MAX_NODES {
        return Err(format!("a flow has at most {MAX_NODES} nodes"));
    }
    let mut nodes = Vec::with_capacity(raw.len());
    for (i, n) in raw.iter().enumerate() {
        let mut fields = n.as_object().cloned().ok_or_else(|| format!("node {i} is not one"))?;
        let id = fields.remove("id").and_then(|v| v.as_str().map(str::to_string)).filter(|s| !s.is_empty()).ok_or_else(|| format!("node {i} has no id"))?;
        // Where it sits on the canvas, and what a person called it, mean nothing here.
        for layout in ["x", "y", "label"] {
            fields.remove(layout);
        }
        let kind: Kind = serde_json::from_value(Value::Object(fields)).map_err(|e| format!("{id}: {e}"))?;
        if nodes.iter().any(|m: &Node| m.id == id) {
            return Err(format!("{id} is there twice"));
        }
        nodes.push(Node { id, kind });
    }
    let edges: Vec<Edge> = match body.get("edges") {
        Some(e) if !e.is_null() => serde_json::from_value(e.clone()).map_err(|e| format!("edges: {e}"))?,
        _ => Vec::new(),
    };
    if edges.len() > MAX_EDGES {
        return Err(format!("a flow has at most {MAX_EDGES} wires"));
    }
    let index = |id: &str| nodes.iter().position(|n| n.id == id);
    let mut wires: Vec<Vec<Wire>> = nodes.iter().map(|_| Vec::new()).collect();
    let mut wired: Vec<BTreeSet<String>> = nodes.iter().map(|_| BTreeSet::new()).collect();
    for e in &edges {
        let from = index(&e.from).ok_or_else(|| format!("a wire leaves {}, which is not here", e.from))?;
        let to = index(&e.to).ok_or_else(|| format!("a wire reaches {}, which is not here", e.to))?;
        if !nodes[from].kind.ports().contains(&e.port) {
            return Err(format!("{} has no way out called {}", e.from, e.port));
        }
        if nodes[to].kind.takes().is_some_and(|t| !t.contains(&e.input.as_str())) {
            return Err(format!("{} takes nothing at {}", e.to, e.input));
        }
        wires[from].push(Wire { port: e.port.clone(), to, input: e.input.clone() });
        wired[to].insert(e.input.clone());
    }
    // Kahn's: what cannot be ordered is on a cycle, which is exactly what a flow may not have.
    let mut waiting: Vec<usize> = wired.iter().map(|_| 0).collect();
    for out in &wires {
        for w in out {
            waiting[w.to] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..nodes.len()).filter(|i| waiting[*i] == 0).collect();
    let mut ordered = 0;
    while let Some(i) = ready.pop() {
        ordered += 1;
        for w in &wires[i] {
            waiting[w.to] -= 1;
            if waiting[w.to] == 0 {
                ready.push(w.to);
            }
        }
    }
    if ordered != nodes.len() {
        let on: Vec<&str> = nodes.iter().zip(&waiting).filter(|(_, w)| **w > 0).map(|(n, _)| n.id.as_str()).collect();
        return Err(format!("a flow cannot go round in a circle, and these do: {}", on.join(", ")));
    }
    let mut reached: Vec<bool> = nodes.iter().map(|n| n.kind.is_source()).collect();
    let mut leads: Vec<usize> = (0..nodes.len()).filter(|i| reached[*i]).collect();
    while let Some(i) = leads.pop() {
        for w in &wires[i] {
            if !reached[w.to] {
                reached[w.to] = true;
                leads.push(w.to);
            }
        }
    }
    let mut formulas = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        if !reached[i] {
            return Err(format!("{}: nothing reaches it", n.id));
        }
        if wires[i].is_empty() && !n.kind.is_action() {
            return Err(format!("{}: nothing comes of it: wire it to something", n.id));
        }
        let missing = |input: &str| !wired[i].contains(input);
        match &n.kind {
            Kind::Hold { .. } if missing("reset") => return Err(format!("{}: say what resets it, or it holds for ever", n.id)),
            Kind::Latch if missing("reset") || missing("set") => return Err(format!("{}: wire both set and reset", n.id)),
            Kind::Alarm { .. } if missing("clear") => return Err(format!("{}: say what clears it, or it stays raised for ever", n.id)),
            Kind::Alarm { severity, .. } if !["Info", "Warning", "Minor", "Major", "Critical"].contains(&severity.as_str()) => {
                return Err(format!("{}: severity is Info, Warning, Minor, Major or Critical", n.id));
            }
            Kind::Range { min, max } if min > max => return Err(format!("{}: {min} is above {max}", n.id)),
            Kind::Count { of: 0, .. } => return Err(format!("{}: count at least one", n.id)),
            Kind::Command { operation, .. } if operation.is_empty() => return Err(format!("{}: say which operation", n.id)),
            Kind::Switch { cases, .. } if cases.is_empty() => return Err(format!("{}: give it some cases", n.id)),
            Kind::Has { paths } if paths.is_empty() => return Err(format!("{}: say which parts", n.id)),
            Kind::Geofence { polygon, circle, .. } => {
                let (corners, round) = (polygon.len(), circle.is_some());
                if (corners < 3 && !round) || (corners > 0 && round) {
                    return Err(format!("{}: draw a polygon of three corners or more, or give a circle", n.id));
                }
                if circle.is_some_and(|c| c.radius_m <= 0.0) {
                    return Err(format!("{}: a circle has a radius", n.id));
                }
            }
            Kind::Schedule { from, to, days, .. } => {
                if minutes_of(from).is_none() || minutes_of(to).is_none() {
                    return Err(format!("{}: times of day are written 08:30", n.id));
                }
                if days.iter().any(|d| !(1..=7).contains(d)) {
                    return Err(format!("{}: days go from 1, Monday, to 7", n.id));
                }
            }
            Kind::Anomaly { window, sigma } if !(ANOMALY_MIN..=MAX_HELD).contains(window) || *sigma <= 0.0 => {
                return Err(format!("{}: look back over {ANOMALY_MIN} to {MAX_HELD} readings, by more than zero deviations", n.id));
            }
            Kind::Reassign { to } if to.is_empty() => return Err(format!("{}: say whose it becomes", n.id)),
            Kind::Normalize { scale, .. } if scale.is_zero() => return Err(format!("{}: a scale of zero makes every reading the same", n.id)),
            Kind::Normalize { min: Some(lo), max: Some(hi), .. } if lo > hi => return Err(format!("{}: {lo} is above {hi}", n.id)),
            Kind::EventWindow { size, lateness, idle, .. } => {
                if *size < Duration::from_secs(1) || *size > Duration::from_secs(86_400) {
                    return Err(format!("{}: a window is between a second and a day long", n.id));
                }
                if *lateness > Duration::from_secs(7 * 86_400) {
                    return Err(format!("{}: corrections are taken for a week at most", n.id));
                }
                if idle.is_some_and(|i| i < Duration::from_secs(1)) {
                    return Err(format!("{}: a source is idle after a second at least", n.id));
                }
            }
            Kind::Episode { above, below, close, .. } => match (above, below) {
                (Some(open), None) if close > open => {
                    return Err(format!("{}: it closes at or under {open}, where it opens, or it would close as it opens", n.id));
                }
                (None, Some(open)) if close < open => {
                    return Err(format!("{}: it closes at or over {open}, where it opens, or it would close as it opens", n.id));
                }
                (Some(_), None) | (None, Some(_)) => {}
                _ => return Err(format!("{}: say where it opens: above a value, or below one", n.id)),
            },
            Kind::Case { title } if title.trim().is_empty() => return Err(format!("{}: give the case a title", n.id)),
            _ => {}
        }
        let text = match &n.kind {
            Kind::Math { formula } => Some(formula),
            Kind::Combine { formula, .. } => formula.as_ref(),
            _ => None,
        };
        formulas.push(text.map(|t| Formula::parse(t, &|name| Ok(name.to_string()))).transpose().map_err(|e| format!("{}: {e}", n.id))?);
    }
    for (from, out) in wires.iter().enumerate() {
        for w in out {
            if matches!(nodes[w.to].kind, Kind::Case { .. }) && !matches!(nodes[from].kind, Kind::Episode { .. }) {
                return Err(format!("{}: a case takes an episode: wire it from one", nodes[w.to].id));
            }
        }
    }
    if !nodes.iter().any(|n| n.kind.is_action()) {
        return Err("this flow does nothing: add an action".into());
    }
    Ok(Compiled { nodes, digest: hex::encode(Sha256::digest(text.as_bytes())), wires, wired, formulas, critical, scope: String::new() })
}

struct Run<'a> {
    state: &'a mut State,
    at: DateTime<Utc>,
    out: Output,
    known: &'a Known,
    /// About the reading being followed: when it was seen, how far it can be trusted, and, once a step
    /// normalized it, what it was before and in which unit.
    event_at: DateTime<Utc>,
    quality: Quality,
    raw: Option<Value>,
    unit: Option<String>,
}

impl<'a> Run<'a> {
    fn new(state: &'a mut State, at: DateTime<Utc>, known: &'a Known) -> Run<'a> {
        Run { state, at, out: Output::default(), known, event_at: at, quality: Quality::Good, raw: None, unit: None }
    }
}

pub fn minutes_of(text: &str) -> Option<i32> {
    let (h, m) = text.split_once(':')?;
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m)
}

/// Whether a moment falls in a schedule. A schedule that runs over midnight belongs to the day it
/// started on.
fn scheduled(at: DateTime<Utc>, from: &str, to: &str, days: &[u8], offset_minutes: i32) -> bool {
    use chrono::{Datelike, Timelike};
    let local = at + TimeDelta::minutes(i64::from(offset_minutes));
    let (Some(from), Some(to)) = (minutes_of(from), minutes_of(to)) else { return false };
    let now = (local.hour() * 60 + local.minute()) as i32;
    let today = local.weekday().number_from_monday() as u8;
    let yesterday = local.weekday().pred().number_from_monday() as u8;
    let on = |day: u8| days.is_empty() || days.contains(&day);
    if from <= to { now >= from && now < to && on(today) } else { (now >= from && on(today)) || (now < to && on(yesterday)) }
}

/// Metres between two points on the earth, near enough for a yard or a city.
fn metres(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat1, lat2) = (a.0.to_radians(), b.0.to_radians());
    let (dlat, dlon) = ((b.0 - a.0).to_radians(), (b.1 - a.1).to_radians());
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * h.sqrt().asin()
}

/// Ray casting: a line from the point crosses the edge of what it is inside an odd number of times.
fn in_polygon(point: (f64, f64), corners: &[[f64; 2]]) -> bool {
    let (lat, lon) = point;
    let mut inside = false;
    let mut j = corners.len() - 1;
    for i in 0..corners.len() {
        let ([ai, oi], [aj, oj]) = (corners[i], corners[j]);
        if (oi > lon) != (oj > lon) && lat < (aj - ai) * (lon - oi) / (oj - oi) + ai {
            inside = !inside;
        }
        j = i;
    }
    inside
}

impl Run<'_> {
    fn memory(&mut self, node: &str, key: &str) -> Option<&mut Memory> {
        let new = !self.state.nodes.get(node).is_some_and(|k| k.contains_key(key));
        if new && self.state.keys() >= MAX_FLOW_KEYS {
            self.out.problems.push(format!("this flow is keeping track of {MAX_FLOW_KEYS} things already: {key} at {node} was left out"));
            return None;
        }
        let keys = self.state.nodes.entry(node.to_string()).or_default();
        if new && keys.len() >= MAX_KEYS {
            self.out.problems.push(format!("{node} is keeping track of too many devices: {key} was left out"));
            return None;
        }
        Some(keys.entry(key.to_string()).or_default())
    }
}

/// A message's way through a node: out of which ports, with what.
type Leaving = Vec<(String, Value)>;

fn out_of(port: &str, value: Value) -> Leaving {
    vec![(port.to_string(), value)]
}

fn statistic(stat: Stat, numbers: &[f64]) -> Option<f64> {
    let first = *numbers.first()?;
    let sum: f64 = numbers.iter().sum();
    Some(match stat {
        Stat::Avg => sum / numbers.len() as f64,
        Stat::Min => numbers.iter().copied().fold(first, f64::min),
        Stat::Max => numbers.iter().copied().fold(first, f64::max),
        Stat::Sum => sum,
        Stat::Count => numbers.len() as f64,
        Stat::Delta => numbers[numbers.len() - 1] - first,
    })
}

fn calculate(formula: &Formula, row: &Map<String, Value>) -> Option<Value> {
    formula.eval(row).and_then(|d| d.to_f64()).map(|n| json!(n))
}

fn row_of(value: &Value) -> Map<String, Value> {
    let mut row = value.as_object().cloned().unwrap_or_default();
    row.insert("value".into(), value.clone());
    row
}

fn fill(template: &Value, value: &Value, device: Option<&str>) -> Result<Value, String> {
    render_json(template, &json!({"value": value, "device": device}))
}

/// When what a message is about was seen, in milliseconds: an event window's end for its result, or
/// the reading's own time.
fn seen_ms(value: &Value, run: &Run<'_>) -> i64 {
    value.get("end").and_then(Value::as_str).and_then(|s| s.parse::<DateTime<Utc>>().ok()).map_or_else(|| pipeline::ms(run.event_at), pipeline::ms)
}

fn window_rule(size: Duration, wait: Duration, lateness: Duration, idle: Option<Duration>) -> WindowRule {
    let ms = |d: Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    WindowRule { size_ms: ms(size).max(1), wait_ms: ms(wait), lateness_ms: ms(lateness), idle_ms: idle.map(ms) }
}

#[allow(clippy::too_many_arguments)]
fn episode_rule(above: Option<f64>, below: Option<f64>, close: f64, on: Duration, off: Duration, reopen: Duration, stale: Option<Duration>) -> EpisodeRule {
    let ms = |d: Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    EpisodeRule {
        high: above.is_some(),
        open: above.or(below).unwrap_or_default(),
        close,
        on_delay_ms: ms(on),
        off_delay_ms: ms(off),
        reopen_ms: ms(reopen),
        stale_ms: stale.map(ms),
    }
}

fn said(out: Vec<Said>) -> Leaving {
    out.iter().map(|s| (s.port.to_string(), s.message())).collect()
}

fn happened(out: Vec<Happened>, device: Option<&str>) -> Leaving {
    out.iter().map(|h| (h.port.to_string(), h.message(device))).collect()
}

impl Compiled {
    fn step(&self, i: usize, input: &str, device: Option<&str>, value: Value, run: &mut Run<'_>) -> Leaving {
        let node = &self.nodes[i];
        let id = node.id.as_str();
        let at = run.at;
        let key = device.unwrap_or_default();
        match &node.kind {
            Kind::Input { .. } | Kind::Metric { .. } | Kind::State { .. } | Kind::Event { .. } | Kind::Attribute { .. } | Kind::Timer { .. } => {
                out_of("out", value)
            }
            Kind::Compare { op, value: right } => match compares(*op, &value, right) {
                Some(true) => out_of("yes", value),
                Some(false) => out_of("no", value),
                None => vec![],
            },
            Kind::Range { min, max } => match number(&value) {
                Some(n) if n >= *min && n <= *max => out_of("yes", value),
                Some(_) => out_of("no", value),
                None => vec![],
            },
            Kind::Changed { by } => {
                let Some(m) = run.memory(id, key) else { return vec![] };
                let moved = match (&m.last, by, number(&value)) {
                    (None, _, _) => true,
                    (Some(last), Some(by), Some(n)) => number(last).is_none_or(|l| (n - l).abs() >= *by),
                    (Some(last), _, _) => *last != value,
                };
                if !moved {
                    return vec![];
                }
                m.last = Some(value.clone());
                out_of("out", value)
            }
            Kind::Hold { hold } => {
                let Some(m) = run.memory(id, key) else { return vec![] };
                if input == "reset" {
                    (m.since, m.on, m.last) = (None, false, None);
                    return vec![];
                }
                m.since.get_or_insert(at);
                m.last = Some(value);
                m.device = device.map(str::to_string);
                Self::due(m, span(*hold), at)
            }
            Kind::Silence { .. } => {
                let Some(m) = run.memory(id, key) else { return vec![] };
                (m.since, m.on, m.last) = (Some(at), false, Some(value));
                m.device = device.map(str::to_string);
                vec![]
            }
            Kind::Limit { every } => {
                let Some(m) = run.memory(id, key) else { return vec![] };
                if m.since.is_some_and(|last| at - last < span(*every)) {
                    return vec![];
                }
                m.since = Some(at);
                out_of("out", value)
            }
            Kind::Delay { wait } => {
                let Some(m) = run.memory(id, key) else { return vec![] };
                if m.held.len() >= MAX_HELD {
                    m.held.pop_front();
                    run.out.problems.push(format!("{id} is holding too much: its oldest message was dropped"));
                    return vec![];
                }
                m.held.push_back((at + span(*wait), value));
                m.device = device.map(str::to_string);
                vec![]
            }
            Kind::Count { of, within } => {
                let Some(m) = run.memory(id, key) else { return vec![] };
                let from = at - span(*within);
                while m.held.front().is_some_and(|(t, _)| *t < from) {
                    m.held.pop_front();
                }
                if m.held.len() < MAX_HELD {
                    m.held.push_back((at, Value::Null));
                }
                if m.held.len() < *of {
                    return vec![];
                }
                m.held.clear();
                out_of("out", json!(of))
            }
            Kind::Window { over, stat, across } => {
                let Some(n) = number(&value) else { return vec![] };
                let Some(m) = run.memory(id, if *across { "" } else { key }) else { return vec![] };
                let from = at - span(*over);
                while m.held.front().is_some_and(|(t, _)| *t < from) || m.held.len() >= MAX_HELD {
                    m.held.pop_front();
                }
                m.held.push_back((at, json!(n)));
                let numbers: Vec<f64> = m.held.iter().filter_map(|(_, v)| v.as_f64()).collect();
                statistic(*stat, &numbers).map_or(vec![], |s| out_of("out", json!(s)))
            }
            Kind::Math { .. } => self.formulas[i].as_ref().and_then(|f| calculate(f, &row_of(&value))).map_or(vec![], |v| out_of("out", v)),
            Kind::Combine { fresh, across, .. } => {
                let key = if *across { "" } else { key };
                let Some(m) = run.memory(id, key) else { return vec![] };
                m.inputs.insert(input.to_string(), (at, value));
                let values: Map<String, Value> =
                    m.inputs.iter().filter(|(_, (when, _))| fresh.is_none_or(|f| at - *when <= span(f))).map(|(k, (_, v))| (k.clone(), v.clone())).collect();
                if !self.wired[i].iter().all(|w| values.contains_key(w)) {
                    return vec![];
                }
                match &self.formulas[i] {
                    Some(f) => calculate(f, &values).map_or(vec![], |v| out_of("out", v)),
                    None => out_of("out", Value::Object(values)),
                }
            }
            Kind::All { any, fresh, across } => {
                let key = if *across { "" } else { key };
                let Some(m) = run.memory(id, key) else { return vec![] };
                m.inputs.insert(input.to_string(), (at, value));
                let holds = |name: &String| m.inputs.get(name).is_some_and(|(when, v)| truthy(v) && fresh.is_none_or(|f| at - *when <= span(f)));
                let now = if *any { self.wired[i].iter().any(holds) } else { self.wired[i].iter().all(holds) };
                let first = m.last.is_none();
                if !first && m.on == now {
                    return vec![];
                }
                (m.on, m.last) = (now, Some(json!(now)));
                out_of(if now { "yes" } else { "no" }, json!(now))
            }
            Kind::Latch => {
                let Some(m) = run.memory(id, key) else { return vec![] };
                let now = input == "set";
                if m.on == now {
                    return vec![];
                }
                m.on = now;
                out_of(if now { "yes" } else { "no" }, value)
            }
            Kind::Map { map, other } => match map.get(&word(&value)).or(other.as_ref()) {
                Some(v) => out_of("out", v.clone()),
                None => vec![],
            },
            Kind::Switch { cases, on } => {
                let said = match on {
                    Some(path) => at_path(&value, path).map_or_else(String::new, word),
                    None => word(&value),
                };
                out_of(if cases.contains(&said) { &said } else { "other" }, value)
            }
            Kind::Set { value: template } => match fill(template, &value, device) {
                Ok(v) => out_of("out", v),
                Err(why) => {
                    run.out.problems.push(format!("{id}: {why}"));
                    vec![]
                }
            },
            Kind::Field { path } => at_path(&value, path).cloned().map_or(vec![], |v| out_of("out", v)),
            Kind::Has { paths } => {
                let all = paths.iter().all(|p| at_path(&value, p).is_some_and(|v| !v.is_null()));
                out_of(if all { "yes" } else { "no" }, value)
            }
            Kind::Delta { per } => {
                let Some(n) = number(&value) else { return vec![] };
                let Some(m) = run.memory(id, key) else { return vec![] };
                let before = m.last.as_ref().and_then(number);
                let (then, now) = (m.since, at);
                (m.last, m.since) = (Some(json!(n)), Some(at));
                let Some(before) = before.filter(|b| n >= *b) else { return vec![] };
                let moved = n - before;
                let told = match (per, then) {
                    (Some(per), Some(then)) => {
                        let secs = (now - then).num_milliseconds() as f64 / 1000.0;
                        if secs <= 0.0 {
                            return vec![];
                        }
                        moved / secs * per.as_secs_f64()
                    }
                    _ => moved,
                };
                out_of("out", json!(told))
            }
            Kind::Geofence { lat, lon, polygon, circle } => {
                let (Some(a), Some(o)) = (at_path(&value, lat).and_then(number), at_path(&value, lon).and_then(number)) else {
                    run.out.problems.push(format!("{id}: there is no position in this"));
                    return vec![];
                };
                let inside = match circle {
                    Some(c) => metres((a, o), (c.lat, c.lon)) <= c.radius_m,
                    None => in_polygon((a, o), polygon),
                };
                let Some(m) = run.memory(id, key) else { return vec![] };
                let first = m.since.is_none();
                let moved = !first && m.on != inside;
                (m.on, m.since) = (inside, Some(at));
                let mut leaving = out_of(if inside { "inside" } else { "outside" }, value.clone());
                if moved {
                    leaving.push((if inside { "entered" } else { "left" }.to_string(), value));
                }
                leaving
            }
            Kind::Dedupe { key: part, within } => {
                let said = match part {
                    Some(path) => at_path(&value, path).map_or_else(String::new, word),
                    None => word(&value),
                };
                let Some(m) = run.memory(id, key) else { return vec![] };
                while m.held.front().is_some_and(|(when, _)| at - *when >= span(*within)) {
                    m.held.pop_front();
                }
                if m.held.iter().any(|(_, seen)| seen.as_str() == Some(said.as_str())) {
                    return vec![];
                }
                if m.held.len() >= MAX_HELD {
                    m.held.pop_front();
                }
                m.held.push_back((at, json!(said)));
                out_of("out", value)
            }
            Kind::Split { path } => {
                let list = match path {
                    Some(p) => at_path(&value, p),
                    None => Some(&value),
                };
                let Some(Value::Array(items)) = list else { return vec![] };
                if items.len() > MAX_SPLIT {
                    run.out.problems.push(format!("{id}: a list of {} is more than one message should become", items.len()));
                }
                items.iter().take(MAX_SPLIT).map(|v| ("out".to_string(), v.clone())).collect()
            }
            Kind::Keys { rename, drop, keep } => {
                let Value::Object(from) = value else { return vec![] };
                let mut to = Map::new();
                for (k, v) in from {
                    if drop.contains(&k) || (!keep.is_empty() && !keep.contains(&k)) {
                        continue;
                    }
                    to.insert(rename.get(&k).cloned().unwrap_or(k), v);
                }
                out_of("out", Value::Object(to))
            }
            Kind::Enrich { metrics, attributes, device: fixed } => {
                let who = fixed.as_deref().or(device).unwrap_or_default();
                let mut row = match value {
                    Value::Object(m) => m,
                    other => Map::from_iter([("value".to_string(), other)]),
                };
                for name in metrics {
                    if let Some(v) = run.known.metric(who, name) {
                        row.insert(name.clone(), v.clone());
                    }
                }
                for name in attributes {
                    if let Some(v) = run.known.attribute(who, name) {
                        row.insert(name.clone(), v.clone());
                    }
                }
                out_of("out", Value::Object(row))
            }
            Kind::Schedule { from, to, days, offset_minutes } => out_of(if scheduled(at, from, to, days, *offset_minutes) { "yes" } else { "no" }, value),
            Kind::Anomaly { window, sigma } => {
                let Some(n) = number(&value) else { return vec![] };
                let Some(m) = run.memory(id, key) else { return vec![] };
                let seen: Vec<f64> = m.held.iter().filter_map(|(_, v)| number(v)).collect();
                let odd = seen.len() >= ANOMALY_MIN && {
                    let mean = seen.iter().sum::<f64>() / seen.len() as f64;
                    let deviation = (seen.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / seen.len() as f64).sqrt();
                    if deviation == 0.0 { n != mean } else { (n - mean).abs() > sigma * deviation }
                };
                // An odd reading is not added: one fault must not teach it that faults are normal.
                if !odd {
                    if m.held.len() >= *window {
                        m.held.pop_front();
                    }
                    m.held.push_back((at, json!(n)));
                }
                out_of(if odd { "odd" } else { "normal" }, value)
            }
            Kind::Reassign { .. } => out_of("out", value),
            Kind::Normalize { scale, offset, unit, min, max } => match pipeline::normalize(&value, *scale, *offset, *min, *max) {
                Ok(n) => match n.to_f64() {
                    Some(f) => {
                        run.raw = Some(value);
                        run.unit = unit.clone();
                        out_of("out", json!(f))
                    }
                    None => out_of("quarantine", json!({"raw": value, "why": "out of range", "device": device})),
                },
                Err(why) => out_of("quarantine", json!({"raw": value, "why": why, "device": device})),
            },
            Kind::QualityGate { stale } => {
                let old = stale.is_some_and(|s| at - run.event_at > span(s));
                out_of(if old { "stale" } else { run.quality.word() }, value)
            }
            Kind::EventWindow { size, stat, wait, lateness, idle, across } => {
                // Only what can be trusted goes into a statistic.
                if !run.quality.is_good() {
                    return vec![];
                }
                let Some(n) = number(&value) else { return vec![] };
                let (rule, stat, seen, now) = (window_rule(*size, *wait, *lateness, *idle), *stat, seen_ms(&value, run), pipeline::ms(at));
                let Some(m) = run.memory(id, if *across { "" } else { key }) else { return vec![] };
                m.device = if *across { None } else { device.map(str::to_string) };
                said(m.windows.get_or_insert_with(Windows::default).observe(seen, n, now, &rule, |v| statistic(stat, v)))
            }
            Kind::Episode { above, below, close, on_delay, off_delay, reopen, stale } => {
                let Some(n) = number(&value) else { return vec![] };
                let rule = episode_rule(*above, *below, *close, *on_delay, *off_delay, *reopen, *stale);
                let (seen, now, quality, scope) = (seen_ms(&value, run), pipeline::ms(at), run.quality, self.scope.clone());
                let Some(m) = run.memory(id, key) else { return vec![] };
                m.device = device.map(str::to_string);
                happened(m.episode.get_or_insert_with(Episode::default).observe(&scope, id, key, seen, n, quality, now, &rule), device)
            }
            Kind::Alarm { name, severity, device: fixed } => {
                let device = fixed.as_deref().or(device);
                let Some(m) = run.memory(id, device.unwrap_or_default()) else { return vec![] };
                let raised = input != "clear";
                if m.on == raised {
                    return vec![];
                }
                m.on = raised;
                let effect = Effect::Alarm {
                    node: id.to_string(),
                    name: name.clone(),
                    severity: severity.clone(),
                    device: device.map(str::to_string),
                    raised,
                    value: number(&value),
                };
                run.out.effects.push(effect);
                vec![]
            }
            Kind::RecordEvent { name } => {
                run.out.effects.push(Effect::Event { device: device.map(str::to_string), name: name.clone(), value });
                vec![]
            }
            Kind::RecordMetric { name, unit } => {
                if let Some(n) = number(&value) {
                    run.out.effects.push(Effect::Metric { device: device.map(str::to_string), name: name.clone(), value: n, unit: unit.clone() });
                }
                vec![]
            }
            Kind::RecordAttribute { key } => {
                run.out.effects.push(Effect::Attribute { device: device.map(str::to_string), key: key.clone(), value });
                vec![]
            }
            Kind::Notify { title, every } => {
                let Some(m) = run.memory(id, key) else { return vec![] };
                if m.since.is_some_and(|last| at - last < span(*every)) {
                    return vec![];
                }
                m.since = Some(at);
                match fill(&json!(title), &value, device) {
                    Ok(title) => run.out.effects.push(Effect::Notify { device: device.map(str::to_string), title: word(&title) }),
                    Err(why) => run.out.problems.push(format!("{id}: {why}")),
                }
                vec![]
            }
            Kind::Export { to } => {
                run.out.effects.push(Effect::Export { to: to.clone(), body: json!({"device": device, "value": value, "at": at}) });
                vec![]
            }
            Kind::Reply { id: path, with } => {
                let Some(asked) = at_path(&value, path).map(word) else {
                    run.out.problems.push(format!("{id}: there is no question here to answer"));
                    return vec![];
                };
                match fill(with, &value, device) {
                    Ok(answer) => run.out.effects.push(Effect::Reply { device: device.map(str::to_string), id: asked, value: answer }),
                    Err(why) => run.out.problems.push(format!("{id}: {why}")),
                }
                vec![]
            }
            Kind::Case { title } => {
                let (Some(episode), Some(phase)) = (value.get("episode").and_then(Value::as_str), value.get("phase").and_then(Value::as_str)) else {
                    run.out.problems.push(format!("{id}: only an episode opens a case"));
                    return vec![];
                };
                let seen = value.get("at").and_then(Value::as_str).and_then(|s| s.parse::<DateTime<Utc>>().ok()).unwrap_or(run.event_at);
                let quality = value.get("quality").and_then(|q| serde_json::from_value(q.clone()).ok()).unwrap_or(run.quality);
                let title = fill(&json!(title), &value, device).map_or_else(|_| title.clone(), |t| word(&t));
                run.out.effects.push(Effect::Condition {
                    node: id.to_string(),
                    device: device.map(str::to_string),
                    episode: episode.to_string(),
                    phase: phase.to_string(),
                    title,
                    at: seen,
                    value: value.get("value").and_then(number),
                    peak: value.get("peak").and_then(Value::as_f64),
                    quality,
                    raw: run.raw.clone(),
                    unit: run.unit.clone(),
                });
                vec![]
            }
            Kind::Output { name } => {
                run.out.effects.push(Effect::Log { node: id.to_string(), device: device.map(str::to_string), text: format!("{name}: {}", word(&value)) });
                vec![]
            }
            Kind::Log { text } => {
                let said = match text {
                    Some(t) => fill(&json!(t), &value, device).map_or_else(|why| why, |v| word(&v)),
                    None => word(&value),
                };
                run.out.effects.push(Effect::Log { node: id.to_string(), device: device.map(str::to_string), text: said });
                vec![]
            }
            Kind::Command { operation, device: fixed, payload, every, expires } => {
                let device = fixed.as_deref().or(device);
                let Some(m) = run.memory(id, device.unwrap_or_default()) else { return vec![] };
                if m.since.is_some_and(|last| at - last < span(*every)) {
                    return vec![];
                }
                m.since = Some(at);
                match fill(payload, &value, device) {
                    Ok(payload) => run.out.effects.push(Effect::Command {
                        node: id.to_string(),
                        device: device.map(str::to_string),
                        operation: operation.clone(),
                        payload,
                        expires_secs: span(*expires).num_seconds(),
                    }),
                    // Half a command must not go anywhere.
                    Err(why) => run.out.problems.push(format!("{id}: {why}")),
                }
                vec![]
            }
        }
    }

    /// A hold that has lasted: says so once.
    fn due(m: &mut Memory, hold: TimeDelta, at: DateTime<Utc>) -> Leaving {
        match (m.since, m.on) {
            (Some(since), false) if at - since >= hold => {
                m.on = true;
                out_of("out", m.last.clone().unwrap_or(Value::Null))
            }
            _ => vec![],
        }
    }

    /// Follows messages until they have all stopped. A flow has no cycles, so they do, and the
    /// limit is for one so wide that a message would cost too much.
    fn follow(&self, start: Vec<(usize, String, Option<String>, Value)>, run: &mut Run<'_>) {
        let mut queue: VecDeque<_> = start.into();
        let mut steps = 0;
        while let Some((i, input, device, value)) = queue.pop_front() {
            steps += 1;
            if steps > MAX_STEPS {
                run.out.problems.push(format!("one message took more than {MAX_STEPS} steps, and the rest of its work was dropped"));
                return;
            }
            let leaving = self.step(i, &input, device.as_deref(), value.clone(), run);
            let device = match &self.nodes[i].kind {
                Kind::Reassign { to } => match fill(&json!(to), &value, device.as_deref()) {
                    Ok(v) => Some(word(&v)),
                    Err(why) => {
                        run.out.problems.push(format!("{}: {why}", self.nodes[i].id));
                        continue;
                    }
                },
                _ => device,
            };
            self.send_on(i, device, value, leaving, &mut queue, run);
        }
    }

    fn send_on(
        &self,
        i: usize,
        device: Option<String>,
        value: Value,
        leaving: Leaving,
        queue: &mut VecDeque<(usize, String, Option<String>, Value)>,
        run: &mut Run<'_>,
    ) {
        run.out.trace.push(Trace { node: self.nodes[i].id.clone(), device: device.clone(), value, ports: leaving.iter().map(|(p, _)| p.clone()).collect() });
        for (port, v) in leaving {
            for w in self.wires[i].iter().filter(|w| w.port == port) {
                queue.push_back((w.to, w.input.clone(), device.clone(), v.clone()));
            }
        }
    }

    /// Takes in one thing a device did.
    pub fn input(&self, state: &mut State, input: &Input, at: DateTime<Utc>) -> Output {
        self.input_knowing(state, input, at, &Known::default())
    }

    /// The same, for a flow that looks things up.
    pub fn input_knowing(&self, state: &mut State, input: &Input, at: DateTime<Utc>, known: &Known) -> Output {
        if self.critical
            && let Some(seen) = input.observed_at
        {
            let source = format!(
                "{}|{}",
                input.device,
                input.metric.as_deref().or(input.event.as_deref()).or(input.attribute.as_deref()).or(input.state.as_ref().map(|_| "state")).unwrap_or_default()
            );
            let times = state.seen.entry(source).or_default();
            if times.contains(&pipeline::ms(seen)) {
                return Output { duplicate: true, ..Output::default() };
            }
            if times.len() >= MAX_SEEN {
                times.pop_front();
            }
            times.push_back(pipeline::ms(seen));
        }
        let mut run = Run::new(state, at, known);
        run.event_at = input.observed_at.unwrap_or(at);
        run.quality = input.quality;
        let matched = |n: &Node| match &n.kind {
            Kind::Metric { device, metric } => input.metric.as_ref() == Some(metric) && device_matches(device, &input.device),
            Kind::Input { name } => input.metric.as_ref() == Some(name),
            Kind::State { device } => input.state.is_some() && device_matches(device, &input.device),
            Kind::Event { device, name } => input.event.as_ref() == Some(name) && device_matches(device, &input.device),
            Kind::Attribute { device, key } => input.attribute.as_ref() == Some(key) && device_matches(device, &input.device),
            _ => false,
        };
        let value = input.state.as_ref().map_or_else(|| input.value.clone(), |s| json!(s));
        let start =
            self.nodes.iter().enumerate().filter(|(_, n)| matched(n)).map(|(i, _)| (i, "in".to_string(), Some(input.device.clone()), value.clone())).collect();
        self.follow(start, &mut run);
        run.out
    }

    /// Whatever the clock alone can cause: timers, holds that have lasted, silences and delays.
    pub fn tick(&self, state: &mut State, at: DateTime<Utc>) -> Output {
        self.tick_knowing(state, at, &Known::default())
    }

    pub fn tick_knowing(&self, state: &mut State, at: DateTime<Utc>, known: &Known) -> Output {
        let mut run = Run::new(state, at, known);
        let mut queue = VecDeque::new();
        for (i, n) in self.nodes.iter().enumerate() {
            let id = n.id.as_str();
            match &n.kind {
                Kind::Timer { every } => {
                    let Some(m) = run.memory(id, "") else { continue };
                    let due = m.since.is_none_or(|last| at - last >= span(*every));
                    if due {
                        m.since = Some(at);
                        self.send_on(i, None, json!(at), out_of("out", json!(at)), &mut queue, &mut run);
                    }
                }
                Kind::Hold { hold } => {
                    for (device, leaving) in Self::each_key(run.state, id, |m| Self::due(m, span(*hold), at)) {
                        self.send_on(i, device, Value::Null, leaving, &mut queue, &mut run);
                    }
                }
                Kind::Silence { quiet } => {
                    let fire = |m: &mut Memory| match (m.since, m.on) {
                        (Some(since), false) if at - since >= span(*quiet) => {
                            m.on = true;
                            out_of("out", m.last.clone().unwrap_or(Value::Null))
                        }
                        _ => vec![],
                    };
                    for (device, leaving) in Self::each_key(run.state, id, fire) {
                        self.send_on(i, device, Value::Null, leaving, &mut queue, &mut run);
                    }
                }
                Kind::EventWindow { size, stat, wait, lateness, idle, .. } => {
                    let (rule, stat, now) = (window_rule(*size, *wait, *lateness, *idle), *stat, pipeline::ms(at));
                    let due = |m: &mut Memory| m.windows.as_mut().map_or(vec![], |w| said(w.due(now, &rule, &|v| statistic(stat, v))));
                    for (device, leaving) in Self::each_key(run.state, id, due) {
                        self.send_on(i, device, Value::Null, leaving, &mut queue, &mut run);
                    }
                }
                Kind::Episode { above, below, close, on_delay, off_delay, reopen, stale } => {
                    let (rule, now) = (episode_rule(*above, *below, *close, *on_delay, *off_delay, *reopen, *stale), pipeline::ms(at));
                    let due = |m: &mut Memory| {
                        let device = m.device.clone();
                        m.episode.as_mut().map_or(vec![], |e| happened(e.due(now, &rule), device.as_deref()))
                    };
                    for (device, leaving) in Self::each_key(run.state, id, due) {
                        self.send_on(i, device, Value::Null, leaving, &mut queue, &mut run);
                    }
                }
                Kind::Delay { .. } => {
                    let release = |m: &mut Memory| {
                        let mut leaving = Vec::new();
                        while m.held.front().is_some_and(|(when, _)| *when <= at) {
                            if let Some((_, v)) = m.held.pop_front() {
                                leaving.push(("out".to_string(), v));
                            }
                        }
                        leaving
                    };
                    for (device, leaving) in Self::each_key(run.state, id, release) {
                        self.send_on(i, device, Value::Null, leaving, &mut queue, &mut run);
                    }
                }
                _ => {}
            }
        }
        let start: Vec<_> = queue.into();
        self.follow(start, &mut run);
        run.out
    }

    pub fn wants(&self) -> Wants {
        let mut w = Wants::default();
        for n in &self.nodes {
            match &n.kind {
                Kind::Metric { metric, .. } => drop(w.metrics.insert(metric.clone())),
                Kind::State { .. } => w.states = true,
                Kind::Event { name, .. } => drop(w.events.insert(name.clone())),
                Kind::Attribute { key, .. } => drop(w.attributes.insert(key.clone())),
                Kind::Enrich { metrics, attributes, .. } => {
                    w.looked_up_metrics.extend(metrics.iter().cloned());
                    w.looked_up_attributes.extend(attributes.iter().cloned());
                }
                _ => {}
            }
        }
        w
    }

    /// The first step that needs a gateway around it: there is no device to answer, and no queue to
    /// export through, anywhere else.
    pub fn needs_a_gateway(&self) -> Option<&str> {
        self.nodes.iter().find(|n| matches!(n.kind, Kind::Reply { .. } | Kind::Export { .. })).map(|n| n.id.as_str())
    }

    /// Whether it wants readings that cannot be trusted as well, with their quality: a flow that judges
    /// quality, normalizes, keeps episodes or event-time windows, or is business-critical. Any other
    /// hears only what passed its device's quality limits, as before.
    pub fn wants_quality(&self) -> bool {
        self.critical
            || self.nodes.iter().any(|n| matches!(n.kind, Kind::QualityGate { .. } | Kind::Episode { .. } | Kind::Normalize { .. } | Kind::EventWindow { .. }))
    }

    /// The same flow, naming its episodes by `scope`: where it runs, the flow's id.
    pub fn scoped(mut self, scope: impl Into<String>) -> Compiled {
        self.scope = scope.into();
        self
    }

    /// Before another revision takes over by draining this one: every open event window says what it
    /// has, marked partial, and every open episode is said to be interrupted, not closed, since nothing
    /// saw its condition clear. What comes of that is acted on like anything else (FLC-EDGE-010).
    pub fn drain(&self, state: &mut State, at: DateTime<Utc>) -> Output {
        let known = Known::default();
        let mut run = Run::new(state, at, &known);
        let mut queue = VecDeque::new();
        let now = pipeline::ms(at);
        for (i, n) in self.nodes.iter().enumerate() {
            let id = n.id.as_str();
            let leaving: Vec<(Option<String>, Leaving)> = match &n.kind {
                Kind::EventWindow { size, stat, wait, lateness, idle, .. } => {
                    let (rule, stat) = (window_rule(*size, *wait, *lateness, *idle), *stat);
                    Self::each_key(run.state, id, |m| m.windows.as_mut().map_or(vec![], |w| said(w.drain(&rule, &|v| statistic(stat, v)))))
                }
                Kind::Episode { .. } => Self::each_key(run.state, id, |m| {
                    let device = m.device.clone();
                    m.episode.as_mut().map_or(vec![], |e| happened(e.drain(now), device.as_deref()))
                }),
                _ => vec![],
            };
            for (device, out) in leaving {
                self.send_on(i, device, Value::Null, out, &mut queue, &mut run);
            }
        }
        let start: Vec<_> = queue.into();
        self.follow(start, &mut run);
        run.out
    }

    /// What this revision keeps of what an earlier one remembered: a step with the same id and the same
    /// settings keeps its memory, and any other starts with nothing. The plan says which, so a person
    /// chooses knowing it (FLC-EDGE-010).
    pub fn migrate(&self, old: &Compiled, state: &State) -> (State, Vec<Carried>) {
        let mut next = State { nodes: BTreeMap::new(), seen: state.seen.clone() };
        let mut plan = Vec::new();
        for (node, memory) in &state.nodes {
            let before = old.nodes.iter().find(|n| &n.id == node);
            let after = self.nodes.iter().find(|n| &n.id == node);
            let why = match (before, after) {
                (_, None) => Some("it is gone"),
                (Some(b), Some(a)) if b.kind == a.kind => None,
                (Some(_), Some(_)) => Some("its settings changed, so what it remembered may mean something else now"),
                (None, Some(_)) => Some("it is new"),
            };
            if why.is_none() {
                next.nodes.insert(node.clone(), memory.clone());
            }
            plan.push(Carried { node: node.clone(), keys: memory.len(), kept: why.is_none(), why: why.map(str::to_string) });
        }
        (next, plan)
    }

    /// Before any state is at hand: which steps of `old` this revision would let keep their memory, by the
    /// same rule as [`Compiled::migrate`], for the person choosing how it takes over.
    pub fn plan(&self, old: &Compiled) -> Vec<Carried> {
        old.nodes
            .iter()
            .map(|b| {
                let why = match self.nodes.iter().find(|a| a.id == b.id) {
                    None => Some("it is gone"),
                    Some(a) if a.kind == b.kind => None,
                    Some(_) => Some("its settings changed, so what it remembered may mean something else now"),
                };
                Carried { node: b.id.clone(), keys: 0, kept: why.is_none(), why: why.map(str::to_string) }
            })
            .collect()
    }

    fn each_key(state: &mut State, node: &str, mut f: impl FnMut(&mut Memory) -> Leaving) -> Vec<(Option<String>, Leaving)> {
        let Some(keys) = state.nodes.get_mut(node) else { return vec![] };
        keys.values_mut()
            .filter_map(|m| {
                let leaving = f(m);
                (!leaving.is_empty()).then(|| (m.device.clone(), leaving))
            })
            .collect()
    }
}

/// What one step keeps, or not, when a new revision takes over by migrating.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Carried {
    pub node: String,
    /// How many devices it remembered something about.
    pub keys: usize,
    pub kept: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// What a new revision does with what the one before it remembered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    /// It starts with nothing remembered: its steps may mean something else now.
    #[default]
    Reset,
    /// What is open in the old one says what it has first, then the new one starts with nothing.
    Drain,
    /// Steps whose settings did not change keep what they remembered; the rest start with nothing.
    Migrate,
}

/// One line of what a simulation is told happened.
#[derive(Debug, Clone, Deserialize)]
pub struct Scripted {
    /// Seconds after the start.
    #[serde(default)]
    pub at: i64,
    /// Seconds after the start the device saw it, when that is not when it arrived: a late or an
    /// out-of-order reading.
    #[serde(default)]
    pub seen: Option<i64>,
    #[serde(flatten)]
    pub input: Input,
}

/// What happened at one moment of a simulation.
#[derive(Debug, Serialize)]
pub struct Moment {
    pub at: i64,
    #[serde(flatten)]
    pub output: Output,
}

/// Walks a script through a flow, a second at a time, by the code a gateway runs. Only moments
/// where something happened come back. `until` is how long to keep the clock going after the last
/// line, so that a hold can be seen to fire.
pub fn simulate(flow: &Compiled, script: &[Scripted], until: i64) -> Result<Vec<Moment>, String> {
    let last = script.iter().map(|s| s.at).max().unwrap_or(0).max(until);
    if last > MAX_SIMULATED_SECS || script.iter().any(|s| s.at < 0) {
        return Err(format!("a simulation covers up to {MAX_SIMULATED_SECS} seconds"));
    }
    let now = Utc::now();
    // Event-time windows are aligned on the clock: simulating one from midnight puts a script's seconds
    // in the same windows each time, so what a reviewer saw does not change with when they looked.
    let windows = flow.nodes.iter().any(|n| matches!(n.kind, Kind::EventWindow { .. }));
    let start = if windows { now.date_naive().and_time(chrono::NaiveTime::MIN).and_utc() } else { now };
    let mut state = State::default();
    let mut known = Known::default();
    let mut moments = Vec::new();
    for second in 0..=last {
        let at = start + TimeDelta::seconds(second);
        let mut merged = flow.tick_knowing(&mut state, at, &known);
        for line in script.iter().filter(|s| s.at == second) {
            let mut input = line.input.clone();
            if let Some(seen) = line.seen {
                input.observed_at = Some(start + TimeDelta::seconds(seen));
            }
            known.note(&input);
            let more = flow.input_knowing(&mut state, &input, at, &known);
            merged.effects.extend(more.effects);
            merged.trace.extend(more.trace);
            merged.problems.extend(more.problems);
            merged.duplicate |= more.duplicate;
        }
        if merged != Output::default() {
            moments.push(Moment { at: second, output: merged });
        }
    }
    Ok(moments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(body: Value) -> Compiled {
        parse(&body.to_string()).unwrap_or_else(|e| panic!("{e}"))
    }

    fn metric(device: &str, value: f64) -> Input {
        Input { device: device.into(), metric: Some("temp".into()), value: json!(value), ..Default::default() }
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    /// Hot for thirty seconds raises an alarm, and cooling clears it, for each device on its own.
    fn hot() -> Compiled {
        flow(json!({
            "nodes": [
                {"id": "t", "kind": "metric", "metric": "temp", "x": 10, "y": 20, "label": "Temperature"},
                {"id": "c", "kind": "compare", "op": ">", "value": 80},
                {"id": "h", "kind": "hold", "for": "30s"},
                {"id": "a", "kind": "alarm", "name": "Running hot", "severity": "Critical"},
            ],
            "edges": [
                {"from": "t", "to": "c"},
                {"from": "c", "port": "yes", "to": "h"},
                {"from": "c", "port": "no", "to": "h", "input": "reset"},
                {"from": "c", "port": "no", "to": "a", "input": "clear"},
                {"from": "h", "to": "a"},
            ],
        }))
    }

    fn raised(o: &Output) -> Vec<(Option<&str>, bool)> {
        o.effects.iter().filter_map(|e| if let Effect::Alarm { device, raised, .. } = e { Some((device.as_deref(), *raised)) } else { None }).collect()
    }

    #[test]
    fn a_hold_waits_and_a_dip_starts_it_again() {
        let f = hot();
        let mut s = State::default();
        assert!(raised(&f.input(&mut s, &metric("P1", 90.0), at(0))).is_empty());
        assert!(raised(&f.tick(&mut s, at(29))).is_empty(), "not yet");
        assert!(raised(&f.input(&mut s, &metric("P1", 70.0), at(29))).is_empty(), "a dip is not an alarm's clearing when none is raised");
        assert!(raised(&f.input(&mut s, &metric("P1", 90.0), at(30))).is_empty());
        assert!(raised(&f.tick(&mut s, at(59))).is_empty(), "the dip started it again");
        assert_eq!(raised(&f.tick(&mut s, at(60))), [(Some("P1"), true)], "the clock alone can fire it");
        assert!(raised(&f.tick(&mut s, at(61))).is_empty(), "and it says so once");
        assert!(raised(&f.input(&mut s, &metric("P1", 95.0), at(62))).is_empty(), "however long it stays hot");
        assert_eq!(raised(&f.input(&mut s, &metric("P1", 60.0), at(63))), [(Some("P1"), false)]);
        assert!(raised(&f.input(&mut s, &metric("P1", 60.0), at(64))).is_empty(), "and clears once");
    }

    #[test]
    fn each_device_is_watched_on_its_own() {
        let f = hot();
        let mut s = State::default();
        f.input(&mut s, &metric("P1", 90.0), at(0));
        f.input(&mut s, &metric("P2", 90.0), at(20));
        assert_eq!(raised(&f.tick(&mut s, at(30))), [(Some("P1"), true)]);
        assert_eq!(raised(&f.tick(&mut s, at(50))), [(Some("P2"), true)]);
        assert_eq!(raised(&f.input(&mut s, &metric("P1", 10.0), at(51))), [(Some("P1"), false)], "one cooling says nothing of the other");
    }

    #[test]
    fn a_restart_does_not_lose_its_place() {
        let f = hot();
        let mut s = State::default();
        f.input(&mut s, &metric("P1", 90.0), at(0));
        let saved = serde_json::to_string(&s).unwrap();
        let mut back: State = serde_json::from_str(&saved).unwrap();
        assert_eq!(raised(&f.tick(&mut back, at(30))), [(Some("P1"), true)], "the hold kept counting");
        let saved = serde_json::to_string(&back).unwrap();
        let mut again: State = serde_json::from_str(&saved).unwrap();
        assert!(raised(&f.input(&mut again, &metric("P1", 95.0), at(31))).is_empty(), "and a raised alarm is not raised twice");
        assert_eq!(raised(&f.input(&mut again, &metric("P1", 10.0), at(32))), [(Some("P1"), false)]);
    }

    #[test]
    fn a_flow_that_could_not_work_is_refused_and_says_where() {
        let source = json!({"id": "s", "kind": "metric", "metric": "t"});
        let action = json!({"id": "e", "kind": "record_event", "name": "x"});
        let fails = |body: Value, why: &str| {
            let e = parse(&body.to_string()).err().unwrap_or_else(|| panic!("{body} passed"));
            assert!(e.contains(why), "{e}");
        };
        fails(
            json!({"nodes": [source, action, {"id": "a", "kind": "limit", "every": "1s"}, {"id": "b", "kind": "limit", "every": "1s"}],
            "edges": [{"from": "s", "to": "a"}, {"from": "a", "to": "b"}, {"from": "b", "to": "a"}, {"from": "b", "to": "e"}]}),
            "circle",
        );
        fails(
            json!({"nodes": [source, action, {"id": "a", "kind": "limit", "every": "1s"}], "edges": [{"from": "s", "to": "a"}, {"from": "a", "to": "a"}, {"from": "a", "to": "e"}]}),
            "circle",
        );
        fails(
            json!({"nodes": [source, action, {"id": "n", "kind": "limit", "every": "1s"}], "edges": [{"from": "s", "to": "e"}, {"from": "n", "to": "e"}]}),
            "nothing reaches",
        );
        fails(json!({"nodes": [source], "edges": []}), "nothing comes of it");
        fails(json!({"nodes": [source, action], "edges": [{"from": "s", "to": "gone"}]}), "not here");
        fails(json!({"nodes": [source, action], "edges": [{"from": "s", "port": "yes", "to": "e"}]}), "no way out");
        fails(json!({"nodes": [source, action], "edges": [{"from": "s", "to": "e", "input": "side"}]}), "takes nothing");
        fails(json!({"nodes": [source, source, action], "edges": []}), "twice");
        fails(json!({"nodes": [{"id": "x", "kind": "run_script", "code": "rm -rf /"}]}), "x:");
        fails(json!({"nodes": [{"id": "x", "kind": "limit", "every": "soon"}]}), "not a duration");
        fails(
            json!({"nodes": [source, action, {"id": "h", "kind": "hold", "for": "1s"}], "edges": [{"from": "s", "to": "h"}, {"from": "h", "to": "e"}]}),
            "resets",
        );
        fails(json!({"nodes": [source, {"id": "a", "kind": "alarm", "name": "x"}], "edges": [{"from": "s", "to": "a"}]}), "clears");
        fails(
            json!({"nodes": [source, {"id": "m", "kind": "math", "formula": "[value] +"}, action], "edges": [{"from": "s", "to": "m"}, {"from": "m", "to": "e"}]}),
            "m:",
        );
        assert!(parse(&json!({"nodes": [source, action], "edges": [{"from": "s", "to": "e"}]}).to_string()).is_ok());
    }

    #[test]
    fn logic_does_what_it_says() {
        let one = |kind: Value, inputs: &[f64]| {
            let mut node = kind;
            node["id"] = json!("n");
            let f = flow(json!({"nodes": [{"id": "s", "kind": "metric", "metric": "temp"}, node, {"id": "e", "kind": "record_metric", "name": "o"}],
                "edges": [{"from": "s", "to": "n"}, {"from": "n", "to": "e"}]}));
            let mut s = State::default();
            let mut got = Vec::new();
            for (i, v) in inputs.iter().enumerate() {
                for e in f.input(&mut s, &metric("P", *v), at(i as i64 * 10)).effects {
                    if let Effect::Metric { value, .. } = e {
                        got.push(value);
                    }
                }
            }
            got
        };
        assert_eq!(one(json!({"kind": "changed"}), &[1.0, 1.0, 2.0, 2.0]), [1.0, 2.0]);
        assert_eq!(one(json!({"kind": "changed", "by": 1.0}), &[1.0, 1.5, 2.0, 2.4]), [1.0, 2.0], "from what last passed, so a slow creep is still seen");
        assert_eq!(one(json!({"kind": "limit", "every": "25s"}), &[1.0, 2.0, 3.0, 4.0]), [1.0, 4.0]);
        assert_eq!(one(json!({"kind": "count", "of": 3, "within": "25s"}), &[1.0, 1.0, 1.0, 1.0, 1.0, 1.0]), [3.0, 3.0]);
        assert_eq!(one(json!({"kind": "count", "of": 3, "within": "15s"}), &[1.0, 1.0, 1.0, 1.0]), Vec::<f64>::new(), "too far apart");
        assert_eq!(one(json!({"kind": "window", "over": "15s", "stat": "avg"}), &[10.0, 20.0, 60.0]), [10.0, 15.0, 40.0], "the oldest has left by the third");
        assert_eq!(one(json!({"kind": "window", "over": "1m", "stat": "max"}), &[1.0, 3.0, 2.0]), [1.0, 3.0, 3.0]);
        assert_eq!(one(json!({"kind": "window", "over": "1m", "stat": "delta"}), &[5.0, 7.0, 4.0]), [0.0, 2.0, -1.0]);
        assert_eq!(one(json!({"kind": "window", "over": "1m", "stat": "count"}), &[5.0, 7.0]), [1.0, 2.0]);
        assert_eq!(one(json!({"kind": "math", "formula": "round([value] * 1.8 + 32, 1)"}), &[100.0]), [212.0]);
        assert_eq!(one(json!({"kind": "math", "formula": "[value] / 0"}), &[1.0]), Vec::<f64>::new(), "no answer is no message");
        assert_eq!(one(json!({"kind": "map", "map": {"1": 100, "2": 200}}), &[1.0, 2.0, 3.0]), [100.0, 200.0]);
        assert_eq!(one(json!({"kind": "map", "map": {"1": 100}, "other": 0}), &[3.0]), [0.0]);
        assert_eq!(one(json!({"kind": "set", "value": "{value}"}), &[7.0]), [7.0]);
    }

    #[test]
    fn numbers_and_words_compare_as_what_they_are() {
        assert_eq!(compares(Comparison::Above, &json!(9), &json!("8.5")), Some(true));
        assert_eq!(compares(Comparison::Is, &json!("Running"), &json!("Running")), Some(true));
        assert_eq!(compares(Comparison::IsNot, &json!("Running"), &json!("Idle")), Some(true));
        assert_eq!(compares(Comparison::Above, &json!("Running"), &json!(5)), None, "neither way: it says nothing");
    }

    #[test]
    fn inputs_combine_when_all_are_there_and_fresh() {
        let f = flow(json!({
            "nodes": [
                {"id": "v", "kind": "metric", "metric": "volts"},
                {"id": "a", "kind": "metric", "metric": "amps"},
                {"id": "k", "kind": "combine", "formula": "[v] * [a] / 1000", "fresh": "10s"},
                {"id": "e", "kind": "record_metric", "name": "kw", "unit": "kW"},
            ],
            "edges": [{"from": "v", "to": "k", "input": "v"}, {"from": "a", "to": "k", "input": "a"}, {"from": "k", "to": "e"}],
        }));
        let reading = |metric: &str, value: f64| Input { device: "M".into(), metric: Some(metric.into()), value: json!(value), ..Default::default() };
        let mut s = State::default();
        assert!(f.input(&mut s, &reading("volts", 400.0), at(0)).effects.is_empty(), "amps have not been heard");
        let o = f.input(&mut s, &reading("amps", 5.0), at(5));
        assert_eq!(o.effects, [Effect::Metric { device: Some("M".into()), name: "kw".into(), value: 2.0, unit: Some("kW".into()) }]);
        assert!(f.input(&mut s, &reading("amps", 6.0), at(20)).effects.is_empty(), "the volts are stale now");
        assert_eq!(f.input(&mut s, &reading("volts", 400.0), at(21)).effects.len(), 1);
    }

    #[test]
    fn a_gate_says_each_change_once() {
        let f = flow(json!({
            "nodes": [
                {"id": "a", "kind": "metric", "metric": "a"},
                {"id": "b", "kind": "metric", "metric": "b"},
                {"id": "g", "kind": "all"},
                {"id": "y", "kind": "record_event", "name": "both"},
                {"id": "n", "kind": "record_event", "name": "not"},
            ],
            "edges": [{"from": "a", "to": "g", "input": "a"}, {"from": "b", "to": "g", "input": "b"}, {"from": "g", "port": "yes", "to": "y"}, {"from": "g", "port": "no", "to": "n"}],
        }));
        let m = |metric: &str, value: f64| Input { device: "M".into(), metric: Some(metric.into()), value: json!(value), ..Default::default() };
        let named = |o: Output| o.effects.iter().filter_map(|e| if let Effect::Event { name, .. } = e { Some(name.clone()) } else { None }).collect::<Vec<_>>();
        let mut s = State::default();
        assert_eq!(named(f.input(&mut s, &m("a", 1.0), at(0))), ["not"]);
        assert_eq!(named(f.input(&mut s, &m("a", 1.0), at(1))), Vec::<String>::new());
        assert_eq!(named(f.input(&mut s, &m("b", 1.0), at(2))), ["both"]);
        assert_eq!(named(f.input(&mut s, &m("b", 2.0), at(3))), Vec::<String>::new());
        assert_eq!(named(f.input(&mut s, &m("a", 0.0), at(4))), ["not"]);
    }

    #[test]
    fn a_source_listens_only_for_what_it_names() {
        let f = flow(
            json!({"nodes": [{"id": "s", "kind": "metric", "metric": "temp", "device": "PRESS-*"}, {"id": "e", "kind": "record_event", "name": "x"}], "edges": [{"from": "s", "to": "e"}]}),
        );
        let mut s = State::default();
        assert_eq!(f.input(&mut s, &metric("PRESS-1", 1.0), at(0)).effects.len(), 1);
        assert!(f.input(&mut s, &metric("OVEN-1", 1.0), at(0)).effects.is_empty());
        assert!(f.input(&mut s, &Input { device: "PRESS-1".into(), metric: Some("rpm".into()), ..Default::default() }, at(0)).effects.is_empty());
    }

    #[test]
    fn silence_and_a_delay_are_noticed_by_the_clock() {
        let f = flow(json!({
            "nodes": [
                {"id": "s", "kind": "metric", "metric": "temp"},
                {"id": "q", "kind": "silence", "for": "1m"},
                {"id": "d", "kind": "delay", "for": "10s"},
                {"id": "e", "kind": "record_event", "name": "quiet"},
                {"id": "l", "kind": "record_event", "name": "late"},
            ],
            "edges": [{"from": "s", "to": "q"}, {"from": "s", "to": "d"}, {"from": "q", "to": "e"}, {"from": "d", "to": "l"}],
        }));
        let mut s = State::default();
        f.input(&mut s, &metric("M", 1.0), at(0));
        assert!(f.tick(&mut s, at(9)).effects.is_empty());
        let late = f.tick(&mut s, at(10));
        assert_eq!(late.effects, [Effect::Event { device: Some("M".into()), name: "late".into(), value: json!(1.0) }]);
        assert!(f.tick(&mut s, at(59)).effects.is_empty());
        assert_eq!(f.tick(&mut s, at(60)).effects.len(), 1);
        assert!(f.tick(&mut s, at(120)).effects.is_empty(), "once");
        f.input(&mut s, &metric("M", 1.0), at(121));
        assert_eq!(
            f.tick(&mut s, at(181)).effects.iter().filter(|e| matches!(e, Effect::Event { name, .. } if name == "quiet")).count(),
            1,
            "and again after it spoke"
        );
    }

    #[test]
    fn a_timer_fires_on_time_and_its_work_has_no_device() {
        let f = flow(
            json!({"nodes": [{"id": "t", "kind": "timer", "every": "1m"}, {"id": "e", "kind": "record_event", "name": "tick"}], "edges": [{"from": "t", "to": "e"}]}),
        );
        let mut s = State::default();
        assert_eq!(f.tick(&mut s, at(0)).effects.len(), 1);
        assert!(f.tick(&mut s, at(59)).effects.is_empty());
        assert_eq!(f.tick(&mut s, at(60)).effects.len(), 1);
    }

    #[test]
    fn a_command_is_asked_for_rarely_and_only_when_whole() {
        let f = flow(json!({
            "nodes": [
                {"id": "s", "kind": "metric", "metric": "temp"},
                {"id": "c", "kind": "command", "operation": "fan.start", "payload": {"speed": "{value}", "for": "{device}"}, "every": "1m", "expires": "2m"},
            ],
            "edges": [{"from": "s", "to": "c"}],
        }));
        let mut s = State::default();
        let first = f.input(&mut s, &metric("M", 3.0), at(0));
        assert_eq!(
            first.effects,
            [Effect::Command {
                node: "c".into(),
                device: Some("M".into()),
                operation: "fan.start".into(),
                payload: json!({"speed": 3.0, "for": "M"}),
                expires_secs: 120
            }]
        );
        assert!(f.input(&mut s, &metric("M", 4.0), at(30)).effects.is_empty(), "a chatty sensor cannot flood the ledger");
        assert_eq!(f.input(&mut s, &metric("N", 4.0), at(30)).effects.len(), 1, "each device has its own");
        assert_eq!(f.input(&mut s, &metric("M", 5.0), at(60)).effects.len(), 1);

        let broken = flow(
            json!({"nodes": [{"id": "t", "kind": "timer", "every": "1s"}, {"id": "c", "kind": "command", "operation": "x", "device": "M", "payload": {"a": "{nope.b}"}}], "edges": [{"from": "t", "to": "c"}]}),
        );
        let mut s = State::default();
        let o = broken.tick(&mut s, at(0));
        assert!(o.effects.is_empty(), "half a command goes nowhere");
        assert!(o.problems.iter().any(|p| p.contains("nope")), "{:?}", o.problems);
    }

    #[test]
    fn nothing_keeps_more_than_it_should() {
        let f = flow(
            json!({"nodes": [{"id": "s", "kind": "metric", "metric": "temp"}, {"id": "w", "kind": "changed"}, {"id": "e", "kind": "record_event", "name": "x"}], "edges": [{"from": "s", "to": "w"}, {"from": "w", "to": "e"}]}),
        );
        let mut s = State::default();
        let mut refused = 0;
        for i in 0..MAX_KEYS + 10 {
            refused += f.input(&mut s, &metric(&format!("D{i}"), 1.0), at(0)).problems.len();
        }
        assert_eq!(refused, 10, "a device that makes up names costs this much");
        assert_eq!(s.nodes["w"].len(), MAX_KEYS);
    }

    /// A flow of one step between a source and something to see what comes out, by port.
    fn through(node: Value, ports: &[&str]) -> Compiled {
        let mut n = node.as_object().cloned().unwrap();
        n.insert("id".into(), json!("step"));
        let mut nodes = vec![json!({"id": "in", "kind": "metric", "metric": "m"}), Value::Object(n)];
        let mut edges = vec![json!({"from": "in", "to": "step"})];
        for port in ports {
            nodes.push(json!({"id": format!("to-{port}"), "kind": "record_event", "name": port}));
            edges.push(json!({"from": "step", "port": port, "to": format!("to-{port}")}));
        }
        flow(json!({"nodes": nodes, "edges": edges}))
    }

    /// What a value becomes: which ports it left by, and with what.
    fn run_through(f: &Compiled, state: &mut State, device: &str, value: Value, at_secs: i64) -> Vec<(String, Value)> {
        let o = f.input(state, &Input { device: device.into(), metric: Some("m".into()), value, ..Default::default() }, at(at_secs));
        events_of(&o)
    }

    fn events_of(o: &Output) -> Vec<(String, Value)> {
        o.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Event { name, value, .. } => Some((name.clone(), value.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_value_can_be_asked_what_it_has() {
        let f = through(json!({"kind": "has", "paths": ["$.a", "$.b.c"]}), &["yes", "no"]);
        let mut s = State::default();
        assert_eq!(run_through(&f, &mut s, "A", json!({"a": 1, "b": {"c": 2}}), 0)[0].0, "yes");
        assert_eq!(run_through(&f, &mut s, "A", json!({"a": 1}), 1)[0].0, "no");
        assert_eq!(run_through(&f, &mut s, "A", json!({"a": 1, "b": {"c": null}}), 2)[0].0, "no", "nothing is not something");
    }

    #[test]
    fn a_counter_says_how_far_it_went() {
        let f = through(json!({"kind": "delta"}), &["out"]);
        let mut s = State::default();
        assert!(run_through(&f, &mut s, "A", json!(100), 0).is_empty(), "the first says nothing");
        assert_eq!(run_through(&f, &mut s, "A", json!(130), 10)[0].1, json!(30.0));
        assert!(run_through(&f, &mut s, "A", json!(5), 20).is_empty(), "it was reset, which is not minus 125");
        assert_eq!(run_through(&f, &mut s, "A", json!(15), 30)[0].1, json!(10.0), "and it carries on from there");
        assert_eq!(run_through(&f, &mut s, "B", json!(1), 30), vec![], "each device its own");
    }

    #[test]
    fn a_counter_can_be_a_rate() {
        let f = through(json!({"kind": "delta", "per": "1m"}), &["out"]);
        let mut s = State::default();
        run_through(&f, &mut s, "A", json!(0), 0);
        assert_eq!(run_through(&f, &mut s, "A", json!(5), 30)[0].1, json!(10.0), "five in half a minute is ten a minute");
    }

    #[test]
    fn a_place_can_be_a_polygon_or_a_circle() {
        let square = through(json!({"kind": "geofence", "polygon": [[0, 0], [0, 10], [10, 10], [10, 0]]}), &["inside", "outside", "entered", "left"]);
        let mut s = State::default();
        let here = |lat: f64, lon: f64| json!({"lat": lat, "lon": lon});
        let ports = |out: Vec<(String, Value)>| out.into_iter().map(|(p, _)| p).collect::<Vec<_>>();
        assert_eq!(ports(run_through(&square, &mut s, "A", here(5.0, 5.0), 0)), ["inside"], "the first position is not an arrival");
        assert_eq!(ports(run_through(&square, &mut s, "A", here(6.0, 6.0), 1)), ["inside"]);
        assert_eq!(ports(run_through(&square, &mut s, "A", here(20.0, 5.0), 2)), ["outside", "left"]);
        assert_eq!(ports(run_through(&square, &mut s, "A", here(5.0, 20.0), 3)), ["outside"]);
        assert_eq!(ports(run_through(&square, &mut s, "A", here(1.0, 1.0), 4)), ["inside", "entered"]);
        let circle = through(json!({"kind": "geofence", "circle": {"lat": 51.5, "lon": -0.12, "radius_m": 1000}}), &["inside", "outside"]);
        let mut s = State::default();
        assert_eq!(ports(run_through(&circle, &mut s, "A", here(51.505, -0.12), 0)), ["inside"], "about 550 metres");
        assert_eq!(ports(run_through(&circle, &mut s, "A", here(51.52, -0.12), 1)), ["outside"], "about 2.2 km");
    }

    #[test]
    fn a_position_that_is_not_one_is_said_to_be_wrong() {
        let f = through(json!({"kind": "geofence", "circle": {"lat": 0, "lon": 0, "radius_m": 10}}), &["inside"]);
        let mut s = State::default();
        let o = f.input(&mut s, &Input { device: "A".into(), metric: Some("m".into()), value: json!(7), ..Default::default() }, at(0));
        assert_eq!(o.problems.len(), 1);
    }

    #[test]
    fn the_same_thing_twice_counts_once_for_a_while() {
        let f = through(json!({"kind": "dedupe", "key": "$.id", "within": "1m"}), &["out"]);
        let mut s = State::default();
        assert_eq!(run_through(&f, &mut s, "A", json!({"id": 1, "n": "first"}), 0).len(), 1);
        assert!(run_through(&f, &mut s, "A", json!({"id": 1, "n": "again"}), 30).is_empty());
        assert_eq!(run_through(&f, &mut s, "A", json!({"id": 2}), 31).len(), 1, "another is another");
        assert_eq!(run_through(&f, &mut s, "A", json!({"id": 1}), 61).len(), 1, "and after a minute it is news again");
    }

    #[test]
    fn a_list_becomes_a_message_each() {
        let f = through(json!({"kind": "split", "path": "$.items"}), &["out"]);
        let mut s = State::default();
        let out = run_through(&f, &mut s, "A", json!({"items": [1, 2, 3]}), 0);
        assert_eq!(out.iter().map(|o| o.1.clone()).collect::<Vec<_>>(), [json!(1), json!(2), json!(3)]);
        assert!(run_through(&f, &mut s, "A", json!({"items": 5}), 1).is_empty());
    }

    #[test]
    fn parts_can_be_renamed_and_left_out() {
        let f = through(json!({"kind": "keys", "rename": {"t": "temperature"}, "drop": ["secret"]}), &["out"]);
        let mut s = State::default();
        assert_eq!(run_through(&f, &mut s, "A", json!({"t": 1, "h": 2, "secret": 3}), 0)[0].1, json!({"temperature": 1, "h": 2}));
        let f = through(json!({"kind": "keys", "keep": ["a"]}), &["out"]);
        assert_eq!(run_through(&f, &mut s, "A", json!({"a": 1, "b": 2}), 0)[0].1, json!({"a": 1}));
    }

    #[test]
    fn a_flow_can_look_up_what_is_known() {
        let f = through(json!({"kind": "enrich", "metrics": ["humidity"], "attributes": ["firmware", "never"]}), &["out"]);
        let mut known = Known::default();
        known.note(&Input { device: "A".into(), metric: Some("humidity".into()), value: json!(40), ..Default::default() });
        known.note(&Input { device: "A".into(), attribute: Some("firmware".into()), value: json!("1.2"), ..Default::default() });
        known.note(&Input { device: "B".into(), metric: Some("humidity".into()), value: json!(99), ..Default::default() });
        let mut s = State::default();
        let o = f.input_knowing(&mut s, &Input { device: "A".into(), metric: Some("m".into()), value: json!(21), ..Default::default() }, at(0), &known);
        assert_eq!(events_of(&o)[0].1, json!({"value": 21, "humidity": 40, "firmware": "1.2"}), "and its own, not another's");
    }

    #[test]
    fn a_schedule_knows_what_day_it_is() {
        // 2026-11-02 is a Monday, and these are UTC.
        let monday = |h: u32, m: u32| chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap().and_hms_opt(h, m, 0).unwrap().and_utc();
        assert!(scheduled(monday(9, 0), "08:00", "18:00", &[1, 2, 3, 4, 5], 0));
        assert!(!scheduled(monday(9, 0), "08:00", "18:00", &[6, 7], 0), "not at the weekend");
        assert!(!scheduled(monday(18, 0), "08:00", "18:00", &[], 0), "until means until");
        assert!(scheduled(monday(23, 0), "22:00", "06:00", &[], 0), "over midnight");
        assert!(scheduled(monday(5, 0), "22:00", "06:00", &[7], 0), "and that belongs to Sunday");
        assert!(!scheduled(monday(5, 0), "22:00", "06:00", &[1], 0), "not Monday");
        assert!(scheduled(monday(4, 0), "09:00", "18:00", &[], 330), "at 09:30 in India");
    }

    #[test]
    fn something_unlike_the_rest_stands_out() {
        let f = through(json!({"kind": "anomaly", "window": 50, "sigma": 3}), &["normal", "odd"]);
        let mut s = State::default();
        for i in 0..30 {
            let v = 20.0 + f64::from(i % 3);
            assert_eq!(run_through(&f, &mut s, "A", json!(v), i64::from(i))[0].0, "normal", "reading {i}");
        }
        assert_eq!(run_through(&f, &mut s, "A", json!(21.5), 30)[0].0, "normal");
        assert_eq!(run_through(&f, &mut s, "A", json!(90), 31)[0].0, "odd");
        assert_eq!(run_through(&f, &mut s, "A", json!(91), 32)[0].0, "odd", "and one fault does not teach it that faults are normal");
        assert_eq!(run_through(&f, &mut s, "B", json!(90), 33)[0].0, "normal", "another device has its own idea of normal");
    }

    #[test]
    fn a_steady_reading_has_no_room_for_doubt() {
        let f = through(json!({"kind": "anomaly"}), &["normal", "odd"]);
        let mut s = State::default();
        for i in 0..12 {
            run_through(&f, &mut s, "A", json!(5), i);
        }
        assert_eq!(run_through(&f, &mut s, "A", json!(5.01), 20)[0].0, "odd");
    }

    #[test]
    fn a_message_can_become_another_devices() {
        let f = flow(json!({
            "nodes": [
                {"id": "in", "kind": "metric", "metric": "m"},
                {"id": "line", "kind": "reassign", "to": "LINE-{device}"},
                {"id": "total", "kind": "record_metric", "name": "seen"},
            ],
            "edges": [{"from": "in", "to": "line"}, {"from": "line", "to": "total"}],
        }));
        let o = f.input(&mut State::default(), &Input { device: "4".into(), metric: Some("m".into()), value: json!(7), ..Default::default() }, at(0));
        assert_eq!(o.effects, vec![Effect::Metric { device: Some("LINE-4".into()), name: "seen".into(), value: 7.0, unit: None }]);
    }

    #[test]
    fn a_flow_says_what_it_needs() {
        let f = flow(json!({
            "nodes": [
                {"id": "a", "kind": "metric", "device": "PRESS-*", "metric": "temp"},
                {"id": "b", "kind": "event", "name": "door"},
                {"id": "c", "kind": "state"},
                {"id": "d", "kind": "attribute", "key": "firmware"},
                {"id": "e", "kind": "enrich", "metrics": ["humidity"], "attributes": ["site"]},
                {"id": "f", "kind": "log"},
            ],
            "edges": [{"from": "a", "to": "e"}, {"from": "b", "to": "e"}, {"from": "c", "to": "e"}, {"from": "d", "to": "e"}, {"from": "e", "to": "f"}],
        }));
        let w = f.wants();
        assert!(w.states && w.metrics.contains("temp") && w.events.contains("door") && w.attributes.contains("firmware"));
        assert!(w.looked_up_metrics.contains("humidity") && w.looked_up_attributes.contains("site"));
        assert_eq!(f.needs_a_gateway(), None);
        let f = through(json!({"kind": "reply"}), &[]);
        assert_eq!(f.needs_a_gateway(), Some("step"));
    }

    #[test]
    fn a_device_asking_something_can_be_answered() {
        let f = flow(json!({
            "nodes": [
                {"id": "asked", "kind": "event", "name": "rpc.request"},
                {"id": "which", "kind": "switch", "on": "$.method", "cases": ["time"]},
                {"id": "answer", "kind": "reply", "with": {"said": "{value.params.a}"}},
            ],
            "edges": [{"from": "asked", "to": "which"}, {"from": "which", "port": "time", "to": "answer"}],
        }));
        let mut s = State::default();
        let asked = |method: &str| Input {
            device: "A".into(),
            event: Some("rpc.request".into()),
            value: json!({"id": "7", "method": method, "params": {"a": 1}}),
            ..Default::default()
        };
        let o = f.input(&mut s, &asked("time"), at(0));
        assert_eq!(o.effects, vec![Effect::Reply { device: Some("A".into()), id: "7".into(), value: json!({"said": 1}) }]);
        assert!(f.input(&mut s, &asked("other"), at(1)).effects.is_empty(), "only what it was told to answer");
    }

    #[test]
    fn across_devices_a_window_takes_them_all() {
        let f = through(json!({"kind": "window", "over": "1m", "stat": "sum", "across": true}), &["out"]);
        let mut s = State::default();
        run_through(&f, &mut s, "A", json!(1), 0);
        assert_eq!(run_through(&f, &mut s, "B", json!(2), 1)[0].1, json!(3.0));
    }

    #[test]
    fn what_cannot_work_is_refused_before_it_runs() {
        let bad = [
            (json!({"kind": "geofence"}), "polygon"),
            (json!({"kind": "geofence", "polygon": [[0, 0], [1, 1]]}), "polygon"),
            (json!({"kind": "geofence", "circle": {"lat": 0, "lon": 0, "radius_m": 0}}), "radius"),
            (json!({"kind": "schedule", "from": "8am", "to": "18:00"}), "08:30"),
            (json!({"kind": "schedule", "from": "08:00", "to": "25:00"}), "08:30"),
            (json!({"kind": "schedule", "from": "08:00", "to": "18:00", "days": [8]}), "Monday"),
            (json!({"kind": "anomaly", "window": 2}), "readings"),
            (json!({"kind": "anomaly", "sigma": 0}), "deviations"),
            (json!({"kind": "has", "paths": []}), "which parts"),
            (json!({"kind": "reassign", "to": ""}), "whose"),
        ];
        for (mut node, why) in bad {
            node["id"] = json!("step");
            let body = json!({"nodes": [{"id": "in", "kind": "metric", "metric": "m"}, node, {"id": "out", "kind": "log"}], "edges": [{"from": "in", "to": "step"}, {"from": "step", "port": "yes", "to": "out"}]});
            let said = parse(&body.to_string()).err().unwrap_or_default();
            let port = body["nodes"][1]["kind"].as_str().unwrap();
            let again = if said.is_empty() || said.contains("no way out") {
                let port = match port {
                    "geofence" => "inside",
                    "anomaly" => "odd",
                    "reassign" | "schedule" | "has" => "out",
                    _ => "out",
                };
                let mut b = body.clone();
                b["edges"][1]["port"] = json!(port);
                parse(&b.to_string()).err().unwrap_or_default()
            } else {
                said
            };
            assert!(again.contains(why), "{node}: {again}");
        }
    }

    #[test]
    fn a_wide_flow_is_cut_off_and_says_so() {
        // Eight rows of ten: 10^8 paths from one message, in a graph with no cycle.
        let mut nodes = vec![json!({"id": "s", "kind": "metric", "metric": "temp"})];
        let mut edges = Vec::new();
        for row in 0..8 {
            for col in 0..10 {
                nodes.push(json!({"id": format!("n{row}.{col}"), "kind": "set", "value": 1}));
                if row == 0 {
                    edges.push(json!({"from": "s", "to": format!("n0.{col}")}));
                } else {
                    for prev in 0..10 {
                        edges.push(json!({"from": format!("n{}.{prev}", row - 1), "to": format!("n{row}.{col}")}));
                    }
                }
            }
        }
        nodes.push(json!({"id": "e", "kind": "record_event", "name": "x"}));
        for col in 0..10 {
            edges.push(json!({"from": format!("n7.{col}"), "to": "e"}));
        }
        let f = flow(json!({"nodes": nodes, "edges": edges}));
        let o = f.input(&mut State::default(), &metric("M", 1.0), at(0));
        assert!(o.problems.iter().any(|p| p.contains("steps")), "{:?}", o.problems);
    }

    #[test]
    fn a_simulation_shows_what_would_happen_and_when() {
        let script: Vec<Scripted> = serde_json::from_value(json!([
            {"at": 0, "device": "P1", "metric": "temp", "value": 90},
            {"at": 40, "device": "P1", "metric": "temp", "value": 60},
        ]))
        .unwrap();
        let moments = simulate(&hot(), &script, 60).unwrap();
        let alarms: Vec<(i64, bool)> = moments.iter().flat_map(|m| raised(&m.output).into_iter().map(|(_, r)| (m.at, r)).collect::<Vec<_>>()).collect();
        assert_eq!(alarms, [(30, true), (40, false)]);
        let first = moments.iter().find(|m| m.at == 0).unwrap();
        assert_eq!(first.output.trace.iter().map(|t| t.node.as_str()).collect::<Vec<_>>(), ["t", "c", "h"], "and the way each message went");
        assert_eq!(first.output.trace[1].ports, ["yes"]);
        assert!(simulate(&hot(), &script, MAX_SIMULATED_SECS + 1).is_err());
        let negative: Vec<Scripted> = serde_json::from_value(json!([{"at": -1, "device": "P", "metric": "temp"}])).unwrap();
        assert!(simulate(&hot(), &negative, 1).is_err());
    }

    #[test]
    fn yaml_is_as_good_as_json() {
        assert!(
            parse("nodes:\n  - {id: s, kind: state, device: 'P*'}\n  - {id: e, kind: notify, title: '{device} is {value}'}\nedges:\n  - {from: s, to: e}\n")
                .is_ok()
        );
    }
}

/// Business-critical pipelines (FLC W5): readings with their own time and quality, through normalizing,
/// event-time windows and episodes to a case, the same at a desk and at a gateway.
#[cfg(test)]
mod pipeline_flows {
    use super::*;

    fn flow(body: Value) -> Compiled {
        parse(&body.to_string()).unwrap_or_else(|e| panic!("{e}"))
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    fn reading(device: &str, value: Value, seen: i64, quality: Quality) -> Input {
        Input { device: device.into(), metric: Some("raw".into()), value, observed_at: Some(at(seen)), quality, ..Default::default() }
    }

    /// Raw counts normalized, an episode over 80 °C held for 30 s, each phase handed on as a case.
    fn oven() -> Compiled {
        flow(json!({
            "profile": "critical",
            "nodes": [
                {"id": "in", "kind": "metric", "metric": "raw"},
                {"id": "norm", "kind": "normalize", "scale": "0.1", "offset": "-40", "unit": "°C", "min": "-50", "max": "400"},
                {"id": "hot", "kind": "episode", "above": 80, "close": 75, "on_delay": "30s", "off_delay": "60s", "reopen": "5m", "stale": "2m"},
                {"id": "case", "kind": "case", "title": "Oven {device} too hot"},
                {"id": "odd", "kind": "log"},
            ],
            "edges": [
                {"from": "in", "to": "norm"},
                {"from": "norm", "to": "hot"},
                {"from": "norm", "port": "quarantine", "to": "odd"},
                {"from": "hot", "port": "opened", "to": "case"},
                {"from": "hot", "port": "reopened", "to": "case"},
                {"from": "hot", "port": "evidence", "to": "case"},
                {"from": "hot", "port": "stale", "to": "case"},
                {"from": "hot", "port": "closed", "to": "case"},
                {"from": "hot", "port": "interrupted", "to": "case"},
            ],
        }))
        .scoped("flow-1")
    }

    fn conditions(out: &Output) -> Vec<(String, String, Option<f64>, Option<Value>)> {
        out.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Condition { episode, phase, value, raw, .. } => Some((episode.clone(), phase.clone(), *value, raw.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn one_case_per_episode_through_duplicates_bad_readings_and_a_restart() {
        let f = oven();
        assert!(f.critical && f.wants_quality());
        let mut state = State::default();
        let mut all = vec![];
        // 1250 counts is 85 °C: holding from second 0, opened at 30.
        for s in [0, 10, 20, 30] {
            all.extend(conditions(&f.input(&mut state, &reading("OVEN-1", json!(1250), s, Quality::Good), at(s))));
        }
        assert_eq!(all.iter().filter(|c| c.1 == "opened").count(), 1);
        let episode = all[0].0.clone();
        assert_eq!(episode, pipeline::episode_id("flow-1", "hot", "OVEN-1", pipeline::ms(at(0))));
        assert_eq!((all[0].2, all[0].3.clone()), (Some(85.0), Some(json!(1250))), "the case keeps the reading and its raw counts");
        // The same reading delivered again: known, nothing comes of it.
        let again = f.input(&mut state, &reading("OVEN-1", json!(1250), 30, Quality::Good), at(31));
        assert!(again.duplicate && again.effects.is_empty());
        // What survives a power cut is where it stood: carry on from the saved state.
        let saved = serde_json::to_string(&state).unwrap();
        let mut state: State = serde_json::from_str(&saved).unwrap();
        let resumed = f.input(&mut state, &reading("OVEN-1", json!(1250), 30, Quality::Good), at(40));
        assert!(resumed.duplicate, "a reading had before the restart is still known after it");
        let more = conditions(&f.input(&mut state, &reading("OVEN-1", json!(1300), 40, Quality::Good), at(41)));
        assert_eq!((more[0].0.clone(), more[0].1.as_str()), (episode.clone(), "evidence"), "the same episode, with more evidence");
        // A bad reading far under the limit is evidence, never a recovery.
        let bad = conditions(&f.input(&mut state, &reading("OVEN-1", json!(100), 50, Quality::Bad), at(51)));
        assert_eq!(bad.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), ["evidence"]);
        // Silence while open is said once, as stale.
        let quiet = conditions(&f.tick(&mut state, at(200)));
        assert_eq!(quiet.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), ["stale"]);
        // Cooled to 70 °C (1100) for a minute: closed, still the same episode.
        let mut ended = vec![];
        for s in [210, 240, 271] {
            ended.extend(conditions(&f.input(&mut state, &reading("OVEN-1", json!(1100), s, Quality::Good), at(s))));
        }
        let closed: Vec<_> = ended.iter().filter(|c| c.1 == "closed").collect();
        assert_eq!((closed.len(), closed[0].0.clone()), (1, episode));
    }

    #[test]
    fn what_cannot_be_a_reading_is_quarantined_and_quality_is_routed() {
        let f = oven();
        let mut state = State::default();
        let out = f.input(&mut state, &reading("OVEN-1", json!("hot"), 0, Quality::Good), at(0));
        assert!(conditions(&out).is_empty());
        assert!(out.effects.iter().any(|e| matches!(e, Effect::Log { text, .. } if text.contains("not a number"))), "{out:?}");
        let out = f.input(&mut state, &reading("OVEN-1", json!(9000), 1, Quality::Good), at(1));
        assert!(out.effects.iter().any(|e| matches!(e, Effect::Log { text, .. } if text.contains("outside what it can be"))), "860 °C is not an oven reading");
        let gate = flow(json!({
            "nodes": [
                {"id": "in", "kind": "metric", "metric": "raw"},
                {"id": "q", "kind": "quality", "stale": "1m"},
                {"id": "good", "kind": "log", "text": "good"},
                {"id": "bad", "kind": "log", "text": "bad"},
                {"id": "old", "kind": "log", "text": "stale"},
            ],
            "edges": [
                {"from": "in", "to": "q"},
                {"from": "q", "port": "good", "to": "good"},
                {"from": "q", "port": "bad", "to": "bad"},
                {"from": "q", "port": "stale", "to": "old"},
            ],
        }));
        let said = |input: Input, now: i64| match &gate.input(&mut State::default(), &input, at(now)).effects[..] {
            [Effect::Log { text, .. }] => text.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(said(reading("D", json!(1), 0, Quality::Good), 1), "good");
        assert_eq!(said(reading("D", json!(1), 0, Quality::Bad), 1), "bad");
        assert_eq!(said(reading("D", json!(1), 0, Quality::Good), 120), "stale", "seen two minutes before it arrived");
    }

    #[test]
    fn an_event_window_corrects_itself_without_repeating_the_action_it_led_to() {
        // The minute's average says out once; a late reading revises the evidence, and only `out` is
        // wired to the command.
        let f = flow(json!({
            "nodes": [
                {"id": "in", "kind": "metric", "metric": "raw"},
                {"id": "w", "kind": "event_window", "size": "1m", "stat": "avg", "wait": "10s", "lateness": "2m"},
                {"id": "ask", "kind": "command", "operation": "vent", "every": "1s"},
                {"id": "fix", "kind": "record_metric", "name": "avg_corrected"},
                {"id": "lost", "kind": "log", "text": "too late"},
            ],
            "edges": [
                {"from": "in", "to": "w"},
                {"from": "w", "to": "ask"},
                {"from": "w", "port": "revised", "to": "fix"},
                {"from": "w", "port": "late", "to": "lost"},
            ],
        }));
        let script: Vec<Scripted> = serde_json::from_value(json!([
            {"at": 5, "seen": 5, "device": "D", "metric": "raw", "value": 10},
            {"at": 20, "seen": 20, "device": "D", "metric": "raw", "value": 20},
            {"at": 71, "seen": 71, "device": "D", "metric": "raw", "value": 1},
            {"at": 80, "seen": 30, "device": "D", "metric": "raw", "value": 60},
            {"at": 200, "seen": 200, "device": "D", "metric": "raw", "value": 2},
            {"at": 201, "seen": 40, "device": "D", "metric": "raw", "value": 99},
        ]))
        .unwrap();
        let moments = simulate(&f, &script, 210).unwrap();
        let effects: Vec<&Effect> = moments.iter().flat_map(|m| &m.output.effects).collect();
        let commands = effects.iter().filter(|e| matches!(e, Effect::Command { .. })).count();
        let corrections: Vec<f64> = effects.iter().filter_map(|e| if let Effect::Metric { value, .. } = e { Some(*value) } else { None }).collect();
        assert_eq!(commands, 2, "minute 0 and minute 1 each led to one request: {effects:?}");
        assert_eq!(corrections, [30.0], "the late reading corrected minute 0's evidence once");
        assert!(effects.iter().any(|e| matches!(e, Effect::Log { text, .. } if text == "too late")));
    }

    #[test]
    fn a_new_revision_drains_or_keeps_only_what_still_means_the_same() {
        let old = oven();
        let mut state = State::default();
        for s in [0, 10, 20, 30] {
            old.input(&mut state, &reading("OVEN-1", json!(1250), s, Quality::Good), at(s));
        }
        // Drained: the open episode is said to be interrupted, not closed.
        let drained = conditions(&old.drain(&mut state.clone(), at(60)));
        assert_eq!(drained.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), ["interrupted"]);
        // Migrated to a revision that only rewords the case: the episode carries on.
        let same_rule = parse(&serde_json::to_string(&json!({
            "profile": "critical",
            "nodes": [
                {"id": "in", "kind": "metric", "metric": "raw"},
                {"id": "norm", "kind": "normalize", "scale": "0.1", "offset": "-40", "unit": "°C", "min": "-50", "max": "400"},
                {"id": "hot", "kind": "episode", "above": 80, "close": 75, "on_delay": "30s", "off_delay": "60s", "reopen": "5m", "stale": "2m"},
                {"id": "case", "kind": "case", "title": "Oven {device} running hot"},
                {"id": "odd", "kind": "log"},
            ],
            "edges": [
                {"from": "in", "to": "norm"}, {"from": "norm", "to": "hot"}, {"from": "norm", "port": "quarantine", "to": "odd"},
                {"from": "hot", "port": "opened", "to": "case"}, {"from": "hot", "port": "evidence", "to": "case"}, {"from": "hot", "port": "closed", "to": "case"},
            ],
        })).unwrap())
        .unwrap()
        .scoped("flow-1");
        let (mut kept, plan) = same_rule.migrate(&old, &state);
        assert!(plan.iter().any(|c| c.node == "hot" && c.kept), "{plan:?}");
        let next = conditions(&same_rule.input(&mut kept, &reading("OVEN-1", json!(1260), 40, Quality::Good), at(41)));
        assert_eq!(next[0].1, "evidence", "the same episode goes on: no second case");
        // A changed limit is a changed meaning: it starts with nothing, and the plan says why.
        let changed = parse(
            &serde_json::to_string(&json!({
                "nodes": [
                    {"id": "in", "kind": "metric", "metric": "raw"},
                    {"id": "hot", "kind": "episode", "above": 90, "close": 85},
                    {"id": "case", "kind": "case", "title": "Too hot"},
                ],
                "edges": [{"from": "in", "to": "hot"}, {"from": "hot", "port": "opened", "to": "case"}],
            }))
            .unwrap(),
        )
        .unwrap();
        let (fresh, plan) = changed.migrate(&old, &state);
        assert_eq!(fresh.keys(), 0);
        assert!(plan.iter().any(|c| c.node == "hot" && !c.kept && c.why.as_deref().unwrap_or_default().contains("settings changed")));
    }

    #[test]
    fn a_flow_keeps_track_of_a_bounded_number_of_untrusted_devices() {
        let f = flow(json!({
            "nodes": [
                {"id": "in", "kind": "metric", "metric": "raw"},
                {"id": "c", "kind": "changed"},
                {"id": "l", "kind": "log"},
            ],
            "edges": [{"from": "in", "to": "c"}, {"from": "c", "to": "l"}],
        }));
        let mut state = State::default();
        let mut problems = 0;
        for i in 0..(MAX_KEYS + 10) {
            problems += f
                .input(&mut state, &Input { device: format!("D-{i}"), metric: Some("raw".into()), value: json!(1), ..Default::default() }, at(0))
                .problems
                .len();
        }
        assert_eq!(state.keys(), MAX_KEYS);
        assert_eq!(problems, 10, "each device left out is said");
    }

    #[test]
    fn the_flows_that_ship_parse_unchanged_and_hear_only_trusted_readings() {
        for (name, text) in
            [("cold-rooms", include_str!("../../solutions/cold-rooms.yaml")), ("energy-meters", include_str!("../../solutions/energy-meters.yaml"))]
        {
            let doc: Value = serde_yaml::from_str(text).unwrap();
            for f in doc["flows"].as_array().into_iter().flatten() {
                let compiled = parse(f["body"].as_str().unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(!compiled.critical && !compiled.wants_quality(), "{name}: an existing flow keeps what it hears");
            }
        }
        assert!(parse(&json!({"nodes": [{"id": "in", "kind": "metric", "metric": "m"}, {"id": "c", "kind": "case", "title": "x"}], "edges": [{"from": "in", "to": "c"}]}).to_string())
            .unwrap_err()
            .contains("a case takes an episode"));
        assert!(parse(&json!({"profile": "fast", "nodes": []}).to_string()).unwrap_err().contains("standard or critical"));
    }
}
