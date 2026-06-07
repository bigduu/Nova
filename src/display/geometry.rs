/// Display geometry — get information about the primary display.
///
/// Uses ScreenCaptureKit's shareable content to enumerate displays.
use crate::types::DisplayInfo;
use screencapturekit::shareable_content::SCShareableContent;

/// Get information about the primary display.
/// Returns `None` if no displays are found or content retrieval fails.
pub fn primary_display() -> Option<DisplayInfo> {
    let content = SCShareableContent::get().ok()?;
    let displays = content.displays();
    let display = displays.first()?;

    Some(DisplayInfo {
        id: display.display_id(),
        width: display.width(),
        height: display.height(),
        scale_factor: 1.0, // TODO: get real scale factor from CoreGraphics
        is_primary: true,
    })
}

/// List all available displays.
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
