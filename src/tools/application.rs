/// Application management tools — list installed apps, launch/focus apps.
///
/// The actual OS work (mdfind discovery, `open` launching) now lives behind
/// `crate::platform::WindowManager` (`src/platform/mac/window.rs`); this
/// module keeps `ApplicationInfo` (the neutral shape the trait returns) and a
/// thin, stable wrapper so existing tool/test call sites don't need to change.
use crate::error::Result;

/// Information about an installed application.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApplicationInfo {
    pub name: String,
    pub path: String,
    pub bundle_id: Option<String>,
}

/// List installed applications using mdfind (Spotlight).
pub fn list_applications() -> Result<Vec<ApplicationInfo>> {
    crate::platform::window_manager().list_applications()
}

/// Launch or focus an application by name.
pub fn open_application(name: &str) -> Result<()> {
    crate::platform::window_manager().open_application(name)
}
