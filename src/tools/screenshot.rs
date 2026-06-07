/// Screenshot tool — MCP-facing wrapper around the capture module.
///
/// Coordinates capture, image processing, and base64 encoding for MCP responses.
use crate::capture::screenshot::capture_display;
use crate::display::geometry::screen_recording_available;

/// Result of a screenshot tool call, ready for MCP.
pub struct ScreenshotImage {
    pub base64_data: String,
    pub width: u32,
    pub height: u32,
    pub mime_type: &'static str,
}

/// Take a screenshot of the primary display.
///
/// Resizes to max 1280px, encodes as JPEG quality 80, returns base64.
pub fn take_screenshot() -> Result<ScreenshotImage, String> {
    let result = capture_display()?;
    Ok(ScreenshotImage {
        base64_data: result.base64_image,
        width: result.width,
        height: result.height,
        mime_type: "image/jpeg",
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
