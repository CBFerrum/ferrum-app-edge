//! Everything the site must not forget, in one SQLite file. WAL with `synchronous=FULL`: when a
//! call returns, what it wrote survives a power cut. A crash can leave any step half done, so each
//! step here is one transaction and the runtime is written around the gaps between them.

use crate::adapter::Binding;
use chrono::{DateTime, Utc};
use ferrum_ops_contract::command::{Report, step};
use ferrum_ops_contract::sync::{Fact, Operator, SiteDevice};
use ferrum_ops_contract::telemetry::{Class, Measurement};
use ferrum_ops_contract::{Command, State, Step};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("journal: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("journal: {0}")]
    Json(#[from] serde_json::Error),
    /// The file is damaged. The runtime must not start on it: restore a backup, and reconcile.
    #[error("the journal failed its integrity check: {0}")]
    Damaged(String),
}

pub type Result<T> = std::result::Result<T, Error>;

const SCHEMA: &str = "
create table if not exists meta (key text primary key, value text not null);
create table if not exists device_transaction (key text primary key, number integer not null, at integer not null);
create table if not exists command (
  id text primary key,
  body text not null,
  state text not null,
  external text,
  -- The intent to dispatch was written. Set and adapter not yet answered means nobody knows.
  intent_at integer,
  -- When the equipment last answered about it.
  answered_at integer,
  cancel_requested integer not null default 0,
  -- The platform was told the journal holds it.
  confirmed integer not null default 0,
  held_reason text,
  received_at integer not null
);
create index if not exists command_state on command (state);
create table if not exists report (seq integer primary key, id text not null unique, command text not null, body text not null);
create index if not exists report_command on report (command);
create table if not exists fact (seq integer primary key, id text not null unique, body text not null);
create table if not exists sample (seq integer primary key, class integer not null, bytes integer not null, body text not null);
create index if not exists sample_shed on sample (class, seq);
create table if not exists device (id text primary key, code text not null, body text not null);
create table if not exists operator (user text primary key, body text not null);
create table if not exists operator_task (command text primary key, state text not null, evidence text, by text);
create table if not exists notice (seq integer primary key, at integer not null, text text not null);
create table if not exists peer_dispatch (id text primary key, device text not null, answer text);
create table if not exists export_item (seq integer primary key, export text not null, body text not null);
create index if not exists export_item_export on export_item (export, seq);
-- How commands ended, after their rows go: what depends on one must not find it gone and call it done.
create table if not exists outcome (id text primary key, state text not null, at integer not null);
create index if not exists outcome_at on outcome (at);
";

/// How long an ending is kept after its command's row goes. A command lives a week at most, so
/// nothing still waiting can be waiting for anything older, and the platform can always be asked.
const KEEP_OUTCOMES_MS: i64 = 8 * 24 * 3_600_000;

pub struct Journal {
    conn: Mutex<Connection>,
}

/// A command as the site holds it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub command: Command,
    pub state: State,
    pub external: Option<String>,
    pub intent_at: Option<i64>,
    pub answered_at: Option<i64>,
    pub cancel_requested: bool,
    pub held_reason: Option<String>,
    /// What it went out on. A journal from before this was kept, or one that cannot be read, has none,
    /// and then nothing can be asked about it.
    pub binding: Option<Binding>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counts {
    pub queued: i64,
    pub active: i64,
    pub unknown: i64,
    pub reports: i64,
    pub facts: i64,
    pub oldest_report_at: Option<i64>,
    pub sample_bytes: i64,
    pub shed: i64,
}

type Row = (String, String, Option<String>, Option<i64>, Option<i64>, i64, Option<String>, Option<String>);

fn entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))
}

const ENTRY: &str = "select body, state, external, intent_at, answered_at, cancel_requested, held_reason, binding from command";

fn decode(row: Row) -> Result<Entry> {
    let (body, state, external, intent_at, answered_at, cancel, held_reason, binding) = row;
    let state = State::parse(&state).ok_or_else(|| Error::Damaged(format!("a command in state {state}")))?;
    let binding = binding.and_then(|b| serde_json::from_str(&b).ok());
    Ok(Entry { command: serde_json::from_str(&body)?, state, external, intent_at, answered_at, cancel_requested: cancel != 0, held_reason, binding })
}

