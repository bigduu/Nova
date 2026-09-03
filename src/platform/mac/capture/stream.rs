//! Persistent ScreenCaptureKit stream — the churn-free capture path.
//!
//! The root cause of nova's high screenshot failure rate (confirmed by sampling a
//! hung worker) is the ONE-SHOT `SCScreenshotManager::capture_image`: each call
//! opens then tears down a replayd XPC connection, and that teardown's
//! `port_destroyed` notification races replayd's reconnect
//! (`issueSandboxExtensionForMainBundleRead` → `consumeSandboxExtension:processNewConnection:`),
//! which intermittently wedges — the capture's completion never fires and it hangs
//! forever.
//!
//! This replaces it with ONE long-lived `SCStream` that stays connected. Frames
//! arrive continuously on a background GCD dispatch queue into a latest-frame slot;
//! a capture just reads the freshest frame from that slot. Because the connection
//! is never torn down between captures (repeated captures of the same window — the
//! exact failing case — reuse it), there is no per-capture `port_destroyed` and no
//! reconnect wedge. Delivering frames on a custom dispatch queue (not the main
//! queue) also means the worker thread never blocks waiting on a main-queue
//! completion, structurally avoiding the deadlock the one-shot path had.
//!
//! Switching target (display ↔ window, or to a different window) calls
//! `update_content_filter`/`update_configuration` — a lightweight reconfigure of
//! the live stream, not a fresh connection.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use screencapturekit::cg::CGRect;
use screencapturekit::cm::{CMSampleBuffer, CMSampleBufferExt};
use screencapturekit::dispatch_queue::{DispatchQoS, DispatchQueue};
use screencapturekit::screenshot_manager::CGImageExt;
use screencapturekit::shareable_content::SCShareableContent;
use screencapturekit::stream::{
    configuration::SCStreamConfiguration, content_filter::SCContentFilter,
    output_type::SCStreamOutputType, SCStream,
};

use crate::capture::screenshot::{rgba_to_rgb, step, RawCapture};
use crate::display::scaling::{
    compute_target_dims, compute_target_dims_capped, REGION_MAX_DIMENSION, WINDOW_MAX_DIMENSION,
};
use crate::display::view::ViewFrame;

/// What the live stream is currently targeting; used to decide whether a capture
/// can reuse the running stream (no churn) or must reconfigure it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Display,
    Window(u32), // SCWindow::window_id()
    Region(u64), // hash of the (rounded) source rect — different rect ⇒ retarget
}

/// The most recent decoded frame, plus a monotonically increasing sequence number
/// so a capture can wait for a frame produced AFTER it (re)targeted the stream.
struct Latest {
    rgb: image::RgbImage,
    seq: u64,
}

/// Resolved capture target: the SCK filter to point the stream at, the dimensions
/// to render at, the coordinate frame, and (for a window) its owning pid.
struct Resolved {
    filter: SCContentFilter,
    width: u32,
    height: u32,
    /// Set for a region/zoom capture: the sub-rectangle of the display to capture
    /// (display points, top-left origin). `None` captures the whole filter target.
    source_rect: Option<CGRect>,
    view: ViewFrame,
    window_pid: Option<i32>,
    target: Target,
}

/// Owns the long-lived stream. Driven from the single-threaded worker loop, so the
/// stream/target are plain fields; only `latest`/`seq` are shared with the GCD
/// output-handler thread via `Arc`.
pub struct StreamCapturer {
    stream: Option<SCStream>,
    target: Option<Target>,
    latest: Arc<Mutex<Option<Latest>>>,
    seq: Arc<AtomicU64>,
    queue: DispatchQueue,
    /// When the stream last served a capture — drives the idle TTL.
    last_use: Instant,
    /// Raised by the stream's stop/error delegate: the stream died underneath
    /// us (replayd restarted, target window destroyed, …). Without this, a
    /// same-target capture would keep serving the last good frame as a
    /// "success" forever — stale screenshots with no error.
    dead: Arc<AtomicBool>,
}

impl Default for StreamCapturer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for StreamCapturer {
    /// Stop the stream on the way out. Dropping a live `SCStream` only releases
    /// the LOCAL object — replayd keeps the stream registered as long as this
    /// process lives, and a lingering registration is exactly what other
    /// same-binary processes' stream starts collide with (proven by the
    /// selftest's direct capture wedging the daemon spawned right after it).
    fn drop(&mut self) {
        self.reset();
    }
}

