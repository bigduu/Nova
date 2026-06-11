//! End-to-end tests for the capture-worker subprocess isolation.
//!
//! Spawns the real `nova --capture-worker` child and drives it. Needs Screen
//! Recording permission, so `#[ignore]`d by default. Run SINGLE-THREADED — each
//! test drives its own worker, and concurrent screen captures from multiple
//! processes contend on the window server (production has one serialized worker):
//!   cargo test --test e2e_capture_worker -- --ignored --test-threads=1

use nova::capture::worker::{CaptureRequest, CaptureWorker};

/// Point the worker at the built nova binary (the test runs under a different
/// executable, so `current_exe` would be wrong).
fn worker() -> CaptureWorker {
    std::env::set_var("NOVA_CAPTURE_WORKER_BIN", env!("CARGO_BIN_EXE_nova"));
    CaptureWorker::new()
}

#[test]
#[ignore = "spawns the capture worker; needs Screen Recording permission"]
fn worker_captures_the_display() {
    let w = worker();
    let raw = w
        .capture(&CaptureRequest::Display)
        .expect("worker should capture the display");
    assert!(
        raw.image.width() > 0 && raw.image.height() > 0,
        "captured image must be non-empty"
    );
    assert!(raw.image.width() <= 1280 && raw.image.height() <= 1280);
    assert!(
        raw.window_pid.is_none(),
        "display capture has no window pid"
    );
}

/// The whole point of the subprocess: after a worker is killed (what the server
/// does when a capture hangs), the next capture must transparently respawn a
/// fresh worker and succeed.
#[test]
#[ignore = "spawns the capture worker; needs Screen Recording permission"]
fn worker_recovers_after_kill() {
    let w = worker();

    let first = w.capture(&CaptureRequest::Display).expect("first capture");
    assert!(first.image.width() > 0);

    // Simulate the server's timeout recovery.
    w.kill();

    // Must respawn and work — not return a dead-pipe error forever.
    let second = w
        .capture(&CaptureRequest::Display)
        .expect("capture after kill should respawn the worker and succeed");
    assert!(
        second.image.width() > 0,
        "recovered capture must be non-empty"
    );
}

/// Many sequential captures reuse the one persistent worker without leaking
/// processes or wedging.
#[test]
#[ignore = "spawns the capture worker; needs Screen Recording permission"]
fn worker_handles_repeated_captures() {
    let w = worker();
    for i in 0..5 {
        let raw = w
            .capture(&CaptureRequest::Display)
            .unwrap_or_else(|e| panic!("capture {i} failed: {e}"));
        assert!(raw.image.width() > 0, "capture {i} empty");
    }
}