impl Journal {
    pub fn open(path: &Path) -> Result<Journal> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "pragma journal_mode = wal; pragma synchronous = full; pragma foreign_keys = on; pragma busy_timeout = 5000; pragma auto_vacuum = incremental;",
        )?;
        let check: String = conn.query_row("pragma quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(Error::Damaged(check));
        }
        conn.execute_batch(SCHEMA)?;
        for table in ["command", "peer_dispatch"] {
            let there = conn.prepare(&format!("select 1 from pragma_table_info('{table}') where name = 'binding'"))?.exists([])?;
            if !there {
                conn.execute_batch(&format!("alter table {table} add column binding text"))?;
            }
        }
        Ok(Journal { conn: Mutex::new(conn) })
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding this cannot leave a half-done transaction behind: SQLite rolls it
        // back when the connection is next used or closed.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db().query_row("select value from meta where key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db().execute("insert into meta (key, value) values (?1, ?2) on conflict (key) do update set value = excluded.value", [key, value])?;
        Ok(())
    }

    pub fn clear(&self, key: &str) -> Result<()> {
        self.db().execute("delete from meta where key = ?1", [key])?;
        Ok(())
    }

    pub fn clear_restored(&self) -> Result<()> {
        self.clear("restored")
    }

    pub fn notice(&self, at: i64, text: &str) -> Result<()> {
        let db = self.db();
        db.execute("insert into notice (at, text) values (?1, ?2)", params![at, text])?;
        db.execute("delete from notice where seq <= (select max(seq) - 500 from notice)", [])?;
        Ok(())
    }

    pub fn notices(&self, limit: i64) -> Result<Vec<(i64, String)>> {
        let db = self.db();
        let mut q = db.prepare("select at, text from notice order by seq desc limit ?1")?;
        let rows = q.query_map([limit], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Takes a command from the platform. A repeat is ignored: the journal's state stands, whatever
    /// the platform's copy says, because the site is the one that knows.
    pub fn receive(&self, c: &Command, now: i64) -> Result<bool> {
        let added = self.db().execute(
            "insert or ignore into command (id, body, state, received_at) values (?1, ?2, ?3, ?4)",
            params![c.id.to_string(), serde_json::to_string(c)?, State::Queued.as_str(), now],
        )?;
        Ok(added > 0)
    }

    pub fn entry(&self, id: Uuid) -> Result<Option<Entry>> {
        let db = self.db();
        let row = db.query_row(&format!("{ENTRY} where id = ?1"), [id.to_string()], entry).optional()?;
        row.map(decode).transpose()
    }

    fn list(&self, filter: &str, limit: i64) -> Result<Vec<Entry>> {
        let db = self.db();
        let mut q = db.prepare(&format!("{ENTRY} where {filter} order by received_at, id limit ?1"))?;
        let rows = q.query_map([limit], entry)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(decode).collect()
    }

    /// Waiting to start, with no intent written.
    pub fn queued(&self, limit: i64) -> Result<Vec<Entry>> {
        self.list("state = 'Queued' and intent_at is null", limit)
    }

    /// Commands whose intent was written and whose adapter never answered: what a crash leaves.
    pub fn in_doubt(&self) -> Result<Vec<Entry>> {
        self.list("state = 'Queued' and intent_at is not null", 10_000)
    }

    /// Started, or perhaps started: followed until the equipment says how they ended.
    pub fn following(&self) -> Result<Vec<Entry>> {
        self.list("state in ('Accepted', 'Executing', 'OutcomeUnknown')", 10_000)
    }

    pub fn unknown(&self) -> Result<Vec<Entry>> {
        self.list("state = 'OutcomeUnknown'", 1000)
    }

    /// The intent, durable before the equipment hears of it, with what it is going out on. After this,
    /// a crash can only leave the command unknown: it can never be sent twice. It is also the last
    /// word on whether it may go: one statement, so nothing a sync writes can land between looking
    /// and writing. Says false if something did, and nothing is then sent.
    pub fn intend(&self, id: Uuid, at: i64, epoch: i64, binding: &Binding) -> Result<bool> {
        Ok(self.db().execute(
            "update command set intent_at = ?2, binding = ?3 where id = ?1 and state = 'Queued' and intent_at is null and cancel_requested = 0 \
             and coalesce((select value from meta where key = 'epoch'), '0') = ?4 and not exists (select 1 from meta where key in ('fenced', 'restored'))",
            params![id.to_string(), at, serde_json::to_string(binding)?, epoch.to_string()],
        )? > 0)
    }

    /// It definitely did not go out: it can wait for another turn.
    pub fn withdraw_intent(&self, id: Uuid) -> Result<()> {
        self.db().execute("update command set intent_at = null where id = ?1 and state = 'Queued'", [id.to_string()])?;
        Ok(())
    }

    pub fn hold(&self, id: Uuid, why: Option<&str>) -> Result<()> {
        self.db().execute("update command set held_reason = ?2 where id = ?1", params![id.to_string(), why])?;
        Ok(())
    }

    /// What the equipment last answered. Without this, silence looks like an answer.
    pub fn answered(&self, id: Uuid, at: i64) -> Result<()> {
        self.db().execute("update command set answered_at = ?2 where id = ?1", params![id.to_string(), at])?;
        Ok(())
    }

    /// Applies what was established, and the report that says so, in one transaction, by the same
    /// state machine the platform uses. Returns what it did.
    pub fn establish(&self, r: &Report, external: Option<&str>) -> Result<Step> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let id = r.command_id.to_string();
        let state: Option<String> = tx.query_row("select state from command where id = ?1", [&id], |row| row.get(0)).optional()?;
        let Some(state) = state.as_deref().and_then(State::parse) else { return Ok(Step::Refuse) };
        let taken = step(state, r.state, r.evidence.is_some());
        if taken == Step::Refuse || taken == Step::Keep && r.state == state {
            return Ok(taken);
        }
        tx.execute("insert or ignore into report (id, command, body) values (?1, ?2, ?3)", params![r.report_id.to_string(), id, serde_json::to_string(r)?])?;
        if taken == Step::Move {
            tx.execute(
                "update command set state = ?2, external = coalesce(?3, external), held_reason = null where id = ?1",
                params![id, r.state.as_str(), external],
            )?;
        }
        tx.commit()?;
        Ok(taken)
    }

    /// The platform says how this ended: a person settled it, or something here waits on it. Nothing
    /// goes back up: it came from there.
    pub fn settled(&self, id: Uuid, to: State, at: i64) -> Result<()> {
        let db = self.db();
        db.execute("update command set state = ?2 where id = ?1 and state = 'OutcomeUnknown'", params![id.to_string(), to.as_str()])?;
        db.execute("insert or replace into outcome (id, state, at) values (?1, ?2, ?3)", params![id.to_string(), to.as_str(), at])?;
        Ok(())
    }

    /// Where a command stands, as far as this site knows, kept past its row. `None` is not an ending:
    /// it means nobody here can say.
    pub fn outcome(&self, id: Uuid) -> Result<Option<State>> {
        let state: Option<String> = self
            .db()
            .query_row("select state from command where id = ?1 union all select state from outcome where id = ?1 limit 1", [id.to_string()], |r| r.get(0))
            .optional()?;
        Ok(state.as_deref().and_then(State::parse))
    }

    /// A peer asks for a command. `None` if this is the first time; otherwise what was answered,
    /// which is `None` if it stopped before it could.
    pub fn peer_begin(&self, id: Uuid, device: &str, binding: &Binding) -> Result<Option<Option<String>>> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let seen: Option<Option<String>> = tx.query_row("select answer from peer_dispatch where id = ?1", [id.to_string()], |r| r.get(0)).optional()?;
        if seen.is_none() {
            tx.execute(
                "insert into peer_dispatch (id, device, binding) values (?1, ?2, ?3)",
                params![id.to_string(), device, serde_json::to_string(binding)?],
            )?;
        }
        tx.commit()?;
        Ok(seen)
    }

    pub fn peer_end(&self, id: Uuid, answer: &str) -> Result<()> {
        self.db().execute("update peer_dispatch set answer = ?2 where id = ?1", params![id.to_string(), answer])?;
        Ok(())
    }

    /// Which device a peer's command was for, and what it went out on.
    pub fn peer_device(&self, id: Uuid) -> Result<Option<(String, Option<Binding>)>> {
        let row: Option<(String, Option<String>)> =
            self.db().query_row("select device, binding from peer_dispatch where id = ?1", [id.to_string()], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        Ok(row.map(|(device, binding)| (device, binding.and_then(|b| serde_json::from_str(&b).ok()))))
    }

    /// Keeps a measurement for an export, up to `budget` of them. Past that the oldest go, and how
    /// many went is said: an export that has been down for a month must not fill the disk.
    pub fn add_exports(&self, export: &str, bodies: &[String], budget: i64) -> Result<i64> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for body in bodies {
            tx.execute("insert into export_item (export, body) values (?1, ?2)", params![export, body])?;
        }
        let shed = tx.execute(
            "delete from export_item where export = ?1 and seq in (select seq from export_item where export = ?1 order by seq desc limit -1 offset ?2)",
            params![export, budget],
        )?;
        tx.commit()?;
        Ok(shed as i64)
    }

    pub fn exports(&self, export: &str, limit: i64) -> Result<(Vec<String>, i64)> {
        let db = self.db();
        let mut q = db.prepare("select seq, body from export_item where export = ?1 order by seq limit ?2")?;
        let rows = q.query_map(params![export, limit], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((rows.iter().map(|r| r.1.clone()).collect(), rows.last().map_or(0, |r| r.0)))
    }

    pub fn exports_gone(&self, export: &str, upto: i64) -> Result<()> {
        self.db().execute("delete from export_item where export = ?1 and seq <= ?2", params![export, upto])?;
        Ok(())
    }

    pub fn export_backlog(&self, export: &str) -> Result<i64> {
        Ok(self.db().query_row("select count(*) from export_item where export = ?1", [export], |r| r.get(0))?)
    }

    pub fn request_cancel(&self, id: Uuid) -> Result<bool> {
        Ok(self.db().execute("update command set cancel_requested = 1 where id = ?1 and cancel_requested = 0", [id.to_string()])? > 0)
    }

    pub fn unconfirmed(&self, limit: i64) -> Result<Vec<Uuid>> {
        let db = self.db();
        let mut q = db.prepare("select id from command where confirmed = 0 order by received_at limit ?1")?;
        let ids = q.query_map([limit], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids.iter().filter_map(|s| s.parse().ok()).collect())
    }

    pub fn confirmed(&self, ids: &[Uuid]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for id in ids {
            tx.execute("update command set confirmed = 1 where id = ?1", [id.to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Finished commands the platform has heard about can go. How each ended stays a while, for what
    /// depends on it.
    pub fn prune(&self, now: i64) -> Result<()> {
        const DONE: &str = "confirmed = 1 and state in ('Completed', 'Failed', 'Aborted', 'Rejected', 'Expired') \
             and not exists (select 1 from report where report.command = command.id)";
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute(&format!("insert or replace into outcome (id, state, at) select id, state, ?1 from command where {DONE}"), [now])?;
        tx.execute(&format!("delete from command where {DONE}"), [])?;
        tx.execute("delete from outcome where at < ?1", [now - KEEP_OUTCOMES_MS])?;
        tx.execute("delete from device_transaction where at < ?1", [now - KEEP_OUTCOMES_MS])?;
        tx.commit()?;
        Ok(())
    }

    pub fn reports(&self, limit: i64) -> Result<Vec<Report>> {
        let db = self.db();
        let mut q = db.prepare("select body from report order by seq limit ?1")?;
        let rows = q.query_map([limit], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.iter().map(|b| serde_json::from_str(b)).collect::<serde_json::Result<_>>()?)
    }

    pub fn reports_gone(&self, ids: &[Uuid]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for id in ids {
            tx.execute("delete from report where id = ?1", [id.to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The number a new transaction goes by, the same one for the same start however often it arrives.
    /// It carries on from where it was: a process's counter would start again, and hand a number that
    /// chargers still hold to someone else.
    pub fn transaction_number(&self, key: &str, at: i64) -> Result<i32> {
        let mut db = self.db();
        let tx = db.transaction()?;
        if let Some(number) = tx.query_row("select number from device_transaction where key = ?1", [key], |r| r.get(0)).optional()? {
            return Ok(number);
        }
        let last: Option<String> = tx.query_row("select value from meta where key = 'device_transaction'", [], |r| r.get(0)).optional()?;
        // From the clock the first time, so a journal lost with its disk does not begin again at one.
        let next = last.and_then(|v| v.parse::<i64>().ok()).map_or((at / 1000) & 0x7fff_ffff, |n| n + 1);
        let next = if next > i64::from(i32::MAX) { 1 } else { next };
        tx.execute(
            "insert into meta (key, value) values ('device_transaction', ?1) on conflict (key) do update set value = excluded.value",
            [next.to_string()],
        )?;
        tx.execute("insert into device_transaction (key, number, at) values (?1, ?2, ?3)", params![key, next, at])?;
        tx.commit()?;
        Ok(next as i32)
    }

    /// A business-critical flow's step, all at once: where it stands now and what it did, in one
    /// transaction. A power cut leaves both or neither, so its readings are neither lost nor acted on twice
    /// (FLC-EDGE-005).
    pub fn commit_flow(&self, key: &str, state: &str, facts: &[Fact]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for f in facts {
            tx.execute("insert or ignore into fact (id, body) values (?1, ?2)", params![f.fact_id.to_string(), serde_json::to_string(f)?])?;
        }
        tx.execute("insert into meta (key, value) values (?1, ?2) on conflict (key) do update set value = excluded.value", [key, state])?;
        crate::crash_point("before_flow_commit");
        tx.commit()?;
        crate::crash_point("after_flow_commit");
        Ok(())
    }

    pub fn add_fact(&self, f: &Fact) -> Result<()> {
        self.db().execute("insert or ignore into fact (id, body) values (?1, ?2)", params![f.fact_id.to_string(), serde_json::to_string(f)?])?;
        Ok(())
    }

    pub fn facts(&self, limit: i64) -> Result<Vec<Fact>> {
        let db = self.db();
        let mut q = db.prepare("select body from fact order by seq limit ?1")?;
        let rows = q.query_map([limit], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.iter().map(|b| serde_json::from_str(b)).collect::<serde_json::Result<_>>()?)
    }

    pub fn facts_gone(&self, ids: &[Uuid]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for id in ids {
            tx.execute("delete from fact where id = ?1", [id.to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn add_samples(&self, samples: &[Measurement]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for m in samples {
            let body = serde_json::to_string(m)?;
            tx.execute("insert into sample (class, bytes, body) values (?1, ?2, ?3)", params![m.class.code(), body.len() as i64, body])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The oldest measurements, with the last sequence taken, to delete once the platform has them.
    pub fn samples(&self, limit: i64) -> Result<(Vec<Measurement>, i64)> {
        let db = self.db();
        let mut q = db.prepare("select seq, body from sample order by seq limit ?1")?;
        let rows = q.query_map([limit], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let last = rows.last().map(|r| r.0).unwrap_or(0);
        Ok((rows.iter().map(|(_, b)| serde_json::from_str(b)).collect::<serde_json::Result<_>>()?, last))
    }

    pub fn samples_gone(&self, upto: i64) -> Result<()> {
        self.db().execute("delete from sample where seq <= ?1", [upto])?;
        Ok(())
    }

    pub fn sample_bytes(&self) -> Result<i64> {
        Ok(self.db().query_row("select coalesce(sum(bytes), 0) from sample", [], |r| r.get(0))?)
    }

    /// Brings unsent measurements under `budget` bytes: the oldest plain samples go first, then
    /// changes of state, and alarms only when nothing else is left. Commands, reports and facts are
    /// never in this budget. Returns how many went.
    pub fn shed(&self, budget: i64) -> Result<i64> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut over: i64 = tx.query_row("select coalesce(sum(bytes), 0) from sample", [], |r| r.get(0))?;
        over -= budget;
        if over <= 0 {
            return Ok(0);
        }
        let mut gone = 0;
        for class in [Class::Sample, Class::State, Class::Alarm] {
            while over > 0 {
                let batch: Vec<(i64, i64)> = {
                    let mut q = tx.prepare("select seq, bytes from sample where class = ?1 order by seq limit 500")?;
                    q.query_map([class.code()], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
                };
                if batch.is_empty() {
                    break;
                }
                for (seq, bytes) in batch {
                    if over <= 0 {
                        break;
                    }
                    tx.execute("delete from sample where seq = ?1", [seq])?;
                    over -= bytes;
                    gone += 1;
                }
            }
        }
        tx.execute(
            "insert into meta (key, value) values ('shed', ?1) on conflict (key) do update set value = cast(cast(value as integer) + ?1 as text)",
            [gone],
        )?;
        tx.commit()?;
        Ok(gone)
    }

    pub fn reclaim(&self) -> Result<()> {
        self.db().execute_batch("pragma incremental_vacuum(2000); pragma wal_checkpoint(truncate);")?;
        Ok(())
    }

    pub fn counts(&self) -> Result<Counts> {
        let db = self.db();
        let count = |sql: &str| -> rusqlite::Result<i64> { db.query_row(sql, [], |r| r.get(0)) };
        Ok(Counts {
            queued: count("select count(*) from command where state = 'Queued'")?,
            active: count("select count(*) from command where state in ('Accepted', 'Executing')")?,
            unknown: count("select count(*) from command where state = 'OutcomeUnknown'")?,
            reports: count("select count(*) from report")?,
            facts: count("select count(*) from fact")?,
            oldest_report_at: db
                .query_row("select body from report order by rowid limit 1", [], |r| r.get::<_, String>(0))
                .optional()?
                .and_then(|body| serde_json::from_str::<Report>(&body).ok())
                .map(|r| r.at_source.timestamp_millis()),
            sample_bytes: count("select coalesce(sum(bytes), 0) from sample")?,
            shed: count("select coalesce((select cast(value as integer) from meta where key = 'shed'), 0)")?,
        })
    }

    pub fn set_devices(&self, devices: &[SiteDevice]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("delete from device", [])?;
        for d in devices {
            tx.execute("insert into device (id, code, body) values (?1, ?2, ?3)", params![d.id.to_string(), d.code, serde_json::to_string(d)?])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn device(&self, id: Uuid) -> Result<Option<SiteDevice>> {
        let body: Option<String> = self.db().query_row("select body from device where id = ?1", [id.to_string()], |r| r.get(0)).optional()?;
        Ok(body.map(|b| serde_json::from_str(&b)).transpose()?)
    }

    pub fn devices(&self) -> Result<Vec<SiteDevice>> {
        let db = self.db();
        let mut q = db.prepare("select body from device order by code")?;
        let rows = q.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.iter().map(|b| serde_json::from_str(b)).collect::<serde_json::Result<_>>()?)
    }

    pub fn set_operators(&self, operators: &[Operator]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("delete from operator", [])?;
        for o in operators {
            tx.execute("insert into operator (user, body) values (?1, ?2)", params![o.user.to_string(), serde_json::to_string(o)?])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn operators(&self) -> Result<Vec<Operator>> {
        let db = self.db();
        let mut q = db.prepare("select body from operator")?;
        let rows = q.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.iter().map(|b| serde_json::from_str(b)).collect::<serde_json::Result<_>>()?)
    }

    /// Work a person does: what a console shows, and what the operator adapter reads back.
    pub fn open_task(&self, command: Uuid) -> Result<()> {
        self.db().execute("insert or ignore into operator_task (command, state) values (?1, 'Waiting')", [command.to_string()])?;
        Ok(())
    }

    pub fn task(&self, command: Uuid) -> Result<Option<(String, Option<Value>, Option<String>)>> {
        let db = self.db();
        let row: Option<(String, Option<String>, Option<String>)> = db
            .query_row("select state, evidence, by from operator_task where command = ?1", [command.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        row.map(|(s, e, b)| Ok((s, e.map(|e| serde_json::from_str(&e)).transpose()?, b))).transpose()
    }

    pub fn set_task(&self, command: Uuid, state: &str, evidence: Option<&Value>, by: &str) -> Result<bool> {
        let evidence = evidence.map(serde_json::to_string).transpose()?;
        Ok(self
            .db()
            .execute("update operator_task set state = ?2, evidence = ?3, by = ?4 where command = ?1", params![command.to_string(), state, evidence, by])?
            > 0)
    }

    pub fn tasks(&self) -> Result<Vec<(Entry, String)>> {
        let ids: Vec<(String, String)> = {
            let db = self.db();
            let mut q = db.prepare("select command, state from operator_task where state in ('Waiting', 'Started') order by rowid")?;
            q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let mut out = Vec::new();
        for (id, state) in ids {
            if let Some(e) = id.parse().ok().map(|id| self.entry(id)).transpose()?.flatten() {
                out.push((e, state));
            }
        }
        Ok(out)
    }

    /// A consistent copy, taken while running. A restored copy is older than the floor: see
    /// `restored` in the runtime.
    pub fn backup(&self, to: &Path) -> Result<()> {
        let db = self.db();
        db.execute("vacuum into ?1", [to.to_string_lossy().as_ref()])?;
        Ok(())
    }

    /// When the file may have been written by a clock that is now behind.
    pub fn high_water(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(self.get("clock_high_water")?.and_then(|v| v.parse::<i64>().ok()).and_then(DateTime::from_timestamp_millis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrum_ops_contract::command::payload_hash;
    use ferrum_ops_contract::telemetry::Quality;
    use serde_json::json;

    fn journal() -> (Journal, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ferrum-site-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        (Journal::open(&dir.join("j.db")).unwrap(), dir)
    }

    fn command() -> Command {
        let payload = json!({"tote": "T-481"});
        Command {
            id: Uuid::now_v7(),
            tenant_id: Uuid::now_v7(),
            site_id: Uuid::now_v7(),
            domain: "north".into(),
            target_id: Uuid::now_v7(),
            operation: "warehouse.move".into(),
            schema_version: 1,
            issued_by: None,
            approval_ref: None,
            epoch: 1,
            expected_state: Value::Null,
            issued_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            payload_hash: payload_hash(&payload),
            payload,
            workflow_id: None,
            step_id: None,
            correlation_id: None,
            config_version: None,
        }
    }

    fn bound() -> Binding {
        Binding { device: "AMR-1".into(), adapter: "fleet".into(), endpoint: "e".into() }
    }

    fn report(c: &Command, state: State) -> Report {
        Report { report_id: Uuid::now_v7(), command_id: c.id, state, at_source: Utc::now(), epoch: 1, detail: Value::Null, evidence: None }
    }

    fn sample(seq: i64, class: Class) -> Measurement {
        Measurement {
            device_id: Uuid::nil(),
            metric: "x".into(),
            value: 1.0,
            unit: None,
            source_time: Utc::now(),
            boot: Uuid::nil(),
            seq,
            quality: Quality::Good,
            class,
        }
    }

    #[test]
    fn the_journal_is_durable_by_configuration() {
        let (j, _dir) = journal();
        let db = j.db();
        assert_eq!(db.query_row::<i64, _, _>("pragma synchronous", [], |r| r.get(0)).unwrap(), 2, "FULL");
        assert_eq!(db.query_row::<String, _, _>("pragma journal_mode", [], |r| r.get(0)).unwrap(), "wal");
    }

    #[test]
    fn a_repeat_changes_nothing() {
        let (j, _dir) = journal();
        let c = command();
        assert!(j.receive(&c, 1).unwrap());
        j.establish(&report(&c, State::Accepted), Some("m-1")).unwrap();
        assert!(!j.receive(&c, 2).unwrap(), "the platform repeats itself until it hears back");
        assert_eq!(j.entry(c.id).unwrap().unwrap().state, State::Accepted, "and the journal's state stands");
    }

    #[test]
    fn a_state_and_its_report_are_one_step() {
        let (j, _dir) = journal();
        let c = command();
        j.receive(&c, 1).unwrap();
        assert_eq!(j.establish(&report(&c, State::Accepted), Some("m-1")).unwrap(), Step::Move);
        assert_eq!(j.reports(10).unwrap().len(), 1);
        let late = report(&c, State::Accepted);
        assert_eq!(j.establish(&late, None).unwrap(), Step::Keep);
        assert_eq!(j.reports(10).unwrap().len(), 1, "saying the same again adds nothing");
        assert_eq!(j.entry(c.id).unwrap().unwrap().external.as_deref(), Some("m-1"), "and loses nothing");
    }

    #[test]
    fn an_intent_survives_and_leaves_the_queue() {
        let (j, dir) = journal();
        let c = command();
        j.receive(&c, 1).unwrap();
        j.set("epoch", "1").unwrap();
        assert!(j.intend(c.id, 5, 1, &bound()).unwrap());
        assert!(j.queued(10).unwrap().is_empty(), "it can never be picked up again");
        assert_eq!(j.entry(c.id).unwrap().unwrap().binding, Some(bound()), "and it says what it went out on");
        drop(j);
        let reopened = Journal::open(&dir.join("j.db")).unwrap();
        assert_eq!(reopened.in_doubt().unwrap().len(), 1, "a crash leaves it in doubt, not forgotten");
        reopened.withdraw_intent(c.id).unwrap();
        assert_eq!(reopened.queued(10).unwrap().len(), 1, "only an adapter that knows nothing left can put it back");
    }

    #[test]
    fn an_intent_is_the_last_word_on_whether_it_may_go() {
        let (j, _dir) = journal();
        j.set("epoch", "1").unwrap();
        let c = command();
        j.receive(&c, 1).unwrap();
        assert!(!j.intend(c.id, 5, 2, &bound()).unwrap(), "issued under another authority");
        j.set("fenced", "1").unwrap();
        assert!(!j.intend(c.id, 5, 1, &bound()).unwrap(), "fenced since it was planned");
        j.clear("fenced").unwrap();
        j.set("restored", "now").unwrap();
        assert!(!j.intend(c.id, 5, 1, &bound()).unwrap(), "restored since it was planned");
        j.clear("restored").unwrap();
        j.request_cancel(c.id).unwrap();
        assert!(!j.intend(c.id, 5, 1, &bound()).unwrap(), "asked to stop since it was planned");
        assert!(j.in_doubt().unwrap().is_empty(), "and in none of those is there anything to be in doubt about");
        let d = command();
        j.receive(&d, 1).unwrap();
        assert!(j.intend(d.id, 5, 1, &bound()).unwrap());
        assert!(!j.intend(d.id, 6, 1, &bound()).unwrap(), "and it goes out once");
    }

    #[test]
    fn a_journal_from_before_bindings_opens_and_binds_nothing() {
        let dir = std::env::temp_dir().join(format!("ferrum-site-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = command();
        {
            let old = Connection::open(dir.join("j.db")).unwrap();
            old.execute_batch("create table command (id text primary key, body text not null, state text not null, external text, intent_at integer, answered_at integer, cancel_requested integer not null default 0, confirmed integer not null default 0, held_reason text, received_at integer not null); create table peer_dispatch (id text primary key, device text not null, answer text);").unwrap();
            old.execute(
                "insert into command (id, body, state, received_at) values (?1, ?2, 'Accepted', 1)",
                params![c.id.to_string(), serde_json::to_string(&c).unwrap()],
            )
            .unwrap();
        }
        let j = Journal::open(&dir.join("j.db")).unwrap();
        assert_eq!(j.entry(c.id).unwrap().unwrap().binding, None);
        drop(j);
        Journal::open(&dir.join("j.db")).expect("and opens again");
    }

    #[test]
    fn an_ending_outlives_its_row() {
        let (j, _dir) = journal();
        let c = command();
        j.receive(&c, 1).unwrap();
        assert_eq!(j.outcome(c.id).unwrap(), Some(State::Queued));
        let done = report(&c, State::Completed);
        j.establish(&done, None).unwrap();
        j.confirmed(&[c.id]).unwrap();
        j.reports_gone(&[done.report_id]).unwrap();
        j.prune(1_000).unwrap();
        assert!(j.entry(c.id).unwrap().is_none(), "the row is gone");
        assert_eq!(j.outcome(c.id).unwrap(), Some(State::Completed), "and how it ended is not");
        assert_eq!(j.outcome(Uuid::now_v7()).unwrap(), None, "what was never here is not an ending");
        j.prune(1_000 + KEEP_OUTCOMES_MS + 1).unwrap();
        assert_eq!(j.outcome(c.id).unwrap(), None, "not for ever");
        let heard = Uuid::now_v7();
        j.settled(heard, State::Failed, 5).unwrap();
        assert_eq!(j.outcome(heard).unwrap(), Some(State::Failed), "and the platform can say");
    }

    #[test]
    fn the_oldest_plain_samples_go_first_and_alarms_last() {
        let (j, _dir) = journal();
        j.add_samples(&[sample(1, Class::Alarm), sample(2, Class::Sample), sample(3, Class::State), sample(4, Class::Sample)]).unwrap();
        let bytes = j.sample_bytes().unwrap();
        assert_eq!(j.shed(bytes).unwrap(), 0, "under budget nothing goes");
        assert_eq!(j.shed(bytes - bytes / 4 - 20).unwrap(), 2, "just over one sample's worth: two plain samples, oldest first");
        let (left, _) = j.samples(10).unwrap();
        assert_eq!(left.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(j.shed(0).unwrap(), 2, "with nothing else left, alarms go too");
        assert_eq!(j.counts().unwrap().shed, 4, "and every one is counted");
    }

    #[test]
    fn nothing_but_measurements_counts_against_the_budget() {
        let (j, _dir) = journal();
        let c = command();
        j.receive(&c, 1).unwrap();
        j.establish(&report(&c, State::Accepted), None).unwrap();
        j.add_fact(&Fact { fact_id: Uuid::now_v7(), kind: "x".into(), at_source: Utc::now(), actor: None, body: Value::Null }).unwrap();
        j.shed(0).unwrap();
        let counts = j.counts().unwrap();
        assert_eq!((counts.active, counts.reports, counts.facts), (1, 1, 1));
    }

    #[test]
    fn finished_work_goes_only_after_the_platform_has_heard() {
        let (j, _dir) = journal();
        let c = command();
        j.receive(&c, 1).unwrap();
        let done = report(&c, State::Completed);
        j.establish(&done, None).unwrap();
        j.confirmed(&[c.id]).unwrap();
        j.prune(1).unwrap();
        assert!(j.entry(c.id).unwrap().is_some(), "its report has not gone up yet");
        j.reports_gone(&[done.report_id]).unwrap();
        j.prune(1).unwrap();
        assert!(j.entry(c.id).unwrap().is_none());
    }

    #[test]
    fn a_damaged_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("ferrum-site-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("j.db");
        std::fs::write(&path, b"this is not a database, and it is long enough to be read as a header by anything").unwrap();
        assert!(Journal::open(&path).is_err());
    }
}
