//! Pause points for the crate's own unit tests, compiled only under `cfg(test)`. The tests in
//! `hooked` use them to hold a race window open, or to kill a child process at a crash point.
//!
//! When `MAESTRO_FLOW_TEST_PAUSE` is `<point>@<dir>`, the process writes `<dir>/reached` on
//! reaching `<point>` and waits (up to 30 s) for `<dir>/go`. Unset, every pause is a no-op. The
//! variable is read once per process.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The variable the tests set on a child process (`tests/common`).
const PAUSE_ENV: &str = "MAESTRO_FLOW_TEST_PAUSE";

pub(crate) fn pause(point: &str) {
    static SPEC: OnceLock<Option<(String, PathBuf)>> = OnceLock::new();
    let spec = SPEC.get_or_init(|| {
        let value = std::env::var(PAUSE_ENV).ok()?;
        let (name, dir) = value.split_once('@')?;
        Some((name.to_string(), PathBuf::from(dir)))
    });
    let Some((name, dir)) = spec else { return };
    if name != point {
        return;
    }
    let _ = std::fs::write(dir.join("reached"), point);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !dir.join("go").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Whether a test asks `operator::archive` to treat its destination as on another filesystem.
pub(crate) fn other_device() -> bool {
    std::env::var_os("MAESTRO_FLOW_TEST_OTHER_DEVICE").is_some()
}
