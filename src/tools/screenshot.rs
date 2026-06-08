/// Screenshot tool — MCP-facing wrapper around the capture module.
///
/// Coordinates capture, image processing, and base64 encoding for MCP responses.
use crate::capture::screenshot::{
    capture_display_with, capture_region_with, capture_window_with, Capture, CaptureOptions, Mark,
};
use crate::display::geometry::screen_recording_available;
use crate::display::view::ViewFrame;

/// Result of a screenshot tool call, ready for MCP.
pub struct ScreenshotImage {
    pub base64_data: String,
    pub width: u32,
    pub height: u32,
    pub mime_type: &'static str,
    /// Maps this image's pixels back to global logical points (for clicks).
    pub view: ViewFrame,
    /// Set-of-Mark annotations (empty unless `marks` was requested).
    pub marks: Vec<Mark>,
}

impl From<Capture> for ScreenshotImage {
    fn from(c: Capture) -> Self {
        ScreenshotImage {
            base64_data: c.result.base64_image,
            width: c.result.width,
            height: c.result.height,
            mime_type: "image/jpeg",
            view: c.view,
            marks: c.marks,
        }
    }
}

/// Take a screenshot of the main display. Resizes to max 1280px, JPEG q80.
/// `grid` overlays a coordinate grid; `marks` draws Set-of-Mark element boxes.
pub fn take_screenshot(grid: bool, marks: bool) -> Result<ScreenshotImage, String> {
    Ok(capture_display_with(CaptureOptions { grid, marks })?.into())
}

/// Take a screenshot of a single on-screen window matching `query` (a
/// case-insensitive substring of the window title or owning app name).
pub fn take_window_screenshot(
    query: &str,
    grid: bool,
    marks: bool,
) -> Result<ScreenshotImage, String> {
    Ok(capture_window_with(query, CaptureOptions { grid, marks })?.into())
}

/// Zoom into `rect` (x, y, w, h in global logical points), captured at native
/// resolution for a sharp, legible crop.
pub fn take_region_screenshot(
    rect: (f64, f64, f64, f64),
    grid: bool,
    marks: bool,
) -> Result<ScreenshotImage, String> {
    Ok(capture_region_with(rect, CaptureOptions { grid, marks })?.into())
}

/// Check if screen recording permission is available.
/// Call this early to give the user a clear error message.
pub fn check_permission() -> Result<(), String> {
    if screen_recording_available() {
        Ok(())
    } else {
        Err("Screen Recording permission not granted. \
             Open System Settings → Privacy & Security → Screen Recording \
             and enable the terminal/application running Nova."
            .to_string())
    }
}
