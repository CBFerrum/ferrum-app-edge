//! OPC UA: watches nodes, and reads, writes and calls on request. A node's id is its `topic` and what
//! arrives is `{value, status, time}`, so a definition reads OPC UA as it reads anything else.
//!
//! `opcua://host:4840/path?security=Basic256Sha256&mode=SignAndEncrypt&user=u&password=p&interval=500`
//!
//! `browse:i=85` shows what is under a node. `events:i=2253` in `subscribe` watches a node's events,
//! which carry `EventId` and `ConditionId`, so a condition can be acknowledged with a `call` to its
//! `i=9111`, with `{type: bytes, value: ...}` and `{type: text, value: ...}`. `history:ns=2;s=x`
//! reads what the server's historian holds since this last asked, an hour back the first time.
//!
//! A value the server calls bad is not passed on: it is not a reading. With security on, this site's
//! own certificate is created under `tls_dir/opcua`, and a server is trusted only once a person has
//! moved its certificate from `rejected` to `trusted` there, as OPC UA intends.

use crate::adapter::Failure;
use crate::adapters::transport::{Limits, Message, Out, Transport};
use async_trait::async_trait;
use chrono::Utc;
use opcua::client::{Client, ClientBuilder, HistoryReadAction, IdentityToken, Session, SubscriptionCallbacks};
use opcua::crypto::SecurityPolicy;
use opcua::types::{
    AttributeId, BrowseDescription, BrowseDirection, BrowseResultMask, ByteString, CallMethodRequest, DataValue, EventFilter, ExtensionObject, HistoryData,
    HistoryReadValueId, LocalizedText, MessageSecurityMode, MonitoredItemCreateRequest, MonitoringMode, MonitoringParameters, NodeId, ObjectTypeId,
    QualifiedName, ReadRawModifiedDetails, ReadValueId, ReferenceTypeId, SimpleAttributeOperand, StatusCode, TimestampsToReturn, UAString, UserTokenPolicy,
    Variant, VariantScalarTypeId, WriteValue,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};

/// Longest a request gets before what became of it is unknown.
const ANSWER: Duration = Duration::from_secs(10);
/// `browse:i=85` reads what is under a node, for finding what a server has.
const BROWSE: &str = "browse:";
/// `events:i=2253` in `subscribe` watches a node's events, which arrive under the same name.
const EVENTS: &str = "events:";
/// `history:ns=2;s=x` reads a node's history.
const HISTORY: &str = "history:";
/// What is asked of each event: every event has these, and `ConditionId` is only on a condition.
const EVENT_FIELDS: [&str; 6] = ["EventType", "SourceName", "Time", "Message", "Severity", "EventId"];
const CONDITION_FIELD: &str = "ConditionId";
/// How much history one ask brings, and how many pages of it.
const HISTORY_PAGE: u32 = 1000;
const HISTORY_PAGES: usize = 20;

/// What to watch: a node's value, or its events.
fn item(node: &str) -> Option<MonitoredItemCreateRequest> {
    let Some(events) = node.strip_prefix(EVENTS) else { return NodeId::from_str(node).ok().map(MonitoredItemCreateRequest::from) };
    let select = |name: &str| SimpleAttributeOperand {
        type_definition_id: ObjectTypeId::BaseEventType.into(),
        browse_path: Some(vec![QualifiedName::new(0, name)]),
        attribute_id: AttributeId::Value as u32,
        index_range: Default::default(),
    };
    // A condition's own node is how it is acknowledged, and it has no path to ask by. An empty path
    // and not a missing one: asyncua's server cannot take a missing one, and it is as good.
    let condition = SimpleAttributeOperand {
        type_definition_id: ObjectTypeId::ConditionType.into(),
        browse_path: Some(vec![]),
        attribute_id: AttributeId::NodeId as u32,
        index_range: Default::default(),
    };
    let clauses = EVENT_FIELDS.iter().map(|name| select(name)).chain([condition]).collect();
    let filter = EventFilter { select_clauses: Some(clauses), where_clause: Default::default() };
    Some(MonitoredItemCreateRequest {
        item_to_monitor: ReadValueId { node_id: NodeId::from_str(events).ok()?, attribute_id: AttributeId::EventNotifier as u32, ..Default::default() },
        monitoring_mode: MonitoringMode::Reporting,
        requested_parameters: MonitoringParameters {
            sampling_interval: 0.0,
            filter: ExtensionObject::from_message(filter),
            queue_size: 100,
            discard_oldest: true,
            ..Default::default()
        },
    })
}

