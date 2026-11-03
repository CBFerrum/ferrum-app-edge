//! The loops: exchange with the platform, start what is ready, follow what is running, read the
//! equipment. They share nothing but the journal, so any of them can stall or crash without the
//! others losing anything, and a restart picks up from the file.

use crate::adapter::{Adapter, Binding, Dispatched, Failure, Observation, Reading};
use crate::adapters::defined::{Defined, LinkHealth};
use crate::adapters::hub::Keeper;
use crate::adapters::transport::Limits as OpenLimits;
use crate::adapters::{operator::OperatorTasks, sim::Sim};
use crate::clock::{Clock, Source};
use crate::config::{AdapterKind, Config};
use crate::crash_point;
use crate::export::{self, Sender};
use crate::journal::{Entry, Journal};
use crate::link::{Link, LinkError};
use crate::peer::Peer;
use crate::update::{self, Pending};
use chrono::{DateTime, Utc};
use ferrum_edge_contract::edge::{AlarmChange, Comparison, ConfigRequest, ConfigResponse, EdgeConfig, Export, FlowSpec, FlowTrace, Release, Rule, Watch};
use ferrum_edge_contract::flow::{self, Effect};
use ferrum_ops_contract::command::Report;
use ferrum_ops_contract::schedule::{self, Held, Resource, Task};
use ferrum_ops_contract::sync::{Fact, Health, SiteDevice};
use ferrum_ops_contract::telemetry::{Class, Limits, Measurement};
use ferrum_ops_contract::{Lifecycle, State, Step, SyncRequest, SyncResponse, VERSION};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration as StdDuration;
use tokio::sync::watch;
use tokio::time::timeout;
use uuid::Uuid;

#[derive(Clone)]
pub struct Slot {
    /// Its name at this site, which a binding keeps.
    pub name: String,
    pub adapter: Arc<dyn Adapter>,
    concurrency: u32,
}

/// What it takes to send a command, as the gate found it.
struct Go {
    device: SiteDevice,
    slot: Slot,
    binding: Binding,
    left: StdDuration,
}

/// The edge's own rules, and where each stands on each device.
#[derive(Default)]
struct Rules {
    rules: Vec<Rule>,
    watches: HashMap<(Uuid, Uuid), Watch>,
}

/// A flow in force, with what it remembers.
struct RunningFlow {
    id: Uuid,
    name: String,
    revision: i64,
    compiled: flow::Compiled,
    state: flow::State,
    /// Someone is watching it.
    debug: bool,
}

/// What is held for the platform of what passed through watched flows. Past this the oldest goes: a
/// trace is for a person watching, and is not worth slowing anything for.
const MAX_TRACES: usize = 1_000;

/// How often what flows remember is written down. A crash loses this much of a window or a hold.
const FLOW_SAVE_MS: i64 = 15_000;

/// How many readings go in one exchange. The platform takes more than this.
const MEASUREMENTS_AT_ONCE: i64 = 20_000;

pub struct Runtime {
    pub cfg: Config,
    pub journal: Arc<Journal>,
    pub clock: Clock,
    link: Link,
    adapters: Vec<(String, Arc<dyn Adapter>)>,
    by_device: HashMap<String, Slot>,
    pub boot: Uuid,
    seq: AtomicI64,
    pub started_at: DateTime<Utc>,
    /// When the platform last answered. A platform that hangs fails no call until it times out, so
    /// silence counts as down.
    heard: Mutex<Option<std::time::Instant>>,
    /// Cancels already passed to the equipment this run.
    cancelled: Mutex<HashSet<Uuid>>,
    latest: Mutex<HashMap<(Uuid, String), f64>>,
    repeats: Mutex<HashMap<(Uuid, String), (f64, u32)>>,
    last_state: Mutex<HashMap<String, String>>,
    last_attribute: Mutex<HashMap<(Uuid, String), Value>>,
    defined: Arc<Defined>,
    /// What flows can look up, kept up as readings pass.
    known: Mutex<flow::Known>,
    /// This gateway's device record, once the platform has said.
    gateway: Mutex<Option<Uuid>>,
    rules: Mutex<Rules>,
    flows: Mutex<Vec<RunningFlow>>,
    flows_saved: AtomicI64,
    traces: Mutex<VecDeque<FlowTrace>>,
    /// Why what this gateway was given could not all start.
    problems: Mutex<Vec<String>>,
    /// Signed calls go one at a time: each must carry a later time than the last that arrived.
    signing: tokio::sync::Mutex<()>,
    exports: Mutex<Vec<Export>>,
    sender: Sender,
    held_back: AtomicBool,
    /// What was read lately, for the console.
    pub recent: Mutex<crate::console::Recent>,
    export_shed: AtomicI64,
}

fn free_disk(path: &Path) -> Option<i64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: the path is a valid C string and `s` is ours to fill.
    let failed = unsafe { libc::statvfs(path.as_ptr(), &mut s) } != 0;
    (!failed).then(|| (s.f_bavail as i64).saturating_mul(s.f_frsize as i64))
}

/// What a device said about money, in the journal before it hears that it was heard.
struct Kept {
    journal: Arc<Journal>,
}

/// One start, one stop or one reading, however many times it arrives and over however many connections.
fn said_key(device: &str, action: &str, said: &Value) -> [u8; 32] {
    Sha256::new().chain_update(device).chain_update([0]).chain_update(action).chain_update([0]).chain_update(said.to_string()).finalize().into()
}

impl Keeper for Kept {
    fn keep(&self, device: &str, call: &str, action: &str, said: &Value) -> Result<(), String> {
        let key = said_key(device, action, said);
        let id = Uuid::from_slice(&key[..16]).map_err(|e| e.to_string())?;
        let mut body = json!({"code": device, "call": call, "action": action, "said": said});
        // 1.6 says what number a start got only in the answer, so it goes with the start.
        if action == "StartTransaction" {
            body["transaction"] = json!(self.number(device, said)?);
        }
        self.journal.add_fact(&Fact { fact_id: id, kind: "edge.transaction".into(), at_source: Utc::now(), actor: None, body }).map_err(|e| e.to_string())
    }

    fn number(&self, device: &str, said: &Value) -> Result<i32, String> {
        self.journal.transaction_number(&hex::encode(said_key(device, "StartTransaction", said)), Utc::now().timestamp_millis()).map_err(|e| e.to_string())
    }
}

