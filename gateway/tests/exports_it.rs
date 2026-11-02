//! Exports against the real thing. Each test names a service by an environment variable, and CI runs
//! them all as containers (`.github/workflows`); without the variable it says so and passes, so that
//! this file runs anywhere. What cannot run in a container is checked against a stand-in here.

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, Uri};
use chrono::Utc;
use ferrum_edge_contract::edge::Export;
use ferrum_site::adapters::transport::{self, Limits, Out};
use ferrum_site::export::Sender;
use ferrum_site::sinks::{self, Aws};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use uuid::Uuid;

fn service(name: &str) -> Option<String> {
    let found = std::env::var(name).ok();
    if found.is_none() {
        eprintln!("skipped: {name} is not set");
    }
    found
}

fn export(to: &str, topic: Option<&str>, sign_with: Option<&str>) -> Export {
    Export { name: "e".into(), to: to.into(), topic: topic.map(str::to_string), only: vec![], sign_with: sign_with.map(str::to_string) }
}

fn readings(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| json!({"device": format!("PRESS-{}", i % 3), "metric": "temp.c", "value": i as f64 + 0.5, "at": "2026-11-02T10:00:00Z"}).to_string())
        .collect()
}

fn sender() -> Sender {
    Sender::new(Limits::default())
}

async fn aws_call(base: &str, service: &str, headers: &[(&str, &str)], body: &[u8]) -> String {
    let host = base.trim_start_matches("http://");
    let aws = Aws { access: "test", secret: "test", region: "us-east-1", service };
    let mut request = reqwest::Client::new().post(format!("{base}/")).body(body.to_vec());
    for (k, v) in aws.sign("POST", host, "/", "", headers, body, Utc::now()) {
        if k != "host" {
            request = request.header(k, v);
        }
    }
    let response = request.send().await.unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    response.text().await.unwrap()
}

#[tokio::test]
async fn nats_takes_a_batch() {
    let Some(host) = service("FERRUM_TEST_NATS") else { return };
    let subject = format!("t{}", Uuid::now_v7().simple());
    let stream = TcpStream::connect(&host).await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("INFO"), "{line}");
    reader.get_mut().write_all(format!("CONNECT {{\"verbose\":false}}\r\nSUB {subject}.> 1\r\nPING\r\n").as_bytes()).await.unwrap();
    loop {
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        if line.starts_with("PONG") {
            break;
        }
    }
    let e = export(&format!("nats://{host}"), Some(&format!("{subject}.{{device}}.{{metric}}")), None);
    sender().deliver(&e, &readings(3), &json!({})).await.expect("taken");
    for i in 0..3 {
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with(&format!("MSG {subject}.PRESS-{i}.temp.c 1 ")), "{line}");
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(line.trim()).unwrap()["value"], json!(i as f64 + 0.5));
    }
}

#[tokio::test]
async fn nats_refuses_a_bad_token_out_loud() {
    let Some(host) = service("FERRUM_TEST_NATS_TOKEN") else { return };
    let e = export(&format!("nats://{host}"), Some("x"), Some("token"));
    let refused = sender().deliver(&e, &readings(1), &json!({"token": "wrong"})).await;
    assert!(refused.unwrap_err().contains("-ERR"));
    sender().deliver(&e, &readings(1), &json!({"token": "right"})).await.expect("taken");
}

