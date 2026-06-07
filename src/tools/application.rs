/// Application management tools — list installed apps, launch/focus apps.
///
/// Uses `mdfind` for app discovery and `open` command for launching.
use crate::error::Result;
use std::process::Command;

/// Information about an installed application.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApplicationInfo {
    pub name: String,
    pub path: String,
    pub bundle_id: Option<String>,
}

/// List installed applications using mdfind (Spotlight).
pub fn list_applications() -> Result<Vec<ApplicationInfo>> {
    let output = Command::new("mdfind")
        .arg("kMDItemContentType == 'com.apple.application-bundle'")
        .output()
        .map_err(|e| crate::error::NovaError::Application(format!("mdfind failed: {e}")))?;

    let paths: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    let apps: Vec<ApplicationInfo> = paths
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

    Ok(apps)
}

/// Launch or focus an application by name.
pub fn open_application(name: &str) -> Result<()> {
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
