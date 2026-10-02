/// Display geometry — logical/physical dimensions of the primary display.
///
/// Click and cursor coordinates live in the *global logical point* space that
/// CoreGraphics (CGEvent / CGWarpMouseCursorPosition) uses, so we read the
/// primary display's geometry from `CGDisplay` rather than ScreenCaptureKit.
/// Screen-recording *permission* is a separate concern, detected via
/// ScreenCaptureKit (see [`screen_recording_available`]).
use crate::display::scaling::{compute_target_dims, screen_to_logical};
use crate::display::view::ViewFrame;
use crate::types::{DisplayInfo, ScreenCoord};
use core_graphics::display::CGDisplay;
use screencapturekit::shareable_content::SCShareableContent;

/// Get the primary display's geometry in logical points, with the real
/// backing-scale factor (2.0 on Retina). Logical dims are the coordinate
/// space mouse events are posted in.
pub fn primary_display() -> DisplayInfo {
    let main = CGDisplay::main();
    let bounds = main.bounds();
    let logical_w = bounds.size.width;
    let logical_h = bounds.size.height;
    // pixels_wide/high report the physical backing resolution; the ratio to
    // logical points is the backing-scale factor.
    let scale_factor = if logical_w > 0.0 {
        main.pixels_wide() as f64 / logical_w
    } else {
        1.0
    };
    DisplayInfo {
        id: main.id,
        width: logical_w as u32,
        height: logical_h as u32,
        scale_factor,
        is_primary: true,
    }
}

/// Backing-scale factor (2.0 on Retina, 1.0 otherwise) of the display that
/// contains the global-logical point `(x, y)`.
///
/// Needed for per-window capture sizing on a MIXED-DPI multi-monitor setup: a
/// window can live on a display whose scale differs from the primary's, so
/// `primary_display().scale_factor` would over- or under-sample it. Finds the
/// display whose logical bounds contain the point and returns its physical/
/// logical ratio; falls back to the primary's scale (then 1.0) when no display
/// matches (e.g. an off-screen point).
pub fn scale_factor_at(x: f64, y: f64) -> f64 {
    if let Ok(ids) = CGDisplay::active_displays() {
        for id in ids {
            let d = CGDisplay::new(id);
            let b = d.bounds();
            let in_x = x >= b.origin.x && x < b.origin.x + b.size.width;
            let in_y = y >= b.origin.y && y < b.origin.y + b.size.height;
            if in_x && in_y && b.size.width > 0.0 {
                return (d.pixels_wide() as f64 / b.size.width).max(1.0);
            }
        }
    }
    primary_display().scale_factor.max(1.0)
}

/// Convert a coordinate from screenshot space (what the LLM sees) to the
/// global logical point coordinates used to post mouse events.
///
/// Screenshots are deterministically derived from the primary display
/// ([`compute_target_dims`]), so the screenshot dimensions can be recomputed
/// here without the server having to remember the last capture.
pub fn screen_to_logical_coords(x: f64, y: f64) -> (f64, f64) {
    let display = primary_display();
    let dims = compute_target_dims(display.width, display.height);
    let logical = screen_to_logical(
        ScreenCoord { x, y },
        (dims.width as f64, dims.height as f64),
        &display,
    );
    (logical.x, logical.y)
}

/// The [`ViewFrame`] for a full-display screenshot of the main display — the
/// default coordinate frame when no window has been captured.
pub fn display_view_frame() -> ViewFrame {
    let display = primary_display();
    let dims = compute_target_dims(display.width, display.height);
    ViewFrame {
        // The main display is at the global origin by macOS definition.
        origin: (0.0, 0.0),
        region: (display.width as f64, display.height as f64),
        screenshot: (dims.width as f64, dims.height as f64),
    }
}

/// Whether Screen Recording permission is granted (and capture is possible).
///
/// `SCShareableContent::get` fails (or yields no displays) when the host app
/// has not been granted Screen Recording in System Settings, which is exactly
/// the signal we want — unlike `CGDisplay`, which works without permission.
pub fn screen_recording_available() -> bool {
    match SCShareableContent::get() {
        Ok(content) => !content.displays().is_empty(),
        Err(_) => false,
    }
}

/// Request Screen Recording (screen-capture) access, prompting the user on the
/// first undecided run and returning whether it is currently granted.
///
/// This is the macOS-recommended way to ask (`CGRequestScreenCaptureAccess`): it
/// surfaces the system prompt when there is no prior decision and is a no-op (no
/// prompt, returns `true`) once granted. Call only for an explicit user action
/// in the main app process, never during startup/refresh or from the headless
/// capture worker. Use [`preflight_screen_capture`] for passive status checks.
pub fn request_screen_recording_access() -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    // SAFETY: a CoreGraphics C function taking no args and returning a bool; it
    // triggers the TCC request and reports the current grant. The menu invokes
    // this on the main thread only after the user selects the request action.
    unsafe { CGRequestScreenCaptureAccess() }
}

/// One-line snapshot of who-am-I + screen-capture authorization, for tracing
/// *why* capture is allowed or denied.
///
/// The critical fact this surfaces: when nova runs as a **child** of another app
/// (e.g. Bodhi), macOS attributes the Screen Recording (TCC) grant to the
/// **responsible process** — the parent app bundle — NOT to nova's own signed
/// identity. So if the parent (`parent=`) is an ad-hoc/linker-signed app whose
/// code identity rotates per build, the user's grant evaporates on every rebuild
/// and `preflight=false` even though nova itself is properly signed.
///
/// Uses only `CGPreflightScreenCaptureAccess` (a cheap TCC-db lookup). It does
/// NOT call `SCShareableContent::get` on purpose — that one can hang on a wedged
/// replayd, and a diagnostic must never hang the path it's instrumenting.
pub fn permission_diagnostics() -> String {
    extern "C" {
        fn getppid() -> i32;
    }
    // SAFETY: argless posix call returning a pid.
    let preflight = preflight_screen_capture();
    let pid = std::process::id();
    let ppid = unsafe { getppid() };
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let parent = proc_path(ppid).unwrap_or_else(|| "?".into());
    format!(
        "pid={pid} ppid={ppid} preflight(ScreenCapture)={preflight} exe={exe} responsible/parent={parent}"
    )
}

/// Whether this process's responsibility chain holds the Screen Recording TCC
/// grant. A cheap CoreGraphics TCC-db lookup — never touches replayd, so it is
/// safe from any process (unlike `SCShareableContent::get`).
pub fn preflight_screen_capture() -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    // SAFETY: argless TCC lookup returning a bool.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Absolute executable path for `pid` via libproc's `proc_pidpath` (part of
/// libSystem; no extra link needed). `None` if the lookup fails.
pub(crate) fn proc_path(pid: i32) -> Option<String> {
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut std::os::raw::c_void, buffersize: u32) -> i32;
    }
    let mut buf = vec![0u8; 4096];
    // SAFETY: writes at most `buf.len()` bytes into our buffer; returns the byte count.
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr() as *mut _, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

/// List all available displays (via ScreenCaptureKit; requires permission).
pub fn list_displays() -> Vec<DisplayInfo> {
    let Ok(content) = SCShareableContent::get() else {
        return vec![];
    };
    content
        .displays()
        .iter()
        .enumerate()
        .map(|(i, d)| DisplayInfo {
            id: d.display_id(),
            width: d.width(),
            height: d.height(),
            scale_factor: 1.0,
            is_primary: i == 0,
        })
        .collect()
}