impl StreamCapturer {
    pub fn new() -> Self {
        // Register the screen-unlock observer once, on this (the worker capture
        // loop's) thread — the run loop we pump and where the notification lands.
        OBSERVER_ONCE.call_once(register_screen_reveal_observer);
        Self {
            stream: None,
            target: None,
            latest: Arc::new(Mutex::new(None)),
            seq: Arc::new(AtomicU64::new(0)),
            // UserInitiated: deliver frames promptly without main-queue contention.
            queue: DispatchQueue::new("com.zenith.nova.capture", DispatchQoS::UserInitiated),
            last_use: Instant::now(),
            dead: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Consume the dead-stream flag; tear down + settle if it was raised.
    fn reset_if_dead(&mut self) {
        if self.dead.swap(false, Ordering::AcqRel) && self.stream.is_some() {
            step("stream: stopped underneath us (replayd restarted / target gone) — rebuilding");
            self.reset();
            pump_run_loop(0.3);
        }
    }

    /// Idle-time maintenance — call between requests (the daemon's capture
    /// thread does, every pump tick):
    /// - eagerly tear down a stream that spanned a screen lock/unlock, instead
    ///   of waiting for the next capture: a stale-but-registered stream sitting
    ///   idle is exactly what new stream starts collide with;
    /// - stop a stream nobody has used for [`STREAM_IDLE_TTL`] — releases the
    ///   replayd session (and the "screen is being captured" indicator) while
    ///   nova is not actively looking.
    pub fn housekeeping(&mut self) {
        self.reset_if_dead();
        if SCREEN_REVEALED.swap(false, Ordering::Relaxed) && self.stream.is_some() {
            step(
                "stream: screen revealed (unlock/screensaver) — eagerly invalidating stale stream",
            );
            self.reset();
            pump_run_loop(0.3); // let replayd finish the teardown
        }
        if self.stream.is_some() && self.last_use.elapsed() >= STREAM_IDLE_TTL {
            step("stream: idle TTL expired — stopping stream until the next capture");
            self.reset();
        }
    }

    /// Capture the main display via the live stream. Returns the freshest frame.
    pub fn capture_display(&mut self) -> Result<RawCapture, String> {
        let resolved = resolve_display()?;
        self.capture_resolved(resolved)
    }

    /// Capture a single on-screen window (matched like the one-shot path) via the
    /// live stream.
    pub fn capture_window(&mut self, query: &str) -> Result<RawCapture, String> {
        let resolved = resolve_window(query)?;
        self.capture_resolved(resolved)
    }

    /// Capture a sub-rectangle of the display (zoom) via the live stream, using a
    /// source rect on the display filter — native-resolution crop, no separate
    /// one-shot path.
    pub fn capture_region(&mut self, rect: (f64, f64, f64, f64)) -> Result<RawCapture, String> {
        let resolved = resolve_region(rect)?;
        self.capture_resolved(resolved)
    }

    fn capture_resolved(&mut self, r: Resolved) -> Result<RawCapture, String> {
        // Drain any pending screen-lock/unlock notification onto this thread's run
        // loop (a few 0-timeout passes service the queued mach message without
        // adding latency), then honor it: a stream that spanned a lock/screensaver
        // is serving a pre-lock frame, so tear it down and cold-rebuild for a
        // guaranteed-fresh first frame. Done BEFORE the cold/retarget/same decision
        // so this very capture starts clean, not the next one.
        for _ in 0..3 {
            pump_run_loop(0.0);
        }
        if SCREEN_REVEALED.swap(false, Ordering::Relaxed) {
            step("stream: screen revealed (unlock/screensaver) since last capture — invalidating stale stream");
            self.reset();
            // Give replayd a beat to finish the async teardown before starting
            // a new stream on the same target. An immediate restart races it
            // and can strand a half-dead registration that every later start
            // collides with (the 23:32 unlock-wedge signature).
            pump_run_loop(0.3);
        }
        // A stream that died underneath us (replayd bounce, window destroyed)
        // must cold-rebuild NOW — its latest-frame slot still holds the last
        // pre-death frame, which a same-target capture would happily serve.
        self.reset_if_dead();
        self.last_use = Instant::now();

        let mut config = SCStreamConfiguration::new()
            .with_width(r.width)
            .with_height(r.height)
            .with_fps(STREAM_FPS)
            // Small queue: we only ever want the LATEST frame, not a backlog.
            .with_queue_depth(3)
            .with_shows_cursor(false);
        if let Some(rect) = r.source_rect {
            config = config.with_source_rect(rect);
        }

        // Retarget only when the target actually changed. The region's rect is
        // baked into its target key, so a different zoom rect already counts as a
        // change here; re-applying the SAME config/filter must be avoided — SCK
        // emits no new frame for an unchanged config, which would stall the wait.
        let was_none = self.stream.is_none();
        let retargeting = self.target != Some(r.target);
        if was_none {
            step(&format!("stream: start (target={:?})", r.target));
            self.start_stream(&r.filter, &config)?;
        } else if retargeting {
            step(&format!("stream: retarget -> {:?}", r.target));
            let s = self.stream.as_ref().unwrap();
            s.update_configuration(&config)
                .map_err(|e| format!("update_configuration: {e}"))?;
            s.update_content_filter(&r.filter)
                .map_err(|e| format!("update_content_filter: {e}"))?;
        }

        // On a cold start or a retarget the latest frame (if any) belongs to the
        // OLD target — clear it and require a brand-new frame so we never return
        // stale/old-target pixels. On the SAME target reuse is fine: the latest
        // frame already reflects the current screen (SCK pushes a frame on every
        // change), so we only briefly wait for an even-fresher one.
        let need_new_target = was_none || retargeting;
        let base = if need_new_target {
            if let Ok(mut slot) = self.latest.lock() {
                *slot = None;
            }
            None
        } else {
            Some(self.seq.load(Ordering::Acquire))
        };
        self.target = Some(r.target);

        let rgb = match self.wait_for_frame(base) {
            Ok(rgb) => rgb,
            Err(e) => {
                // Self-heal: drop the (apparently stalled) stream so the NEXT
                // capture rebuilds a fresh one. No replayd restart, no process
                // kill — just a clean in-process reset. The caller gets a plain
                // error and can retry.
                step(&format!(
                    "stream: stalled ({e}) — dropping stream for rebuild"
                ));
                self.reset();
                return Err(e);
            }
        };

        let (width, height) = (rgb.width(), rgb.height());
        // The stream renders at our requested dims; keep the view's screenshot dims
        // in sync with what actually came back.
        let mut view = r.view;
        view.screenshot = (width as f64, height as f64);
        Ok(RawCapture {
            image: rgb,
            view,
            window_pid: r.window_pid,
        })
    }

    /// Tear down the current stream so the next capture starts a fresh one.
    fn reset(&mut self) {
        if let Some(s) = self.stream.take() {
            // A failed stop means replayd may still hold a half-dead
            // registration for this target — log it so a later wedge can be
            // traced back here. (The daemon's watchdog is the safety net: if
            // that registration wedges the next start, the daemon exits and
            // its death clears every registration.)
            if let Err(e) = s.stop_capture() {
                step(&format!("stream: stop_capture failed during reset ({e})"));
            }
        }
        self.target = None;
        if let Ok(mut slot) = self.latest.lock() {
            *slot = None;
        }
    }

    fn start_stream(
        &mut self,
        filter: &SCContentFilter,
        config: &SCStreamConfiguration,
    ) -> Result<(), String> {
        // The delegate is the ONLY way to learn the stream died underneath us
        // (replayd restart, captured window destroyed): frames just stop, and
        // the latest-frame slot would otherwise serve its stale last frame as
        // a success forever.
        self.dead.store(false, Ordering::Release);
        let dead_on_stop = self.dead.clone();
        let dead_on_err = self.dead.clone();
        let callbacks = screencapturekit::stream::delegate_trait::StreamCallbacks::new()
            .on_stop(move |err| {
                step(&format!("stream: did_stop (err={err:?})"));
                dead_on_stop.store(true, Ordering::Release);
            })
            .on_error(move |e| {
                step(&format!("stream: error delegate fired ({e})"));
                dead_on_err.store(true, Ordering::Release);
            });
        let mut stream = SCStream::new_with_delegate(filter, config, callbacks);
        let latest = self.latest.clone();
        let seq = self.seq.clone();
        stream.add_output_handler_with_queue(
            move |sample: CMSampleBuffer, _ty: SCStreamOutputType| {
                // Idle/blank frames (no screen change) have no decodable image —
                // skip them and keep the previous good frame.
                let Ok(cg) = sample.cg_image() else {
                    return;
                };
                let (w, h) = (cg.width() as u32, cg.height() as u32);
                let Ok(rgba) = cg.rgba_data() else {
                    return;
                };
                let rgb_bytes = rgba_to_rgb(&rgba, w as usize, h as usize);
                let Some(rgb) = image::RgbImage::from_raw(w, h, rgb_bytes) else {
                    return;
                };
                let n = seq.fetch_add(1, Ordering::AcqRel) + 1;
                if let Ok(mut slot) = latest.lock() {
                    *slot = Some(Latest { rgb, seq: n });
                }
            },
            SCStreamOutputType::Screen,
            Some(&self.queue),
        );
        stream
            .start_capture()
            .map_err(|e| format!("start_capture: {e}"))?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Block until a frame with `seq > base` is available, or time out. Polls a
    /// shared slot (never blocks on replayd), so a stream that stalls fails fast
    /// instead of hanging — the caller can then fall back / recover.
    /// Return a frame from the latest-frame slot.
    ///
    /// - `base = None` (cold start / retarget): wait until ANY frame is present —
    ///   the first frame of the new target — up to [`FRAME_WAIT_TIMEOUT`].
    /// - `base = Some(seq)` (same target): prefer a frame newer than `seq` if one
    ///   arrives within a short grace (catches a just-happened on-screen change),
    ///   otherwise return the existing latest frame (the screen is static — SCK
    ///   only emits on change, so the latest IS current). Never stalls when a
    ///   frame already exists; only errors if no frame is available at all.
    fn wait_for_frame(&self, base: Option<u64>) -> Result<image::RgbImage, String> {
        let start = Instant::now();
        let deadline = start + FRAME_WAIT_TIMEOUT;
        loop {
            if let Ok(slot) = self.latest.lock() {
                if let Some(latest) = slot.as_ref() {
                    match base {
                        // New target: any frame is the one we want.
                        None => return Ok(latest.rgb.clone()),
                        // Same target: a fresher frame arrived.
                        Some(b) if latest.seq > b => return Ok(latest.rgb.clone()),
                        // Same target, no fresher frame yet but grace elapsed:
                        // the screen is static — the latest frame is current.
                        Some(_) if start.elapsed() >= FRESH_FRAME_GRACE => {
                            return Ok(latest.rgb.clone());
                        }
                        Some(_) => {}
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "stream produced no frame within {FRAME_WAIT_TIMEOUT:?}"
                ));
            }
            // Pump this thread's CFRunLoop instead of a bare sleep. SCStream's
            // frame delivery is scheduled on the run loop; in a plain CLI (no app
            // run loop) the output handler never fires unless it is serviced.
            // Doubles as the poll interval.
            pump_run_loop(0.02);
        }
    }
}

/// Run the current thread's CFRunLoop for `seconds`, so ScreenCaptureKit can
/// deliver stream frames (and service its internal scheduling) while we wait.
/// Also used by the capture daemon's idle loop as its poll sleep.
pub(crate) fn pump_run_loop(seconds: f64) {
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRunLoopRunInMode(
            mode: *const std::ffi::c_void,
            seconds: f64,
            return_after_source_handled: u8,
        ) -> i32;
        static kCFRunLoopDefaultMode: *const std::ffi::c_void;
    }
    // SAFETY: standard CoreFoundation run-loop pump on the current thread.
    let start = Instant::now();
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 0);
    }
    // CFRunLoopRunInMode returns IMMEDIATELY (kCFRunLoopRunFinished) when the run
    // loop has no sources/timers to service — the daemon's steady state between
    // captures, once the idle stream is stopped. Every caller uses this as a
    // ~`seconds` poll interval (the capture thread's idle loop, wait_for_frame),
    // so without backfilling the unslept remainder the idle loop spins a CPU core
    // at 100% (a spinning daemon is also slow to answer handshakes, so clients
    // mistake it for dead and spawn a SECOND daemon — two same-binary SCK clients,
    // the exact replayd wedge this broker exists to prevent). Sleep the remainder
    // so the interval holds regardless of source state; when sources DO exist
    // CFRunLoopRunInMode already ran the full duration and checked_sub yields None
    // (no extra sleep), leaving frame-delivery latency unchanged.
    if seconds > 0.0 {
        if let Some(rem) = Duration::from_secs_f64(seconds).checked_sub(start.elapsed()) {
            std::thread::sleep(rem);
        }
    }
}

