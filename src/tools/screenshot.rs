/// Screenshot tool — MCP-facing wrapper around the capture module.
///
/// Coordinates capture, image processing, and base64 encoding for MCP responses.
use crate::capture::screenshot::{capture_display_with, capture_window_with, CaptureOptions};
use crate::display::geometry::{primary_display, screen_recording_available};
use crate::display::view::ViewFrame;

/// Result of a screenshot tool call, ready for MCP.
pub struct ScreenshotImage {
    pub base64_data: String,
    pub width: u32,
    pub height: u32,
    pub mime_type: &'static str,
    /// Maps this image's pixels back to global logical points (for clicks).
    pub view: ViewFrame,
}

/// Take a screenshot of the main display.
///
/// Resizes to max 1280px, encodes as JPEG quality 80, returns base64.
/// When `grid` is set, overlays a labeled coordinate grid.
pub fn take_screenshot(grid: bool) -> Result<ScreenshotImage, String> {
    let result = capture_display_with(CaptureOptions { grid })?;
    let display = primary_display();
    Ok(ScreenshotImage {
        view: ViewFrame {
            origin: (0.0, 0.0),
            region: (display.width as f64, display.height as f64),
            screenshot: (result.width as f64, result.height as f64),
        },
        base64_data: result.base64_image,
        width: result.width,
        height: result.height,
        mime_type: "image/jpeg",
    })
}

/// Take a screenshot of a single on-screen window matching `query` (a
/// case-insensitive substring of the window title or owning app name).
pub fn take_window_screenshot(query: &str, grid: bool) -> Result<ScreenshotImage, String> {
    let (result, view) = capture_window_with(query, CaptureOptions { grid })?;
    Ok(ScreenshotImage {
        base64_data: result.base64_image,
        width: result.width,
        height: result.height,
        mime_type: "image/jpeg",
        view,
    })
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
