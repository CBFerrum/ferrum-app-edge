//! Message buses as a way in: Kafka topics and AMQP queues, arriving by name so that a definition reads
//! them as it reads MQTT. The exports in `sinks.rs` speak the same protocols the other way, and the
//! connections are theirs.
//!
//! `amqp://reader:{secret.x}@broker/plant` names a vhost, and `subscribe` its queues, which it does not
//! create. `kafka://bus:9092?from=start`, with `subscribe` naming topics, reads what is already there
//! the first time, and what comes next without `from`. `amqps` and `kafkas` are the same over TLS.

use crate::adapter::Failure;
use crate::adapters::tags::query;
use crate::adapters::transport::{Message, Out, Transport};
use crate::sinks;
use async_trait::async_trait;
use reqwest::Url;
use tokio::sync::mpsc;

fn refused() -> Failure {
    Failure::NotSent("this only reads: an export is the way out".into())
}

/// `user:password@place` as the place alone and who to log in as. A password may hold anything.
fn parted(scheme: &str, rest: &str) -> Result<(Url, String, String), String> {
    let (login, place) = rest.rsplit_once('@').unwrap_or(("", rest));
    let (user, password) = login.split_once(':').unwrap_or((login, ""));
    let url = Url::parse(&format!("{scheme}://{place}")).map_err(|e| format!("{place}: {e}"))?;
    Ok((url, user.to_string(), password.to_string()))
}

pub struct AmqpSource {
    url: Url,
    user: String,
    password: String,
    queues: Vec<String>,
}

impl AmqpSource {
    pub fn new(scheme: &str, rest: &str, queues: &[String]) -> Result<AmqpSource, String> {
        let (url, user, password) = parted(scheme, rest)?;
        Ok(AmqpSource { url, user, password: if password.is_empty() { "guest".into() } else { password }, queues: queues.to_vec() })
    }
}

#[async_trait]
impl Transport for AmqpSource {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        sinks::amqp_read(&self.url, (&self.user, &self.password), &self.queues, tx, ready).await
    }

    async fn send(&self, _: Out) -> Result<Option<Message>, Failure> {
        Err(refused())
    }
}

#[cfg(feature = "kafka")]
pub struct KafkaSource {
    url: Url,
    password: Option<String>,
    topics: Vec<String>,
    from_start: bool,
    kept: Option<std::path::PathBuf>,
    kafka: sinks::Kafka,
}

#[cfg(feature = "kafka")]
impl KafkaSource {
    pub fn new(scheme: &str, rest: &str, topics: &[String], state_dir: Option<&std::path::Path>) -> Result<KafkaSource, String> {
        let (mut url, user, password) = parted(scheme, rest)?;
        let _ = url.set_username(&user);
        let name = rest.split('?').next().unwrap_or(rest).rsplit('@').next().unwrap_or(rest);
        let kept = state_dir.map(|d| d.join(format!("kafka-{:016x}.json", fnv(&format!("{name} {}", topics.join(" "))))));
        let from_start = query(rest, "from").as_deref() == Some("start");
        Ok(KafkaSource { url, password: Some(password).filter(|p| !p.is_empty()), topics: topics.to_vec(), from_start, kept, kafka: sinks::Kafka::default() })
    }
}

#[cfg(feature = "kafka")]
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(feature = "kafka")]
#[async_trait]
impl Transport for KafkaSource {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let reading = sinks::Reading { topics: &self.topics, from_start: self.from_start, kept: self.kept.as_deref() };
        self.kafka.read(&self.url, self.password.as_deref(), reading, tx, ready).await
    }

    async fn send(&self, _: Out) -> Result<Option<Message>, Failure> {
        Err(refused())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_can_hold_anything() {
        let (url, user, password) = parted("amqp", "reader:p/a@ss@broker:5673/plant").unwrap();
        assert_eq!((user.as_str(), password.as_str()), ("reader", "p/a@ss"));
        assert_eq!((url.host_str(), url.port(), url.path()), (Some("broker"), Some(5673), "/plant"));
        let (url, user, _) = parted("kafka", "bus:9092?from=start").unwrap();
        assert_eq!((url.host_str(), user.as_str()), (Some("bus"), ""));
    }
}