/// Set by the screen-unlock / screensaver-stop observer (a CoreFoundation
/// distributed-notification callback). A persistent SCStream STOPS delivering
/// frames while the screen is locked or the screensaver runs, and the system
/// switches to loginwindow's secure session; on unlock our latest-frame slot
/// still holds the pre-lock frame. Without invalidation a same-target capture
/// would keep serving that stale frame indefinitely (the bug screenpipe hit:
/// captures stuck on the lock screen for 50+ minutes). On the next capture we
/// observe this flag and rebuild the stream so the first post-unlock frame is
/// guaranteed fresh. Process-global because there is exactly one capture worker
/// (hence one stream) per process.
static SCREEN_REVEALED: AtomicBool = AtomicBool::new(false);
static OBSERVER_ONCE: Once = Once::new();

/// CoreFoundation distributed-notification callback. Must do the bare minimum
/// (it runs on the run-loop servicing thread): just raise the flag.
extern "C" fn on_screen_revealed(
    _center: *const c_void,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _user_info: *const c_void,
) {
    SCREEN_REVEALED.store(true, Ordering::Relaxed);
}

/// Register (once per process) for the screen-unlock / screensaver-stop
/// distributed notifications, so a stream that spanned a lock/screensaver is
/// invalidated on the next capture. Must be called on the thread whose run loop
/// is pumped (the worker capture loop, i.e. the main thread) — distributed
/// notifications are delivered there. If the center is unavailable this is a
/// no-op and capture simply behaves as before (graceful degradation).
fn register_screen_reveal_observer() {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFNotificationCenterGetDistributedCenter() -> *const c_void;
        fn CFNotificationCenterAddObserver(
            center: *const c_void,
            observer: *const c_void,
            call_back: extern "C" fn(
                *const c_void,
                *mut c_void,
                *const c_void,
                *const c_void,
                *const c_void,
            ),
            name: *const c_void,
            object: *const c_void,
            suspension_behavior: i64,
        );
    }
    // CFNotificationSuspensionBehaviorDeliverImmediately — fire even though we are
    // not a UI app with a foreground run loop in the usual sense.
    const DELIVER_IMMEDIATELY: i64 = 4;
    // SAFETY: standard CFNotificationCenter registration. The name CFStrings are
    // intentionally leaked (`forget`) so they outlive the process-lifetime
    // observer; the callback only touches a static AtomicBool.
    unsafe {
        let center = CFNotificationCenterGetDistributedCenter();
        if center.is_null() {
            return;
        }
        let observer = &SCREEN_REVEALED as *const AtomicBool as *const c_void;
        for name in [
            "com.apple.screenIsUnlocked",
            "com.apple.screensaver.didstop",
        ] {
            let cf = CFString::new(name);
            let name_ref = cf.as_concrete_TypeRef() as *const c_void;
            std::mem::forget(cf);
            CFNotificationCenterAddObserver(
                center,
                observer,
                on_screen_revealed,
                name_ref,
                std::ptr::null(),
                DELIVER_IMMEDIATELY,
            );
        }
    }
}