impl Runtime {
    pub fn open(cfg: Config, key: ed25519_dalek::SigningKey) -> Result<Arc<Runtime>, String> {
        let journal = Arc::new(Journal::open(&cfg.journal_path()).map_err(|e| e.to_string())?);
        let key_id: Option<Uuid> = journal.get("key_id").map_err(|e| e.to_string())?.and_then(|v| v.parse().ok());
        // A box that only answers a peer has no platform, so nothing to enroll with.
        let key_id = key_id.or(cfg.peer_only.then(Uuid::nil)).ok_or("this site is not enrolled: run `ferrum-site enroll`")?;
        let link = Link::new(&cfg.server, &cfg.tenant, key.clone(), key_id);
        let (mut adapters, mut by_device): (Vec<(String, Arc<dyn Adapter>)>, HashMap<String, Slot>) = (Vec::new(), HashMap::new());
        for a in &cfg.adapters {
            let adapter: Arc<dyn Adapter> = match &a.kind {
                AdapterKind::Sim(c) => Arc::new(Sim::new(c.clone(), cfg.state_dir.join(format!("{}.sim", a.name)), a.devices.clone())),
                AdapterKind::Operator => Arc::new(OperatorTasks::new(journal.clone())),
                #[cfg(feature = "mqtt")]
                AdapterKind::Mqtt(c) => Arc::new(crate::adapters::mqtt::Mqtt::start(&a.name, c.clone())),
                #[cfg(feature = "mqtt")]
                AdapterKind::Vda5050(c) => Arc::new(crate::adapters::vda5050::Vda::start(&a.name, c.clone(), a.devices.clone())),
                #[cfg(feature = "modbus")]
                AdapterKind::Modbus(c) => Arc::new(crate::adapters::modbus::Modbus::new(c.clone())),
            };
            for d in &a.devices {
                by_device.insert(d.clone(), Slot { name: a.name.clone(), adapter: adapter.clone(), concurrency: a.concurrency.max(1) });
            }
            adapters.push((a.name.clone(), adapter));
        }
        for p in &cfg.peers {
            let adapter: Arc<dyn Adapter> = Arc::new(Peer::new(&p.url, key.clone()));
            for d in &p.devices {
                by_device.insert(d.clone(), Slot { name: format!("peer {}", p.name), adapter: adapter.clone(), concurrency: p.concurrency.max(1) });
            }
            adapters.push((format!("peer {}", p.name), adapter));
        }
        let limits = OpenLimits {
            drivers_dir: cfg.drivers_dir.clone(),
            allow: cfg.allow.clone(),
            listen: cfg.listen.clone(),
            tls_dir: Some(cfg.state_dir.join("tls")),
            firmware_dir: Some(cfg.state_dir.join("firmware")),
            files_dir: cfg.files_dir.clone(),
            state_dir: Some(cfg.state_dir.clone()),
            keeper: None,
        };
        let exporting = OpenLimits { allow: cfg.allow.clone(), listen: cfg.listen.clone(), tls_dir: Some(cfg.state_dir.join("tls")), ..OpenLimits::default() };
        let keeper: Arc<dyn Keeper> = Arc::new(Kept { journal: journal.clone() });
        let limits = OpenLimits { keeper: Some(keeper), ..limits };
        let defined = Arc::new(Defined::new(limits, json!(cfg.secrets)));
        adapters.push(("definitions".to_string(), defined.clone()));
        let high_water = journal.high_water().map_err(|e| e.to_string())?.map(|t| t.timestamp_millis()).unwrap_or(0);
        let offset = journal.get("clock_offset").map_err(|e| e.to_string())?.and_then(|v| v.parse().ok()).unwrap_or(0);
        let saved = journal.get("clock").map_err(|e| e.to_string())?.and_then(|t| serde_json::from_str(&t).ok());
        // A box that only answers a peer never hears the platform, so nothing would ever reset a margin.
        let drift = if cfg.peer_only { 0 } else { cfg.clock_drift_ppm };
        let runtime = Arc::new(Runtime {
            cfg,
            journal,
            clock: Clock::start(saved, high_water, offset, drift, Source::default()),
            link,
            adapters,
            by_device,
            boot: Uuid::now_v7(),
            seq: AtomicI64::new(0),
            started_at: Utc::now(),
            heard: Mutex::default(),
            cancelled: Mutex::default(),
            latest: Mutex::default(),
            repeats: Mutex::default(),
            last_state: Mutex::default(),
            last_attribute: Mutex::default(),
            defined,
            known: Mutex::default(),
            gateway: Mutex::new(None),
            rules: Mutex::default(),
            flows: Mutex::default(),
            flows_saved: AtomicI64::new(0),
            traces: Mutex::default(),
            problems: Mutex::default(),
            signing: tokio::sync::Mutex::new(()),
            exports: Mutex::default(),
            sender: Sender::new(exporting),
            held_back: AtomicBool::new(false),
            recent: Mutex::default(),
            export_shed: AtomicI64::new(0),
        });
        runtime.settle_update();
        runtime.restore_alarms();
        runtime.configure();
        Ok(runtime)
    }

    /// Definitions kept on this machine, by file name.
    fn local_definitions(&self) -> BTreeMap<String, String> {
        let Some(dir) = &self.cfg.definitions_dir else { return BTreeMap::new() };
        let Ok(files) = std::fs::read_dir(dir) else { return BTreeMap::new() };
        let mut out = BTreeMap::new();
        for path in files.filter_map(|f| f.ok()).map(|f| f.path()).filter(|p| p.extension().is_some_and(|e| e == "yaml" || e == "yml")) {
            if let (Some(name), Ok(text)) = (path.file_stem().map(|s| s.to_string_lossy().to_string()), std::fs::read_to_string(&path)) {
                out.insert(name, text);
            }
        }
        out
    }

    /// What this gateway holds from the platform, as it was last told.
    fn managed(&self) -> Option<EdgeConfig> {
        let text = self.journal.get("edge_config").ok().flatten()?;
        serde_json::from_str(&text).ok()
    }

    /// Becomes what its configuration says. What is on this machine wins over what the platform
    /// sent, and what has not changed keeps its connections.
    fn configure(&self) {
        let managed = self.managed().filter(|_| self.cfg.managed).unwrap_or_default();
        let mut definitions = managed.definitions.clone();
        definitions.extend(self.local_definitions());
        let mut devices = self.cfg.devices.clone();
        for d in &managed.devices {
            if !devices.iter().any(|mine| mine.code == d.code) && !self.by_device.contains_key(&d.code) {
                devices.push(d.clone());
            }
        }
        let mut problems = self.defined.apply(&definitions, &devices);
        self.defined.allow_joining(&managed.provisioning);
        *self.gateway.lock().unwrap_or_else(|e| e.into_inner()) = managed.gateway;
        problems.extend(self.start_flows(managed.flows));
        for p in &problems {
            tracing::warn!("{p}");
        }
        *self.problems.lock().unwrap_or_else(|e| e.into_inner()) = problems;
        self.rules.lock().unwrap_or_else(|e| e.into_inner()).rules = managed.rules;
        #[cfg(feature = "opcua")]
        self.sender.keep_servers(&managed.exports);
        *self.exports.lock().unwrap_or_else(|e| e.into_inner()) = managed.exports;
    }

    /// Becomes the flows it was given. One unchanged keeps what it remembers, and a new revision
    /// starts afresh: its nodes may mean something else now.
    fn start_flows(&self, specs: Vec<FlowSpec>) -> Vec<String> {
        let saved: HashMap<String, flow::State> = self.journal.get("flow_state").ok().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let mut running = self.flows.lock().unwrap_or_else(|e| e.into_inner());
        let mut was: Vec<RunningFlow> = running.drain(..).collect();
        let mut problems = Vec::new();
        for spec in specs {
            if let Some(i) = was.iter().position(|f| f.id == spec.id && f.revision == spec.revision) {
                let mut same = was.swap_remove(i);
                same.debug = spec.debug;
                running.push(same);
                continue;
            }
            match flow::parse(&spec.body) {
                Ok(compiled) => {
                    let state = saved.get(&format!("{}:{}", spec.id, spec.revision)).cloned().unwrap_or_default();
                    running.push(RunningFlow { id: spec.id, name: spec.name, revision: spec.revision, compiled, state, debug: spec.debug });
                }
                Err(why) => problems.push(format!("flow {}: {why}", spec.name)),
            }
        }
        problems
    }

    /// Passes something a device did to every flow, and does what they make of it.
    fn feed(&self, input: &flow::Input, devices: &HashMap<String, SiteDevice>, batch: &mut Vec<Measurement>) {
        let now = self.clock.now();
        let mut known = self.known.lock().unwrap_or_else(|e| e.into_inner());
        known.note(input);
        let mut running = self.flows.lock().unwrap_or_else(|e| e.into_inner());
        for f in running.iter_mut() {
            let output = f.compiled.input_knowing(&mut f.state, input, now, &known);
            self.watched(f, &output, now);
            self.act(f.id, &f.name, output, devices, batch);
        }
    }