#[tokio::test]
async fn amqp_is_confirmed_by_the_broker() {
    let Some(host) = service("FERRUM_TEST_AMQP") else { return };
    let manager = service("FERRUM_TEST_AMQP_MANAGER").expect("the management address too");
    let queue = format!("q{}", Uuid::now_v7().simple());
    let web = reqwest::Client::new();
    let created =
        web.put(format!("{manager}/api/queues/%2F/{queue}")).basic_auth("guest", Some("guest")).json(&json!({"durable": false})).send().await.unwrap();
    assert!(created.status().is_success(), "{}", created.status());

    let e = export(&format!("amqp://guest@{host}/%2F"), Some(&queue), Some("password"));
    sender().deliver(&e, &readings(50), &json!({"password": "guest"})).await.expect("confirmed");
    let got: Value = web
        .post(format!("{manager}/api/queues/%2F/{queue}/get"))
        .basic_auth("guest", Some("guest"))
        .json(&json!({"count": 100, "ackmode": "ack_requeue_false", "encoding": "auto"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got.as_array().unwrap().len(), 50, "{got}");
    assert_eq!(got[0]["properties"]["delivery_mode"], 2, "it outlives a restart of the broker");
    assert_eq!(serde_json::from_str::<Value>(got[0]["payload"].as_str().unwrap()).unwrap()["metric"], "temp.c");

    let wrong = sender().deliver(&e, &readings(1), &json!({"password": "wrong"})).await;
    assert!(wrong.is_err(), "the password is the broker's to refuse");
    let nowhere = export(&format!("amqp://guest@{host}/%2F?exchange=no-such-exchange"), Some("x"), Some("password"));
    let refused = sender().deliver(&nowhere, &readings(1), &json!({"password": "guest"})).await;
    assert!(refused.unwrap_err().contains("no-such-exchange"), "and it says why");
}

#[cfg(feature = "opcua")]
#[tokio::test]
async fn a_gateway_serves_its_readings_over_opc_ua_and_takes_no_writes() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let bind = format!("127.0.0.1:{port}");
    let dir = std::env::temp_dir().join(format!("ferrum-opcua-{}", Uuid::now_v7().simple()));
    let e = export(&format!("listen+opcua://{bind}"), None, None);
    assert!(Sender::new(Limits::default()).deliver(&e, &readings(1), &json!({})).await.is_err(), "a port is the site's to open");

    let sender = Sender::new(Limits { listen: vec![bind.clone()], tls_dir: Some(dir.clone()), ..Default::default() });
    sender.deliver(&e, &readings(2), &json!({})).await.expect("served");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let limits = Limits { tls_dir: Some(dir.clone()), ..Default::default() };
    let client = transport::open(&format!("opcua://{bind}/"), transport::Options::default(), &limits).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let (ready, mut up) = tokio::sync::mpsc::channel(1);
    let runner = client.clone();
    tokio::spawn(async move { runner.run(tx, ready).await });
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tokio::time::timeout(Duration::from_secs(10), up.recv()).await.expect("it connected");
    let node = "ns=2;s=PRESS-1/temp.c";
    let read = async |node: &str| {
        let m = client.send(Out::Node { node: node.into(), value: None }).await.expect("read").expect("a value");
        serde_json::from_slice::<Value>(&m.body).unwrap()
    };
    let said = read(node).await;
    assert_eq!(said["value"], json!(1.5), "the latest: {said}");
    assert!(said["time"].is_string(), "{said}");

    let refused = client.send(Out::Node { node: node.into(), value: Some(json!(9.0)) }).await;
    assert!(refused.is_err(), "nothing is written through here: {refused:?}");
    assert_eq!(read(node).await["value"], json!(1.5));

    let found = client.send(Out::Node { node: "browse:i=85".into(), value: None }).await.expect("browsed").expect("an answer");
    assert!(String::from_utf8_lossy(&found.body).contains("PRESS-1"), "a device is a folder");

    sender.keep_servers(&[]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(tokio::net::TcpStream::connect(&bind).await.is_err(), "the port closes when no export names it");
    let _ = std::fs::remove_dir_all(dir);
}

/// Connects, or says why not.
#[cfg(feature = "opcua")]
async fn reach(address: &str, limits: &Limits) -> Result<std::sync::Arc<dyn transport::Transport>, String> {
    let link = transport::open(address, transport::Options::default(), limits)?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let (ready, mut up) = tokio::sync::mpsc::channel(1);
    let runner = link.clone();
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let run = tokio::spawn(async move { runner.run(tx, ready).await });
    tokio::select! {
        Some(()) = up.recv() => Ok(link),
        ended = run => Err(format!("{ended:?}")),
    }
}

#[cfg(feature = "opcua")]
fn trust(store: &std::path::Path) {
    for file in std::fs::read_dir(store.join("rejected")).expect("something was kept back").flatten() {
        std::fs::rename(file.path(), store.join("trusted").join(file.file_name())).unwrap();
    }
}

#[cfg(feature = "opcua")]
#[tokio::test(flavor = "multi_thread")]
async fn a_served_login_wants_a_password_and_a_client_someone_trusted() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let bind = format!("127.0.0.1:{port}");
    let dir = std::env::temp_dir().join(format!("ferrum-opcua-{}", Uuid::now_v7().simple()));
    let (server, client) = (dir.join("server"), dir.join("client"));
    std::fs::create_dir_all(&client).unwrap();
    let e = export(&format!("listen+opcua://reader@{bind}"), None, Some("reading"));
    let sender = Sender::new(Limits { listen: vec![bind.clone()], tls_dir: Some(server.clone()), ..Default::default() });
    let secrets = json!({"reading": "s3cret"});
    sender.deliver(&e, &readings(2), &secrets).await.expect("served");
    tokio::time::sleep(Duration::from_secs(1)).await;

    let limits = Limits { tls_dir: Some(client.clone()), ..Default::default() };
    let open = format!("opcua://{bind}/");
    assert!(reach(&open, &limits).await.is_err(), "there is no way in without encryption");

    let secure = |password: &str| format!("opcua://{bind}/?security=Basic256Sha256&mode=SignAndEncrypt&user=reader&password={password}");
    assert!(reach(&secure("s3cret"), &limits).await.is_err(), "that server has not said it trusts this client");
    let kept = std::fs::read_dir(server.join("opcua-server/rejected")).unwrap().count();
    assert_eq!(kept, 1, "it is kept back for a person to look at");
    trust(&server.join("opcua-server"));
    assert!(reach(&secure("s3cret"), &limits).await.is_err(), "and this client has not said it trusts that server");
    trust(&client.join("opcua"));
    assert!(reach(&secure("wrong"), &limits).await.is_err(), "a wrong password gets nowhere, trusted or not");
    let link = reach(&secure("s3cret"), &limits).await.expect("each trusts the other now, and the password is right");
    let m = link.send(Out::Node { node: "ns=2;s=PRESS-1/temp.c".into(), value: None }).await.expect("read").expect("a value");
    assert_eq!(serde_json::from_slice::<Value>(&m.body).unwrap()["value"], json!(1.5));
    assert!(link.send(Out::Node { node: "ns=2;s=PRESS-1/temp.c".into(), value: Some(json!(9.0)) }).await.is_err(), "a login reads, and no more");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_table_of_someone_elses_takes_a_batch_once() {
    let url = std::env::var("FERRUM_SITE_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_site_test".into());
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(connection);
    let table = format!("t{}", Uuid::now_v7().simple());
    client
        .batch_execute(&format!("create table {table} (device text, metric text, value float8, at timestamptz, primary key (device, metric, at))"))
        .await
        .unwrap();
    let url: reqwest::Url = url.parse().unwrap();
    let to = format!("postgres://{}@{}:{}{}?table={table}", url.username(), url.host_str().unwrap(), url.port().unwrap_or(5432), url.path());
    let e = export(&to, None, Some("password"));
    let secrets = json!({"password": url.password().unwrap_or_default()});
    sender().deliver(&e, &readings(4), &secrets).await.expect("taken");
    sender().deliver(&e, &readings(4), &secrets).await.expect("a batch sent twice");
    let count: i64 = client.query_one(&format!("select count(*) from {table}"), &[]).await.unwrap().get(0);
    assert_eq!(count, 4, "lands once");
    let value: f64 = client
        .query_one(&format!("select value from {table} where device = 'PRESS-0' and metric = 'temp.c' order by value limit 1"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(value, 0.5);
    client.batch_execute(&format!("drop table {table}")).await.unwrap();
}

#[tokio::test]
async fn a_queue_can_be_read_as_well() {
    let Some(host) = service("FERRUM_TEST_AMQP") else { return };
    let manager = service("FERRUM_TEST_AMQP_MANAGER").expect("the management address too");
    let queue = format!("q{}", Uuid::now_v7().simple());
    let web = reqwest::Client::new();
    let created =
        web.put(format!("{manager}/api/queues/%2F/{queue}")).basic_auth("guest", Some("guest")).json(&json!({"durable": false})).send().await.unwrap();
    assert!(created.status().is_success(), "{}", created.status());
    let e = export(&format!("amqp://guest@{host}/%2F"), Some(&queue), Some("password"));
    sender().deliver(&e, &readings(3), &json!({"password": "guest"})).await.expect("confirmed");

    let topics = [queue.clone()];
    let options = transport::Options { subscribe: &topics, ..Default::default() };
    let link = transport::open(&format!("amqp://guest:guest@{host}/%2F"), options, &Limits::default()).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let (ready, _up) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move { link.run(tx, ready).await });
    for i in 0..3 {
        let m = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("a message").unwrap();
        assert_eq!(m.topic, queue, "by its routing key");
        assert_eq!(serde_json::from_slice::<Value>(&m.body).unwrap()["value"], json!(i as f64 + 0.5));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let left: Value = web.get(format!("{manager}/api/queues/%2F/{queue}")).basic_auth("guest", Some("guest")).send().await.unwrap().json().await.unwrap();
    assert_eq!(left["messages_unacknowledged"].as_i64().unwrap_or(0), 0, "each was acknowledged once it was handed on: {left}");
}

#[cfg(feature = "kafka")]
#[tokio::test]
async fn a_topic_can_be_read_as_well() {
    use rskafka::client::ClientBuilder;
    let Some(host) = service("FERRUM_TEST_KAFKA") else { return };
    let topic = format!("t{}", Uuid::now_v7().simple());
    let client = ClientBuilder::new(vec![host.clone()]).build().await.unwrap();
    client.controller_client().unwrap().create_topic(&topic, 1, 1, 5_000).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let e = export(&format!("kafka://{host}"), Some(&topic), None);
    sender().deliver(&e, &readings(2), &json!({})).await.expect("taken");
    let topics = [topic.clone()];
    let options = transport::Options { subscribe: &topics, ..Default::default() };
    let link = transport::open(&format!("kafka://{host}?from=start"), options, &Limits::default()).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let (ready, _up) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move { link.run(tx, ready).await });
    for i in 0..2 {
        let m = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.expect("a record").unwrap();
        assert_eq!(m.topic, topic);
        assert_eq!(serde_json::from_slice::<Value>(&m.body).unwrap()["value"], json!(i as f64 + 0.5));
    }
}

#[cfg(feature = "kafka")]
#[tokio::test]
async fn kafka_keeps_a_device_in_order() {
    use rskafka::client::ClientBuilder;
    use rskafka::client::partition::UnknownTopicHandling;
    let Some(host) = service("FERRUM_TEST_KAFKA") else { return };
    let topic = format!("t{}", Uuid::now_v7().simple());
    let client = ClientBuilder::new(vec![host.clone()]).build().await.unwrap();
    client.controller_client().unwrap().create_topic(&topic, 2, 1, 5_000).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let e = export(&format!("kafka://{host}"), Some(&topic), None);
    sender().deliver(&e, &readings(30), &json!({})).await.expect("taken");
    let mut seen = 0;
    for partition in 0..2 {
        let reader = client.partition_client(topic.clone(), partition, UnknownTopicHandling::Retry).await.unwrap();
        let (records, _) = reader.fetch_records(0, 1..1_000_000, 1_000).await.unwrap();
        let mut last: std::collections::HashMap<Vec<u8>, f64> = Default::default();
        for r in records {
            let value: Value = serde_json::from_slice(&r.record.value.unwrap()).unwrap();
            let device = r.record.key.unwrap();
            assert_eq!(value["device"].as_str().unwrap().as_bytes(), device, "keyed by its device");
            let n = value["value"].as_f64().unwrap();
            assert!(last.insert(device, n).is_none_or(|before| before < n), "in order");
            seen += 1;
        }
    }
    assert_eq!(seen, 30);
}

#[tokio::test]
async fn influx_stores_measurements() {
    let Some(base) = service("FERRUM_TEST_INFLUX") else { return };
    let host = base.trim_start_matches("http://");
    let e = export(&format!("influx://{host}/?org=ferrum&bucket=readings"), None, Some("token"));
    sender().deliver(&e, &readings(3), &json!({"token": "ferrum-test-token"})).await.expect("written");
    let flux = r#"from(bucket:"readings") |> range(start: 2026-11-01T00:00:00Z, stop: 2036-01-01T00:00:00Z) |> filter(fn: (r) => r._measurement == "temp.c") |> count()"#;
    let answer = reqwest::Client::new()
        .post(format!("{base}/api/v2/query?org=ferrum"))
        .header("authorization", "Token ferrum-test-token")
        .header("content-type", "application/vnd.flux")
        .body(flux)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(answer.contains("PRESS-0"), "{answer}");
    let refused = sender().deliver(&e, &readings(1), &json!({"token": "wrong"})).await;
    assert!(refused.unwrap_err().contains("401"), "a bad token is not a delivery");
}

#[tokio::test]
async fn clickhouse_stores_rows() {
    let Some(base) = service("FERRUM_TEST_CLICKHOUSE") else { return };
    let web = reqwest::Client::new();
    let table = format!("t{}", Uuid::now_v7().simple());
    let run = async |sql: String| {
        let r = web.post(format!("{base}/")).basic_auth("default", Some("test")).body(sql).send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        assert!(status.is_success(), "{status}: {text}");
        text
    };
    run(format!("create table {table} (device String, metric String, value Float64, at DateTime64(3)) engine = MergeTree order by (device, at)")).await;
    let host = base.trim_start_matches("http://");
    let e = export(&format!("clickhouse://default@{host}/{table}"), None, Some("password"));
    sender().deliver(&e, &readings(10), &json!({"password": "test"})).await.expect("written");
    assert_eq!(run(format!("select count() from {table}")).await.trim(), "10");
}

#[tokio::test]
async fn s3_keeps_a_batch_as_one_object() {
    let Some(base) = service("FERRUM_TEST_AWS") else { return };
    let bucket = format!("t{}", Uuid::now_v7().simple());
    let host = base.trim_start_matches("http://");
    let aws = Aws { access: "test", secret: "test", region: "us-east-1", service: "s3" };
    let path = format!("/{bucket}");
    let web = reqwest::Client::new();
    let mut made = web.put(format!("{base}{path}"));
    for (k, v) in aws.sign("PUT", host, &path, "", &[], b"", Utc::now()) {
        if k != "host" {
            made = made.header(k, v);
        }
    }
    assert!(made.send().await.unwrap().status().is_success());

    let e = export(&format!("s3://{bucket}/plant?region=us-east-1&endpoint={base}"), None, Some("keys"));
    let keys = json!({"keys": "test:test"});
    let batch = readings(4);
    sender().deliver(&e, &batch, &keys).await.expect("stored");
    sender().deliver(&e, &batch, &keys).await.expect("and again, the same");
    let query = "list-type=2";
    let mut list = web.get(format!("{base}{path}?{query}"));
    for (k, v) in aws.sign("GET", host, &path, query, &[], b"", Utc::now()) {
        if k != "host" {
            list = list.header(k, v);
        }
    }
    let listing = list.send().await.unwrap().text().await.unwrap();
    assert_eq!(listing.matches("<Key>plant/").count(), 1, "a batch sent twice is the same object: {listing}");
}

#[tokio::test]
async fn sqs_takes_more_than_one_call_can() {
    let Some(base) = service("FERRUM_TEST_AWS") else { return };
    let queue = format!("q{}", Uuid::now_v7().simple());
    let target = |t: &'static str| [("content-type", "application/x-amz-json-1.0"), ("x-amz-target", t)];
    let made = aws_call(&base, "sqs", &target("AmazonSQS.CreateQueue"), json!({"QueueName": queue}).to_string().as_bytes()).await;
    let url: Value = serde_json::from_str(&made).unwrap();
    let url = url["QueueUrl"].as_str().unwrap().to_string();
    let path = reqwest::Url::parse(&url).unwrap().path().to_string();
    let e = export(&format!("sqs://sqs.us-east-1.amazonaws.com{path}?endpoint={base}"), None, Some("keys"));
    sender().deliver(&e, &readings(25), &json!({"keys": "test:test"})).await.expect("sent");
    let attributes = json!({"QueueUrl": url, "AttributeNames": ["ApproximateNumberOfMessages"]}).to_string();
    let seen = aws_call(&base, "sqs", &target("AmazonSQS.GetQueueAttributes"), attributes.as_bytes()).await;
    let seen: Value = serde_json::from_str(&seen).unwrap();
    assert_eq!(seen["Attributes"]["ApproximateNumberOfMessages"], "25", "{seen}");
}

#[tokio::test]
async fn sns_publishes_in_tens() {
    let Some(base) = service("FERRUM_TEST_AWS") else { return };
    let form = [("content-type", "application/x-www-form-urlencoded")];
    let name = format!("t{}", Uuid::now_v7().simple());
    let made = aws_call(&base, "sns", &form, format!("Action=CreateTopic&Version=2010-03-31&Name={name}").as_bytes()).await;
    let arn = made.split("<TopicArn>").nth(1).and_then(|t| t.split("</TopicArn>").next()).unwrap();
    let e = export(&format!("sns://sns.us-east-1.amazonaws.com/?topic={arn}&endpoint={base}"), None, Some("keys"));
    sender().deliver(&e, &readings(12), &json!({"keys": "test:test"})).await.expect("published");
}

/// What Azure and Google's APIs are told, because neither runs in a container that proves anything:
/// this says the calls have the documented shape, and nothing about their service accepting them.
#[derive(Clone, Default)]
struct Heard(Arc<Mutex<Vec<(String, HeaderMap, Bytes)>>>);

async fn stand_in(answer: &'static str) -> (String, Heard) {
    let heard = Heard::default();
    let keep = heard.clone();
    let app = Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: Bytes| {
        let keep = keep.clone();
        async move {
            keep.0.lock().unwrap().push((uri.to_string(), headers, body));
            answer
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{address}"), heard)
}

#[tokio::test]
async fn azure_gets_a_batch_with_a_signature() {
    let (base, heard) = stand_in("").await;
    let e = export(&format!("azure://myhub.azure-devices.net/press-1?endpoint={base}"), None, Some("key"));
    sender().deliver(&e, &readings(2), &json!({"key": "c2VjcmV0"})).await.expect("sent");
    let hub = export(&format!("eventhub://plant.servicebus.windows.net/readings?endpoint={base}"), None, Some("key"));
    sender().deliver(&hub, &readings(2), &json!({"key": "Send:c2VjcmV0"})).await.expect("sent");
    let heard = heard.0.lock().unwrap();
    assert_eq!(heard[0].0, "/devices/press-1/messages/events?api-version=2021-04-12");
    assert!(heard[0].1["authorization"].to_str().unwrap().starts_with("SharedAccessSignature sr=myhub.azure-devices.net%2Fdevices%2Fpress-1&sig="));
    assert_eq!(heard[0].1["content-type"], "application/vnd.microsoft.iothub.json");
    let items: Value = serde_json::from_slice(&heard[0].2).unwrap();
    assert_eq!(items[0]["base64Encoded"], true);
    assert_eq!(heard[1].0, "/readings/messages?timeout=60&api-version=2014-01");
    assert!(heard[1].1["authorization"].to_str().unwrap().ends_with("&skn=Send"));
    assert_eq!(heard[1].1["content-type"], "application/vnd.microsoft.servicebus.json");
    let items: Value = serde_json::from_slice(&heard[1].2).unwrap();
    assert_eq!(serde_json::from_str::<Value>(items[1]["Body"].as_str().unwrap()).unwrap()["metric"], "temp.c");
}

#[tokio::test]
async fn pubsub_uses_the_emulator_without_a_token() {
    let (base, heard) = stand_in("{}").await;
    let e = export(&format!("pubsub://my-project/readings?endpoint={base}"), None, None);
    sender().deliver(&e, &readings(1), &json!({})).await.expect("sent");
    let heard = heard.0.lock().unwrap();
    assert_eq!(heard[0].0, "/v1/projects/my-project/topics/readings:publish");
    assert!(!heard[0].1.contains_key("authorization"));
    let body: Value = serde_json::from_slice(&heard[0].2).unwrap();
    assert!(body["messages"][0]["data"].is_string());
}

/// Signed the way a service account signs, and checked by `openssl`, which did not write it.
#[test]
fn a_google_assertion_is_a_signed_token() {
    use std::process::Command;
    let dir = std::env::temp_dir().join(format!("assertion-{}", Uuid::now_v7().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("key.pem");
    let made = Command::new("openssl").args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out"]).arg(&key).output();
    if !made.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: no openssl here");
        return;
    }
    let pem = std::fs::read_to_string(&key).unwrap();
    let account = json!({"client_email": "gw@p.iam.gserviceaccount.com", "private_key": pem, "token_uri": "https://oauth2.googleapis.com/token"});
    let token = sinks::google_assertion(&account, "https://www.googleapis.com/auth/pubsub", 1_800_000_000).unwrap();
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let plain = |part: &str| {
        let padded = format!("{}{}", part.replace('-', "+").replace('_', "/"), "=".repeat((4 - part.len() % 4) % 4));
        let out =
            Command::new("openssl").args(["base64", "-d", "-A"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        std::io::Write::write_all(&mut out.stdin.as_ref().unwrap(), padded.as_bytes()).unwrap();
        out.wait_with_output().unwrap().stdout
    };
    let claims: Value = serde_json::from_slice(&plain(parts[1])).unwrap();
    assert_eq!((claims["iss"].as_str(), claims["exp"].as_i64()), (Some("gw@p.iam.gserviceaccount.com"), Some(1_800_003_600)));
    let (public, signature, signed) = (dir.join("public.pem"), dir.join("signature"), dir.join("signed"));
    assert!(Command::new("openssl").args(["pkey", "-pubout", "-in"]).arg(&key).arg("-out").arg(&public).status().unwrap().success());
    std::fs::write(&signature, plain(parts[2])).unwrap();
    std::fs::write(&signed, format!("{}.{}", parts[0], parts[1])).unwrap();
    let verified = Command::new("openssl").args(["dgst", "-sha256", "-verify"]).arg(&public).arg("-signature").arg(&signature).arg(&signed).output().unwrap();
    assert!(verified.status.success(), "{}", String::from_utf8_lossy(&verified.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
}
