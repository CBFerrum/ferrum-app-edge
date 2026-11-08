//! What a business-critical pipeline adds to flows (Ferrum's Logic Platform, W5): readings that say
//! how far they can be trusted, numbers brought to engineering units exactly with the raw value kept,
//! statistics over stretches of the time a reading was *seen* rather than when it arrived, and
//! episodes: a sustained condition with one identity from the moment it holds until it clears.
//!
//! Pure, like the rest of the contract: the gateway runs it and the platform simulates it with the same
//! code. The ideas are the ones stream processors and alarm management settled on: a watermark that
//! waits a bounded time for stragglers and corrections accepted for a declared lateness after that
//! (Apache Beam and Flink), a source gone idle not holding windows open for ever, and alarms with a
//! deadband and on and off delays (ISA-18.2), so a value hovering at a limit is one episode, not fifty.

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::str::FromStr;

/// How far a reading can be trusted, in OPC UA's three classes. A device or its gateway says; a
/// reading that does not is good.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    #[default]
    Good,
    /// It may be right: a value stuck too long, or outside what was expected but not impossible.
    Uncertain,
    /// It is not a reading: a sensor fault, a value outside what can be.
    Bad,
}

impl Quality {
    pub fn is_good(&self) -> bool {
        *self == Quality::Good
    }

    pub fn word(&self) -> &'static str {
        match self {
            Quality::Good => "good",
            Quality::Uncertain => "uncertain",
            Quality::Bad => "bad",
        }
    }
}

/// A number from a device brought to engineering units: `raw × scale + offset`, worked out exactly.
/// `Err` says why it is not a reading: what is not a number, or what lands outside `min`..`max`, is
/// quarantined rather than made normal (FLC-EDGE-002).
pub fn normalize(raw: &Value, scale: Decimal, offset: Decimal, min: Option<Decimal>, max: Option<Decimal>) -> Result<Decimal, String> {
    let exact = match raw {
        // A device's number as written, so 0.1 stays 0.1.
        Value::Number(n) => Decimal::from_str(&n.to_string()).or_else(|_| Decimal::from_scientific(&n.to_string())).ok(),
        Value::String(s) => Decimal::from_str(s.trim()).ok(),
        _ => None,
    }
    .ok_or_else(|| format!("{} is not a number", short(raw)))?;
    let value = exact.checked_mul(scale).and_then(|v| v.checked_add(offset)).ok_or_else(|| format!("{exact} scaled is out of range"))?;
    if min.is_some_and(|m| value < m) || max.is_some_and(|m| value > m) {
        return Err(format!("{} is outside what it can be ({}..{})", value.normalize(), opt(min), opt(max)));
    }
    Ok(value.normalize())
}

fn opt(d: Option<Decimal>) -> String {
    d.map_or_else(String::new, |d| d.normalize().to_string())
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 40 { format!("{}…", s.chars().take(40).collect::<String>()) } else { s }
}

pub(crate) fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

pub(crate) fn time(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

// ───────────────────────────── event-time windows ─────────────────────────────

/// How an event-time window behaves, from its node.
#[derive(Debug, Clone, Copy)]
pub struct WindowRule {
    /// Each window's length.
    pub size_ms: i64,
    /// How long to wait for stragglers before a window's first result: the watermark is the latest
    /// time seen less this.
    pub wait_ms: i64,
    /// How long after its first result a window still takes a late reading, and says the corrected
    /// result on `revised`. Later than that is `late`, and changes nothing.
    pub lateness_ms: i64,
    /// A source quiet this long, by the gateway's clock, closes what it has open: a device that stops
    /// cannot hold a window open for ever.
    pub idle_ms: Option<i64>,
}

/// Most windows one device keeps open, and most readings one window keeps.
pub const MAX_WINDOWS: usize = 1_000;
pub const MAX_IN_WINDOW: usize = 10_000;

/// One window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Readings by the millisecond they were seen: the same reading twice counts once.
    pub values: BTreeMap<i64, f64>,
    /// What it said last, once it has said anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<f64>,
    /// How many times it has been corrected.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revisions: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// One device's windows (or all devices', for `across`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Windows {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub open: BTreeMap<i64, Cell>,
    /// The latest time a reading was seen at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<i64>,
    /// When the gateway last heard from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard: Option<i64>,
}

