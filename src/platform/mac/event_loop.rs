//! Keep AppKit's main run loop live while Tokio serves desktop requests.
//!
//! NSWorkspace.runningApplications only reflects launches and exits after the
//! main run loop runs in a common mode. A Tokio executor on the main thread
//! does not service those notifications; the capture worker's loop is separate.

use anyhow::{Context, Result};
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use std::future::Future;
use std::time::{Duration, Instant};

/// Final entrypoint for the menu app process, not an embeddable runtime API.
/// The UI stops/joins its listener before returning. Native RPC workers may
/// still be inside uncancellable blocking APIs, so do not wait for them while
/// exiting the process. The caller must immediately return this result from
/// main; process exit, not background shutdown, reclaims those workers.
pub fn run_app_process(
    runtime: tokio::runtime::Runtime,
    ui: impl FnOnce(&tokio::runtime::Runtime) -> Result<()>,
) -> Result<()> {
    let result = ui(&runtime);
    runtime.shutdown_background();
    result
}

/// The menu-bar app needs AppKit event dispatch as well as CF notifications.
/// Keep this separate from the no-UI service loop used by direct transports
/// and resident discovery tests: those must not create an NSApplication.
pub(crate) fn pump_application(app: &objc2_app_kit::NSApplication) {
    use objc2_app_kit::NSEventMask;
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
    objc2::rc::autoreleasepool(|_| {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
        // SAFETY: Foundation owns this static run-loop mode for the process.
        let mode = unsafe { NSDefaultRunLoopMode };
        if let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&until),
            mode,
            true,
        ) {
            app.sendEvent(&event);
        }
        app.updateWindows();
    });
}

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