pub struct Opcua {
    url: String,
    security: String,
    mode: MessageSecurityMode,
    user: Option<(String, String)>,
    interval: Duration,
    nodes: Vec<String>,
    store: Option<std::path::PathBuf>,
    session: Mutex<Option<Arc<Session>>>,
    /// How far each node's history has been read, for as long as this runs.
    since: Mutex<HashMap<String, chrono::DateTime<Utc>>>,
}

fn query(rest: &str, key: &str) -> Option<String> {
    let (_, q) = rest.split_once('?')?;
    q.split('&').filter_map(|p| p.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
}

/// What a value is, as JSON. What has no JSON of its own is shown as text.
fn json_of(v: &Variant) -> Value {
    match v {
        Variant::Empty => Value::Null,
        Variant::Boolean(b) => json!(b),
        Variant::SByte(n) => json!(n),
        Variant::Byte(n) => json!(n),
        Variant::Int16(n) => json!(n),
        Variant::UInt16(n) => json!(n),
        Variant::Int32(n) => json!(n),
        Variant::UInt32(n) => json!(n),
        Variant::Int64(n) => json!(n),
        Variant::UInt64(n) => json!(n),
        Variant::Float(n) => json!(n),
        Variant::Double(n) => json!(n),
        Variant::String(s) => json!(s.as_ref()),
        Variant::Array(a) => Value::Array(a.values.iter().map(json_of).collect()),
        Variant::ByteString(b) => json!(b.as_base64()),
        other => json!(other.to_string()),
    }
}

fn message(node: &str, value: &DataValue) -> Option<Message> {
    let status = value.status.unwrap_or(StatusCode::Good);
    if status.is_bad() {
        return None;
    }
    let time = value.source_timestamp.as_ref().or(value.server_timestamp.as_ref()).map(|t| t.as_chrono().to_rfc3339());
    let body = json!({"value": value.value.as_ref().map_or(Value::Null, json_of), "status": status.to_string(), "time": time});
    Some(Message { topic: node.to_string(), body: body.to_string().into_bytes(), status: None, from: None })
}

/// A value written as `{type: int32, value: 5}`, or as JSON, which says what it is well enough for
/// a string, a boolean or a double.
fn variant(v: &Value) -> Result<Variant, String> {
    if let (Some(kind), Some(value)) = (v["type"].as_str(), v.get("value")) {
        let target = match kind {
            "boolean" => VariantScalarTypeId::Boolean,
            "int8" => VariantScalarTypeId::SByte,
            "uint8" => VariantScalarTypeId::Byte,
            "int16" => VariantScalarTypeId::Int16,
            "uint16" => VariantScalarTypeId::UInt16,
            "int32" => VariantScalarTypeId::Int32,
            "uint32" => VariantScalarTypeId::UInt32,
            "int64" => VariantScalarTypeId::Int64,
            "uint64" => VariantScalarTypeId::UInt64,
            "float" => VariantScalarTypeId::Float,
            "double" => VariantScalarTypeId::Double,
            "string" => VariantScalarTypeId::String,
            "bytes" => return Ok(Variant::ByteString(ByteString::from_base64(value.as_str().unwrap_or_default()).ok_or("that is not base64")?)),
            "text" => return Ok(Variant::LocalizedText(Box::new(LocalizedText::new("", value.as_str().unwrap_or_default())))),
            other => return Err(format!("{other} is not a type here")),
        };
        return like(value, target);
    }
    Ok(match v {
        Value::Bool(b) => Variant::Boolean(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Variant::Int64(i),
            None => Variant::Double(n.as_f64().ok_or("that number has no form here")?),
        },
        Value::String(s) => Variant::String(UAString::from(s.as_str())),
        other => return Err(format!("{other} cannot be written")),
    })
}

/// `value` as `target`, refusing anything that would change it: a command must not reach a machine
/// rounded or wrapped.
fn like(value: &Value, target: VariantScalarTypeId) -> Result<Variant, String> {
    let wrong = || format!("{value} does not fit {target:?}");
    let source = variant(value)?;
    let cast = source.cast(target);
    if matches!(cast, Variant::Empty) {
        return Err(wrong());
    }
    let same = match (json_of(&cast), value) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64() || matches!(target, VariantScalarTypeId::Float),
        (a, b) => a == *b,
    };
    same.then_some(cast).ok_or_else(wrong)
}

