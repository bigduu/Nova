/// Screenshot tool — MCP-facing wrapper around the capture module.
///
/// Coordinates capture, image processing, and base64 encoding for MCP responses.
use crate::capture::screenshot::{Capture, Mark};
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
    /// The marked elements' live AX handles + centers, for clicking by mark
    /// number (empty unless `marks` was requested).
    pub mark_targets: Vec<crate::tools::elements::CachedElement>,
    /// Owning process id for a single-window capture (for background input
    /// routing); `None` for full-display captures.
    pub target_pid: Option<i32>,
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
            mark_targets: c.mark_targets,
            target_pid: c.target_pid,
        }
    }
}
