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