/// What a window says: its result, a correction, or a reading that came too late.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    pub port: &'static str,
    pub start: i64,
    pub end: i64,
    pub value: Value,
    pub count: usize,
    pub revision: u32,
    /// Closed before its time: its source went quiet, or the flow was drained.
    pub partial: bool,
}

impl Said {
    /// The message it sends on: the result as the value, and where it came from.
    pub fn message(&self) -> Value {
        json!({"value": self.value, "start": time(self.start), "end": time(self.end), "count": self.count, "revision": self.revision, "partial": self.partial})
    }
}

impl Windows {
    fn watermark(&self, rule: &WindowRule) -> Option<i64> {
        self.latest.map(|l| l - rule.wait_ms)
    }

    /// Takes a reading seen at `seen`, heard at `now`. What it closes or corrects comes back.
    pub fn observe(&mut self, seen: i64, value: f64, now: i64, rule: &WindowRule, stat: impl Fn(&[f64]) -> Option<f64>) -> Vec<Said> {
        self.heard = Some(now);
        let start = seen.div_euclid(rule.size_ms) * rule.size_ms;
        let end = start + rule.size_ms;
        let mut out = vec![];
        let fired_until = self.watermark(rule);
        // Past its window's end and its lateness: too late to change anything.
        if fired_until.is_some_and(|w| end + rule.lateness_ms <= w) {
            out.push(Said { port: "late", start, end, value: json!(value), count: 1, revision: 0, partial: false });
            return out;
        }
        if !self.open.contains_key(&start) && self.open.len() >= MAX_WINDOWS {
            // The oldest goes first, saying what it has: nothing is dropped without a word.
            if let Some((&oldest, _)) = self.open.iter().next() {
                out.extend(self.close(oldest, rule, &stat, true));
            }
        }
        let cell = self.open.entry(start).or_default();
        if cell.values.len() < MAX_IN_WINDOW || cell.values.contains_key(&seen) {
            cell.values.insert(seen, value);
        }
        // A window that has spoken takes a correction until its lateness is up.
        if let Some(said) = cell.said {
            let numbers: Vec<f64> = cell.values.values().copied().collect();
            if let Some(now_says) = stat(&numbers)
                && now_says != said
            {
                cell.said = Some(now_says);
                cell.revisions += 1;
                out.push(Said { port: "revised", start, end, value: json!(now_says), count: numbers.len(), revision: cell.revisions, partial: false });
            }
        }
        self.latest = Some(self.latest.map_or(seen, |l| l.max(seen)));
        out.extend(self.due(now, rule, &stat));
        out
    }

    /// What the watermark, lateness and quiet decide at `now`: first results of windows the watermark
    /// passed, windows past their lateness forgotten, and an idle source's windows closed.
    pub fn due(&mut self, now: i64, rule: &WindowRule, stat: &impl Fn(&[f64]) -> Option<f64>) -> Vec<Said> {
        let mut out = vec![];
        let idle = rule.idle_ms.is_some_and(|i| self.heard.is_some_and(|h| now - h >= i));
        let watermark = self.watermark(rule);
        let starts: Vec<i64> = self.open.keys().copied().collect();
        for start in starts {
            let end = start + rule.size_ms;
            let passed = watermark.is_some_and(|w| end <= w);
            let spoken = self.open.get(&start).is_some_and(|c| c.said.is_some());
            if !spoken && (passed || idle) {
                out.extend(self.speak(start, rule, stat, idle && !passed));
            }
            // Its lateness is over, or its source is quiet: it is done with.
            if watermark.is_some_and(|w| end + rule.lateness_ms <= w) || idle {
                self.open.remove(&start);
            }
        }
        out
    }

    fn speak(&mut self, start: i64, rule: &WindowRule, stat: &impl Fn(&[f64]) -> Option<f64>, partial: bool) -> Vec<Said> {
        let Some(cell) = self.open.get_mut(&start) else { return vec![] };
        let numbers: Vec<f64> = cell.values.values().copied().collect();
        let Some(v) = stat(&numbers) else { return vec![] };
        cell.said = Some(v);
        vec![Said { port: "out", start, end: start + rule.size_ms, value: json!(v), count: numbers.len(), revision: 0, partial }]
    }

