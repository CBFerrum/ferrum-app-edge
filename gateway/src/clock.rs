//! Time on a site. Deadlines and leases are written in the platform's time, so time here is the
//! platform's as last heard, carried forward by a clock nothing on this machine can set. The wall
//! clock is only where a machine starts from when it has nothing better.
//!
//! | What happens | What it does to time here |
//! |---|---|
//! | The wall clock is set, either way | Nothing |
//! | The machine is suspended | That time counts, because the clock read keeps going through it |
//! | This process restarts | Nothing: the same clock, read against what was written down |
//! | The machine restarts | Nobody can say how long it was off. Time goes on from the later of the last time written down and the wall clock, and it is unsure: no new work until the platform answers or a person vouches for it |
//! | The platform answers | Time is what it says, backwards included: its deadlines are in its time, and a clock that ran ahead must not lock a gateway out of signing |
//!
//! A clock carried forward drifts, and a lease must not outlast its allowance because a crystal ran
//! slow, so elapsed time counts for a little more than it reads. `clock_drift_ppm` is that margin.

use chrono::{DateTime, Utc};
use ferrum_ops_contract::sign::MAX_CLOCK_SKEW_MS;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

/// Milliseconds on a clock nothing can set, which keeps going while the machine is suspended.
fn boot_ms() -> i64 {
    #[cfg(target_os = "linux")]
    const CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `t` is ours to fill, and these clocks exist wherever this builds.
    unsafe { libc::clock_gettime(CLOCK, &mut t) };
    t.tv_sec * 1000 + t.tv_nsec / 1_000_000
}

/// What tells one run of this machine from the next, so that the boot clock can be read across a
/// restart of this process. In a container it is the host's, as the clock is.
fn boot_id() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let (mut text, mut len) = ([0u8; 64], 64usize);
        // SAFETY: the name is a C string, and `text` and `len` describe memory that is ours.
        let failed = unsafe { libc::sysctlbyname(c"kern.bootsessionuuid".as_ptr(), text.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) } != 0;
        (!failed).then(|| String::from_utf8_lossy(&text[..len]).trim_end_matches('\0').to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok().map(|id| id.trim().to_string())
    }
}

/// Where time comes from, apart, so that a test can move each clock on its own.
pub struct Source {
    pub boot: Box<dyn Fn() -> i64 + Send + Sync>,
    pub wall: Box<dyn Fn() -> i64 + Send + Sync>,
    pub boot_id: Option<String>,
}

impl Default for Source {
    fn default() -> Source {
        Source { boot: Box::new(boot_ms), wall: Box::new(|| Utc::now().timestamp_millis()), boot_id: boot_id() }
    }
}

/// What is written down, to carry on from after a restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    boot: Option<String>,
    boot_ms: i64,
    at_ms: i64,
    sure: bool,
}

/// `at_ms` was the time when the boot clock read `boot_ms`.
struct Mark {
    at_ms: i64,
    boot_ms: i64,
    sure: bool,
}

pub struct Clock {
    source: Source,
    mark: Mutex<Mark>,
    last_signed: AtomicI64,
    drift_ppm: i64,
}

impl Clock {
    /// Picks up from what was written down. `high_water` is the last time written, `offset` how far
    /// the wall clock was from the platform's.
    pub fn start(saved: Option<Saved>, high_water: i64, offset: i64, drift_ppm: i64, source: Source) -> Clock {
        let now = (source.boot)();
        let mark = match saved.filter(|s| s.boot.is_some() && s.boot == source.boot_id && s.boot_ms <= now) {
            Some(s) => Mark { at_ms: s.at_ms, boot_ms: s.boot_ms, sure: s.sure },
            None => Mark { at_ms: high_water.max((source.wall)() + offset), boot_ms: now, sure: false },
        };
        Clock { mark: Mutex::new(mark), last_signed: AtomicI64::new(high_water), drift_ppm, source }
    }

    fn mark(&self) -> std::sync::MutexGuard<'_, Mark> {
        self.mark.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn now_ms(&self) -> i64 {
        let m = self.mark();
        let passed = ((self.source.boot)() - m.boot_ms).max(0);
        m.at_ms + passed + passed * self.drift_ppm / 1_000_000
    }

