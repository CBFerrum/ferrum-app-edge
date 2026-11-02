//! A meter over the DLMS wrapper. The far end is a stand-in written from the Green Book, because no
//! independent server could be run: what is sent was compared by hand with Gurux's client instead.

use ferrum_site::adapters::dlms::Dlms;
use ferrum_site::adapters::tags::Trouble;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const ACCEPTED: [u8; 18] = [0x61, 0x10, 0xa1, 0x09, 6, 7, 0x60, 0x85, 0x74, 5, 8, 1, 1, 0xa2, 3, 2, 1, 0];
const REFUSED: [u8; 18] = [0x61, 0x10, 0xa1, 0x09, 6, 7, 0x60, 0x85, 0x74, 5, 8, 1, 1, 0xa2, 3, 2, 1, 1];

async fn hear(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut head = [0u8; 8];
    s.read_exact(&mut head).await.ok()?;
    assert_eq!(head[..2], [0, 1], "the wrapper's version");
    assert_eq!(head[2..6], [0, 16, 0, 1], "from the client, to the logical device");
    let mut body = vec![0u8; usize::from(u16::from_be_bytes([head[6], head[7]]))];
    s.read_exact(&mut body).await.ok()?;
    Some(body)
}

async fn say(s: &mut TcpStream, apdu: &[u8]) {
    let mut frame = vec![0, 1, 0, 1, 0, 16];
    frame.extend((apdu.len() as u16).to_be_bytes());
    frame.extend(apdu);
    s.write_all(&frame).await.unwrap();
}

/// A meter holding 1234.5 kWh as 12345 with a scaler of 10⁻¹, a serial number that needs two blocks,
/// and nothing else.
async fn meter(listener: TcpListener, password: &'static [u8]) {
    let (mut s, _) = listener.accept().await.unwrap();
    let asked = hear(&mut s).await.unwrap();
    assert_eq!(asked[0], 0x60);
    let logged_in = password.is_empty() || asked.windows(password.len()).any(|w| w == password);
    say(&mut s, if logged_in { &ACCEPTED } else { &REFUSED }).await;
    while let Some(asked) = hear(&mut s).await {
        match asked[..] {
            [0xc0, 1, _, 0, 3, 1, 0, 1, 8, 0, 255, 2, 0] => say(&mut s, &[0xc4, 1, 0xc1, 0, 0x06, 0, 0, 0x30, 0x39]).await,
            [0xc0, 1, _, 0, 3, 1, 0, 1, 8, 0, 255, 3, 0] => say(&mut s, &[0xc4, 1, 0xc1, 0, 0x02, 2, 0x0f, 0xff, 0x16, 30]).await,
            [0xc0, 1, _, 0, 1, 0, 0, 96, 1, 0, 255, 2, 0] => say(&mut s, &[0xc4, 2, 0xc1, 0, 0, 0, 0, 1, 0, 5, 0x09, 7, b'S', b'N', b'-']).await,
            [0xc0, 2, _, 0, 0, 0, 1] => say(&mut s, &[0xc4, 2, 0xc1, 1, 0, 0, 0, 2, 0, 4, b'4', b'2', b'0', b'0']).await,
            _ => say(&mut s, &[0xc4, 1, 0xc1, 1, 3]).await,
        }
    }
}

async fn start(password: &'static [u8]) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(meter(listener, password));
    port
}

#[tokio::test]
async fn a_meter_answers_by_obis_code_scaled_and_in_blocks() {
    let port = start(b"12345678").await;
    let mut d = Dlms::connect(&format!("127.0.0.1:{port}?client=16&server=1&password=12345678")).await.expect("an association");
    assert_eq!(d.read("1.0.1.8.0.255").await.ok(), Some(json!(1234.5)), "a Register comes back in its unit");
    assert_eq!(d.read("3:1.0.1.8.0.255").await.ok(), Some(json!(1234.5)), "the scaler is asked for once");
    assert_eq!(d.read("3:1.0.1.8.0.255:3").await.ok(), Some(json!([-1, 30])), "any attribute can be read as it is");
    assert_eq!(d.read("1:0.0.96.1.0.255").await.ok(), Some(json!("SN-4200")), "in two blocks");
    assert!(matches!(d.read("1.0.2.8.0.255").await, Err(Trouble::Refused(_))), "what a meter does not have is a refusal, not a lost link");
    assert!(matches!(d.read("nonsense").await, Err(Trouble::Refused(_))));
    assert!(matches!(d.write("1.0.1.8.0.255", &json!(0)).await, Err(Trouble::Refused(_))), "a meter is never written");
}

#[tokio::test]
async fn a_wrong_password_is_said_and_not_retried_as_a_fault_of_the_line() {
    let port = start(b"12345678").await;
    let refused = Dlms::connect(&format!("127.0.0.1:{port}?password=wrong")).await.err().expect("refused");
    assert!(refused.contains("refused the association"), "{refused}");
}

#[tokio::test]
async fn a_meter_that_is_not_there_says_so() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert!(Dlms::connect(&format!("127.0.0.1:{port}")).await.is_err());
}
