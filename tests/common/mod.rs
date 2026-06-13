//! Shared helpers for nova's end-to-end tests.
//!
//! (A module under `tests/` — Cargo compiles it into each test binary that
//! declares `mod common;`, NOT as its own test target, so nothing here runs as a
//! test on its own.)

use std::time::Duration;

/// Point the capture broker at the built nova binary and an isolated per-test
/// socket. REQUIRED before any test path that reaches the capture daemon
/// (screenshots through the server, `tools::window::list_windows`, …):
/// without it the broker would try to run THIS test-harness executable with
/// `--capture-daemon` (which can't work) and would touch the user's real
/// daemon socket/lockfile.
#[allow(dead_code)] // not every test binary that includes this module uses it
pub fn use_isolated_capture_daemon() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::env::set_var("NOVA_CAPTURE_BIN", env!("CARGO_BIN_EXE_nova"));
        let sock = format!("/tmp/nova-test-cap-{}.sock", std::process::id());
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_file(format!("{sock}.lock"));
        std::env::set_var("NOVA_CAPTURE_SOCK", &sock);
        // Stray test daemons share the production binary PATH, so until they
        // idle-exit they collide with the user's real daemon (same-binary
        // replayd identity). Keep their lives short.
        std::env::set_var("NOVA_DAEMON_IDLE_EXIT_SECS", "20");
        // And kill every daemon that is ALREADY running (production ones, and
        // a previous suite's stragglers): two same-binary daemons cannot hold
        // streams concurrently — a live production daemon would wedge every
        // stream start our test daemon makes (verified live: test display
        // captures hung at `stream: start` exactly while a production daemon
        // held its warm stream). They respawn on demand; a user session at
        // most sees one transient capture error while tests run.
        let _ = std::process::Command::new("/usr/bin/pkill")
            .args(["-f", "--", "--capture-daemon"])
            .status();
        std::thread::sleep(Duration::from_millis(400));
    });
}

/// Run a blocking call on a side thread and FAIL (panic) if it doesn't finish in
/// `secs`, instead of hanging the whole test run.
///
/// A wedged ScreenCaptureKit `start_capture` / `SCShareableContent::get` can't be
/// cancelled in-thread, so the spawned worker thread may leak until the test
/// process exits — but the TEST fails fast, which is the point: a replayd wedge
/// must surface as a failed test, never as a hung suite (which is what happens
/// when these blocking capture calls run unbounded — e.g. forgetting
/// `--test-threads=1` lets concurrent cold starts wedge replayd and hang forever).
#[allow(dead_code)] // not every test binary that includes this module uses it
pub fn with_timeout<T: Send + 'static>(
    secs: u64,
    label: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(v) => v,
        Err(_) => panic!(
            "{label} did not complete within {secs}s — capture/replayd is wedged; \
             failing fast instead of hanging the run"
        ),
    }
}
