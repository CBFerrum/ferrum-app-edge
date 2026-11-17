//! Shared by the tests that need software this repository does not ship.

/// What `name` says (an address, a Python, a jar), or `None`, in which case the test returns and passes.
/// With `EDGE_REAL_SOFTWARE=1` a missing `name` fails the test instead, so that a run meant to prove the
/// real-software rows cannot pass by skipping.
pub fn real_software(name: &str) -> Option<String> {
    let found = std::env::var(name).ok();
    if found.is_none() {
        assert!(
            std::env::var("EDGE_REAL_SOFTWARE").as_deref() != Ok("1"),
            "EDGE_REAL_SOFTWARE=1 and {name} is not set: this test needs real software and would have skipped"
        );
        eprintln!("skipped: {name} is not set");
    }
    found
}