    fn close(&mut self, start: i64, rule: &WindowRule, stat: &impl Fn(&[f64]) -> Option<f64>, partial: bool) -> Vec<Said> {
        let spoken = self.open.get(&start).is_some_and(|c| c.said.is_some());
        let out = if spoken { vec![] } else { self.speak(start, rule, stat, partial) };
        self.open.remove(&start);
        out
    }

    /// Everything still open says what it has, marked partial, and is forgotten: a flow drained before
    /// a new revision takes over leaves nothing unsaid.
    pub fn drain(&mut self, rule: &WindowRule, stat: &impl Fn(&[f64]) -> Option<f64>) -> Vec<Said> {
        let starts: Vec<i64> = self.open.keys().copied().collect();
        let mut out = vec![];
        for start in starts {
            out.extend(self.close(start, rule, stat, true));
        }
        out
    }
}

// ───────────────────────────── episodes ─────────────────────────────

/// When a condition holds and when it has cleared: on above `open`, off below `close` (or the other
/// way round for `below`), the gap between them a deadband where nothing changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EpisodeRule {
    pub high: bool,
    pub open: f64,
    pub close: f64,
    /// How long the condition must hold before the episode opens.
    pub on_delay_ms: i64,
    /// How long it must stay cleared before the episode closes.
    pub off_delay_ms: i64,
    /// An episode that holds again within this of closing is the same episode, reopened.
    pub reopen_ms: i64,
    /// No good reading for this long while open: said once, and the episode stays open. Silence is
    /// not recovery.
    pub stale_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Clear,
    /// Holding, not yet for long enough.
    Opening,
    Open,
    /// Cleared, not yet for long enough.
    Closing,
}

/// One device's episode.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    #[serde(default)]
    pub phase: Phase,
    /// Its identity, from when it opened; kept after it closes, for a reopening.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// When the condition began to hold (event time), and when it opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<i64>,
    /// The furthest past the limit it went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak: Option<f64>,
    /// The last good reading, by event time, and when the gateway heard it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_good: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    /// Readings it has had, and the latest one's time: the same reading again adds nothing.
    #[serde(default)]
    pub evidence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<i64>,
}

/// What an episode says.
#[derive(Debug, Clone, PartialEq)]
pub struct Happened {
    /// `opened`, `reopened`, `evidence`, `stale`, `closed` or `interrupted`.
    pub port: &'static str,
    pub id: String,
    pub at: i64,
    pub value: Option<f64>,
    pub quality: Quality,
    pub peak: Option<f64>,
    pub opened: Option<i64>,
}

impl Happened {
    pub fn message(&self, device: Option<&str>) -> Value {
        json!({
            "episode": self.id, "phase": self.port, "device": device, "at": time(self.at), "value": self.value,
            "quality": self.quality.word(), "peak": self.peak, "opened": self.opened.map(time),
        })
    }
}

/// An episode's identity: the same scope, node, device and opening moment give the same id, so a
/// gateway that says it again after a restart names the same episode, and the platform starts one
/// case for it (FLC-EDGE-007).
pub fn episode_id(scope: &str, node: &str, device: &str, opened: i64) -> String {
    let digest = Sha256::digest(format!("ferrum.edge.episode/1\n{scope}\n{node}\n{device}\n{opened}").as_bytes());
    hex::encode(&digest[..16])
}

impl EpisodeRule {
    fn holds(&self, v: f64) -> bool {
        if self.high { v > self.open } else { v < self.open }
    }

    fn cleared(&self, v: f64) -> bool {
        if self.high { v < self.close } else { v > self.close }
    }

    fn further(&self, a: f64, b: f64) -> f64 {
        if self.high { a.max(b) } else { a.min(b) }
    }
}