/// Frames per second for the live stream. Low enough to keep idle GPU/CPU cost
/// negligible, high enough that a freshly-requested frame arrives in well under
/// the old per-capture latency.
const STREAM_FPS: u32 = 10;
/// Max time to wait for a frame before declaring the stream stalled. Kept below
/// the server's per-attempt capture timeout so a stall still leaves room to
/// recover within that window.
const FRAME_WAIT_TIMEOUT: Duration = Duration::from_secs(3);
/// On a same-target capture, how long to wait for an even-fresher frame before
/// accepting the current latest. Long enough to catch a just-rendered change,
/// short enough to stay snappy when the screen is static.
const FRESH_FRAME_GRACE: Duration = Duration::from_millis(350);
/// Stop the warm stream after this much idle time. Long enough that an agent
/// taking screenshots in a loop never pays a cold start; short enough that nova
/// doesn't sit on a replayd session (and the capture indicator) for hours.
const STREAM_IDLE_TTL: Duration = Duration::from_secs(60);

fn resolve_display() -> Result<Resolved, String> {
    step("stream: resolve display (SCShareableContent::get)");
    let content = SCShareableContent::get().map_err(|e| format!("SCShareableContent::get: {e}"))?;
    let displays = content.displays();
    let main_id = core_graphics::display::CGDisplay::main().id;
    let display = displays
        .iter()
        .find(|d| d.display_id() == main_id)
        .or_else(|| displays.first())
        .ok_or_else(|| "no displays found — SCShareableContent lists no displays while the display is asleep/locked; wake it and retry".to_string())?;
    let disp = crate::platform::mac::geometry::primary_display();
    let dims = compute_target_dims(disp.width, disp.height);
    let filter = SCContentFilter::create()
        .with_display(display)
        .with_excluding_windows(&[])
        .build();
    Ok(Resolved {
        filter,
        width: dims.width,
        height: dims.height,
        source_rect: None,
        view: ViewFrame {
            origin: (0.0, 0.0),
            region: (disp.width as f64, disp.height as f64),
            screenshot: (dims.width as f64, dims.height as f64),
        },
        window_pid: None,
        target: Target::Display,
    })
}

