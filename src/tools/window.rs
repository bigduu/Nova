/// Window management tools — list windows via the shared capture daemon.
///
/// These USED to call `SCShareableContent` directly, which quietly made every
/// nova server process a long-lived replayd XPC client — and two same-binary
/// replayd clients evict each other's identity in a connect/cancel storm that
/// wedges stream starts (see `capture::broker`). All ScreenCaptureKit traffic,
/// including metadata-only enumeration, now goes through the one daemon.
///
/// The actual OS call now lives behind `crate::platform::WindowManager`
/// (`src/platform/mac/window.rs`); the pure business logic below (frontmost
/// heuristics, largest-area matching, `CGWindowID` disambiguation) stays here
/// unchanged, just adapted to read `crate::platform::WindowHandle` instead of
/// the old `WireWindow`.
use crate::types::WindowInfo;

/// List all on-screen windows across all applications.
/// Excludes desktop windows (wallpaper) and windows without titles.
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    Ok(crate::platform::window_manager()
        .list_windows()?
        .into_iter()
        .filter(|w| !w.title.is_empty())
        .map(|w| WindowInfo {
            title: w.title,
            app_name: w.app_name,
            x: w.x,
            y: w.y,
            width: w.width,
            height: w.height,
            is_visible: w.is_visible,
        })
        .collect())
}

/// Process id and global-logical frame `(x, y, w, h)` of the LARGEST on-screen
/// window whose title OR owning-app name contains `query` (case-insensitive).
/// For CLI debugging (`--dump-ax`, `--marks`) where we target an app by name.
/// The frame is used as the off-screen cull clip. Needs Screen Recording.
pub fn pid_for_window(query: &str) -> Option<(i32, (f64, f64, f64, f64))> {
    let q = query.to_lowercase();
    crate::platform::window_manager()
        .list_windows()
        .ok()?
        .into_iter()
        .filter(|w| {
            w.pid > 0
                && (w.title.to_lowercase().contains(&q) || w.app_name.to_lowercase().contains(&q))
        })
        // Largest-area match, mirroring the capture path's `resolve_window`. A
        // query like "Arc" matches several windows — the real main window PLUS
        // tiny 600x600 auxiliary/PiP windows — and first-match returned an
        // auxiliary one, giving a content-less clip that marked nothing. Largest
        // area is the real window and is stable across enumeration-order shifts.
        .max_by(|a, b| {
            (a.width * a.height)
                .partial_cmp(&(b.width * b.height))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|w| (w.pid, (w.x, w.y, w.width, w.height)))
}

/// `CGWindowID` of the window owned by `pid` whose global frame matches `rect`
/// (the capture's clip). Lets mark discovery match an AX window node to the
/// captured window EXACTLY (via `_AXUIElementGetWindow`) instead of by size —
/// which is ambiguous when an app has two same-sized windows. Two windows can't
/// share the same global origin, so the closest-by-position match is unique.
/// `None` if no window is within `TOL` points (or no Screen Recording).
pub fn window_id_for_rect(pid: i32, rect: (f64, f64, f64, f64)) -> Option<u32> {
    const TOL: f64 = 4.0;
    let (rx, ry, rw, rh) = rect;
    crate::platform::window_manager()
        .list_windows()
        .ok()?
        .into_iter()
        .filter(|w| w.pid == pid && w.id != 0)
        .map(|w| {
            let d =
                (w.x - rx).abs() + (w.y - ry).abs() + (w.width - rw).abs() + (w.height - rh).abs();
            (d, w.id)
        })
        .filter(|(d, _)| *d <= TOL)
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, id)| id as u32)
}

/// System UI layers to skip when guessing the frontmost user app.
fn is_system_ui(app: &str) -> bool {
    matches!(
        app,
        "" | "Window Server"
            | "Dock"
            | "SystemUIServer"
            | "Spotlight"
            | "Control Center"
            | "控制中心"
            | "Notification Center"
            | "通知中心"
    )
}

/// Best-effort process id of the frontmost user application — the first
/// on-screen, titled window that isn't a system UI layer (the daemon lists
/// windows frontmost-first). Used to scope Set-of-Mark element discovery.
/// Returns `None` if nothing suitable is found (e.g. no Screen Recording
/// permission).
pub fn frontmost_app_pid() -> Option<i32> {
    crate::platform::window_manager()
        .list_windows()
        .ok()?
        .into_iter()
        .find_map(|w: crate::platform::WindowHandle| {
            if w.title.is_empty() || is_system_ui(&w.app_name) {
                return None;
            }
            (w.pid > 0).then_some(w.pid)
        })
}
