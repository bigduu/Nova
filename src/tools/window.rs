/// Window management tools — list windows via ScreenCaptureKit.
use crate::types::WindowInfo;
use screencapturekit::shareable_content::SCShareableContent;

/// List all on-screen windows across all applications.
/// Excludes desktop windows (wallpaper) and windows without titles.
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    let content = SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .with_exclude_desktop_windows(true)
        .get()
        .map_err(|e| format!("SCShareableContent::get: {e}"))?;

    let windows: Vec<WindowInfo> = content
        .windows()
        .iter()
        .filter_map(|w| {
            let title = w.title()?;
            if title.is_empty() {
                return None;
            }
            let frame = w.frame();
            let app_name = w
                .owning_application()
                .map(|a| a.application_name())
                .unwrap_or_default();
            Some(WindowInfo {
                title,
                app_name,
                x: frame.origin.x,
                y: frame.origin.y,
                width: frame.size.width,
                height: frame.size.height,
                is_visible: w.is_on_screen(),
            })
        })
        .collect();

    Ok(windows)
}

/// Process id and global-logical frame `(x, y, w, h)` of the first on-screen
/// window whose title OR owning-app name contains `query` (case-insensitive).
/// For CLI debugging (`--dump-ax`, `--marks`) where we target an app by name.
/// The frame is used as the off-screen cull clip. Needs Screen Recording.
pub fn pid_for_window(query: &str) -> Option<(i32, (f64, f64, f64, f64))> {
    let q = query.to_lowercase();
    let content = SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .with_exclude_desktop_windows(true)
        .get()
        .ok()?;

    content.windows().into_iter().find_map(|w| {
        let title = w.title().unwrap_or_default();
        let app = w.owning_application()?;
        let app_name = app.application_name();
        if title.to_lowercase().contains(&q) || app_name.to_lowercase().contains(&q) {
            let pid = app.process_id();
            let f = w.frame();
            (pid > 0).then_some((pid, (f.origin.x, f.origin.y, f.size.width, f.size.height)))
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
/// on-screen, titled window that isn't a system UI layer. Used to scope
/// Set-of-Mark element discovery. Returns `None` if nothing suitable is found
/// (e.g. no Screen Recording permission).
pub fn frontmost_app_pid() -> Option<i32> {
    let content = SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .with_exclude_desktop_windows(true)
        .get()
        .ok()?;

    content.windows().into_iter().find_map(|w| {
        let title = w.title()?;
        if title.is_empty() {
            return None;
        }
        let app = w.owning_application()?;
        if is_system_ui(&app.application_name()) {
            return None;
        }
        let pid = app.process_id();
        (pid > 0).then_some(pid)
    })
}