fn node_of(text: &str) -> Result<NodeId, Failure> {
    NodeId::from_str(text).map_err(|_| Failure::NotSent(format!("{text} is not a node id")))
}

/// Stops the session's loop when the connection's run ends, however it ends.
struct Stop(tokio::task::AbortHandle);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Opcua {
    pub fn new(rest: &str, nodes: &[String], limits: &Limits) -> Result<Opcua, String> {
        let url = format!("opc.tcp://{}", rest.split('?').next().unwrap_or(rest));
        let mode = match query(rest, "mode").as_deref() {
            None | Some("None") => MessageSecurityMode::None,
            Some("Sign") => MessageSecurityMode::Sign,
            Some("SignAndEncrypt") => MessageSecurityMode::SignAndEncrypt,
            Some(other) => return Err(format!("{other} is not a security mode")),
        };
        let security = query(rest, "security").unwrap_or_else(|| "None".to_string());
        if security.parse::<SecurityPolicy>().is_err() {
            return Err(format!("{security} is not a security policy"));
        }
        if security != "None" && limits.tls_dir.is_none() {
            return Err("OPC UA security needs somewhere to keep certificates: set tls_dir".into());
        }
        let user = query(rest, "user").zip(query(rest, "password"));
        let interval = Duration::from_millis(query(rest, "interval").and_then(|i| i.parse().ok()).unwrap_or(1000).clamp(50, 60_000));
        let store = limits.tls_dir.as_ref().map(|d| d.join("opcua"));
        Ok(Opcua { url, security, mode, user, interval, nodes: nodes.to_vec(), store, session: Mutex::new(None), since: Mutex::default() })
    }

    fn client(&self) -> Result<Client, String> {
        let mut builder = ClientBuilder::new()
            .application_name("Ferrum site runtime")
            .application_uri("urn:ferrum-site")
            .product_uri("urn:ferrum-site")
            // The link reconnects, with its own backoff and its own account of why.
            .session_retry_limit(0);
        // A key takes seconds to make, and with security off nothing would use it.
        builder = match self.store.as_ref().filter(|_| self.security != "None") {
            Some(dir) => builder.pki_dir(dir.clone()).create_sample_keypair(true).trust_server_certs(false),
            None => builder.create_sample_keypair(false).trust_server_certs(false),
        };
        builder.client().map_err(|e| e.join("; "))
    }

    async fn session(&self) -> Result<Arc<Session>, Failure> {
        self.session.lock().await.clone().ok_or_else(|| Failure::NotSent("not connected".into()))
    }
}

/// What a historian holds for a node, oldest first.
async fn history(session: &Session, id: NodeId, from: chrono::DateTime<Utc>, until: chrono::DateTime<Utc>) -> Result<Vec<Value>, Failure> {
    let details = ReadRawModifiedDetails {
        is_read_modified: false,
        start_time: from.into(),
        end_time: until.into(),
        num_values_per_node: HISTORY_PAGE,
        return_bounds: false,
    };
    let (mut samples, mut continuation) = (Vec::new(), ByteString::null());
    for _ in 0..HISTORY_PAGES {
        let nodes = [HistoryReadValueId { node_id: id.clone(), continuation_point: continuation, ..Default::default() }];
        let asked = session.history_read(HistoryReadAction::ReadRawModifiedDetails(details.clone()), TimestampsToReturn::Both, false, &nodes);
        let got = tokio::time::timeout(ANSWER, asked)
            .await
            .map_err(|_| Failure::NotSent("it did not answer".into()))?
            .map_err(|e| Failure::NotSent(e.to_string()))?;
        let page = got.into_iter().next().ok_or_else(|| Failure::NotSent("it answered nothing".into()))?;
        if page.status_code.is_bad() {
            return Err(Failure::NotSent(format!("{id} has no history here: {}", page.status_code)));
        }
        let data = page.history_data.inner_as::<HistoryData>().and_then(|h| h.data_values.clone()).unwrap_or_default();
        samples.extend(data.iter().filter(|d| !d.status.unwrap_or(StatusCode::Good).is_bad()).map(|d| {
            let time = d.source_timestamp.as_ref().or(d.server_timestamp.as_ref()).map(|t| t.as_chrono().to_rfc3339());
            json!({"value": d.value.as_ref().map_or(Value::Null, json_of), "time": time})
        }));
        continuation = page.continuation_point;
        if continuation.is_null() {
            break;
        }
    }
    Ok(samples)
}