/// Resolve a region/zoom: capture the main display filtered to `rect` (display
/// points) via a source rect, at the region's native resolution (capped to the
/// model budget) — replaces the one-shot source-rect path.
fn resolve_region(rect: (f64, f64, f64, f64)) -> Result<Resolved, String> {
    let (x, y, w, h) = rect;
    if ![x, y, w, h].iter().all(|value| value.is_finite()) {
        return Err("region coordinates must all be finite".to_string());
    }
    if x < 0.0 || y < 0.0 || w <= 0.0 || h <= 0.0 {
        return Err("region must have a non-negative origin and positive size".to_string());
    }
    let main = core_graphics::display::CGDisplay::main();
    let logical = main.bounds().size;
    if logical.width <= 0.0 || logical.height <= 0.0 {
        return Err("main display has no geometry".to_string());
    }
    let right = x + w;
    let bottom = y + h;
    if !right.is_finite() || !bottom.is_finite() || right > logical.width || bottom > logical.height
    {
        return Err(format!(
            "region ({x}, {y}, {w}, {h}) lies outside the main display ({}x{}); \
             refusing to clamp because that would invalidate OCR/click coordinates",
            logical.width, logical.height
        ));
    }
    let scale = main.pixels_wide() as f64 / logical.width;
    let region_native_w = (w * scale).round().max(1.0) as u32;
    let region_native_h = (h * scale).round().max(1.0) as u32;
    // A zoom is for reading fine detail — give it the larger region budget
    // (high-res models accept up to 2576px @ 1:1). Already native pixels.
    let dims = compute_target_dims_capped(region_native_w, region_native_h, REGION_MAX_DIMENSION);

    step("stream: resolve region (SCShareableContent::get)");
    let content = SCShareableContent::get().map_err(|e| format!("SCShareableContent::get: {e}"))?;
    let displays = content.displays();
    let main_id = main.id;
    let display = displays
        .iter()
        .find(|d| d.display_id() == main_id)
        .or_else(|| displays.first())
        .ok_or_else(|| "no displays found — SCShareableContent lists no displays while the display is asleep/locked; wake it and retry".to_string())?;
    let filter = SCContentFilter::create()
        .with_display(display)
        .with_excluding_windows(&[])
        .build();

    // Identity = rounded rect, so an identical re-zoom can reuse, others retarget.
    let key = {
        let r = |v: f64| (v.round() as i64) as u64;
        r(x).wrapping_mul(73856093)
            ^ r(y).wrapping_mul(19349663)
            ^ r(w).wrapping_mul(83492791)
            ^ r(h).wrapping_mul(2654435761)
    };
    Ok(Resolved {
        filter,
        width: dims.width,
        height: dims.height,
        source_rect: Some(CGRect::new(x, y, w, h)),
        view: ViewFrame {
            origin: (x, y),
            region: (w, h),
            screenshot: (dims.width as f64, dims.height as f64),
        },
        window_pid: None,
        target: Target::Region(key),
    })
}

