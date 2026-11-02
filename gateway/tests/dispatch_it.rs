//! What stands between a plan and equipment, with a real journal and a simulated fleet whose speed
//! can be chosen. No platform: what a site was told is written into its journal directly, and whatever
//! changes during a batch changes the way a sync would change it, from another task.

use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use ferrum_ops_contract::command::{Report, payload_hash};
use ferrum_ops_contract::sync::SiteDevice;
use ferrum_ops_contract::{Command, State};
use ferrum_site::adapter::Binding;
use ferrum_site::config::Config;
use ferrum_site::journal::Journal;
use ferrum_site::runtime::Runtime;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use uuid::Uuid;

const AMR_1: Uuid = Uuid::from_u128(1);
const AMR_2: Uuid = Uuid::from_u128(2);

fn device(id: Uuid, code: &str, lifecycle: &str) -> SiteDevice {
    SiteDevice { id, code: code.into(), kind: "Robot".into(), lifecycle: lifecycle.into(), operations: vec!["warehouse.move".into()], limits: BTreeMap::new() }
}

struct Site {
    rt: Arc<Runtime>,
    dir: PathBuf,
}

impl Site {
    /// A site that holds its lease, with one fleet that takes `slow` to answer a dispatch and one
    /// that answers at once. `vouched` is whether anything has said what time it is.
    fn open(slow: u64, vouched: bool) -> Site {
        let dir = std::env::temp_dir().join(format!("ferrum-dispatch-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = format!(
            "server: http://127.0.0.1:9\ntenant: t\nstate_dir: {}\nreserve_mb: 1\nadapters:\n  - {{name: slow, kind: sim, devices: [AMR-1], concurrency: 4, dispatch_millis: {slow}, mission_millis: 50}}\n  - {{name: quick, kind: sim, devices: [AMR-2], concurrency: 4, mission_millis: 50}}\n",
            dir.display()
        );
        let path = dir.join("site.yaml");
        std::fs::write(&path, text).unwrap();
        let cfg = Config::load(&path).unwrap();
        {
            let j = Journal::open(&cfg.journal_path()).unwrap();
            j.set("key_id", &Uuid::now_v7().to_string()).unwrap();
            j.set("epoch", "1").unwrap();
            j.set("lease_until", &(Utc::now() + Duration::hours(1)).to_rfc3339()).unwrap();
            j.set_devices(&[device(AMR_1, "AMR-1", "Active"), device(AMR_2, "AMR-2", "Active")]).unwrap();
        }
        let rt = Runtime::open(cfg, SigningKey::from_bytes(&[3; 32])).unwrap();
        if vouched {
            rt.vouch_time().unwrap();
        }
        Site { rt, dir }
    }

    fn missions(&self, fleet: &str, id: Uuid) -> usize {
        std::fs::read_to_string(self.dir.join(format!("{fleet}.sim"))).unwrap_or_default().lines().filter(|l| l.contains(&id.to_string())).count()
    }

    fn queue(&self, target: Uuid, ms: i64, task: Value) -> Uuid {
        self.queue_as(Uuid::now_v7(), target, ms, task)
    }

    fn queue_as(&self, id: Uuid, target: Uuid, ms: i64, task: Value) -> Uuid {
        let payload = json!({"task": task});
        let c = Command {
            id,
            tenant_id: Uuid::nil(),
            site_id: Uuid::nil(),
            domain: "north".into(),
            target_id: target,
            operation: "warehouse.move".into(),
            schema_version: 1,
            issued_by: None,
            approval_ref: None,
            epoch: 1,
            expected_state: Value::Null,
            issued_at: Utc::now(),
            expires_at: Utc::now() + Duration::milliseconds(ms),
            payload_hash: payload_hash(&payload),
            payload,
            workflow_id: None,
            step_id: None,
            correlation_id: None,
            config_version: None,
        };
        self.rt.journal.receive(&c, 1).unwrap();
        c.id
    }

    fn state(&self, id: Uuid) -> State {
        self.rt.journal.entry(id).unwrap().unwrap().state
    }

    fn held(&self, id: Uuid) -> String {
        self.rt.journal.entry(id).unwrap().unwrap().held_reason.unwrap_or_default()
    }

    /// Establishes how a command ended, as the equipment or a person would.
    fn ends(&self, id: Uuid, state: State) {
        let r = Report {
            report_id: Uuid::now_v7(),
            command_id: id,
            state,
            at_source: Utc::now(),
            epoch: 1,
            detail: Value::Null,
            evidence: Some(json!({"it": "ended"})),
        };
        self.rt.journal.establish(&r, None).unwrap();
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A batch whose first command holds things up, with `change` happening meanwhile. Says what became
/// of the second, which by then has no right to go, whatever the plan said.
async fn during_a_batch(change: impl FnOnce(&Site) + Send + 'static) -> (Site, Uuid) {
    let site = Site::open(400, true);
    site.queue(AMR_1, 60_000, json!({"priority": 9}));
    let second = site.queue(AMR_2, 60_000, json!({"priority": 1}));
    let (rt, dir) = (site.rt.clone(), site.dir.clone());
    let batch = tokio::spawn(async move { rt.dispatch_ready().await });
    tokio::time::sleep(StdDuration::from_millis(120)).await;
    change(&site);
    batch.await.unwrap();
    assert!(dir.exists());
    (site, second)
}

#[tokio::test]
async fn a_slow_adapter_cannot_carry_what_comes_after_it_past_its_deadline() {
    let site = Site::open(500, true);
    let first = site.queue(AMR_1, 60_000, json!({"priority": 9}));
    let second = site.queue(AMR_2, 200, json!({"priority": 1}));
    site.rt.dispatch_ready().await;
    assert_eq!(site.state(first), State::Accepted);
    assert_eq!(site.state(second), State::Expired, "it was fine when it was planned, and not when its turn came");
    assert_eq!(site.missions("quick", second), 0, "and nothing went");
}

#[tokio::test]
async fn a_fence_during_a_batch_stops_what_is_left() {
    let (site, second) = during_a_batch(|s| s.rt.journal.set("fenced", "1").unwrap()).await;
    assert_eq!(site.missions("quick", second), 0);
    assert!(site.held(second).contains("Another gateway"), "{}", site.held(second));
}

#[tokio::test]
async fn a_new_authority_during_a_batch_stops_what_is_left() {
    let (site, second) = during_a_batch(|s| s.rt.journal.set("epoch", "2").unwrap()).await;
    assert_eq!(site.missions("quick", second), 0);
    assert_eq!(site.state(second), State::Rejected, "it was issued under another authority");
}

#[tokio::test]
async fn a_lease_that_ends_during_a_batch_stops_what_is_left() {
    let (site, second) = during_a_batch(|s| s.rt.journal.set("lease_until", &(Utc::now() - Duration::seconds(1)).to_rfc3339()).unwrap()).await;
    assert_eq!(site.missions("quick", second), 0);
    assert!(site.held(second).contains("Offline past"), "{}", site.held(second));
}

#[tokio::test]
async fn a_device_taken_out_of_service_during_a_batch_is_not_sent_to() {
    let (site, second) =
        during_a_batch(|s| s.rt.journal.set_devices(&[device(AMR_1, "AMR-1", "Active"), device(AMR_2, "AMR-2", "Maintenance")]).unwrap()).await;
    assert_eq!(site.missions("quick", second), 0);
    assert_eq!(site.state(second), State::Rejected);
}

#[tokio::test]
async fn an_operation_taken_away_during_a_batch_is_not_sent() {
    let (site, second) = during_a_batch(|s| {
        let mut changed = device(AMR_2, "AMR-2", "Active");
        changed.operations.clear();
        s.rt.journal.set_devices(&[device(AMR_1, "AMR-1", "Active"), changed]).unwrap();
    })
    .await;
    assert_eq!(site.missions("quick", second), 0);
    assert_eq!(site.state(second), State::Rejected);
}

#[tokio::test]
async fn a_cancel_during_a_batch_is_believed() {
    let site = Site::open(400, true);
    site.queue(AMR_1, 60_000, json!({"priority": 9}));
    let second = site.queue(AMR_2, 60_000, json!({"priority": 1}));
    let rt = site.rt.clone();
    let batch = tokio::spawn(async move { rt.dispatch_ready().await });
    tokio::time::sleep(StdDuration::from_millis(120)).await;
    site.rt.journal.request_cancel(second).unwrap();
    batch.await.unwrap();
    assert_eq!(site.missions("quick", second), 0);
    assert_eq!(site.state(second), State::Aborted);
}

#[tokio::test]
async fn nothing_changing_leaves_a_batch_alone() {
    let (site, second) = during_a_batch(|_| {}).await;
    assert_eq!(site.missions("quick", second), 1, "what all this stops is what should be stopped");
}

#[tokio::test]
async fn a_dependency_is_satisfied_by_a_recorded_completion_and_nothing_else() {
    let site = Site::open(0, true);
    let never_seen = Uuid::now_v7();
    let unknown = site.queue(AMR_2, 60_000, json!({"after": [never_seen]}));
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", unknown), 0, "absent is not finished");
    assert!(site.held(unknown).contains("Waiting to hear"), "{}", site.held(unknown));

    let garbled = site.queue(AMR_2, 60_000, json!({"after": ["not an id"]}));
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", garbled), 0, "a dependency nobody can read is still a dependency");

    let running = site.queue(AMR_2, 60_000, json!({}));
    let waiting = site.queue(AMR_2, 60_000, json!({"after": [running]}));
    site.rt.dispatch_ready().await;
    assert_eq!(site.state(running), State::Accepted);
    assert_eq!(site.missions("quick", waiting), 0, "started is not finished");

    site.ends(running, State::Failed);
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", waiting), 0, "and failed is not finished");
    assert!(site.held(waiting).contains("ended Failed"), "{}", site.held(waiting));

    let good = site.queue(AMR_2, 60_000, json!({}));
    let next = site.queue(AMR_2, 60_000, json!({"after": [good]}));
    site.rt.dispatch_ready().await;
    site.ends(good, State::Completed);
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", next), 1, "completed is");
}

#[tokio::test]
async fn a_dependency_outside_the_batch_is_not_done() {
    let site = Site::open(0, true);
    // One pass looks at the oldest two hundred. Its predecessor came after them all, so it is in the
    // journal and in nothing this pass sees: which is not the same as finished.
    let predecessor = Uuid::from_u128(u128::MAX);
    let waiting = site.queue(AMR_2, 60_000, json!({"priority": 100, "after": [predecessor]}));
    for _ in 0..199 {
        site.queue(AMR_1, 60_000, json!({}));
    }
    site.queue_as(predecessor, AMR_1, 60_000, json!({}));
    site.rt.dispatch_ready().await;
    assert_eq!(site.state(predecessor), State::Queued, "it was not looked at");
    assert_eq!(site.missions("quick", waiting), 0, "and what waits for it did not go");
    assert!(site.held(waiting).contains("Waiting for"), "{}", site.held(waiting));
}

#[tokio::test]
async fn an_ending_is_remembered_after_the_row_has_gone() {
    let site = Site::open(0, true);
    let first = site.queue(AMR_2, 60_000, json!({}));
    site.rt.dispatch_ready().await;
    site.ends(first, State::Completed);
    let journal = &site.rt.journal;
    journal.confirmed(&[first]).unwrap();
    let sent: Vec<Uuid> = journal.reports(100).unwrap().iter().map(|r| r.report_id).collect();
    journal.reports_gone(&sent).unwrap();
    journal.prune(1).unwrap();
    assert!(journal.entry(first).unwrap().is_none(), "compacted");
    let after = site.queue(AMR_2, 60_000, json!({"after": [first]}));
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", after), 1, "its completion is still on record");
}

#[tokio::test]
async fn what_only_the_platform_knows_comes_from_the_platform() {
    let site = Site::open(0, true);
    let elsewhere = Uuid::now_v7();
    let waiting = site.queue(AMR_2, 60_000, json!({"after": [elsewhere]}));
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", waiting), 0);
    site.rt.journal.settled(elsewhere, State::Completed, 1).unwrap();
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", waiting), 1, "the platform said it finished");
}

fn other_binding() -> Binding {
    Binding { device: "AMR-2".into(), adapter: "somewhere else".into(), endpoint: String::new() }
}

#[tokio::test]
async fn a_command_is_followed_only_through_what_it_went_out_on() {
    let site = Site::open(0, true);
    let c = site.queue(AMR_2, 60_000, json!({}));
    site.rt.dispatch_ready().await;
    let sent = site.rt.journal.entry(c).unwrap().unwrap();
    assert_eq!(sent.binding, Some(Binding { device: "AMR-2".into(), adapter: "quick".into(), endpoint: String::new() }), "written with the intent");

    // A command that went out on something that is no longer what reaches its device.
    let moved = site.queue(AMR_2, 60_000, json!({}));
    assert!(site.rt.journal.intend(moved, 1, 1, &other_binding()).unwrap());
    let r =
        Report { report_id: Uuid::now_v7(), command_id: moved, state: State::Accepted, at_source: Utc::now(), epoch: 1, detail: Value::Null, evidence: None };
    site.rt.journal.establish(&r, None).unwrap();
    site.rt.follow().await;
    assert_eq!(site.state(moved), State::OutcomeUnknown, "nothing here can ask about it, and it says so at once");
    assert_eq!(site.state(c), State::Executing, "what is bound carries on");
}

#[tokio::test]
async fn after_a_crash_a_binding_that_no_longer_holds_is_unknown_and_never_resent() {
    let site = Site::open(0, true);
    let c = site.queue(AMR_2, 60_000, json!({}));
    assert!(site.rt.journal.intend(c, 1, 1, &other_binding()).unwrap());
    site.rt.recover().await;
    assert_eq!(site.state(c), State::OutcomeUnknown, "the simulator would have said it never heard of this: it was not asked");
    assert_eq!(site.missions("quick", c), 0);
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", c), 0, "and it is not sent on a guess");
}

#[tokio::test]
async fn after_a_crash_the_exact_endpoint_is_asked() {
    let site = Site::open(0, true);
    let c = site.queue(AMR_2, 60_000, json!({}));
    assert!(site.rt.journal.intend(c, 1, 1, &Binding { device: "AMR-2".into(), adapter: "quick".into(), endpoint: String::new() }).unwrap());
    site.rt.recover().await;
    assert_eq!(site.state(c), State::Queued, "it never heard of it, so it never went");
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", c), 1);
}

#[tokio::test]
async fn a_machine_that_restarted_starts_nothing_until_someone_says_what_time_it_is() {
    let site = Site::open(0, false);
    let c = site.queue(AMR_2, 60_000, json!({}));
    assert!(site.rt.stopped().unwrap().contains("restarted"), "{:?}", site.rt.stopped());
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", c), 0);
    assert!(site.held(c).contains("restarted"), "and it says why");
    site.rt.vouch_time().unwrap();
    assert_eq!(site.rt.stopped(), None);
    site.rt.dispatch_ready().await;
    assert_eq!(site.missions("quick", c), 1);
    assert!(site.rt.journal.get("clock").unwrap().unwrap().contains("\"sure\":true"), "and that is written down");
}
