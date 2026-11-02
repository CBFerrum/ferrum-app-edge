//! The site runtime (Ferrum docs/plan/industrial.md): one binary, next to the equipment, that keeps work
//! going when the link to the platform is down, inside what was authorized before it went.
//!
//! It coordinates. Motion, machine sequencing and safety functions stay with their controllers,
//! and nothing here is a real-time or functional-safety component: a safety function must never
//! depend on this process being alive.

pub mod adapter;
pub mod adapters;
pub mod clock;
pub mod config;
pub mod console;
pub mod export;
pub mod journal;
pub mod link;
pub mod peer;
pub mod relay;
pub mod runtime;
pub mod sinks;
pub mod update;

// In the library rather than the binary, so that the tests run on what ships. On musl's own allocator
// a gateway stored about 15% fewer readings at full load than the same code on glibc. This one stores
// as many, and settles at the same memory after a peak about 30 MB higher.
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// A deliberate crash at a named point, for the recovery tests: set `FERRUM_SITE_CRASH_AT`.
#[inline]
pub fn crash_point(_name: &str) {
    #[cfg(feature = "failpoints")]
    if std::env::var("FERRUM_SITE_CRASH_AT").is_ok_and(|p| p == _name) {
        std::process::abort();
    }
}
