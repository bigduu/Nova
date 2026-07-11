/// Screenshot capture — captures the primary display using ScreenCaptureKit.
///
/// Returns raw RGBA pixel data along with the capture dimensions.
use crate::display::view::ViewFrame;
use base64::Engine;

use std::sync::atomic::{AtomicBool, Ordering};

/// When enabled (in the capture daemon), each capture sub-step is appended with
/// a timestamp to a log file, so a hang can be pinpointed to the exact step
/// (window enumeration vs stream start vs encode) by reading the last line
/// written. Reliable regardless of the host's log level — unlike
/// stderr/tracing, which the MCP host may drop.
static STEP_TRACE: AtomicBool = AtomicBool::new(false);

/// Path of the capture-worker step-trace log.
pub const STEP_TRACE_PATH: &str = "/tmp/nova-capture-worker.log";

/// Enable step tracing for this process (called by the capture worker at startup).
pub fn enable_step_trace() {
    STEP_TRACE.store(true, Ordering::Relaxed);
}

/// Append one timestamped step line to [`STEP_TRACE_PATH`] when tracing is on.
pub fn step(s: &str) {
    if !STEP_TRACE.load(Ordering::Relaxed) {
        return;
    }
    use std::io::Write;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(STEP_TRACE_PATH)
    {
        let _ = writeln!(f, "{ms} pid={} {s}", std::process::id());
    }
}

/// Result of capturing a screenshot.
pub struct ScreenshotResult {
    /// base64-encoded JPEG image data
    pub base64_image: String,
    /// Width of the captured image (in pixels of the returned image)
    pub width: u32,
    /// Height of the captured image
    pub height: u32,
}

/// Options controlling how the display is rendered for the model.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureOptions {
    /// Overlay a labeled coordinate grid to help the model read off positions.
    pub grid: bool,
    /// Set-of-Mark: draw numbered boxes over actionable UI elements (via the
    /// Accessibility API) so the model can pick a target by its mark.
    pub marks: bool,
}

/// A Set-of-Mark annotation drawn on a capture: a numbered actionable element
/// with its center in screenshot-pixel coordinates (ready to click).
#[derive(Debug, Clone)]
pub struct Mark {
    pub number: u32,
    pub role: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
}

/// A capture plus the metadata clicks and Set-of-Mark depend on.
pub struct Capture {
    pub result: ScreenshotResult,
    /// Maps this image's pixels back to global logical points.
    pub view: ViewFrame,
    /// Set-of-Mark annotations (empty unless `marks` was requested).
    pub marks: Vec<Mark>,
    /// The marked elements' live AX handles + centers, keyed by mark number, so
    /// the server can click by mark number (AX action, coordinate fallback).
    /// Parallel to `marks`; empty unless `marks` was requested.
    pub mark_targets: Vec<crate::tools::elements::CachedElement>,
    /// Owning process id of the captured window, when this is a single-window
    /// capture. Lets the server deliver subsequent input directly to that
    /// process (background-style) instead of the global event stream. `None`
    /// for full-display captures.
    pub target_pid: Option<i32>,
}

/// A raw capture before any overlays/marks: just the pixels, the coordinate
/// frame, and (for a window capture) the owning process. This is what crosses
/// the capture-worker process boundary — it holds no Accessibility handles, so
/// it is `Send` and serializable. The marks/AX walk runs later, in-process, via
/// [`finish_capture`].
pub struct RawCapture {
    /// The captured pixels, no overlays.
    pub image: image::RgbImage,
    /// Maps this image's pixels back to global logical points.
    pub view: ViewFrame,
    /// Owning process id, set ONLY for a single-window capture (it is both the
    /// input-routing target and the app whose AX tree marks are walked on).
    pub window_pid: Option<i32>,
}

/// Turn a [`RawCapture`] into a finished [`Capture`]: apply overlays, walk the
/// Set-of-Mark Accessibility tree (in THIS process — the cached handles can't
/// cross a process boundary), and encode. The marks are walked on the captured
/// window's app, or — for a display/region capture — on the frontmost app.
pub fn finish_capture(raw: RawCapture, opts: CaptureOptions) -> Result<Capture, String> {
    let RawCapture {
        image,
        view,
        window_pid,
    } = raw;
    let marks_pid = if opts.marks {
        window_pid.or_else(crate::tools::window::frontmost_app_pid)
    } else {
        None
    };
    let mut capture = finish(image, opts, view, marks_pid)?;
    // A single-window capture routes later input to its owning process.
    capture.target_pid = window_pid;
    Ok(capture)
}