    /// What the clock alone causes in flows, and now and then writing down where they stand.
    fn tick_flows(&self, devices: &HashMap<String, SiteDevice>, batch: &mut Vec<Measurement>) {
        let now = self.clock.now();
        let known = self.known.lock().unwrap_or_else(|e| e.into_inner());
        let mut running = self.flows.lock().unwrap_or_else(|e| e.into_inner());
        for f in running.iter_mut() {
            let output = f.compiled.tick_knowing(&mut f.state, now, &known);
            self.watched(f, &output, now);
            self.act(f.id, &f.name, output, devices, batch);
        }
        let now_ms = self.clock.now_ms();
        if !running.is_empty() && now_ms - self.flows_saved.load(Ordering::Relaxed) >= FLOW_SAVE_MS {
            self.flows_saved.store(now_ms, Ordering::Relaxed);
            let states: HashMap<String, &flow::State> = running.iter().map(|f| (format!("{}:{}", f.id, f.revision), &f.state)).collect();
            let _ = self.journal.set("flow_state", &json!(states).to_string());
        }
    }

    fn watched(&self, f: &RunningFlow, output: &flow::Output, at: DateTime<Utc>) {
        if !f.debug || output.trace.is_empty() {
            return;
        }
        let mut traces = self.traces.lock().unwrap_or_else(|e| e.into_inner());
        traces.extend(output.trace.iter().cloned().map(|step| FlowTrace::new(f.id, at, step)));
        let over = traces.len().saturating_sub(MAX_TRACES);
        traces.drain(..over);
    }

