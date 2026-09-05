//! Keep AppKit's main run loop live while Tokio serves desktop requests.
//!
//! NSWorkspace.runningApplications only reflects launches and exits after the
//! main run loop runs in a common mode. A Tokio executor on the main thread
//! does not service those notifications; the capture worker's loop is separate.

use anyhow::{Context, Result};
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use std::future::Future;
use std::time::{Duration, Instant};

/// Run a desktop service until it completes, without opening UI or requesting
/// permissions. Call from the process main thread after its existing bootstrap.
pub fn run(
    runtime: &tokio::runtime::Runtime,
    service: impl Future<Output = Result<()>> + Send + 'static,
) -> Result<()> {
    anyhow::ensure!(
        objc2::MainThreadMarker::new().is_some(),
        "desktop event loop must run on the process main thread"
    );
    let task = runtime.spawn(service);
    let interval = Duration::from_millis(20);
    while !task.is_finished() {
        let started = Instant::now();
        // SAFETY: the framework-owned default mode is valid for the lifetime
        // of the process. It belongs to the main loop's common modes.
        objc2::rc::autoreleasepool(|_| {
            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, interval, false);
        });
        // A loop with no sources returns immediately. Backfill that interval
        // so an idle service cannot busy-spin before AppKit installs a source.
        if let Some(remainder) = interval.checked_sub(started.elapsed()) {
            std::thread::sleep(remainder);
        }
    }
    runtime
        .block_on(task)
        .context("desktop service task failed")?
}