fn resolve_window(query: &str) -> Result<Resolved, String> {
    step(&format!(
        "stream: resolve window:{query:?} (SCShareableContent::get)"
    ));
    let content = SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .with_exclude_desktop_windows(true)
        .get()
        .map_err(|e| format!("SCShareableContent::get: {e}"))?;
    let q = query.to_lowercase();
    let windows = content.windows();
    let is_real = |w: &screencapturekit::shareable_content::SCWindow| {
        if !w.is_on_screen() {
            return false;
        }
        if w.title().unwrap_or_default().is_empty() {
            return false;
        }
        let f = w.frame();
        f.size.width > 0.0 && f.size.height > 0.0
    };
    // Pick the LARGEST matching on-screen window, deterministically. A query like
    // "微信" matches several of WeChat's windows; `find` (first match) returns a
    // different one as enumeration order shifts, so consecutive captures would keep
    // RETARGETING the stream (and a retarget that stalls falls back to the
    // wedge-prone one-shot path). Largest-area is stable AND is the real main window.
    let window = windows
        .iter()
        .filter(|w| {
            if !is_real(w) {
                return false;
            }
            let title = w.title().unwrap_or_default();
            let app = w
                .owning_application()
                .map(|a| a.application_name())
                .unwrap_or_default();
            title.to_lowercase().contains(&q) || app.to_lowercase().contains(&q)
        })
        .max_by(|a, b| {
            let area = |w: &screencapturekit::shareable_content::SCWindow| {
                let f = w.frame();
                f.size.width * f.size.height
            };
            area(a)
                .partial_cmp(&area(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .ok_or_else(|| {
            let mut avail: Vec<String> = windows
                .iter()
                .filter(|w| is_real(w))
                .map(|w| {
                    let title = w.title().unwrap_or_default();
                    let app = w
                        .owning_application()
                        .map(|a| a.application_name())
                        .unwrap_or_default();
                    if app.is_empty() {
                        title
                    } else {
                        format!("{app} — {title}")
                    }
                })
                .collect();
            avail.sort();
            avail.dedup();
            let more = avail.len().saturating_sub(25);
            let shown = avail
                .iter()
                .take(25)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ");
            let suffix = if more > 0 {
                format!(" (+{more} more)")
            } else {
                String::new()
            };
            format!(
                "no on-screen window matching {query:?} (matched against on-screen \
                 windows' title or app name, case-insensitive). On-screen windows: \
                 {shown}{suffix}"
            )
        })?;

    let frame = window.frame();
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
        return Err(format!("window {query:?} has zero size"));
    }
    // Size the capture from PHYSICAL (Retina) pixels, not logical points: a
    // window smaller than the cap would otherwise be rendered at 1×, throwing
    // away backing-scale detail and leaving small text soft. Multiplying by the
    // display's scale factor lets SCK render at native resolution, capped to the
    // window budget. `ViewFrame.region` stays in logical points, so clicks map
    // correctly regardless of the output pixel size. Use the scale of the
    // display the window is ON (not the primary's) so a window on a non-primary
    // display in a mixed-DPI setup isn't under- or over-sampled.
    let scale = crate::platform::mac::geometry::scale_factor_at(
        frame.origin.x + frame.size.width / 2.0,
        frame.origin.y + frame.size.height / 2.0,
    );
    let phys_w = (frame.size.width * scale).round().max(1.0) as u32;
    let phys_h = (frame.size.height * scale).round().max(1.0) as u32;
    let dims = compute_target_dims_capped(phys_w, phys_h, WINDOW_MAX_DIMENSION);
    let pid = window.owning_application().map(|a| a.process_id());
    let window_id = window.window_id();
    let filter = SCContentFilter::create().with_window(window).build();
    Ok(Resolved {
        filter,
        width: dims.width,
        height: dims.height,
        source_rect: None,
        view: ViewFrame {
            origin: (frame.origin.x, frame.origin.y),
            region: (frame.size.width, frame.size.height),
            screenshot: (dims.width as f64, dims.height as f64),
        },
        window_pid: pid,
        target: Target::Window(window_id),
    })
}
