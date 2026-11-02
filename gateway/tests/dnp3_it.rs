//! A DNP3 outstation through a whole conversation. opendnp3's own outstation has met this by hand
//! (`docs/compatibility.md`); it does not fit in CI, so what it showed stays true here against a stand-in
//! that does just as it did, and goes wrong in the ways it would not on request.

use ferrum_site::adapter::Failure;
use ferrum_site::adapters::dnp3::{Dnp3, link, take};
use ferrum_site::adapters::transport::{Message, Out, Transport};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;

const RESTARTED: u8 = 0x80;
const CONFIRM: u8 = 0;
const READ: u8 = 1;
const WRITE: u8 = 2;
const SELECT: u8 = 3;
const OPERATE: u8 = 4;
const DIRECT: u8 = 5;
const ENABLE: u8 = 0x14;

#[derive(Default)]
struct Log(Mutex<Vec<(u8, u8)>>);

impl Log {
    fn saw(&self, control: u8, function: u8) {
        self.0.lock().unwrap().push((control, function));
    }

    fn functions(&self) -> Vec<u8> {
        self.0.lock().unwrap().iter().map(|(_, f)| *f).collect()
    }

    fn confirmed_unasked(&self) -> bool {
        self.0.lock().unwrap().iter().any(|(c, f)| *f == CONFIRM && c & 0x10 != 0)
    }
}

async fn say(s: &mut TcpStream, application: &[u8]) {
    let mut user = vec![0xc0];
    user.extend(application);
    s.write_all(&link(0x44, 1, 10, &user)).await.unwrap();
}

/// Point 9 is in local control, and point 8 takes an operate and then says nothing at all.
async fn outstation(listener: TcpListener, log: Arc<Log>) {
    let (mut s, _) = listener.accept().await.unwrap();
    let (mut pending, mut restarted) = (Vec::new(), true);
    loop {
        if s.read_buf(&mut pending).await.unwrap_or(0) == 0 {
            return;
        }
        while let Some(frame) = take(&mut pending) {
            assert_eq!((frame.to, frame.from), (10, 1));
            let [_, control, function, objects @ ..] = &frame.user[..] else { continue };
            log.saw(*control, *function);
            let sequence = control & 0x0f;
            let iin = if restarted { RESTARTED } else { 0 };
            let mut answer = vec![0xc0 | sequence, 0x81, iin, 0];
            match *function {
                READ => {
                    // A static value, asking to be confirmed.
                    answer[0] |= 0x20;
                    answer.extend([30, 1, 0, 3, 3, 1, 42, 0, 0, 0]);
                }
                WRITE => {
                    assert_eq!(objects, [80, 1, 0, 7, 7, 0], "the restart indication, cleared");
                    restarted = false;
                    answer[2] = 0;
                }
                SELECT | OPERATE | DIRECT => {
                    let index = objects[5];
                    let mut echo = objects.to_vec();
                    if index == 9 {
                        *echo.last_mut().unwrap() = 7;
                    }
                    if index == 8 && *function == OPERATE {
                        continue;
                    }
                    answer.extend(echo);
                }
                CONFIRM => continue,
                _ => {}
            }
            say(&mut s, &answer).await;
            if *function == ENABLE {
                // An event with a time, asking to be confirmed, sent without being asked.
                let mut unasked = vec![0xf0, 0x82, 0, 0, 32, 7, 0x28, 1, 0, 3, 0, 1];
                unasked.extend(2.5f32.to_le_bytes());
                unasked.extend(&1_700_000_000_000i64.to_le_bytes()[..6]);
                say(&mut s, &unasked).await;
            }
        }
    }
}

async fn started(query: &str) -> (Arc<Dnp3>, mpsc::Receiver<Message>, Arc<Log>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Log::default());
    tokio::spawn(outstation(listener, log.clone()));
    let dnp3 = Arc::new(Dnp3::new(&format!("127.0.0.1:{port}?{query}")));
    let (tx, rx) = mpsc::channel(32);
    let (ready, mut up) = mpsc::channel(1);
    let runner = dnp3.clone();
    tokio::spawn(async move { runner.run(tx, ready).await });
    timeout(Duration::from_secs(5), up.recv()).await.expect("it came up");
    (dnp3, rx, log)
}

async fn value(rx: &mut mpsc::Receiver<Message>, point: &str) -> Value {
    loop {
        let m = timeout(Duration::from_secs(5), rx.recv()).await.expect(point).unwrap();
        if m.topic == point {
            return serde_json::from_slice(&m.body).unwrap();
        }
    }
}

fn write(node: &str, value: Value) -> Out {
    Out::Node { node: node.into(), value: Some(value) }
}

#[tokio::test]
async fn an_outstation_is_read_cleared_and_heard_unasked() {
    let (_dnp3, mut rx, log) = started("poll=60").await;
    assert_eq!(value(&mut rx, "analog-input:3").await, json!({"value": 42, "flags": 1}));
    let event = value(&mut rx, "analog-input:3").await;
    let event = if event["value"] == json!(42) { value(&mut rx, "analog-input:3").await } else { event };
    let event = if event["value"] == json!(42) { value(&mut rx, "analog-input:3").await } else { event };
    assert_eq!(event, json!({"value": 2.5, "flags": 1, "at": 1_700_000_000_000i64}), "what it sent by itself, with its own time");
    for _ in 0..100 {
        if log.confirmed_unasked() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(log.confirmed_unasked(), "an outstation holds an event until it is confirmed: {:?}", log.functions());
    let seen = log.functions();
    let cleared = seen.iter().position(|f| *f == WRITE).expect("the restart was cleared");
    assert!(seen[cleared..].contains(&READ), "and everything asked for again, since what it held is gone: {seen:?}");
}

#[tokio::test]
async fn a_command_is_selected_before_it_is_operated() {
    let (dnp3, _rx, log) = started("poll=60").await;
    dnp3.send(write("binary-output:2", json!(true))).await.expect("taken");
    dnp3.send(write("analog-output:3", json!(2.5))).await.expect("taken");
    let seen = log.functions();
    let order: Vec<u8> = seen.iter().copied().filter(|f| matches!(*f, SELECT | OPERATE)).collect();
    assert_eq!(order, [SELECT, OPERATE, SELECT, OPERATE]);
}

#[tokio::test]
async fn a_direct_outstation_is_only_operated() {
    let (dnp3, _rx, log) = started("poll=60&direct=true").await;
    dnp3.send(write("binary-output:2", json!(true))).await.expect("taken");
    assert!(!log.functions().contains(&SELECT));
    assert!(log.functions().contains(&DIRECT));
}

#[tokio::test]
async fn what_it_refuses_is_not_sent_and_what_it_never_answers_is_not_known() {
    let (dnp3, _rx, _) = started("poll=60").await;
    let Err(Failure::NotSent(why)) = dnp3.send(write("binary-output:9", json!(true))).await else { panic!("refused") };
    assert!(why.contains("local"), "{why}");
    assert!(matches!(dnp3.send(write("binary-output:2", json!(5))).await, Err(Failure::NotSent(_))), "not a value a relay takes");
    assert!(matches!(dnp3.send(write("analog-input:2", json!(5))).await, Err(Failure::NotSent(_))), "an input takes no commands");
    let silent = dnp3.send(write("binary-output:8", json!(true))).await;
    assert!(matches!(silent, Err(Failure::Unknown(_))), "it was selected and told to operate, and may have: {silent:?}");
}