/// A request that had time to arrive has unknown effects if no answer came.
fn lost(e: impl std::fmt::Display) -> Failure {
    Failure::Unknown(e.to_string())
}

#[async_trait]
impl Transport for Opcua {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        let mut client = self.client()?;
        let endpoint: opcua::types::EndpointDescription = (self.url.as_str(), self.security.as_str(), self.mode, UserTokenPolicy::anonymous()).into();
        let identity = match &self.user {
            Some((user, password)) => IdentityToken::new_user_name(user.clone(), password.clone()),
            None => IdentityToken::Anonymous,
        };
        let connect = client.connect_to_matching_endpoint(endpoint, identity);
        let (session, event_loop) = tokio::time::timeout(ANSWER, connect).await.map_err(|_| "timed out connecting".to_string())?.map_err(|e| e.to_string())?;
        let mut loop_task = event_loop.spawn();
        let _stop = Stop(loop_task.abort_handle());
        let connected = tokio::time::timeout(ANSWER, session.wait_for_connection()).await.map_err(|_| "timed out starting a session".to_string())?;
        if !connected {
            return Err("the server would not start a session".into());
        }
        let watch = tx.clone();
        let events = tx.clone();
        let callback = SubscriptionCallbacks::new(
            |_| {},
            move |value, item| {
                let node = item.item_to_monitor().node_id.to_string();
                if let Some(m) = message(&node, &value) {
                    // A burst costs samples, never memory.
                    let _ = watch.try_send(m);
                }
            },
            move |fields, item| {
                let topic = format!("{EVENTS}{}", item.item_to_monitor().node_id);
                let said: Vec<Value> = fields.iter().flatten().map(json_of).collect();
                let names = EVENT_FIELDS.iter().copied().chain([CONDITION_FIELD]);
                let body = names.zip(said).map(|(name, value)| (name.to_string(), value)).collect::<serde_json::Map<_, _>>();
                let _ = events.try_send(Message { topic, body: Value::Object(body).to_string().into_bytes(), status: None, from: None });
            },
        );
        let subscription = session.create_subscription(self.interval, 30, 10, 0, 0, true, callback).await.map_err(|e| e.to_string())?;
        let items = self.nodes.iter().map(|n| item(n).ok_or_else(|| format!("{n} is not a node id"))).collect::<Result<Vec<_>, _>>()?;
        if !items.is_empty() {
            let made = session.create_monitored_items(subscription, TimestampsToReturn::Both, items).await.map_err(|e| e.to_string())?;
            let refused: Vec<String> =
                self.nodes.iter().zip(&made).filter(|(_, m)| m.result.status_code.is_bad()).map(|(n, m)| format!("{n}: {}", m.result.status_code)).collect();
            if refused.len() == made.len() {
                return Err(format!("the server refused every node: {}", refused.join(", ")));
            }
            for why in refused {
                tracing::warn!(server = %self.url, "not watching {why}");
            }
        }
        *self.session.lock().await = Some(session.clone());
        let _ = ready.send(()).await;
        let ended = tokio::select! {
            ended = &mut loop_task => ended.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string()),
            () = tx.closed() => {
                let _ = session.disconnect().await;
                *self.session.lock().await = None;
                return Ok(());
            }
        };
        *self.session.lock().await = None;
        Err(ended)
    }

    async fn send(&self, out: Out) -> Result<Option<Message>, Failure> {
        let session = self.session().await?;
        let read = |node: NodeId| ReadValueId { node_id: node, attribute_id: AttributeId::Value as u32, ..Default::default() };
        match out {
            Out::Node { node, value: None } if node.starts_with(BROWSE) => {
                let id = node_of(&node[BROWSE.len()..])?;
                let request = BrowseDescription {
                    node_id: id,
                    browse_direction: BrowseDirection::Forward,
                    reference_type_id: ReferenceTypeId::HierarchicalReferences.into(),
                    include_subtypes: true,
                    node_class_mask: 0,
                    result_mask: BrowseResultMask::All as u32,
                };
                let answer = tokio::time::timeout(ANSWER, session.browse(&[request], 0, None)).await;
                let results = answer.map_err(|_| Failure::NotSent("it did not answer".into()))?.map_err(|e| Failure::NotSent(e.to_string()))?;
                let children: Vec<Value> = results
                    .iter()
                    .flat_map(|r| r.references.iter().flatten())
                    .map(|c| json!({"node": c.node_id.node_id.to_string(), "name": c.browse_name.name.as_ref(), "label": c.display_name.text.as_ref(), "class": format!("{:?}", c.node_class)}))
                    .collect();
                let body = json!({"value": children}).to_string().into_bytes();
                Ok(Some(Message { topic: node, body, status: None, from: None }))
            }
            Out::Node { node, value: None } if node.starts_with(HISTORY) => {
                let id = node_of(&node[HISTORY.len()..])?;
                let until = Utc::now();
                let from = self.since.lock().await.get(&node).copied().unwrap_or(until - chrono::Duration::hours(1));
                let samples = history(&session, id, from, until).await?;
                let newest = samples.iter().filter_map(|s| s["time"].as_str()?.parse::<chrono::DateTime<Utc>>().ok()).max();
                // Just past the newest, since a range includes both its ends.
                self.since.lock().await.insert(node.clone(), newest.map_or(from, |t| t + chrono::Duration::milliseconds(1)));
                let body = json!({"value": samples}).to_string().into_bytes();
                Ok(Some(Message { topic: node, body, status: None, from: None }))
            }
            Out::Node { node, value: None } => {
                let id = node_of(&node)?;
                let answer = tokio::time::timeout(ANSWER, session.read(&[read(id)], TimestampsToReturn::Both, 0.0)).await;
                let values = answer.map_err(|_| Failure::NotSent("it did not answer".into()))?.map_err(|e| Failure::NotSent(e.to_string()))?;
                let value = values.first().ok_or_else(|| Failure::NotSent("it answered nothing".into()))?;
                message(&node, value).map(Some).ok_or_else(|| Failure::NotSent(format!("{node} has no good value")))
            }
            Out::Node { node, value: Some(value) } => {
                let id = node_of(&node)?;
                // Its current value says what type it wants: servers refuse anything else.
                let current = tokio::time::timeout(ANSWER, session.read(&[read(id.clone())], TimestampsToReturn::Neither, 0.0)).await;
                let current = current.map_err(|_| Failure::NotSent("it did not answer".into()))?.map_err(|e| Failure::NotSent(e.to_string()))?;
                let kind = current.first().and_then(|d| d.value.as_ref()).and_then(Variant::scalar_type_id);
                let typed = match kind {
                    Some(kind) if value.get("type").is_none() => like(&value, kind),
                    _ => variant(&value),
                }
                .map_err(Failure::NotSent)?;
                let write =
                    WriteValue { node_id: id, attribute_id: AttributeId::Value as u32, index_range: Default::default(), value: DataValue::value_only(typed) };
                let answer =
                    tokio::time::timeout(ANSWER, session.write(&[write])).await.map_err(|_| Failure::Unknown("it did not answer".into()))?.map_err(lost)?;
                let status = answer.first().copied().unwrap_or(StatusCode::BadUnexpectedError);
                if status.is_bad() {
                    return Err(Failure::NotSent(format!("{node} refused: {status}")));
                }
                Ok(None)
            }
            Out::Call { object, method, args } => {
                let args = args.iter().map(variant).collect::<Result<Vec<_>, _>>().map_err(Failure::NotSent)?;
                let request = CallMethodRequest { object_id: node_of(&object)?, method_id: node_of(&method)?, input_arguments: Some(args) };
                let answer =
                    tokio::time::timeout(ANSWER, session.call_one(request)).await.map_err(|_| Failure::Unknown("it did not answer".into()))?.map_err(lost)?;
                if answer.status_code.is_bad() {
                    return Err(Failure::NotSent(format!("{method} refused: {}", answer.status_code)));
                }
                let output: Vec<Value> = answer.output_arguments.iter().flatten().map(json_of).collect();
                let body = json!({"status": answer.status_code.to_string(), "output": output});
                Ok(Some(Message { topic: method, body: body.to_string().into_bytes(), status: None, from: None }))
            }
            _ => Err(Failure::NotSent("OPC UA takes a node".into())),
        }
    }
}
