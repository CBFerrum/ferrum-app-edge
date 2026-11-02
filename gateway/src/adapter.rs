//! What every adapter promises. An adapter says only what it can prove: an accepted message is not
//! work done, and an error must say whether the work can have gone out, because the answer decides
//! whether a retry is safe.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ferrum_ops_contract::telemetry::Class;
use ferrum_ops_contract::{Command, sync::SiteDevice};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// What a command went out on, written with the intent. Asking about it later, stopping it or
/// retrying it means going back to exactly this: another device that happens to take the same
/// operation knows nothing about it, and hearing nothing from one is not hearing it from the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    /// Its code when it was sent.
    pub device: String,
    /// Which of this site's adapters.
    pub adapter: String,
    /// What that adapter reached it through, in words that change when that does.
    pub endpoint: String,
}

/// Whole seconds for a message's own expiry, rounded up, or nothing if there is no time left: an
/// expired command is not sent with a second to live.
pub fn expiry_secs(left: Duration) -> Option<u32> {
    u32::try_from(left.as_millis().div_ceil(1000)).ok().filter(|s| *s > 0)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Dispatched {
    /// The equipment or its manager took it. `external` is its name for the work, if it has one.
    Accepted { external: Option<String> },
    /// It declined. Nothing happened.
    Rejected(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Failure {
    /// It did not leave this machine: safe to try again.
    NotSent(String),
    /// It may have arrived. Never retried: the outcome is unknown.
    Unknown(String),
}

/// What the equipment says about work now. Only `Completed`, `Failed` and `Aborted` settle it, and
/// their value is the evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Observation {
    Waiting,
    Running,
    Completed(Value),
    Failed(Value),
    Aborted(Value),
    /// The equipment has no record of it.
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Reading {
    Value {
        device: String,
        metric: String,
        value: f64,
        unit: Option<String>,
        class: Class,
        source_time: Option<DateTime<Utc>>,
    },
    /// `state`: Running, Idle, Stopped, Maintenance or Unknown.
    State {
        device: String,
        state: String,
        reason: Option<String>,
    },
    /// A fact about it that changes rarely.
    Attribute {
        device: String,
        key: String,
        value: Value,
    },
    /// Something that happened.
    Event {
        device: String,
        name: String,
        value: Value,
    },
    /// A device added itself, and the platform has yet to meet it.
    Joined {
        device: String,
        definition: String,
        token_hash: String,
    },
}

#[async_trait]
pub trait Adapter: Send + Sync {
    /// Hands the work over, once. The runtime has already made its intent durable. `left` is how long
    /// it has, by the site's clock: equipment that can hold a deadline is told it.
    async fn dispatch(&self, command: &Command, device: &SiteDevice, left: Duration) -> Result<Dispatched, Failure>;

    /// What the equipment says about work it took. Everything after a dispatch names the device it
    /// went to, and asks that device and no other.
    async fn observe(&self, command: &Command, device: &str, external: Option<&str>) -> Result<Observation, Failure>;

    /// After a crash between the intent and an answer: does the equipment know this command? `None`
    /// means it cannot say, and the outcome stays unknown. Only an adapter whose equipment keeps
    /// its own record may answer `NotFound`, because that makes a resend safe.
    async fn find(&self, _command: &Command, _device: &str) -> Result<Option<Observation>, Failure> {
        Ok(None)
    }

    /// Asks it to stop. Not a statement that it stopped: `observe` says that.
    async fn cancel(&self, command: &Command, device: &str, external: Option<&str>) -> Result<(), Failure>;

    /// What reaches this device now, for [`Binding::endpoint`]. `None` means nothing does.
    fn endpoint(&self, _device: &str) -> Option<String> {
        Some(String::new())
    }

    /// Readings since the last call.
    async fn read(&self) -> Result<Vec<Reading>, Failure> {
        Ok(vec![])
    }

    /// One line for the console and the platform's view of this site.
    fn health(&self) -> String {
        "ok".into()
    }
}
