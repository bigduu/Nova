//! Decisive test: does capture hang under `#[tokio::main]` when the main thread
//! is consumed by the tokio executor from the very start (like the real nova
//! binary), with capture only ever invoked via spawn_blocking?
//!
//! If this hangs (~timeout) while `bench_capture` (which captures on a clean
//! main thread first) is fast, the root cause is: ScreenCaptureKit needs a
//! pumped main run loop for its completion handler, which `#[tokio::main]`
//! never provides.
//!
//! Run: `cargo run --release --example bench_tokio_main`

use std::time::{Duration, Instant};

use nova::capture::screenshot::{capture_display_with, CaptureOptions};

#[tokio::main]
async fn main() {
    eprintln!("tokio::main capture test — main thread is the executor from t=0");

    // Capture ONLY via spawn_blocking — never on a clean main thread.
    let t = Instant::now();
    let h = tokio::task::spawn_blocking(|| {
        let s = Instant::now();
        let r = capture_display_with(CaptureOptions::default());
        (s.elapsed().as_secs_f64() * 1000.0, r.is_ok())
    });
    // Same 20s guard the server uses, so we observe the hang as a timeout.
    match tokio::time::timeout(Duration::from_secs(20), h).await {
        Ok(Ok((dur, ok))) => eprintln!(
            "  spawn_blocking capture: {dur:.0} ms ok={ok}  (total {:.0} ms)",
            t.elapsed().as_secs_f64() * 1000.0
        ),
        Ok(Err(e)) => eprintln!("  join error: {e}"),
        Err(_) => eprintln!(
            "  TIMED OUT after 20s — capture HUNG under tokio::main (total {:.0} ms)",
            t.elapsed().as_secs_f64() * 1000.0
        ),
    }
}
