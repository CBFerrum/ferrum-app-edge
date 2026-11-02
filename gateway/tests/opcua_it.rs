//! A gateway and a stranger on OPC UA, each way: Python's asyncua reads a gateway's server, and a
//! gateway hears asyncua's events and reads its history. It runs when `FERRUM_TEST_PYTHON` names a
//! Python that has asyncua, as CI's does. What the other tests prove against async-opcua, which is both
//! the client and the server here, could be a shared mistake. This could not.

use chrono::{DateTime, Utc};
use ferrum_edge_contract::edge::Export;
use ferrum_site::adapters::transport::{self, Limits, Message, Out};
use ferrum_site::export::Sender;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;

const PEER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/opcua_peer.py");

fn python() -> Option<String> {
    let found = std::env::var("FERRUM_TEST_PYTHON").ok();
    if found.is_none() {
        eprintln!("skipped: FERRUM_TEST_PYTHON is not set");
    }
    found
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn peer(python: &str, args: &[&str]) -> (Child, tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) {
    let mut child = Command::new(python).arg(PEER).args(args).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true).spawn().unwrap();
    let lines = BufReader::new(child.stdout.take().unwrap()).lines();
    (child, lines)
}

async fn next(lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) -> Value {
    let line = timeout(Duration::from_secs(20), lines.next_line()).await.expect("it said something").unwrap().expect("a line");
    serde_json::from_str(&line).unwrap()
}

fn reading(value: f64, at: DateTime<Utc>) -> Vec<String> {
    vec![json!({"device": "PRESS-1", "metric": "temp.c", "value": value, "at": at.to_rfc3339()}).to_string()]
}

#[tokio::test]
async fn a_stranger_reads_a_gateway_and_cannot_write_to_it() {
    let Some(python) = python() else { return };
    let bind = format!("127.0.0.1:{}", free_port());
    let dir = std::env::temp_dir().join(format!("ferrum-peer-{bind}"));
    let export = Export { name: "e".into(), to: format!("listen+opcua://{bind}"), topic: None, only: vec![], sign_with: None };
    let sender = Sender::new(Limits { listen: vec![bind.clone()], tls_dir: Some(dir.clone()), ..Default::default() });
    let at = Utc::now() - chrono::Duration::minutes(5);
    sender.deliver(&export, &reading(21.5, at), &json!({})).await.expect("served");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let url = format!("opc.tcp://{bind}/");
    let (_child, mut lines) = peer(&python, &["client", &url, "PRESS-1", "temp.c"]);
    let found = next(&mut lines).await;
    assert!(found["devices"].as_array().unwrap().contains(&json!("PRESS-1")), "{found}");
    assert_eq!(next(&mut lines).await["metric"], "temp.c");
    let value = next(&mut lines).await;
    assert_eq!(value["value"], json!(21.5), "{value}");
    assert!(value["time"].as_str().unwrap().starts_with(&at.format("%Y-%m-%d %H:%M:%S").to_string()), "the device's own time: {value}");
    let wrote = next(&mut lines).await;
    assert_eq!(wrote["wrote"], json!(false), "{wrote}");
    assert_eq!(next(&mut lines).await["watching"], json!(21.5));
    // A clock running ahead of this one is the device's problem, and no reason to hold a reading back.
    sender.deliver(&export, &reading(22.5, Utc::now() + chrono::Duration::hours(1)), &json!({})).await.expect("served");
    assert_eq!(next(&mut lines).await["changed"], json!(22.5), "a subscription hears the next reading");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_gateway_hears_a_strangers_events_and_reads_its_history() {
    let Some(python) = python() else { return };
    let port = free_port();
    let (_child, mut lines) = peer(&python, &["server", &port.to_string()]);
    let node = next(&mut lines).await["ready"].as_str().unwrap().to_string();
    let watching = ["events:i=2253".to_string()];
    let options = transport::Options { subscribe: &watching, ..Default::default() };
    let client = transport::open(&format!("opcua://127.0.0.1:{port}/"), options, &Limits::default()).unwrap();
    let (tx, mut rx) = mpsc::channel::<Message>(8);
    let (ready, mut up) = mpsc::channel(1);
    let runner = client.clone();
    tokio::spawn(async move { runner.run(tx, ready).await });
    timeout(Duration::from_secs(10), up.recv()).await.expect("it connected");

    let event = timeout(Duration::from_secs(10), rx.recv()).await.expect("an event").unwrap();
    assert_eq!(event.topic, "events:i=2253");
    let said: Value = serde_json::from_slice(&event.body).unwrap();
    assert_eq!(said["Message"], "door open", "{said}");
    assert_eq!(said["Severity"], json!(500), "{said}");
    assert!(said["EventId"].as_str().is_some_and(|id| !id.is_empty()), "what a condition is acknowledged by: {said}");

    let history = format!("history:{node}");
    let ask = || Out::Node { node: history.clone(), value: None };
    let first = client.send(ask()).await.expect("read").expect("an answer");
    let first: Value = serde_json::from_slice(&first.body).unwrap();
    let held: Vec<f64> = first["value"].as_array().expect("samples").iter().filter_map(|s| s["value"].as_f64()).collect();
    assert!(held.ends_with(&[21.0, 22.0, 23.0, 24.0]), "{first}");
    let again = client.send(ask()).await.expect("read").expect("an answer");
    assert_eq!(serde_json::from_slice::<Value>(&again.body).unwrap()["value"], json!([]), "only what is new since it last asked");
}