impl Episode {
    /// Takes a reading seen at `seen` (event time) and heard at `now`. Only a good reading moves the
    /// episode; any other is evidence of what was seen and changes nothing (FLC-EDGE-003).
    pub fn observe(&mut self, scope: &str, node: &str, device: &str, seen: i64, value: f64, quality: Quality, now: i64, rule: &EpisodeRule) -> Vec<Happened> {
        // The same reading again, or an older one: evidence already had, or too old to move anything.
        if self.seen.is_some_and(|s| seen <= s) {
            return vec![];
        }
        self.seen = Some(seen);
        let mut out = vec![];
        let happened = |e: &Episode, port: &'static str, at: i64, value: Option<f64>, quality: Quality| Happened {
            port,
            id: e.id.clone().unwrap_or_default(),
            at,
            value,
            quality,
            peak: e.peak,
            opened: e.opened,
        };
        if !quality.is_good() {
            if matches!(self.phase, Phase::Open | Phase::Closing) {
                self.evidence += 1;
                out.push(happened(self, "evidence", seen, Some(value), quality));
            }
            return out;
        }
        self.last_good = Some(seen);
        self.heard = Some(now);
        if self.stale {
            self.stale = false;
        }
        match self.phase {
            Phase::Clear if rule.holds(value) => {
                self.since = Some(seen);
                self.phase = Phase::Opening;
                out.extend(self.settle(scope, node, device, seen, value, rule));
            }
            Phase::Clear => {}
            Phase::Opening if rule.holds(value) => out.extend(self.settle(scope, node, device, seen, value, rule)),
            Phase::Opening => {
                self.phase = Phase::Clear;
                self.since = None;
            }
            Phase::Open => {
                self.peak = Some(self.peak.map_or(value, |p| rule.further(p, value)));
                self.evidence += 1;
                out.push(happened(self, "evidence", seen, Some(value), quality));
                if rule.cleared(value) {
                    self.phase = Phase::Closing;
                    self.since = Some(seen);
                    out.extend(self.settle(scope, node, device, seen, value, rule));
                }
            }
            Phase::Closing => {
                self.evidence += 1;
                out.push(happened(self, "evidence", seen, Some(value), quality));
                if rule.cleared(value) {
                    out.extend(self.settle(scope, node, device, seen, value, rule));
                } else {
                    // Back past the close line before the off delay: still the same episode, still open.
                    self.phase = Phase::Open;
                    self.since = None;
                    self.peak = Some(self.peak.map_or(value, |p| rule.further(p, value)));
                }
            }
        }
        out
    }

    /// An opening or a closing whose delay is up, at event time `at`.
    fn settle(&mut self, scope: &str, node: &str, device: &str, at: i64, value: f64, rule: &EpisodeRule) -> Vec<Happened> {
        let since = self.since.unwrap_or(at);
        match self.phase {
            Phase::Opening if at - since >= rule.on_delay_ms => {
                let reopen = self.closed.is_some_and(|c| since - c <= rule.reopen_ms) && self.id.is_some();
                if !reopen {
                    self.id = Some(episode_id(scope, node, device, since));
                    self.opened = Some(since);
                    self.peak = Some(value);
                    self.evidence = 0;
                }
                self.phase = Phase::Open;
                self.closed = None;
                self.since = None;
                self.evidence += 1;
                vec![Happened {
                    port: if reopen { "reopened" } else { "opened" },
                    id: self.id.clone().unwrap_or_default(),
                    at,
                    value: Some(value),
                    quality: Quality::Good,
                    peak: self.peak,
                    opened: self.opened,
                }]
            }
            Phase::Closing if at - since >= rule.off_delay_ms => {
                self.phase = Phase::Clear;
                self.closed = Some(at);
                self.since = None;
                vec![Happened {
                    port: "closed",
                    id: self.id.clone().unwrap_or_default(),
                    at,
                    value: Some(value),
                    quality: Quality::Good,
                    peak: self.peak,
                    opened: self.opened,
                }]
            }
            _ => vec![],
        }
    }

    /// What the gateway's clock decides at `now`: an episode with no good reading for its stale time
    /// says so once and stays open.
    pub fn due(&mut self, now: i64, rule: &EpisodeRule) -> Vec<Happened> {
        let quiet = rule.stale_ms.is_some_and(|s| self.heard.is_some_and(|h| now - h >= s));
        if matches!(self.phase, Phase::Open | Phase::Closing) && quiet && !self.stale {
            self.stale = true;
            return vec![Happened {
                port: "stale",
                id: self.id.clone().unwrap_or_default(),
                at: self.last_good.unwrap_or(now),
                value: None,
                quality: Quality::Uncertain,
                peak: self.peak,
                opened: self.opened,
            }];
        }
        vec![]
    }

    /// An open episode handed over when its flow is drained: said as interrupted, not closed, because the
    /// condition was not seen to clear.
    pub fn drain(&mut self, now: i64) -> Vec<Happened> {
        if !matches!(self.phase, Phase::Open | Phase::Closing) {
            return vec![];
        }
        self.phase = Phase::Clear;
        self.since = None;
        vec![Happened {
            port: "interrupted",
            id: self.id.clone().unwrap_or_default(),
            at: now,
            value: None,
            quality: Quality::Uncertain,
            peak: self.peak,
            opened: self.opened,
        }]
    }
}