/// Apply overlays (grid, Set-of-Mark) and encode the final capture.
fn finish(
    mut img: image::RgbImage,
    opts: CaptureOptions,
    view: ViewFrame,
    pid: Option<i32>,
) -> Result<Capture, String> {
    if opts.grid {
        crate::capture::overlay::draw_grid(&mut img);
    }
    let (marks, mark_targets) = match (opts.marks, pid) {
        (true, Some(pid)) => build_marks(&mut img, view, pid),
        _ => (Vec::new(), Vec::new()),
    };

    let (width, height) = (img.width(), img.height());
    let base64_image = encode_jpeg_base64(&img)?;
    Ok(Capture {
        result: ScreenshotResult {
            base64_image,
            width,
            height,
        },
        view,
        marks,
        mark_targets,
        // Set by the window-capture path; full-display/region captures leave it
        // None (no single owning process to target).
        target_pid: None,
    })
}

/// Maximum number of Set-of-Mark boxes to draw. Raised from 60 so a dense web
/// view (e.g. a full mail list) gets numbered comprehensively rather than cut
/// off after the chrome; still bounded to keep the overlay legible.
const MAX_MARKS: usize = 150;

/// Enumerate actionable elements of `pid`, draw numbered boxes for those visible
/// in `view`, and return the mark list with screenshot-pixel centers.
fn build_marks(
    img: &mut image::RgbImage,
    view: ViewFrame,
    pid: i32,
) -> (Vec<Mark>, Vec<crate::tools::elements::CachedElement>) {
    let (sw, sh) = (img.width() as f64, img.height() as f64);
    let mut marks = Vec::new();
    let mut targets = Vec::new();
    // Clip element discovery to the captured view's global-logical rectangle so
    // the walk skips off-screen subtrees (background tabs, scrolled-off rows).
    let clip = (view.origin.0, view.origin.1, view.region.0, view.region.1);
    for (el, handle) in crate::platform::ui_tree().collect_actionable(pid, 400, Some(clip)) {
        let (cx, cy) = el.center();
        let (px, py) = view.to_screenshot(cx, cy);
        // Keep only elements whose center is inside the captured image.
        if px < 0.0 || py < 0.0 || px > sw || py > sh {
            continue;
        }
        let (tlx, tly) = view.to_screenshot(el.x, el.y);
        let (brx, bry) = view.to_screenshot(el.x + el.width, el.y + el.height);
        let number = marks.len() as u32 + 1;
        crate::capture::overlay::draw_mark(img, tlx, tly, brx - tlx, bry - tly, number);
        marks.push(Mark {
            number,
            role: el.role.clone(),
            label: el.label.clone(),
            x: px,
            y: py,
        });
        // Cache the live handle + global-logical center so the server can click
        // this mark by number (AX action first, coordinate fallback).
        targets.push(crate::tools::elements::CachedElement {
            number,
            handle,
            center: (cx, cy),
            role: el.role,
            label: el.label,
            pid,
        });
        if marks.len() >= MAX_MARKS {
            break;
        }
    }
    (marks, targets)
}

/// Encode an RGB image as base64 JPEG (quality 90).
///
/// Quality 90 over 80: image tokens are billed purely on pixel dimensions, never
/// file size, so a higher quality costs ZERO extra tokens — it only adds a little
/// upload bandwidth. UI screenshots are sharp text + thin mark/grid lines, where
/// JPEG's ringing artifacts hurt most; 90 keeps glyphs and 1px overlays crisp.
fn encode_jpeg_base64(img: &image::RgbImage) -> Result<String, String> {
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90)
        .encode(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("encode: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

/// Convert RGBA raw bytes to RGB (dropping alpha channel). Only macOS's
/// ScreenCaptureKit path (`platform::mac::capture::stream`) hands back RGBA
/// frames; Windows' GDI path (`platform::windows::capture`) reads BGRA DIBs
/// and does its own inline channel reorder, so this is legitimately unused —
/// not dead code — on a non-macOS build.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn rgba_to_rgb(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let pixel_count = width * height;
    let mut rgb = Vec::with_capacity(pixel_count * 3);
    for i in 0..pixel_count {
        let offset = i * 4;
        rgb.push(rgba[offset]); // R
        rgb.push(rgba[offset + 1]); // G
        rgb.push(rgba[offset + 2]); // B
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_to_rgb_strips_alpha() {
        // 2 pixels: RGBA(255,0,0,255) + RGBA(0,255,0,128)
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 128];
        let rgb = rgba_to_rgb(&rgba, 2, 1);
        assert_eq!(rgb, vec![255, 0, 0, 0, 255, 0]);
        assert_eq!(rgb.len(), 6); // 2 pixels * 3 channels
    }

    #[test]
    fn rgba_to_rgb_empty() {
        let rgb = rgba_to_rgb(&[], 0, 0);
        assert!(rgb.is_empty());
    }

    #[test]
    fn rgba_to_rgb_single_pixel_discards_alpha() {
        let rgba = vec![10, 20, 30, 40];
        let rgb = rgba_to_rgb(&rgba, 1, 1);
        assert_eq!(rgb, vec![10, 20, 30]);
    }
}
