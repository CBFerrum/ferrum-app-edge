//! Reads and writes one piece of equipment by hand, to try a protocol from a shell with no platform.
//!
//! `cargo run --example tag -- s7://10.0.0.7:102 read DB1.DBD0:real write M0.1 true wait 5`

use ferrum_site::adapters::transport::{self, Limits, Options, Out};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((address, steps)) = args.split_first() else {
        eprintln!("say an address, and what to do: read NODE, write NODE VALUE");
        std::process::exit(2);
    };
    let bind = address.split_once("://").map(|(_, rest)| rest.split('?').next().unwrap_or(rest).to_string());
    let limits = Limits { listen: bind.into_iter().collect(), ..Default::default() };
    let watch: Vec<String> = std::env::var("TAG_SUBSCRIBE").map(|w| w.split(',').map(str::to_string).collect()).unwrap_or_default();
    let options = Options { subscribe: &watch, ..Options::default() };
    let link = transport::open(address, options, &limits).unwrap_or_else(|e| panic!("{e}"));
    let (tx, mut rx) = mpsc::channel(16);
    let (ready, mut up) = mpsc::channel(1);
    let runner = link.clone();
    tokio::spawn(async move {
        if let Err(why) = runner.run(tx, ready).await {
            eprintln!("{why}");
        }
    });
    tokio::time::timeout(Duration::from_secs(5), up.recv()).await.expect("it did not connect");
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            println!("heard {}: {}", m.topic, String::from_utf8_lossy(&m.body));
        }
    });
    let mut steps = steps.iter();
    while let Some(step) = steps.next() {
        if step == "wait" {
            let secs = steps.next().and_then(|s| s.parse().ok()).expect("how long");
            tokio::time::sleep(Duration::from_secs(secs)).await;
            continue;
        }
        if matches!(step.as_str(), "get" | "put" | "post" | "delete") {
            let path = steps.next().expect("which path").clone();
            let body = (step != "get" && step != "delete").then(|| steps.next().expect("what body").clone().into_bytes());
            match link.send(Out::Http { method: step.to_uppercase(), path: path.clone(), body, headers: vec![] }).await {
                Ok(Some(m)) => println!("{path} = {} bytes: {}", m.body.len(), String::from_utf8_lossy(&m.body).chars().take(80).collect::<String>()),
                other => println!("{path}: {other:?}"),
            }
            continue;
        }
        if step == "call" {
            let object = steps.next().expect("which object").clone();
            let method = steps.next().expect("which method").clone();
            let args = serde_json::from_str(steps.next().expect("what arguments, as a JSON list")).expect("a JSON list");
            match link.send(Out::Call { object, method: method.clone(), args }).await {
                Ok(Some(m)) => println!("{method} = {}", String::from_utf8_lossy(&m.body)),
                other => println!("{method}: {other:?}"),
            }
            continue;
        }
        if step == "publish" {
            let topic = steps.next().expect("which topic").clone();
            let body = steps.next().expect("what body").clone().into_bytes();
            println!("{topic}: {:?}", link.send(Out::Publish { topic: topic.clone(), body, expiry: None, retain: false }).await.map(drop));
            continue;
        }
        let node = steps.next().expect("which node").clone();
        let value = (step == "write").then(|| {
            let text = steps.next().expect("what value");
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
        });
        match link.send(Out::Node { node: node.clone(), value }).await {
            Ok(Some(m)) => println!("{node} = {}", String::from_utf8_lossy(&m.body)),
            Ok(None) => println!("{node} written"),
            Err(e) => println!("{node} failed: {e:?}"),
        }
    }
}