/// A statistic as a window works it out.
pub fn decimal_of(v: f64) -> Option<Decimal> {
    Decimal::from_f64(v)
}

pub fn f64_of(d: Decimal) -> Option<f64> {
    d.to_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avg(v: &[f64]) -> Option<f64> {
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    }

    #[test]
    fn normalization_is_exact_keeps_what_it_cannot_be_out() {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        // Raw counts of 0..4095 for 4..20 mA, in tenths of a degree: 2048 × 0.1 − 40.
        assert_eq!(normalize(&json!(2048), d("0.1"), d("-40"), None, None).unwrap(), d("164.8"));
        assert_eq!(normalize(&json!(0.1), d("3"), d("0"), None, None).unwrap(), d("0.3"), "no binary float drift");
        assert_eq!(normalize(&json!("12.5"), d("1"), d("0"), None, None).unwrap(), d("12.5"));
        assert!(normalize(&json!("hot"), d("1"), d("0"), None, None).unwrap_err().contains("not a number"));
        assert!(normalize(&json!(null), d("1"), d("0"), None, None).is_err());
        assert!(normalize(&json!(9000), d("0.1"), d("-40"), Some(d("-50")), Some(d("200"))).unwrap_err().contains("outside"));
    }

    fn rule() -> WindowRule {
        WindowRule { size_ms: 60_000, wait_ms: 10_000, lateness_ms: 120_000, idle_ms: Some(300_000) }
    }

    #[test]
    fn a_window_waits_for_stragglers_takes_corrections_and_names_what_is_too_late() {
        let mut w = Windows::default();
        let r = rule();
        // Minute 0: 10 and 20, the second seen out of order.
        assert!(w.observe(20_000, 20.0, 20_000, &r, avg).is_empty());
        assert!(w.observe(5_000, 10.0, 21_000, &r, avg).is_empty());
        // The same reading again counts once.
        assert!(w.observe(5_000, 10.0, 22_000, &r, avg).is_empty());
        // Seen at 70 s: the watermark is 60 s, minute 0 says 15.
        let said = w.observe(70_000, 1.0, 70_000, &r, avg);
        assert_eq!(said.len(), 1);
        assert_eq!((said[0].port, said[0].value.clone(), said[0].count), ("out", json!(15.0), 2));
        // A late reading for minute 0, inside its lateness: a correction, not a second first result.
        let corrected = w.observe(30_000, 60.0, 80_000, &r, avg);
        assert_eq!((corrected[0].port, corrected[0].value.clone(), corrected[0].revision), ("revised", json!(30.0), 1));
        // Minute 3 seen: the watermark passes minute 0's end plus its lateness, and minute 0 is gone.
        w.observe(190_000, 2.0, 190_000, &r, avg);
        let too_late = w.observe(40_000, 99.0, 191_000, &r, avg);
        assert_eq!(too_late[0].port, "late", "{too_late:?}");
        assert!(!w.open.contains_key(&0));
    }

    #[test]
    fn a_quiet_source_closes_its_windows_partial_and_a_drain_says_what_is_open() {
        let mut w = Windows::default();
        let r = rule();
        w.observe(5_000, 3.0, 5_000, &r, avg);
        assert!(w.due(200_000, &r, &avg).is_empty(), "not yet quiet for long enough, and the watermark has not passed");
        let said = w.due(305_000, &r, &avg);
        assert_eq!((said[0].port, said[0].partial), ("out", true));
        assert!(w.open.is_empty());
        w.observe(400_000, 4.0, 400_000, &r, avg);
        let drained = w.drain(&r, &avg);
        assert_eq!((drained[0].value.clone(), drained[0].partial), (json!(4.0), true));
        assert!(w.open.is_empty());
    }

    fn high() -> EpisodeRule {
        EpisodeRule { high: true, open: 80.0, close: 75.0, on_delay_ms: 30_000, off_delay_ms: 60_000, reopen_ms: 300_000, stale_ms: Some(120_000) }
    }

    fn ports(h: &[Happened]) -> Vec<&'static str> {
        h.iter().map(|x| x.port).collect()
    }

    #[test]
    fn an_episode_opens_after_its_delay_ignores_the_deadband_and_closes_after_its_own() {
        let (mut e, r) = (Episode::default(), high());
        let mut at = 0;
        let mut feed = |e: &mut Episode, v: f64| {
            at += 10_000;
            e.observe("flow", "hot", "OVEN-1", at, v, Quality::Good, at, &r)
        };
        assert!(feed(&mut e, 81.0).is_empty(), "holding, not yet for 30 s");
        assert!(feed(&mut e, 79.0).is_empty(), "back under: the opening is off");
        assert!(feed(&mut e, 82.0).is_empty());
        assert!(feed(&mut e, 85.0).is_empty());
        assert!(feed(&mut e, 83.0).is_empty());
        let opened = feed(&mut e, 84.0);
        assert_eq!(ports(&opened), ["opened"]);
        let id = opened[0].id.clone();
        assert_eq!(id, episode_id("flow", "hot", "OVEN-1", 30_000), "named by when it began to hold");
        // Hovering in the deadband between 75 and 80: one episode, evidence each time.
        for v in [78.0, 79.5, 77.0, 90.0, 76.0] {
            assert!(ports(&feed(&mut e, v)).iter().all(|p| *p == "evidence"));
        }
        // Under 75 for 60 s closes it.
        let mut closing = vec![];
        for v in [70.0, 71.0, 70.0, 72.0, 70.0, 70.0, 70.0] {
            closing.extend(feed(&mut e, v));
        }
        let closed: Vec<&Happened> = closing.iter().filter(|h| h.port == "closed").collect();
        assert_eq!(closed.len(), 1);
        assert_eq!((closed[0].id.clone(), closed[0].peak), (id.clone(), Some(90.0)));
    }

    #[test]
    fn duplicates_and_bad_readings_are_evidence_silence_is_not_recovery_and_a_quick_return_reopens() {
        let (mut e, r) = (Episode::default(), high());
        for at in [0, 10_000, 20_000, 30_000] {
            e.observe("f", "n", "D", at, 90.0, Quality::Good, at, &r);
        }
        assert_eq!(e.phase, Phase::Open);
        let id = e.id.clone().unwrap();
        // The same reading again changes nothing and is not counted twice.
        let before = e.evidence;
        assert!(e.observe("f", "n", "D", 30_000, 90.0, Quality::Good, 31_000, &r).is_empty());
        assert_eq!(e.evidence, before);
        // A bad reading, even far under the limit, is evidence and never a recovery.
        let bad = e.observe("f", "n", "D", 40_000, 10.0, Quality::Bad, 40_000, &r);
        assert_eq!((ports(&bad), e.phase), (vec!["evidence"], Phase::Open));
        // Silence: said once, still open.
        assert_eq!(ports(&e.due(200_000, &r)), ["stale"]);
        assert!(e.due(300_000, &r).is_empty());
        assert_eq!(e.phase, Phase::Open);
        // A good reading again, then cleared for its off delay: closed.
        for at in [310_000, 340_000, 371_000] {
            e.observe("f", "n", "D", at, 70.0, Quality::Good, at, &r);
        }
        assert_eq!(e.phase, Phase::Clear);
        // Holding again within five minutes of closing: the same episode, reopened.
        let mut again = vec![];
        for at in [400_000, 431_000] {
            again.extend(e.observe("f", "n", "D", at, 95.0, Quality::Good, at, &r));
        }
        assert_eq!(ports(&again), ["reopened"]);
        assert_eq!(again[0].id, id);
        // What survives a restart is the same episode.
        let back: Episode = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        assert_eq!(ports(&e.clone().drain(500_000)), ["interrupted"]);
    }
}