    pub fn now(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(self.now_ms()).unwrap_or_else(Utc::now)
    }

    /// Whether anything but this machine has vouched for the time since it last started.
    pub fn sure(&self) -> bool {
        self.mark().sure
    }

    /// What to write down, whenever the mark changes.
    pub fn saved(&self) -> Saved {
        let m = self.mark();
        Saved { boot: self.source.boot_id.clone(), boot_ms: m.boot_ms, at_ms: m.at_ms, sure: m.sure }
    }

    /// The platform says what time it is.
    pub fn heard(&self, server: DateTime<Utc>) {
        *self.mark() = Mark { at_ms: server.timestamp_millis(), boot_ms: (self.source.boot)(), sure: true };
    }

    /// The platform refused a signature for its time. Whatever was signed last may be far ahead, and
    /// the next must still rise past what it last took, which is at most its time plus the skew.
    pub fn refused(&self, server: DateTime<Utc>) {
        self.heard(server);
        self.last_signed.fetch_min(server.timestamp_millis() + MAX_CLOCK_SKEW_MS - 5_000, Ordering::Relaxed);
    }

    /// A person at the site says the time will do, with no platform to ask.
    pub fn vouch(&self) {
        self.mark().sure = true;
    }

    /// A time to sign with: rising on every call, as the platform requires.
    pub fn signing_ms(&self) -> i64 {
        let now = self.now_ms();
        let mut last = self.last_signed.load(Ordering::Relaxed);
        loop {
            let next = now.max(last + 1);
            match self.last_signed.compare_exchange(last, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return next,
                Err(seen) => last = seen,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::sync::Arc;

    const START: i64 = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;

    /// A boot clock and a wall clock a test can move.
    #[derive(Clone)]
    struct Hands {
        boot: Arc<AtomicI64>,
        wall: Arc<AtomicI64>,
    }

    impl Hands {
        fn new() -> Hands {
            Hands { boot: Arc::new(AtomicI64::new(5_000)), wall: Arc::new(AtomicI64::new(START)) }
        }

        fn source(&self, id: &str) -> Source {
            let (boot, wall) = (self.boot.clone(), self.wall.clone());
            Source { boot: Box::new(move || boot.load(Ordering::SeqCst)), wall: Box::new(move || wall.load(Ordering::SeqCst)), boot_id: Some(id.into()) }
        }

        /// Real time passes: both clocks move.
        fn pass(&self, ms: i64) {
            self.boot.fetch_add(ms, Ordering::SeqCst);
            self.wall.fetch_add(ms, Ordering::SeqCst);
        }
    }

    fn at(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(ms).unwrap()
    }

    /// A clock the platform has spoken to.
    fn started(hands: &Hands) -> Clock {
        let clock = Clock::start(None, 0, 0, 0, hands.source("boot-1"));
        clock.heard(at(START));
        clock
    }

    #[test]
    fn setting_the_wall_clock_changes_nothing() {
        let hands = Hands::new();
        let clock = started(&hands);
        let lease = START + 10 * 60_000;
        hands.pass(60_000);
        hands.wall.fetch_sub(HOUR, Ordering::SeqCst);
        assert_eq!(clock.now_ms(), START + 60_000, "an hour back on the wall takes nothing back");
        hands.pass(9 * 60_000);
        assert!(clock.now_ms() >= lease, "and a ten minute lease lasts ten minutes");
        hands.wall.fetch_add(5 * HOUR, Ordering::SeqCst);
        assert_eq!(clock.now_ms(), START + 10 * 60_000, "nor does a jump forward end anything early");
    }

    #[test]
    fn a_suspended_machine_has_been_away() {
        let hands = Hands::new();
        let clock = started(&hands);
        // The boot clock keeps going through a suspend, and the wall may not have been told.
        hands.boot.fetch_add(2 * HOUR, Ordering::SeqCst);
        assert_eq!(clock.now_ms(), START + 2 * HOUR);
    }

    #[test]
    fn a_process_restart_loses_nothing() {
        let hands = Hands::new();
        let clock = started(&hands);
        hands.pass(60_000);
        let saved = clock.saved();
        drop(clock);
        hands.pass(120_000);
        hands.wall.fetch_sub(HOUR, Ordering::SeqCst);
        let again = Clock::start(Some(saved), 0, 0, 0, hands.source("boot-1"));
        assert_eq!(again.now_ms(), START + 180_000, "the time it was down counts, whatever the wall says");
        assert!(again.sure(), "and nothing about the time became doubtful");
    }

    #[test]
    fn a_reboot_is_not_believed() {
        let hands = Hands::new();
        let clock = started(&hands);
        hands.pass(60_000);
        let (saved, high_water) = (clock.saved(), clock.now_ms());
        drop(clock);
        // The power goes for a day, and comes back on a wall clock with no battery behind it.
        hands.boot.store(1_000, Ordering::SeqCst);
        hands.wall.store(START - 300 * 24 * HOUR, Ordering::SeqCst);
        let again = Clock::start(Some(saved.clone()), high_water, 0, 0, hands.source("boot-2"));
        assert_eq!(again.now_ms(), high_water, "time does not go back to before it stopped");
        assert!(!again.sure(), "and nobody can say how long it was off");
        hands.wall.store(START + 24 * HOUR, Ordering::SeqCst);
        let ahead = Clock::start(Some(saved), high_water, 0, 0, hands.source("boot-2"));
        assert_eq!(ahead.now_ms(), START + 24 * HOUR, "a wall clock ahead of that can only use up what was allowed");
        assert!(!ahead.sure());
        ahead.vouch();
        assert!(ahead.sure() && ahead.saved().sure, "a person can say so, and that is written down");
        again.heard(at(START + HOUR));
        assert!(again.sure(), "or the platform can");
    }

    #[test]
    fn a_boot_clock_that_cannot_be_read_across_restarts_is_a_reboot() {
        let hands = Hands::new();
        let clock = started(&hands);
        let saved = clock.saved();
        hands.boot.store(1, Ordering::SeqCst);
        assert!(!Clock::start(Some(saved), START, 0, 0, hands.source("boot-1")).sure(), "the same id and a clock behind where it was");
        let unnamed = Source { boot_id: None, ..hands.source("x") };
        let nameless = Clock::start(None, 0, 0, 0, unnamed);
        assert!(!nameless.sure());
        let saved = nameless.saved();
        assert!(!Clock::start(Some(saved), 0, 0, 0, Source { boot_id: None, ..hands.source("x") }).sure(), "no name, no continuity");
    }

    #[test]
    fn the_platform_is_believed_going_backwards() {
        let hands = Hands::new();
        // A wall clock a year ahead wrote that down, and then signatures could never be taken.
        let year = 365 * 24 * HOUR;
        let clock = Clock::start(None, START + year, 0, 0, hands.source("boot-1"));
        let wrong = clock.signing_ms();
        assert!(wrong > START + year - 1000);
        clock.refused(at(START));
        assert_eq!(clock.now_ms(), START);
        let signed = clock.signing_ms();
        assert!((signed - START).abs() <= MAX_CLOCK_SKEW_MS, "it can sign again: {}", signed - START);
        assert!(signed > START + MAX_CLOCK_SKEW_MS - 10_000, "past anything the platform last took");
    }

    #[test]
    fn signatures_always_rise() {
        let hands = Hands::new();
        let clock = Clock::start(None, 0, 0, 0, hands.source("boot-1"));
        let mut last = 0;
        for _ in 0..1000 {
            let t = clock.signing_ms();
            assert!(t > last);
            last = t;
        }
        clock.heard(at(START - HOUR));
        assert!(clock.signing_ms() > last, "and hearing the time does not reset them: it took those");
    }

    #[test]
    fn a_slow_crystal_cannot_stretch_a_lease() {
        let hands = Hands::new();
        let clock = Clock::start(None, 0, 0, 100, hands.source("boot-1"));
        clock.heard(at(START));
        hands.pass(72 * HOUR);
        let measured = clock.now_ms() - START - 72 * HOUR;
        assert_eq!(Duration::milliseconds(measured).num_seconds(), 25, "100 ppm over three days");
    }
}
