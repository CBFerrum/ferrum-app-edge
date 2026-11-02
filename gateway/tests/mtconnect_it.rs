//! An agent's sample stream. The agent here is a stand-in that speaks the HTTP of the MTConnect
//! standard: nothing else has checked this.

use ferrum_site::adapters::transport::{self, Limits, Message, Options};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;

fn xml(next: u64, value: &str) -> String {
    format!(
        r#"<MTConnectStreams><Header nextSequence="{next}"/><Streams><DeviceStream name="M1"><ComponentStream><Samples><Position dataItemId="x">{value}</Position></Samples></ComponentStream></DeviceStream></Streams></MTConnectStreams>"#
    )
}

fn part(body: &str) -> String {
    format!("--B\r\nContent-type: text/xml\r\nContent-length: {}\r\n\r\n{body}\r\n", body.len())
}

#[tokio::test]
async fn everything_now_and_then_the_stream_from_there() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (asked, mut requests) = mpsc::channel::<String>(4);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let asked = asked.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                let n = socket.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..n]).lines().next().unwrap_or_default().to_string();
                let _ = asked.send(request.clone()).await;
                if request.contains("/current") {
                    let body = xml(41, "1.0");
                    let _ =
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await;
                } else {
                    let stream = format!("{}{}", part(&xml(42, "2.0")), part(&xml(43, "3.0")));
                    let (first, second) = stream.split_at(stream.len() / 3);
                    let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace;boundary=B\r\nConnection: close\r\n\r\n").await;
                    let _ = socket.write_all(first.as_bytes()).await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let _ = socket.write_all(second.as_bytes()).await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            });
        }
    });
    let link = transport::open(&format!("mtconnect://{address}?interval=100"), Options::default(), &Limits::default()).unwrap();
    let (tx, mut rx) = mpsc::channel::<Message>(8);
    let (ready, mut up) = mpsc::channel(1);
    tokio::spawn(async move { link.run(tx, ready).await });
    let hear = async |rx: &mut mpsc::Receiver<Message>| timeout(Duration::from_secs(3), rx.recv()).await.expect("something").unwrap();
    let first = hear(&mut rx).await;
    assert_eq!(first.topic, "/current");
    assert!(String::from_utf8_lossy(&first.body).contains(">1.0<"));
    timeout(Duration::from_secs(3), up.recv()).await.unwrap();
    assert!(requests.recv().await.unwrap().contains("/current"));
    assert!(requests.recv().await.unwrap().contains("/sample?from=41&interval=100"), "from where the first look ended");
    let second = hear(&mut rx).await;
    assert!(String::from_utf8_lossy(&second.body).contains(">2.0<"), "a part is whole, however it arrived");
    let third = hear(&mut rx).await;
    assert!(String::from_utf8_lossy(&third.body).contains(">3.0<"));
}
