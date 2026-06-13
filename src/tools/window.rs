/// Window management tools — list windows via the shared capture daemon.
///
/// These USED to call `SCShareableContent` directly, which quietly made every
/// nova server process a long-lived replayd XPC client — and two same-binary
/// replayd clients evict each other's identity in a connect/cancel storm that
/// wedges stream starts (see `capture::broker`). All ScreenCaptureKit traffic,
/// including metadata-only enumeration, now goes through the one daemon.
use crate::capture::broker::{shared_client, WireWindow};
use crate::types::WindowInfo;

/// List all on-screen windows across all applications.
/// Excludes desktop windows (wallpaper) and windows without titles.
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    Ok(shared_client()
        .windows()?
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

/// Process id and global-logical frame `(x, y, w, h)` of the first on-screen
/// window whose title OR owning-app name contains `query` (case-insensitive).
/// For CLI debugging (`--dump-ax`, `--marks`) where we target an app by name.
/// The frame is used as the off-screen cull clip. Needs Screen Recording.
pub fn pid_for_window(query: &str) -> Option<(i32, (f64, f64, f64, f64))> {
    let q = query.to_lowercase();
    shared_client().windows().ok()?.into_iter().find_map(|w| {
        if w.title.to_lowercase().contains(&q) || w.app_name.to_lowercase().contains(&q) {
            (w.pid > 0).then_some((w.pid, (w.x, w.y, w.width, w.height)))
        } else {
            None
        }
    })
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
    shared_client()
        .windows()
        .ok()?
        .into_iter()
        .find_map(|w: WireWindow| {
            if w.title.is_empty() || is_system_ui(&w.app_name) {
                return None;
            }
            (w.pid > 0).then_some(w.pid)
        })
}
