//! An MTConnect agent, read as it is meant to be: everything now, and then its sample stream from
//! there on, so that nothing between two looks is lost. MTConnect is read-only, and this adds nothing
//! to that. What arrives is the agent's XML, which a definition reads with `format: xml`.
//!
//! `mtconnect://agent:5000?interval=1000`

use crate::adapter::Failure;
use crate::adapters::tags::{ANSWER, query};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use tokio::sync::mpsc;

/// What can wait for its end. A part that never ends costs this much.
const MAX_PART: usize = 16 << 20;

pub struct Mtconnect {
    base: String,
    interval: u32,
    http: reqwest::Client,
}

impl Mtconnect {
    pub fn new(scheme: &str, rest: &str, limits: &super::transport::Limits) -> Mtconnect {
        let place = rest.split('?').next().unwrap_or(rest).trim_end_matches('/');
        let web = if scheme == "mtconnects" { "https" } else { "http" };
        let http = reqwest::Client::builder().connect_timeout(ANSWER).redirect(limits.redirects()).build().unwrap_or_default();
        Mtconnect { base: format!("{web}://{place}"), interval: query(rest, "interval").and_then(|i| i.parse().ok()).unwrap_or(1000), http }
    }
}

/// Where the next thing to ask for starts, from what an agent said.
pub fn next_sequence(xml: &str) -> Option<u64> {
    let (_, rest) = xml.split_once("nextSequence=\"")?;
    rest.split('"').next()?.parse().ok()
}

/// The whole parts in `buffer`, taken out of it. Each says how long it is, so its content can hold
/// anything at all.
pub fn parts(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else { return out };
        let head = String::from_utf8_lossy(&buffer[..end]).to_lowercase();
        let Some(length) = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|n| n.trim().parse::<usize>().ok()) else {
            buffer.drain(..end + 4);
            continue;
        };
        if buffer.len() < end + 4 + length {
            return out;
        }
        out.push(buffer[end + 4..end + 4 + length].to_vec());
        buffer.drain(..end + 4 + length);
    }
}

#[async_trait]
impl Transport for Mtconnect {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let current = self.http.get(format!("{}/current", self.base)).send().await.map_err(|e| e.to_string())?;
        if !current.status().is_success() {
            return Err(format!("it answered {}", current.status()));
        }
        let text = current.text().await.map_err(|e| e.to_string())?;
        let next = next_sequence(&text).ok_or("that is not an MTConnect agent")?;
        let _ = tx.send(Message { topic: "/current".into(), body: text.into_bytes(), status: None, from: None }).await;
        let _ = ready.send(()).await;
        let url = format!("{}/sample?from={next}&interval={}&count=1000", self.base, self.interval);
        let mut stream = self.http.get(url).send().await.map_err(|e| e.to_string())?;
        if !stream.status().is_success() {
            return Err(format!("it answered {}", stream.status()));
        }
        let mut buffer = Vec::new();
        loop {
            let piece = tokio::select! {
                () = tx.closed() => return Ok(()),
                piece = stream.chunk() => piece.map_err(|e| e.to_string())?,
            };
            let Some(piece) = piece else { return Err("the stream ended".into()) };
            buffer.extend_from_slice(&piece);
            if buffer.len() > MAX_PART {
                return Err("that part never ended".into());
            }
            for part in parts(&mut buffer) {
                if tx.send(Message { topic: "/sample".into(), body: part, status: None, from: None }).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    async fn send(&self, _: Out) -> Result<Option<Message>, Failure> {
        Err(Failure::NotSent("MTConnect is read-only".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_is_cut_by_what_each_part_says() {
        let mut buffer = b"--b\r\nContent-type: text/xml\r\nContent-length: 5\r\n\r\nhello\r\n--b\r\nContent-length: 3\r\n\r\nby".to_vec();
        assert_eq!(parts(&mut buffer), [b"hello".to_vec()]);
        buffer.extend_from_slice(b"e\r\n");
        assert_eq!(parts(&mut buffer), [b"bye".to_vec()], "one that had not finished comes with its end");
    }

    #[test]
    fn where_to_go_on_from() {
        assert_eq!(next_sequence(r#"<Header creationTime="x" nextSequence="1234" firstSequence="1"/>"#), Some(1234));
        assert_eq!(next_sequence("<html/>"), None);
    }
}
