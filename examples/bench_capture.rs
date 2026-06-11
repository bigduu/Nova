//! Timing benchmark for the screenshot capture pipeline.
//!
//! Answers: when a screenshot "times out", is it blocking, a parallelism
//! problem, or raw performance? Isolates each phase + tests concurrency.
//!
//! Run: `cargo run --release --example bench_capture`
//! (needs Screen Recording permission for the running terminal).

use std::time::Instant;

use nova::capture::screenshot::{capture_display_with, capture_region_with, CaptureOptions};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn bench_plain(rounds: usize) {
    eprintln!("\n== plain full-display capture x{rounds} (sequential) ==");
    for i in 0..rounds {
        let t = Instant::now();
        match capture_display_with(CaptureOptions::default()) {
            Ok(c) => eprintln!(
                "  round {i}: {:>8.1} ms  ({}x{} px)",
                ms(t),
                c.result.width,
                c.result.height
            ),
            Err(e) => eprintln!("  round {i}: ERROR {e}"),
        }
    }
}

fn bench_region(rounds: usize) {
    eprintln!("\n== region zoom (sourceRect — captures only the region) x{rounds} ==");
    let rect = (100.0, 100.0, 400.0, 300.0);
    for i in 0..rounds {
        let t = Instant::now();
        match capture_region_with(rect, CaptureOptions::default()) {
            Ok(c) => eprintln!(
                "  round {i}: {:>8.1} ms  ({}x{} px out)",
                ms(t),
                c.result.width,
                c.result.height
            ),
            Err(e) => eprintln!("  round {i}: ERROR {e}"),
        }
    }
}

/// The user's explicit hypothesis: "不支持并行" (no parallelism). Fire N captures
/// concurrently on a multi-thread runtime via spawn_blocking and compare total
/// wall-clock to the sequential sum. If ScreenCaptureKit serializes internally
/// (or deadlocks), we'll see it here.
async fn bench_concurrent(n: usize) {
    eprintln!("\n== {n} concurrent captures (spawn_blocking on multi-thread rt) ==");
    let t = Instant::now();
    let mut handles = Vec::new();
    for i in 0..n {
        handles.push(tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            let r = capture_display_with(CaptureOptions::default());
            (i, start.elapsed().as_secs_f64() * 1000.0, r.is_ok())
        }));
    }
    for h in handles {
        match h.await {
            Ok((i, dur, ok)) => eprintln!("  task {i}: {dur:>8.1} ms  ok={ok}"),
            Err(e) => eprintln!("  task join error: {e}"),
        }
    }
    eprintln!(
        "  --- total wall-clock for {n} concurrent: {:>8.1} ms ---",
        ms(t)
    );
}

fn main() {
    eprintln!("nova capture benchmark — measuring where the time goes");
    eprintln!(
        "rt worker threads = {}",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    );

    eprintln!("\n== cold start (first capture pays framework init) ==");
    let t = Instant::now();
    let _ = capture_display_with(CaptureOptions::default());
    eprintln!("  first capture_display_with: {:>8.1} ms", ms(t));

    bench_plain(5);
    bench_region(3);

    // Concurrency test needs a runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        bench_concurrent(4).await;
        bench_concurrent(8).await;
    });

    eprintln!("\ndone.");
}
