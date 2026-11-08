//! A business-critical flow at a gateway through a power cut (FLC-EDGE-005, LRP-03): the process is
//! killed at each side of the step that writes where the flow stands and what it did, and what the
//! journal holds afterwards is both or neither. A reading delivered again after the restart is acted on
//! once: before the cut nothing was written, so it is decided afresh; after it, the flow knows it.
//!
//! The killing is real: the test runs itself again as a child (`CRITICAL_CHILD`), which aborts at the
//! named crash point (`FERRUM_SITE_CRASH_AT`, feature `failpoints`) on the real SQLite file.

use chrono::{DateTime, Utc};
use ferrum_edge_contract::flow::{self, Compiled, Effect, Input, State};
use ferrum_edge_contract::pipeline::Quality;
use ferrum_ops_contract::sync::Fact;
use ferrum_site::journal::Journal;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const KEY: &str = "flow_state:oven:1";

fn oven() -> Compiled {
    flow::parse(
        &json!({
            "profile": "critical",
            "nodes": [
                {"id": "in", "kind": "metric", "metric": "raw"},
                {"id": "norm", "kind": "normalize", "scale": "0.1", "offset": "-40", "unit": "°C"},
                {"id": "hot", "kind": "episode", "above": 80, "close": 75},
                {"id": "case", "kind": "case", "title": "Oven too hot"},
            ],
            "edges": [
                {"from": "in", "to": "norm"},
                {"from": "norm", "to": "hot"},
                {"from": "hot", "port": "opened", "to": "case"},
                {"from": "hot", "port": "evidence", "to": "case"},
            ],
        })
        .to_string(),
    )
    .unwrap()
    .scoped("oven")
}

fn at(s: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + s, 0).unwrap()
}

fn reading(seen: i64, raw: i64) -> Input {
    Input { device: "OVEN-1".into(), metric: Some("raw".into()), value: json!(raw), observed_at: Some(at(seen)), quality: Quality::Good, ..Default::default() }
}

/// One step as the gateway takes it: the reading through the flow, and what it did as facts.
fn step(f: &Compiled, state: &mut State, input: &Input) -> Vec<Fact> {
    let out = f.input(state, input, at(0));
    out.effects
        .into_iter()
        .filter_map(|e| match e {
            Effect::Condition { episode, phase, value, .. } => Some(Fact {
                fact_id: Uuid::now_v7(),
                kind: "edge.condition".into(),
                at_source: at(0),
                actor: None,
                body: json!({"episode": episode, "phase": phase, "value": value}),
            }),
            _ => None,
        })
        .collect()
}

fn load(j: &Journal) -> State {
    j.get(KEY).unwrap().map(|t| serde_json::from_str(&t).unwrap()).unwrap_or_default()
}

fn facts(j: &Journal) -> Vec<(String, String)> {
    j.facts(100)
        .unwrap()
        .into_iter()
        .map(|f| (f.body["episode"].as_str().unwrap_or_default().to_string(), f.body["phase"].as_str().unwrap_or_default().to_string()))
        .collect()
}

/// The child: takes one step from what the journal holds and writes it, dying where it is told to.
#[test]
fn child() {
    let Ok(path) = std::env::var("CRITICAL_CHILD") else { return };
    let j = Journal::open(Path::new(&path)).unwrap();
    let f = oven();
    let mut state = load(&j);
    let raw: i64 = std::env::var("CRITICAL_RAW").unwrap().parse().unwrap();
    let seen: i64 = std::env::var("CRITICAL_SEEN").unwrap().parse().unwrap();
    let facts = step(&f, &mut state, &reading(seen, raw));
    j.commit_flow(KEY, &json!(state).to_string(), &facts).unwrap();
}

fn run_child(path: &Path, seen: i64, raw: i64, crash: Option<&str>) -> bool {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "child", "--nocapture", "--test-threads", "1"])
        .env("CRITICAL_CHILD", path)
        .env("CRITICAL_SEEN", seen.to_string())
        .env("CRITICAL_RAW", raw.to_string());
    if let Some(point) = crash {
        cmd.env("FERRUM_SITE_CRASH_AT", point);
    }
    cmd.output().unwrap().status.success()
}

fn journal_at(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    dir.join("journal.db")
}

#[test]
fn a_power_cut_at_either_side_of_a_step_leaves_both_or_neither() {
    if std::env::var("CRITICAL_CHILD").is_ok() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ferrum-critical-{}", Uuid::now_v7()));
    let path = journal_at(&dir);
    let f = oven();
    // Three readings over 80 °C: the episode opens on the first (no on-delay).
    assert!(run_child(&path, 0, 1250, None), "a step with no crash completes");
    {
        let j = Journal::open(&path).unwrap();
        assert_eq!(facts(&j).iter().map(|x| x.1.as_str()).collect::<Vec<_>>(), ["opened"]);
    }

    // Killed before the commit: neither the state nor the evidence is there.
    assert!(!run_child(&path, 10, 1260, Some("before_flow_commit")), "it died at its crash point");
    let j = Journal::open(&path).unwrap();
    let before = load(&j);
    assert_eq!(facts(&j).len(), 1, "nothing of the step was written");
    // The reading comes again after the restart: decided afresh, once.
    let mut state = before.clone();
    let again = step(&f, &mut state, &reading(10, 1260));
    assert_eq!(again.iter().map(|x| x.body["phase"].as_str().unwrap()).collect::<Vec<_>>(), ["evidence"]);
    drop(j);

    // Killed after the commit: both are there, and the same reading again is known.
    assert!(!run_child(&path, 10, 1260, Some("after_flow_commit")), "it died at its crash point");
    let j = Journal::open(&path).unwrap();
    let written = facts(&j);
    assert_eq!(written.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(), ["opened", "evidence"], "the step was written whole");
    assert!(written.iter().all(|x| x.0 == written[0].0), "one episode throughout");
    let mut state = load(&j);
    let out = f.input(&mut state, &reading(10, 1260), at(0));
    assert!(out.duplicate && out.effects.is_empty(), "the reading delivered again changes nothing");
    let next = step(&f, &mut state, &reading(20, 1270));
    assert_eq!(next.len(), 1, "the next reading is evidence for the same episode");
    let _ = std::fs::remove_dir_all(&dir);
    let _: Value = json!(null);
}