    /// Does what a flow decided. Everything here is recorded before it counts, and nothing here
    /// reaches equipment: a command is only a request, for the ledger to decide.
    fn act(&self, id: Uuid, name: &str, output: flow::Output, devices: &HashMap<String, SiteDevice>, batch: &mut Vec<Measurement>) {
        for p in output.problems {
            tracing::warn!(flow = %name, "{p}");
        }
        let fact = |kind: &str, body: Value| {
            let _ = self.journal.add_fact(&Fact { fact_id: Uuid::now_v7(), kind: kind.into(), at_source: self.clock.now(), actor: None, body });
        };
        let known = |code: &Option<String>| code.as_ref().and_then(|c| devices.get(c));
        for effect in output.effects {
            match effect {
                Effect::Alarm { node, name: title, severity, device, raised, value } => {
                    let Some(d) = known(&device) else {
                        tracing::warn!(flow = %name, "{node} has no device to raise an alarm on");
                        continue;
                    };
                    let change = AlarmChange { rule: id, device: d.id, raised, value, node: Some(node), name: Some(title), severity: Some(severity) };
                    fact("edge.alarm", json!(change));
                }
                Effect::Event { device, name: event, value } => {
                    if let Some(d) = known(&device) {
                        fact("edge.event", json!({"device": d.id, "name": event, "value": value}));
                    }
                }
                Effect::Attribute { device, key, value } => {
                    if let Some(d) = known(&device) {
                        fact("edge.attribute", json!({"device": d.id, "key": key, "value": value}));
                    }
                }
                Effect::Metric { device, name: metric, value, unit } => {
                    if let Some(d) = known(&device) {
                        batch.push(Measurement {
                            device_id: d.id,
                            metric,
                            value,
                            unit,
                            source_time: self.clock.now(),
                            boot: self.boot,
                            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
                            quality: ferrum_ops_contract::Quality::Good,
                            class: Class::Sample,
                        });
                    }
                }
                Effect::Notify { device, title } => fact("edge.notify", json!({"flow": id, "device": known(&device).map(|d| d.id), "title": title})),
                Effect::Export { to, body } => {
                    // Nothing would ever take it from a queue with no export behind it.
                    if !self.exports.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|e| e.name == to) {
                        tracing::warn!(flow = %name, "there is no export called {to} at this site");
                        continue;
                    }
                    if let Ok(shed) = self.journal.add_exports(&to, &[body.to_string()], export::BUDGET) {
                        self.export_shed.fetch_add(shed, Ordering::Relaxed);
                    }
                }
                Effect::Reply { device, id: asked, value } => {
                    if let Some(code) = device {
                        self.defined.reply(&code, &asked, &value);
                    }
                }
                Effect::Log { node, device, text } => tracing::info!(flow = %name, node = %node, device = device.as_deref().unwrap_or("-"), "{text}"),
                Effect::Command { node, device, operation, payload, expires_secs } => {
                    let Some(d) = known(&device) else {
                        tracing::warn!(flow = %name, "{node} has no device to ask {operation} of");
                        continue;
                    };
                    fact(
                        "edge.request",
                        json!({"flow": id, "node": node, "device": d.id, "operation": operation, "payload": payload, "expires_secs": expires_secs}),
                    );
                }
            }
        }
    }

    /// An adapter on this machine, for a peer that asks. What is itself reached through a peer is
    /// not offered on: one hop, so that nobody can build a loop.
    pub fn local_adapter(&self, code: &str) -> Option<Arc<dyn Adapter>> {
        let slot = self.adapter_for(code)?;
        (!self.cfg.peers.iter().any(|p| p.devices.iter().any(|d| d == code))).then_some(slot.adapter)
    }

    /// What this machine's equipment has said, for the peer that holds authority over it.
    pub async fn read_local(&self) -> Vec<Reading> {
        let mut out = Vec::new();
        for (name, a) in self.adapters.iter().filter(|(n, _)| !n.starts_with("peer ")) {
            match timeout(self.call_limit(), a.read()).await {
                Ok(Ok(readings)) => out.extend(readings),
                Ok(Err(e)) => tracing::debug!(adapter = %name, "read: {e:?}"),
                Err(_) => tracing::debug!(adapter = %name, "read timed out"),
            }
        }
        out
    }

    /// Queues measurements for whatever wants them.
    fn offer_exports(&self, batch: &[Measurement], devices: &HashMap<Uuid, SiteDevice>) {
        let exports = self.exports.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for e in &exports {
            let lines: Vec<String> = batch
                .iter()
                .filter(|m| export::wanted(e, &m.metric))
                .map(|m| json!({"device": devices.get(&m.device_id).map(|d| d.code.as_str()), "metric": m.metric, "value": m.value, "unit": m.unit, "at": m.source_time, "quality": m.quality}).to_string())
                .collect();
            if !lines.is_empty()
                && let Ok(shed) = self.journal.add_exports(&e.name, &lines, export::BUDGET)
            {
                self.export_shed.fetch_add(shed, Ordering::Relaxed);
            }
        }
    }

    /// One round of delivery. An export that is down waits and holds up nothing else.
    pub async fn deliver_exports(&self) {
        let exports = self.exports.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for e in &exports {
            let Ok((bodies, upto)) = self.journal.exports(&e.name, export::BATCH) else { continue };
            if bodies.is_empty() {
                continue;
            }
            match self.sender.deliver(e, &bodies, &json!(self.cfg.secrets)).await {
                Ok(()) => {
                    let _ = self.journal.exports_gone(&e.name, upto);
                }
                Err(why) => tracing::debug!(export = %e.name, "{why}"),
            }
        }
    }

    /// Deals with an update that was under way when this started.
    fn settle_update(&self) {
        let Ok(exe) = std::env::current_exe() else { return };
        let pending: Option<Pending> = self.journal.get("update_pending").ok().flatten().and_then(|t| serde_json::from_str(&t).ok());
        match update::at_start(&exe, pending, env!("CARGO_PKG_VERSION")) {
            Ok(Some(p)) => {
                let _ = self.journal.set("update_pending", &json!(p).to_string());
            }
            Ok(None) => {
                let _ = self.journal.clear("update_pending");
            }
            Err(version) => {
                let _ = self.journal.clear("update_pending");
                let _ = self.journal.set("update_refused", &version);
                let _ = self.journal.notice(self.clock.now_ms(), &format!("{version} kept failing, so this went back to what worked before"));
                std::process::exit(update::RESTART);
            }
        }
    }

    /// Takes a release that is meant for this site. Everything is checked before anything changes,
    /// and work in hand is not interrupted: it waits for a moment with nothing running.
    async fn consider_update(&self, release: &Release) -> Result<(), String> {
        let refused = self.journal.get("update_refused").ok().flatten();
        update::judge(release, env!("CARGO_PKG_VERSION"), &self.cfg.update_keys, self.clock.now(), refused.as_deref())?;
        let counts = self.journal.counts().map_err(|e| e.to_string())?;
        if counts.active > 0 || counts.unknown > 0 {
            return Err("waiting for what is running to end".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let download = self.cfg.state_dir.join("update.bin");
        let bytes = self.sender.fetch(&release.url).await?;
        std::fs::write(&download, bytes).map_err(|e| e.to_string())?;
        // Written first: a build that went in unrecorded could never be rolled back. A record with no
        // build behind it ends at the next start, because that start is not running this version.
        let pending = Pending { version: release.version.clone(), tries: 0 };
        self.journal.set("update_pending", &json!(pending).to_string()).map_err(|e| e.to_string())?;
        update::install(&exe, &download, release)?;
        let _ = self.journal.notice(self.clock.now_ms(), &format!("Updated to {}: restarting", release.version));
        std::process::exit(update::RESTART);
    }

    fn restore_alarms(&self) {
        let raised: Vec<(Uuid, Uuid)> = self.journal.get("alarms_raised").ok().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        self.rules.lock().unwrap_or_else(|e| e.into_inner()).watches = raised.into_iter().map(|k| (k, Watch::still_raised())).collect();
    }

    /// Writes down an alarm's change: a fact for the platform, and what is raised, for a restart.
    fn alarm(&self, rules: &Rules, rule: Uuid, device: Uuid, raised: bool, value: Option<f64>) {
        let change = AlarmChange { rule, device, raised, value, node: None, name: None, severity: None };
        let fact = Fact { fact_id: Uuid::now_v7(), kind: "edge.alarm".into(), at_source: self.clock.now(), actor: None, body: json!(change) };
        let _ = self.journal.add_fact(&fact);
        let up: Vec<&(Uuid, Uuid)> = rules.watches.iter().filter(|(_, w)| w.raised()).map(|(k, _)| k).collect();
        let _ = self.journal.set("alarms_raised", &json!(up).to_string());
    }

    fn judge(&self, code: &str, device: Uuid, metric: &str, value: f64) {
        let at = self.clock.now();
        let mut guard = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let matching: Vec<Rule> =
            guard.rules.iter().filter(|r| r.metric == metric && (r.devices.is_empty() || r.devices.iter().any(|d| d == code))).cloned().collect();
        for rule in matching {
            let changed = guard.watches.entry((rule.id, device)).or_default().feed(&rule, value, at);
            if let Some(raised) = changed {
                self.alarm(&guard, rule.id, device, raised, Some(value));
            }
        }
    }

    /// Silence is only noticed by the clock.
    fn tick_rules(&self) {
        let now = self.clock.now();
        let mut guard = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let rules = guard.rules.clone();
        for rule in rules.iter().filter(|r| r.when == Comparison::Silent) {
            let keys: Vec<(Uuid, Uuid)> = guard.watches.keys().filter(|k| k.0 == rule.id).copied().collect();
            for key in keys {
                let changed = guard.watches.get_mut(&key).and_then(|w| w.tick(rule, now));
                if let Some(raised) = changed {
                    self.alarm(&guard, rule.id, key.1, raised, None);
                }
            }
        }
    }

    pub fn links(&self) -> Vec<LinkHealth> {
        self.defined.links()
    }

    /// Asks the platform what this gateway should be. Nothing here can stop work: a site keeps
    /// running what it was last told.
    pub async fn exchange_config(&self) {
        let have = self.managed().map(|c| c.version).unwrap_or_default();
        let request = ConfigRequest {
            have,
            strangers: self.defined.strangers(),
            problems: self.problems.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            links: json!(self.defined.links()),
            runtime: env!("CARGO_PKG_VERSION").to_string(),
            traces: self.traces.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect(),
        };
        let answer = {
            let _turn = self.signing.lock().await;
            self.link.edge_config(&request, self.clock.signing_ms()).await
        };
        match answer {
            Ok(ConfigResponse { config: Some(config) }) => {
                let text = serde_json::to_string(&config).unwrap_or_default();
                if let Err(e) = self.journal.set("edge_config", &text) {
                    tracing::warn!("could not keep the new configuration: {e}");
                    return;
                }
                self.configure();
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("edge configuration: {e:?}"),
        }
        if let Some(release) = self.managed().and_then(|c| c.release)
            && let Err(why) = self.consider_update(&release).await
        {
            tracing::debug!("release {}: {why}", release.version);
        }
    }

    fn link_up(&self) -> bool {
        let quiet = StdDuration::from_secs(self.cfg.sync_secs * 3 + 5);
        self.heard.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|at| at.elapsed() < quiet)
    }

    fn epoch(&self) -> i64 {
        self.journal.get("epoch").ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn lease_until(&self) -> Option<DateTime<Utc>> {
        self.journal.get("lease_until").ok().flatten().and_then(|v| v.parse().ok())
    }

    fn report(&self, command: Uuid, state: State, detail: Value, evidence: Option<Value>) -> Report {
        Report { report_id: Uuid::now_v7(), command_id: command, state, at_source: self.clock.now(), epoch: self.epoch(), detail, evidence }
    }

    fn establish(&self, r: Report, external: Option<&str>) {
        if let Err(e) = self.journal.establish(&r, external) {
            // The worst case here: what happened cannot be written down. Nothing new starts while
            // that is so, because the gate sees the same failure.
            tracing::error!(command = %r.command_id, state = r.state.as_str(), "not recorded: {e}");
        }
    }

    /// Why no new work may start now, if it may not. In the order a person should deal with them.
    pub fn stopped(&self) -> Option<String> {
        if self.journal.get("restored").ok().flatten().is_some() {
            return Some("Restored from a backup. The equipment may be ahead of this journal: reconcile at the console before work starts".into());
        }
        if self.journal.get("fenced").ok().flatten().is_some() {
            return Some("Another gateway holds this domain. This one must not dispatch".into());
        }
        let Some(lease) = self.lease_until() else { return Some("Waiting for the first exchange with the platform".into()) };
        if self.clock.now() >= lease {
            return Some(format!("Offline past the site's limit, since {lease}. Running work is followed; nothing new starts"));
        }
        if !self.clock.sure() {
            return Some("This machine restarted, and nothing has said how long it was off. Nothing new starts until the platform answers, or a person vouches for the time here".into());
        }
        match free_disk(&self.cfg.state_dir) {
            Some(free) if free < self.cfg.reserve_mb << 20 => Some(format!("Only {} MB of disk left: keeping what remains for what is running", free >> 20)),
            None => Some("Cannot tell how much disk is left".into()),
            _ => None,
        }
    }

    fn adapter_for(&self, code: &str) -> Option<Slot> {
        self.by_device.get(code).cloned().or_else(|| {
            let concurrency = self.defined.concurrency(code)?;
            Some(Slot { name: "definitions".into(), adapter: self.defined.clone(), concurrency })
        })
    }

    /// What reaches a device now, and how to say so.
    pub fn reach(&self, code: &str) -> Option<(Slot, Binding)> {
        let slot = self.adapter_for(code)?;
        let endpoint = slot.adapter.endpoint(code)?;
        let binding = Binding { device: code.to_string(), adapter: slot.name.clone(), endpoint };
        Some((slot, binding))
    }

    /// What a command went out on, if that is still what reaches its device. Anything else asks the
    /// wrong thing: an answer from another endpoint says nothing about this command, and an answer of
    /// "never heard of it" would look like leave to send it again.
    fn bound(&self, e: &Entry) -> Option<(Slot, String)> {
        let binding = e.binding.as_ref()?;
        let (slot, now) = self.reach(&binding.device)?;
        (now == *binding).then(|| (slot, binding.device.clone()))
    }

    /// A person at the site says the time will do.
    pub fn vouch_time(&self) -> Result<(), String> {
        self.clock.vouch();
        self.journal.set("clock", &json!(self.clock.saved()).to_string()).map_err(|e| e.to_string())
    }

    /// Writes down the time the platform gave.
    fn keep_time(&self, server: DateTime<Utc>) {
        let _ = self.journal.set("clock", &json!(self.clock.saved()).to_string());
        let _ = self.journal.set("clock_offset", &(server.timestamp_millis() - Utc::now().timestamp_millis()).to_string());
    }

    fn devices_by_id(&self) -> HashMap<Uuid, SiteDevice> {
        self.journal.devices().unwrap_or_default().into_iter().map(|d| (d.id, d)).collect()
    }

    fn call_limit(&self) -> StdDuration {
        StdDuration::from_secs(self.cfg.adapter_timeout_secs)
    }

    /// What a crash left: commands whose intent was written with no answer recorded. Nobody retries
    /// them on a guess. The equipment is asked, and if it cannot say, the outcome is unknown.
    pub async fn recover(&self) {
        let doubts = self.journal.in_doubt().unwrap_or_default();
        if doubts.is_empty() {
            return;
        }
        for e in doubts {
            let found = match self.bound(&e) {
                Some((slot, device)) => timeout(self.call_limit(), slot.adapter.find(&e.command, &device)).await.unwrap_or(Ok(None)).unwrap_or(None),
                None => None,
            };
            let id = e.command.id;
            match found {
                Some(Observation::NotFound) => {
                    // The equipment keeps its own record and never saw it: it did not go out.
                    let _ = self.journal.withdraw_intent(id);
                }
                Some(seen @ (Observation::Waiting | Observation::Running)) => {
                    self.establish(self.report(id, State::Accepted, json!({"recovered": true}), None), Some(&id.to_string()));
                    if seen == Observation::Running {
                        self.establish(self.report(id, State::Executing, json!({"recovered": true}), None), None);
                    }
                }
                Some(Observation::Completed(v)) => self.establish(self.report(id, State::Completed, json!({"recovered": true}), Some(v)), None),
                Some(Observation::Failed(v)) => self.establish(self.report(id, State::Failed, json!({"recovered": true}), Some(v)), None),
                Some(Observation::Aborted(v)) => self.establish(self.report(id, State::Aborted, json!({"recovered": true}), Some(v)), None),
                None => self.establish(
                    self.report(id, State::OutcomeUnknown, json!({"why": "this runtime stopped between deciding to send it and hearing back"}), None),
                    None,
                ),
            }
        }
    }

    /// Everything that was running when a backup was taken may have moved on: none of it can be
    /// believed, and dispatch stays off until a person says the floor matches.
    pub fn after_restore(&self) -> Result<(), String> {
        self.journal.set("restored", &self.clock.now().to_rfc3339()).map_err(|e| e.to_string())?;
        let mut doubts = self.journal.following().map_err(|e| e.to_string())?;
        doubts.extend(self.journal.in_doubt().map_err(|e| e.to_string())?);
        for e in doubts {
            if e.state != State::OutcomeUnknown {
                self.establish(
                    self.report(e.command.id, State::OutcomeUnknown, json!({"why": "restored from a backup taken while this was running"}), None),
                    None,
                );
            }
        }
        Ok(())
    }

    /// Whether a command may leave now, and what it would take. Everything here is asked when work is
    /// planned and again the moment before it goes: an adapter ahead of it may have taken its time,
    /// and a deadline, the lease, a device or the authority may have changed meanwhile. Saying no here
    /// also says why, in the journal.
    fn admit(&self, e: &Entry) -> Option<Go> {
        let id = e.command.id;
        let now = self.clock.now();
        // A cancel needs no one's permission, and what is past its deadline needs no equipment.
        if e.cancel_requested {
            self.establish(self.report(id, State::Aborted, json!({"reason": "cancelled before it started"}), None), None);
            return None;
        }
        if now >= e.command.expires_at {
            self.establish(self.report(id, State::Expired, json!({"reason": "its deadline passed before it started"}), None), None);
            return None;
        }
        if let Some(why) = self.stopped() {
            let _ = self.journal.hold(id, Some(&why));
            return None;
        }
        let reject = |why: String| self.establish(self.report(id, State::Rejected, json!({"reason": why}), None), None);
        if let Err(why) = e.command.startable(now, self.epoch()) {
            reject(why.to_string());
            return None;
        }
        let Some(device) = self.journal.device(e.command.target_id).ok().flatten() else {
            reject("this site does not know that device".into());
            return None;
        };
        if Lifecycle::parse(&device.lifecycle) != Some(Lifecycle::Active) {
            reject(format!("{} is {}", device.code, device.lifecycle));
            return None;
        }
        if !device.operations.contains(&e.command.operation) {
            reject(format!("{} was not commissioned for {}", device.code, e.command.operation));
            return None;
        }
        let Some((slot, binding)) = self.reach(&device.code) else {
            reject(format!("nothing at this site reaches {}", device.code));
            return None;
        };
        Some(Go { device, slot, binding, left: (e.command.expires_at - now).to_std().ok()? })
    }

    /// Starts what is ready. The plan says what comes next, and only the gate says it may go.
    pub async fn dispatch_ready(&self) {
        let queued = self.journal.queued(200).unwrap_or_default();
        if queued.is_empty() {
            return;
        }
        let startable: Vec<Entry> = queued.into_iter().filter(|e| self.admit(e).is_some()).collect();
        let plan = self.plan(&startable, &self.devices_by_id(), self.clock.now());
        for (task, why) in &plan.held {
            let _ = self.journal.hold(*task, Some(&self.held_text(why)));
        }
        for a in plan.assign {
            let Ok(Some(e)) = self.journal.entry(a.task) else { continue };
            if e.state != State::Queued || e.intent_at.is_some() {
                continue;
            }
            if let Some(go) = self.admit(&e) {
                self.dispatch(&e, go).await;
            }
        }
    }

    /// What a command has to wait for. An id that cannot be read is held as one nothing can finish:
    /// a dependency nobody can name is not one that can be left out.
    fn after(payload: &Value) -> Vec<Uuid> {
        payload["task"]["after"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().and_then(|s| s.parse().ok()).unwrap_or(Uuid::nil())).collect())
            .unwrap_or_default()
    }

    /// What things waiting here depend on and this site has no record of. Only the platform can say.
    fn unknown_predecessors(&self, limit: usize) -> Vec<Uuid> {
        let queued = self.journal.queued(200).unwrap_or_default();
        let mut wanted: Vec<Uuid> = queued.iter().flat_map(|e| Self::after(&e.command.payload)).filter(|id| !id.is_nil()).collect();
        wanted.sort();
        wanted.dedup();
        wanted.retain(|id| matches!(self.journal.outcome(*id), Ok(None)));
        wanted.truncate(limit);
        wanted
    }

    fn held_text(&self, h: &Held) -> String {
        match h {
            Held::Waiting(id) => match self.journal.outcome(*id) {
                Ok(Some(s)) if s.is_final() => format!("{id} ended {}, so this cannot start", s.as_str()),
                Ok(None) => format!("Waiting to hear how {id} ended"),
                _ => format!("Waiting for {id} to finish"),
            },
            Held::ZoneClosed(z) => format!("{z} is closed"),
            Held::StationFull(s) => format!("{s} is full"),
            Held::WorkInProgressLimit => "At the limit of work in progress".into(),
            Held::NoResource(why) => format!("Nothing can take it: {why}"),
        }
    }

    fn plan(&self, entries: &[Entry], devices: &HashMap<Uuid, SiteDevice>, now: DateTime<Utc>) -> schedule::Plan {
        let tasks: Vec<Task> = entries
            .iter()
            .map(|e| {
                let t = &e.command.payload["task"];
                Task {
                    id: e.command.id,
                    priority: t["priority"].as_i64().unwrap_or(0) as i32,
                    created_at: e.command.issued_at,
                    due_at: Some(e.command.expires_at),
                    needs: format!("target:{}", e.command.target_id),
                    zone: t["zone"].as_str().map(str::to_string),
                    station: t["station"].as_str().map(str::to_string),
                    load_kg: t["load_kg"].as_f64().unwrap_or(0.0),
                    energy: t["energy"].as_f64().unwrap_or(0.0),
                    after: Self::after(&e.command.payload),
                }
            })
            .collect();
        let following = self.journal.following().unwrap_or_default();
        let mut busy: HashMap<Uuid, u32> = HashMap::new();
        for e in &following {
            *busy.entry(e.command.target_id).or_default() += 1;
        }
        let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut resources = Vec::new();
        for d in devices.values() {
            let Some(slot) = self.adapter_for(&d.code) else { continue };
            let left = slot.concurrency.saturating_sub(busy.get(&d.id).copied().unwrap_or(0));
            // A command in doubt holds what it may be using: the robot may still be carrying it.
            for n in 0..left {
                resources.push(Resource {
                    id: Uuid::from_u128(d.id.as_u128() ^ u128::from(n)),
                    can: vec![format!("target:{}", d.id)],
                    payload_kg: None,
                    battery: latest.get(&(d.id, "battery.pct".to_string())).map(|b| b / 100.0),
                    reserve: 0.0,
                    zone: None,
                    available: true,
                });
            }
        }
        // Only an ending on record satisfies a dependency, and only Completed, whether the equipment
        // said so or a person who looked did. Anything else, absence above all, waits.
        let done = tasks.iter().flat_map(|t| t.after.iter().copied()).filter(|id| matches!(self.journal.outcome(*id), Ok(Some(State::Completed)))).collect();
        schedule::plan(&tasks, &resources, &schedule::Limits { ageing_per_hour: 1.0, done, ..Default::default() }, now)
    }

    async fn dispatch(&self, e: &Entry, go: Go) {
        let id = e.command.id;
        // Written and flushed first. If this fails, or the authority moved since the gate, nothing is
        // sent, and the next pass says why.
        match self.journal.intend(id, self.clock.now_ms(), e.command.epoch, &go.binding) {
            Ok(true) => {}
            Ok(false) => return,
            Err(err) => {
                tracing::error!(command = %id, "cannot record the intent, so it stays unsent: {err}");
                return;
            }
        }
        crash_point("after_intent");
        let answer = timeout(self.call_limit(), go.slot.adapter.dispatch(&e.command, &go.device, go.left)).await;
        crash_point("after_dispatch");
        match answer {
            Ok(Ok(Dispatched::Accepted { external })) => {
                let _ = self.journal.answered(id, self.clock.now_ms());
                self.establish(self.report(id, State::Accepted, Value::Null, None), external.as_deref());
            }
            Ok(Ok(Dispatched::Rejected(why))) => self.establish(self.report(id, State::Rejected, json!({"reason": why}), None), None),
            Ok(Err(Failure::NotSent(why))) => {
                let _ = self.journal.withdraw_intent(id);
                let _ = self.journal.hold(id, Some(&format!("Waiting: {why}")));
            }
            Ok(Err(Failure::Unknown(why))) => self.establish(self.report(id, State::OutcomeUnknown, json!({"why": why}), None), None),
            Err(_) => self.establish(self.report(id, State::OutcomeUnknown, json!({"why": "the adapter did not answer in time"}), None), None),
        }
    }

    /// Follows what is running, and passes on requests to stop.
    pub async fn follow(&self) {
        for e in self.journal.following().unwrap_or_default() {
            let id = e.command.id;
            let Some((slot, device)) = self.bound(&e) else {
                if e.state != State::OutcomeUnknown {
                    self.establish(
                        self.report(id, State::OutcomeUnknown, json!({"why": "what this went out on has changed, so nothing here can ask about it"}), None),
                        None,
                    );
                }
                continue;
            };
            if e.cancel_requested && self.cancelled.lock().unwrap_or_else(|p| p.into_inner()).insert(id) {
                let asked = timeout(self.call_limit(), slot.adapter.cancel(&e.command, &device, e.external.as_deref())).await;
                if !matches!(asked, Ok(Ok(()))) {
                    self.cancelled.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
                }
            }
            let now = self.clock.now_ms();
            match timeout(self.call_limit(), slot.adapter.observe(&e.command, &device, e.external.as_deref())).await {
                Ok(Ok(seen)) => {
                    let _ = self.journal.answered(id, now);
                    match seen {
                        Observation::Waiting => {}
                        Observation::Running => {
                            if e.state == State::Accepted {
                                self.establish(self.report(id, State::Executing, Value::Null, None), None);
                            }
                        }
                        Observation::Completed(v) => self.establish(self.report(id, State::Completed, Value::Null, Some(v)), None),
                        Observation::Failed(v) => self.establish(self.report(id, State::Failed, Value::Null, Some(v)), None),
                        Observation::Aborted(v) => self.establish(self.report(id, State::Aborted, Value::Null, Some(v)), None),
                        Observation::NotFound if e.state != State::OutcomeUnknown => {
                            self.establish(self.report(id, State::OutcomeUnknown, json!({"why": "the equipment no longer has a record of it"}), None), None);
                        }
                        Observation::NotFound => {}
                    }
                }
                _ if e.state == State::OutcomeUnknown => {}
                _ => {
                    let since = e.answered_at.or(e.intent_at).unwrap_or(now);
                    if now - since > self.cfg.unknown_after_secs * 1000 {
                        self.establish(self.report(id, State::OutcomeUnknown, json!({"why": "no answer from the equipment"}), None), None);
                    }
                }
            }
        }
    }

    fn health(&self) -> Health {
        let counts = self.journal.counts().unwrap_or_default();
        Health {
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: Some(self.started_at),
            backlog_secs: counts.oldest_report_at.map_or(0, |at| ((self.clock.now_ms() - at) / 1000).max(0)),
            queued_commands: counts.queued,
            unknown_outcomes: counts.unknown,
            telemetry_bytes: counts.sample_bytes,
            telemetry_shed: counts.shed,
            disk_free_bytes: free_disk(&self.cfg.state_dir).unwrap_or(0),
            stopped: self.stopped(),
            adapters: self.adapters.iter().map(|(n, a)| (n.clone(), a.health())).collect(),
        }
    }

    /// One exchange. Everything sent stays in the journal until the answer says it arrived.
    /// Says whether there is more to send than one exchange carries.
    pub async fn exchange(&self) -> Result<bool, LinkError> {
        let reports = self.journal.reports(500).unwrap_or_default();
        let facts = self.journal.facts(500).unwrap_or_default();
        let (measurements, upto) = self.journal.samples(MEASUREMENTS_AT_ONCE).unwrap_or_default();
        let full = measurements.len() as i64 >= MEASUREMENTS_AT_ONCE;
        let held = self.journal.unconfirmed(500).unwrap_or_default();
        let counts = self.journal.counts().unwrap_or_default();
        let room = (self.cfg.max_queued - counts.queued).clamp(0, 100);
        // A site that has stopped takes nothing it cannot start, and a fenced one can start nothing.
        let want = if self.stopped().is_some_and(|s| s.contains("disk")) { 0 } else { room as u32 };
        let req = SyncRequest {
            contract: VERSION,
            epoch: self.epoch(),
            boot: self.boot,
            reports,
            measurements,
            facts,
            health: self.health(),
            held: held.clone(),
            in_doubt: self.journal.unknown().unwrap_or_default().iter().take(500).map(|e| e.command.id).chain(self.unknown_predecessors(500)).collect(),
            want,
            config_version: self.journal.get("config_version").ok().flatten().unwrap_or_default(),
        };
        let answer = {
            let _turn = self.signing.lock().await;
            self.link.sync(&req, self.clock.signing_ms()).await
        };
        crash_point("after_exchange");
        match answer {
            Ok(r) => {
                *self.heard.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
                // It came up and reached the platform, so it stays.
                let _ = self.journal.clear("update_pending");
                let taken = !r.measurements_held;
                self.absorb(r, &held, upto);
                Ok(full && taken)
            }
            Err(LinkError::Clock(server)) => {
                self.clock.refused(server);
                self.keep_time(server);
                Err(LinkError::Clock(server))
            }
            Err(e) => {
                *self.heard.lock().unwrap_or_else(|e| e.into_inner()) = None;
                Err(e)
            }
        }
    }

    fn absorb(&self, r: SyncResponse, held: &[Uuid], samples_upto: i64) {
        self.clock.heard(r.server_time);
        self.keep_time(r.server_time);
        let at = self.clock.now_ms();
        let _ = self.journal.confirmed(held);
        let _ = self.journal.reports_gone(&r.reports_done);
        for (id, why) in &r.reports_refused {
            let _ = self.journal.notice(at, &format!("The platform refused report {id}: {why}"));
        }
        let refused: Vec<Uuid> = r.reports_refused.iter().map(|(id, _)| *id).collect();
        let _ = self.journal.reports_gone(&refused);
        let _ = self.journal.facts_gone(&r.facts_done);
        // Over a workspace's allowance nothing was taken: they stay here, within the budget, for later.
        if !r.measurements_held {
            let _ = self.journal.samples_gone(samples_upto);
        }
        if self.held_back.swap(r.measurements_held, Ordering::Relaxed) != r.measurements_held {
            let said = if r.measurements_held {
                "This workspace has used today's allowance of readings, so they are being kept here."
            } else {
                "There is room for readings again."
            };
            let _ = self.journal.notice(at, said);
        }
        if r.fenced {
            if self.journal.get("fenced").ok().flatten().is_none() {
                let _ = self.journal.notice(at, "This gateway no longer holds its domain. Dispatch has stopped.");
            }
            let _ = self.journal.set("fenced", "1");
        } else if let Some(lease) = &r.lease {
            let _ = self.journal.set("epoch", &lease.epoch.to_string());
            let _ = self.journal.set("lease_until", &lease.valid_until.to_rfc3339());
        }
        if let Some(devices) = &r.devices {
            let _ = self.journal.set_devices(devices);
        }
        if let Some(operators) = &r.operators {
            let _ = self.journal.set_operators(operators);
        }
        if !r.config_version.is_empty() {
            let _ = self.journal.set("config_version", &r.config_version);
        }
        for c in &r.commands {
            if let Err(e) = self.journal.receive(c, at) {
                tracing::error!(command = %c.id, "could not journal a command: {e}");
            }
        }
        for id in &r.cancel {
            let _ = self.journal.request_cancel(*id);
        }
        for (id, state) in &r.settled {
            let _ = self.journal.settled(*id, *state, at);
        }
        let _ = self.journal.prune(at);
    }

    /// Reads the equipment and keeps within the telemetry budget.
    pub async fn read_equipment(&self) {
        let devices: HashMap<String, SiteDevice> = self.journal.devices().unwrap_or_default().into_iter().map(|d| (d.code.clone(), d)).collect();
        let mut batch = Vec::new();
        for (name, a) in &self.adapters {
            match timeout(self.call_limit(), a.read()).await {
                Ok(Ok(readings)) => {
                    for r in readings {
                        self.take(r, &devices, &mut batch);
                    }
                }
                Ok(Err(e)) => tracing::debug!(adapter = %name, "read: {e:?}"),
                Err(_) => tracing::debug!(adapter = %name, "read timed out"),
            }
        }
        self.tick_flows(&devices, &mut batch);
        // Past the reserve only what someone has to know is kept, and what cannot be kept is counted.
        if free_disk(&self.cfg.state_dir).is_some_and(|f| f < self.cfg.reserve_mb << 20) {
            batch.retain(|m| m.class == Class::Alarm);
        }
        let by_id = self.devices_by_id();
        self.offer_exports(&batch, &by_id);
        {
            let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
            for m in &batch {
                if let Some(d) = by_id.get(&m.device_id) {
                    recent.heard(&d.code, &m.metric, m.source_time.timestamp_millis(), m.value);
                }
            }
        }
        if let Err(e) = self.journal.add_samples(&batch) {
            tracing::warn!("measurements not kept: {e}");
        }
        if let Err(e) = self.journal.shed(self.cfg.telemetry_budget_mb << 20) {
            tracing::warn!("telemetry budget: {e}");
        }
        self.tick_rules();
    }

    fn take(&self, reading: Reading, devices: &HashMap<String, SiteDevice>, batch: &mut Vec<Measurement>) {
        match reading {
            Reading::Value { device, metric, value, unit, class, source_time } => {
                let Some(d) = devices.get(&device) else { return };
                let key = (d.id, metric.clone());
                let repeats = {
                    let mut seen = self.repeats.lock().unwrap_or_else(|e| e.into_inner());
                    let entry = seen.entry(key.clone()).or_insert((value, 0));
                    entry.1 = if entry.0 == value { entry.1 + 1 } else { 1 };
                    entry.0 = value;
                    entry.1
                };
                let limits: Limits = d.limits.get(&metric).cloned().unwrap_or_default();
                let quality = limits.judge(value, repeats);
                if quality != ferrum_ops_contract::Quality::Bad {
                    self.latest.lock().unwrap_or_else(|e| e.into_inner()).insert(key, value);
                    self.judge(&device, d.id, &metric, value);
                    let input = flow::Input { device: device.clone(), metric: Some(metric.clone()), value: json!(value), ..Default::default() };
                    self.feed(&input, devices, batch);
                }
                batch.push(Measurement {
                    device_id: d.id,
                    metric,
                    value,
                    unit,
                    source_time: source_time.unwrap_or_else(|| self.clock.now()),
                    boot: self.boot,
                    seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
                    quality,
                    class,
                });
            }
            Reading::Attribute { device, key, value } => {
                let Some(d) = devices.get(&device) else { return };
                let fresh = self.last_attribute.lock().unwrap_or_else(|e| e.into_inner()).insert((d.id, key.clone()), value.clone()).as_ref() != Some(&value);
                if fresh {
                    let fact = Fact {
                        fact_id: Uuid::now_v7(),
                        kind: "edge.attribute".into(),
                        at_source: self.clock.now(),
                        actor: None,
                        body: json!({"device": d.id, "key": key, "value": value}),
                    };
                    let _ = self.journal.add_fact(&fact);
                    self.feed(&flow::Input { device, attribute: Some(key), value, ..Default::default() }, devices, batch);
                }
            }
            Reading::Joined { device, definition, token_hash } => {
                let gateway = *self.gateway.lock().unwrap_or_else(|e| e.into_inner());
                let body = json!({"code": device, "definition": definition, "token_hash": token_hash, "gateway": gateway});
                let _ = self.journal.add_fact(&Fact { fact_id: Uuid::now_v7(), kind: "edge.joined".into(), at_source: self.clock.now(), actor: None, body });
            }
            Reading::Event { device, name, value } => {
                let Some(d) = devices.get(&device) else { return };
                // A device that calls in is wherever its connection is, which the platform has to know.
                if name == "ocpp.Connection" {
                    let kind = if value["connected"] == json!(true) { "connection.opened" } else { "connection.closed" };
                    let _ = self.journal.add_fact(&Fact {
                        fact_id: Uuid::now_v7(),
                        kind: kind.into(),
                        at_source: self.clock.now(),
                        actor: None,
                        body: json!({"device": d.id}),
                    });
                }
                let fact = Fact {
                    fact_id: Uuid::now_v7(),
                    kind: "edge.event".into(),
                    at_source: self.clock.now(),
                    actor: None,
                    body: json!({"device": d.id, "name": name, "value": value}),
                };
                let _ = self.journal.add_fact(&fact);
                self.feed(&flow::Input { device, event: Some(name), value, ..Default::default() }, devices, batch);
            }
            Reading::State { device, state, reason } => {
                let Some(d) = devices.get(&device) else { return };
                let changed = self.last_state.lock().unwrap_or_else(|e| e.into_inner()).insert(device.clone(), state.clone()).as_ref() != Some(&state);
                if changed {
                    let fact = Fact {
                        fact_id: Uuid::now_v7(),
                        kind: "equipment.state".into(),
                        at_source: self.clock.now(),
                        actor: None,
                        body: json!({"device": d.id, "state": state, "reason": reason}),
                    };
                    let _ = self.journal.add_fact(&fact);
                    self.feed(&flow::Input { device, state: Some(state), ..Default::default() }, devices, batch);
                }
            }
        }
    }

    /// Records a fact from the console, as a person stated it.
    pub fn record_fact(&self, kind: &str, actor: Option<String>, body: Value) -> Result<Uuid, String> {
        let fact = Fact { fact_id: Uuid::now_v7(), kind: kind.to_string(), at_source: self.clock.now(), actor, body };
        self.journal.add_fact(&fact).map_err(|e| e.to_string())?;
        Ok(fact.fact_id)
    }

    /// A person settles an unknown outcome at the console. Goes up like any other report, so it
    /// works with the link down.
    pub fn settle(&self, id: Uuid, to: State, found: &str, by: &str) -> Result<(), String> {
        if !matches!(to, State::Completed | State::Failed | State::Aborted) {
            return Err("settle as completed, failed or aborted".into());
        }
        if found.trim().is_empty() {
            return Err("say what you found".into());
        }
        let r = self.report(id, to, Value::Null, Some(json!({"found": found, "by": by})));
        match self.journal.establish(&r, None).map_err(|e| e.to_string())? {
            Step::Move => Ok(()),
            _ => Err("this command has nothing to settle".into()),
        }
    }

    pub fn status(&self) -> Value {
        let counts = self.journal.counts().unwrap_or_default();
        json!({
            "stopped": self.stopped(),
            "link_up": self.link_up(),
            "server": self.cfg.server,
            "epoch": self.epoch(),
            "lease_until": self.lease_until(),
            "now": self.clock.now(),
            "clock_sure": self.clock.sure(),
            "boot": self.boot,
            "queued": counts.queued,
            "running": counts.active,
            "unknown": counts.unknown,
            "unsent_reports": counts.reports,
            "unsent_facts": counts.facts,
            "telemetry_bytes": counts.sample_bytes,
            "telemetry_shed": counts.shed,
            "disk_free_mb": free_disk(&self.cfg.state_dir).map(|f| f >> 20),
            "adapters": self.adapters.iter().map(|(n, a)| (n.clone(), a.health())).collect::<BTreeMap<_, _>>(),
            "links": self.defined.links(),
            "problems": *self.problems.lock().unwrap_or_else(|e| e.into_inner()),
            "strangers": self.defined.strangers(),
            "flows": self.flows.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|f| json!({"name": f.name, "revision": f.revision})).collect::<Vec<_>>(),
            "exports": self.exports.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|e| json!({"name": e.name, "waiting": self.journal.export_backlog(&e.name).unwrap_or(0)})).collect::<Vec<_>>(),
            "exports_shed": self.export_shed.load(Ordering::Relaxed),
            "notices": self.journal.notices(20).unwrap_or_default(),
        })
    }

    /// Runs until `stop` says otherwise. Each loop owns its own timing, so a slow adapter does not
    /// hold up the exchange with the platform.
    pub async fn run(self: Arc<Self>, stop: watch::Receiver<bool>) {
        if self.cfg.peer_only {
            // It only answers. Its adapters hold what they read until the peer asks.
            let mut s = stop;
            let _ = s.changed().await;
            return;
        }
        self.recover().await;
        let sync = self.clone();
        let mut s = stop.clone();
        let exchange = tokio::spawn(async move {
            let mut failures = 0u32;
            loop {
                let wait = match sync.exchange().await {
                    // A backlog goes as fast as the platform takes it, which is what paces this.
                    Ok(more) => {
                        failures = 0;
                        if more { 0 } else { sync.cfg.sync_secs * 1000 }
                    }
                    Err(LinkError::Clock(_)) => 100,
                    Err(e) => {
                        failures = failures.saturating_add(1);
                        if failures == 1 {
                            tracing::warn!("link: {e:?}");
                        }
                        if let LinkError::Refused(why) = &e
                            && failures == 1
                        {
                            let _ = sync.journal.notice(sync.clock.now_ms(), &format!("The platform turned this site away: {why}"));
                        }
                        backoff(failures, sync.cfg.sync_secs)
                    }
                };
                let _ = sync.journal.set("clock_high_water", &sync.clock.now_ms().to_string());
                tokio::select! { _ = s.changed() => return, _ = tokio::time::sleep(StdDuration::from_millis(wait)) => {} }
            }
        });
        let work = self.clone();
        let mut s = stop.clone();
        let working = tokio::spawn(async move {
            let mut last_tidy = std::time::Instant::now();
            loop {
                work.dispatch_ready().await;
                work.follow().await;
                if last_tidy.elapsed() > StdDuration::from_secs(600) {
                    last_tidy = std::time::Instant::now();
                    if let Err(e) = work.journal.reclaim() {
                        tracing::warn!("reclaiming journal space: {e}");
                    }
                }
                tokio::select! { _ = s.changed() => return, _ = tokio::time::sleep(StdDuration::from_millis(work.cfg.work_millis)) => {} }
            }
        });
        let courier = self.clone();
        let mut s = stop.clone();
        let exporting = tokio::spawn(async move {
            loop {
                courier.deliver_exports().await;
                tokio::select! { _ = s.changed() => return, _ = tokio::time::sleep(StdDuration::from_secs(2)) => {} }
            }
        });
        let manager = self.clone();
        let mut s = stop.clone();
        let managing = tokio::spawn(async move {
            if !manager.cfg.managed {
                return;
            }
            loop {
                manager.exchange_config().await;
                tokio::select! { _ = s.changed() => return, _ = tokio::time::sleep(StdDuration::from_secs(manager.cfg.sync_secs * 6)) => {} }
            }
        });
        let reader = self.clone();
        let mut s = stop.clone();
        let reading = tokio::spawn(async move {
            loop {
                reader.read_equipment().await;
                tokio::select! {
                    _ = s.changed() => return,
                    _ = tokio::time::sleep(StdDuration::from_secs(reader.cfg.read_secs)) => {}
                    () = reader.defined.pressing.notified() => {}
                }
            }
        });
        let _ = tokio::join!(exchange, working, reading, managing, exporting);
    }
}

/// How long to wait after failures: up to a minute, with up to a fifth off at random, so that sites
/// that lost the platform together do not all come back together.
fn backoff(failures: u32, base_secs: u64) -> u64 {
    let ms = (base_secs * 1000).saturating_mul(1 << failures.min(6)).min(60_000);
    ms - ms * rand::random_range(0..=20) / 100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_stays_bounded_and_spreads() {
        let waits: Vec<u64> = (1..20).map(|n| backoff(n, 5)).collect();
        assert!(waits.iter().all(|w| *w <= 60_000));
        assert!(backoff(12, 5) >= 48_000);
        assert!(backoff(1, 5) <= 10_000);
    }
}
