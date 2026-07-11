//! Window/application enumeration, activation, and launching (macOS).
//!
//! Window listing goes through the shared capture daemon
//! (`crate::platform::mac::capture::broker::shared_client`) rather than calling
//! `SCShareableContent` directly — two same-binary replayd clients evict each
//! other's XPC identity in a connect/cancel storm that wedges stream starts
//! (see `capture::broker`'s module doc for the full story). ALL
//! ScreenCaptureKit traffic, including metadata-only window enumeration, goes
//! through that one daemon. This module does not own the daemon/socket
//! plumbing itself (that belongs to the `ScreenCapture` subsystem, i.e.
//! `crate::platform::mac::capture::broker`) — it only calls it, exactly as `tools::window`
//! used to before this move.
//!
//! Application listing/launching shells out to `mdfind` (Spotlight) and
//! `open`, unchanged in substance from the old `tools::application`.
use crate::platform::mac::capture::broker::shared_client;
use crate::platform::WindowHandle;
use crate::tools::application::ApplicationInfo;
use std::process::Command;

/// On-screen windows, frontmost first, via the shared capture daemon. Raw and
/// unfiltered (includes untitled/desktop windows) — callers that want the
/// MCP-facing "titled windows only" view filter on top of this, same as
/// before the move.
pub fn list_windows() -> Result<Vec<WindowHandle>, String> {
    Ok(shared_client()
        .windows()?
        .into_iter()
        .map(|w| WindowHandle {
            id: w.window_id as u64,
            pid: w.pid,
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

/// List installed applications using mdfind (Spotlight).
pub fn list_applications() -> crate::error::Result<Vec<ApplicationInfo>> {
    let output = Command::new("mdfind")
        .arg("kMDItemContentType == 'com.apple.application-bundle'")
        .output()
        .map_err(|e| crate::error::NovaError::Application(format!("mdfind failed: {e}")))?;

    let paths: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        // The `application-bundle` content type also matches non-`.app`
        // directories Spotlight has tagged (e.g. CocoaPods test fixtures), which
        // are not launchable apps. Keep only real `.app` bundles.
        .filter(|l| l.ends_with(".app"))
        .map(|l| l.to_string())
        .collect();

    let mut apps: Vec<ApplicationInfo> = paths
        .into_iter()
        .filter_map(|path| {
            let name = std::path::Path::new(&path)
                .file_stem()?
                .to_str()?
                .to_string();
            Some(ApplicationInfo {
                name,
                path,
                bundle_id: None, // TODO: read Info.plist for bundle ID
            })
        })
        .collect();

    // Stable, de-duplicated ordering so the agent gets a predictable list.
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.name == b.name && a.path == b.path);

    Ok(apps)
}

/// Launch or focus an application by name.
pub fn open_application(name: &str) -> crate::error::Result<()> {
    let status = Command::new("open")
        .arg("-a")
        .arg(name)
        .status()
        .map_err(|e| crate::error::NovaError::Application(format!("open failed: {e}")))?;

    if !status.success() {
        return Err(crate::error::NovaError::Application(format!(
            "failed to open: {name}"
        )));
    }
    Ok(())
}

/// The macOS [`crate::platform::WindowManager`]: shared-daemon window
/// enumeration plus `mdfind`/`open`-backed application listing/launching, via
/// the free functions above.
pub struct MacWindowManager;

impl crate::platform::WindowManager for MacWindowManager {
    fn list_windows(&self) -> Result<Vec<WindowHandle>, String> {
        list_windows()
    }

    fn list_applications(&self) -> crate::error::Result<Vec<ApplicationInfo>> {
        list_applications()
    }

    fn open_application(&self, name: &str) -> crate::error::Result<()> {
        open_application(name)
    }
}
