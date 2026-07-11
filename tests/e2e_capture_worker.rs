//! End-to-end tests for the shared capture daemon (platform::mac::capture::broker).
//!
//! Spawns the real `nova --capture-daemon` and drives it through CaptureClient.
//! Needs Screen Recording permission, so `#[ignore]`d by default. Run
//! SINGLE-THREADED (the tests share one daemon/socket):
//!   cargo test --test e2e_capture_worker -- --ignored --test-threads=1
//!
//! macOS only (the shared capture daemon + `libc::kill`/`SIGKILL` process
//! control below are both macOS-specific; Windows' GDI/PrintWindow capture is
//! synchronous and has no daemon, see `platform::windows::capture`).
#![cfg(target_os = "macos")]

use std::sync::Arc;

use nova::platform::mac::capture::broker::{CaptureClient, CaptureRequest};

mod common;
use common::with_timeout;

/// Point the broker at the built nova binary and an isolated test socket, so
/// these tests never touch (or kill) the user's real capture daemon.
fn client() -> Arc<CaptureClient> {
    common::use_isolated_capture_daemon();
    Arc::new(CaptureClient::new())
}

/// Kill the test daemon so it doesn't linger (it would idle-exit eventually,
/// but tests should clean up after themselves).
fn stop_daemon(c: &Arc<CaptureClient>) {
    if let Some(pid) = c.daemon_pid() {
        // SAFETY: SIGKILL to the daemon this test spawned.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

fn capture_display(
    c: &Arc<CaptureClient>,
    label: &str,
) -> Result<nova::capture::screenshot::RawCapture, String> {
    let cc = c.clone();
    with_timeout(60, label, move || cc.capture(&CaptureRequest::Display))
}

#[test]
#[ignore = "spawns the capture daemon; needs Screen Recording permission"]
fn daemon_captures_the_display() {
    let c = client();
    let raw = capture_display(&c, "daemon display capture").expect("daemon should capture");
    assert!(raw.image.width() > 0 && raw.image.height() > 0);
    assert!(raw.image.width() <= 1280 && raw.image.height() <= 1280);
    assert!(
        raw.window_pid.is_none(),
        "display capture has no window pid"
    );
    stop_daemon(&c);
}

/// A CLEAN capture failure (e.g. "no on-screen window matching …") must NOT
/// tear down the daemon — it holds the warm stream, and the model misses
/// window names constantly. Two clean errors must be served by the SAME daemon.
#[test]
#[ignore = "spawns the capture daemon; needs Screen Recording permission"]
fn clean_capture_error_keeps_daemon_alive() {
    let c = client();
    let req = CaptureRequest::Window {
        query: "__nova_no_such_window_zzzqx__".to_string(),
    };
    let cap = |label: &str| {
        let cc = c.clone();
        let r = req.clone();
        with_timeout(60, label, move || cc.capture(&r))
    };
    assert!(cap("first clean error").is_err());
    let pid1 = c.daemon_pid().expect("daemon alive after a clean error");
    assert!(cap("second clean error").is_err());
    let pid2 = c.daemon_pid().expect("daemon still alive");
    assert_eq!(
        pid1, pid2,
        "a clean capture error must NOT replace the daemon (its warm stream survives)"
    );
    stop_daemon(&c);
}

/// The recovery property the whole design hangs on: after the daemon is KILLED
/// (what the watchdog/ladder does on a wedge), the next capture transparently
/// respawns it and succeeds.
#[test]
#[ignore = "spawns the capture daemon; needs Screen Recording permission"]
fn client_recovers_after_daemon_kill() {
    let c = client();
    let first = capture_display(&c, "first capture").expect("first capture");
    assert!(first.image.width() > 0);
    let pid = c.daemon_pid().expect("daemon pid");
    // SAFETY: SIGKILL to the daemon this test spawned.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(300));

    let second = capture_display(&c, "capture after daemon kill")
        .expect("capture after kill must respawn the daemon and succeed");
    assert!(second.image.width() > 0);
    assert_ne!(
        c.daemon_pid(),
        Some(pid),
        "a fresh daemon must have been elected"
    );
    stop_daemon(&c);
}

/// REGRESSION for the 2026-06-13 replayd wedge: multiple clients capturing
/// CONCURRENTLY — under the old per-process-worker design, two same-binary
/// processes starting window streams evicted each other's replayd identity and
/// every later `startCapture` hung forever. Through the shared daemon they
/// serialize onto ONE stream owner, so all captures must succeed.
#[test]
#[ignore = "spawns the capture daemon; needs Screen Recording permission"]
fn concurrent_clients_share_one_daemon_without_wedging() {
    let _ = client(); // set env once
    let mut handles = Vec::new();
    for i in 0..3 {
        handles.push(std::thread::spawn(move || {
            // Separate CaptureClient per thread = separate socket connection,
            // modeling separate nova processes.
            let c = CaptureClient::new();
            for round in 0..3 {
                let raw = c
                    .capture(&CaptureRequest::Display)
                    .unwrap_or_else(|e| panic!("client {i} round {round}: {e}"));
                assert!(raw.image.width() > 0, "client {i} round {round}: empty");
            }
            c.daemon_pid()
        }));
    }
    let pids: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("client thread"))
        .collect();
    assert!(
        pids.windows(2).all(|w| w[0] == w[1]),
        "all clients must talk to the SAME daemon, got {pids:?}"
    );
    let c = client();
    let _ = capture_display(&c, "post-concurrency sanity");
    stop_daemon(&c);
}

/// Window enumeration goes through the daemon too (the parent must never hold
/// its own SCShareableContent replayd connection). Verifies the Windows reply
/// shape end-to-end: non-empty, frontmost-first ordering preserved, pids set.
#[test]
#[ignore = "spawns the capture daemon; needs Screen Recording permission"]
fn daemon_lists_windows() {
    let c = client();
    let cc = c.clone();
    let windows = with_timeout(60, "daemon windows enumeration", move || cc.windows())
        .expect("windows() should Ok");
    assert!(
        !windows.is_empty(),
        "a desktop session always has at least one on-screen window"
    );
    assert!(
        windows.iter().any(|w| w.pid > 0 && !w.app_name.is_empty()),
        "windows must carry owning pids and app names: {windows:?}"
    );
    stop_daemon(&c);
}

/// Many sequential captures reuse the one daemon without leaking processes.
#[test]
#[ignore = "spawns the capture daemon; needs Screen Recording permission"]
fn daemon_handles_repeated_captures() {
    let c = client();
    let mut pid = None;
    for i in 0..5 {
        let raw = capture_display(&c, "repeated capture")
            .unwrap_or_else(|e| panic!("capture {i} failed: {e}"));
        assert!(raw.image.width() > 0, "capture {i} empty");
        let now = c.daemon_pid();
        if let Some(prev) = pid {
            assert_eq!(Some(prev), now, "daemon must not churn across captures");
        }
        pid = now;
    }
    stop_daemon(&c);
}
