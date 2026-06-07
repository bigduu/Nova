/// Window management tools — list windows, get window info.
///
/// Uses macOS Accessibility API (`accessibility` crate)
/// and ScreenCaptureKit's ShareableContent for window enumeration.
use crate::error::Result;
use crate::types::WindowInfo;

/// List all visible windows across all applications.
pub fn list_windows() -> Result<Vec<WindowInfo>> {
    // TODO: implement with accessibility + screencapturekit
    Err(crate::error::NovaError::Window("not yet implemented".into()))
}

/// Get detailed info about a specific window by title or index.
pub fn get_window_info(identifier: &str) -> Result<WindowInfo> {
    let _ = identifier;
    Err(crate::error::NovaError::Window("not yet implemented".into()))
}
