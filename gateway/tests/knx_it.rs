//! A tunnel to a KNX interface. The far end here is a stand-in written from the KNXnet/IP
//! specification, because nothing independent could be run as one: nothing else has checked this.

use ferrum_site::adapters::knx::Knx;
use ferrum_site::adapters::transport::{Message, Out, Transport};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::timeout;

fn frame(service: u16, body: &[u8]) -> Vec<u8> {
    let mut out = vec![6, 0x10];
    out.extend(service.to_be_bytes());
    out.extend(((body.len() + 6) as u16).to_be_bytes());
    out.extend(body);
    out
}

async fn hear(socket: &UdpSocket) -> (Vec<u8>, std::net::SocketAddr) {
    let mut buffer = [0u8; 512];
    let (n, peer) = timeout(Duration::from_secs(3), socket.recv_from(&mut buffer)).await.expect("something").unwrap();
    (buffer[..n].to_vec(), peer)
}

#[tokio::test]
async fn a_tunnel_opens_hears_the_bus_and_writes_to_it() {
    let interface = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = interface.local_addr().unwrap();
    let knx = Arc::new(Knx::new(&address.to_string(), &["1/2/3:dpt9".to_string(), "1/2/4:dpt1".to_string()]));
    let (tx, mut rx) = mpsc::channel::<Message>(8);
    let (ready, mut up) = mpsc::channel(1);
    let runner = knx.clone();
    tokio::spawn(async move { runner.run(tx, ready).await });

    let (asked, peer) = hear(&interface).await;
    assert_eq!(asked[2..4], [2, 5], "a connect request");
    assert_eq!(asked[22..], [4, 4, 2, 0], "for a tunnel on the link layer");
    interface.send_to(&frame(0x0206, &[7, 0, 8, 1, 127, 0, 0, 1, 0x0e, 0x57, 4, 4, 0x11, 1]), peer).await.unwrap();
    timeout(Duration::from_secs(3), up.recv()).await.unwrap();

    // The bus says 21.5 on 1/2/3, in a frame with no additional information.
    let cemi = |dst: [u8; 2], apci: u8, data: &[u8]| {
        let mut c = vec![0x29, 0, 0xbc, 0xe0, 0x11, 0x01, dst[0], dst[1], (data.len() + 1) as u8, 0, apci];
        c.extend(data);
        c
    };
    let tunnel = |sequence: u8, cemi: Vec<u8>| frame(0x0420, &[&[4, 7, sequence, 0][..], &cemi].concat());
    interface.send_to(&tunnel(0, cemi([0x0a, 0x03], 0x80, &[0x0c, 0x33])), peer).await.unwrap();
    let (ack, _) = hear(&interface).await;
    assert_eq!(ack[2..], [0x04, 0x21, 0, 10, 4, 7, 0, 0], "each is acknowledged");
    let m = timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
    assert_eq!(m.topic, "1/2/3");
    let said: Value = serde_json::from_slice(&m.body).unwrap();
    assert!((said["value"].as_f64().unwrap() - 21.5).abs() < 0.3, "{said}");

    // A repeat is acknowledged and not heard twice. A one-bit value lives in the APCI.
    interface.send_to(&tunnel(0, cemi([0x0a, 0x03], 0x80, &[0x0c, 0x33])), peer).await.unwrap();
    hear(&interface).await;
    interface.send_to(&tunnel(1, cemi([0x0a, 0x04], 0x81, &[])), peer).await.unwrap();
    hear(&interface).await;
    let m = timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
    assert_eq!((m.topic.as_str(), serde_json::from_slice::<Value>(&m.body).unwrap()["value"].clone()), ("1/2/4", json!(true)));

    // One this was not told about still says what it said.
    interface.send_to(&tunnel(2, cemi([0x0a, 0x05], 0x80, &[9])), peer).await.unwrap();
    hear(&interface).await;
    let m = timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&m.body).unwrap()["raw"], "09");

    knx.send(Out::Node { node: "1/2/5:dpt9".into(), value: Some(json!(20.0)) }).await.unwrap();
    let (written, _) = hear(&interface).await;
    assert_eq!(written[2..4], [0x04, 0x20]);
    assert_eq!(written[10..], [0x11, 0, 0xbc, 0xe0, 0, 0, 0x0a, 0x05, 3, 0, 0x80, 0x07, 0xd0], "2000 hundredths, in KNX's float");
    knx.send(Out::Node { node: "1/2/5:dpt9".into(), value: None }).await.unwrap();
    let (read, _) = hear(&interface).await;
    assert_eq!(read[8], 1, "numbered");
    assert_eq!(read[10..], [0x11, 0, 0xbc, 0xe0, 0, 0, 0x0a, 0x05, 1, 0, 0]);
    assert!(knx.send(Out::Node { node: "1/2/5".into(), value: None }).await.is_err(), "the type has to be said");
}
