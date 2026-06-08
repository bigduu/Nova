/// Screenshot capture — captures the primary display using ScreenCaptureKit.
///
/// Returns raw RGBA pixel data along with the capture dimensions.
use crate::display::scaling::{compute_target_dims, TargetDims};
use crate::display::view::ViewFrame;
use base64::Engine;
use screencapturekit::{
    screenshot_manager::{CGImageExt, SCScreenshotManager},
    shareable_content::SCShareableContent,
    stream::{configuration::SCStreamConfiguration, content_filter::SCContentFilter},
};

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
}

/// Capture the main display as a JPEG screenshot (no overlays).
///
/// Returns base64-encoded JPEG data ready for MCP ImageContent.
/// The image is resized to fit within 1280px max dimension.
pub fn capture_display() -> Result<ScreenshotResult, String> {
    Ok(capture_display_with(CaptureOptions::default())?.result)
}

/// Capture the main display, applying any requested overlays before encoding.
pub fn capture_display_with(opts: CaptureOptions) -> Result<Capture, String> {
    let (filter, target) = main_display_filter()?;
    let img = capture_rgb_via(filter, target)?;

    let display = crate::display::geometry::primary_display();
    let view = ViewFrame {
        origin: (0.0, 0.0),
        region: (display.width as f64, display.height as f64),
        screenshot: (img.width() as f64, img.height() as f64),
    };
    // Set-of-Mark on a full-display shot marks the frontmost app's elements.
    let pid = if opts.marks {
        crate::tools::window::frontmost_app_pid()
    } else {
        None
    };
    finish(img, opts, view, pid)
}

/// Capture a single on-screen window matching `query` — a case-insensitive
/// substring of the window title or its owning application name. Returns the
/// screenshot plus the [`ViewFrame`] that maps the image's pixels back to global
/// logical points, so clicks against this image land on the real window.
///
/// Capturing just the relevant window cuts the image (and thus LLM context) down
/// to what matters and reduces downscaling, which improves coordinate precision.
pub fn capture_window_with(query: &str, opts: CaptureOptions) -> Result<Capture, String> {
    let content = SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .with_exclude_desktop_windows(true)
        .get()
        .map_err(|e| format!("SCShareableContent::get: {e}"))?;

    let q = query.to_lowercase();
    let window = content
        .windows()
        .into_iter()
        .find(|w| {
            let title = w.title().unwrap_or_default();
            if title.is_empty() {
                return false;
            }
            let app = w
                .owning_application()
                .map(|a| a.application_name())
                .unwrap_or_default();
            title.to_lowercase().contains(&q) || app.to_lowercase().contains(&q)
        })
        .ok_or_else(|| format!("no on-screen window matching {query:?}"))?;

    let frame = window.frame();
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
        return Err(format!("window {query:?} has zero size"));
    }
    let pid = window.owning_application().map(|a| a.process_id());

    let target = compute_target_dims(frame.size.width as u32, frame.size.height as u32);
    let filter = SCContentFilter::create().with_window(&window).build();
    let img = capture_rgb_via(filter, target)?;

    let view = ViewFrame {
        origin: (frame.origin.x, frame.origin.y),
        region: (frame.size.width, frame.size.height),
        screenshot: (img.width() as f64, img.height() as f64),
    };
    finish(img, opts, view, pid)
}

/// Zoom: capture the global-logical rectangle `(x, y, w, h)` at the display's
/// *native* resolution and crop to it, so a small region fills the model's
/// resolution budget and becomes legible. This is the grounding tool for apps
/// that expose no Accessibility tree (WeChat, Electron, games): the model takes
/// an overview, then zooms a region to read exact positions. Clicks against the
/// zoomed image map back through its [`ViewFrame`].
pub fn capture_region_with(
    rect: (f64, f64, f64, f64),
    opts: CaptureOptions,
) -> Result<Capture, String> {
    let (x, y, w, h) = rect;
    if w <= 0.0 || h <= 0.0 {
        return Err("region has zero size".to_string());
    }

    // Capture the whole main display at native pixel resolution.
    let main = core_graphics::display::CGDisplay::main();
    let logical = main.bounds().size;
    if logical.width <= 0.0 || logical.height <= 0.0 {
        return Err("main display has no geometry".to_string());
    }
    let native_w = main.pixels_wide() as u32;
    let native_h = (native_w as f64 * logical.height / logical.width).round() as u32;

    let (filter, _) = main_display_filter()?;
    let img = capture_rgb_via(
        filter,
        TargetDims {
            width: native_w,
            height: native_h,
        },
    )?;

    // Map the logical rect to actual captured pixels (robust to SCK rounding).
    let sx = img.width() as f64 / logical.width;
    let sy = img.height() as f64 / logical.height;
    let cx = (x * sx).clamp(0.0, img.width() as f64 - 1.0) as u32;
    let cy = (y * sy).clamp(0.0, img.height() as f64 - 1.0) as u32;
    let cw = ((w * sx).round() as u32).clamp(1, img.width() - cx);
    let ch = ((h * sy).round() as u32).clamp(1, img.height() - cy);

    let crop = image::imageops::crop_imm(&img, cx, cy, cw, ch).to_image();

    // Downscale the crop only if it still exceeds the max dimension.
    let target = compute_target_dims(crop.width(), crop.height());
    let out = if target.width == crop.width() && target.height == crop.height() {
        crop
    } else {
        image::imageops::resize(
            &crop,
            target.width,
            target.height,
            image::imageops::FilterType::Lanczos3,
        )
    };

    let view = ViewFrame {
        origin: (x, y),
        region: (w, h),
        screenshot: (out.width() as f64, out.height() as f64),
    };
    let pid = if opts.marks {
        crate::tools::window::frontmost_app_pid()
    } else {
        None
    };
    finish(out, opts, view, pid)
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
    let marks = match (opts.marks, pid) {
        (true, Some(pid)) => build_marks(&mut img, view, pid),
        _ => Vec::new(),
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
    })
}

/// Maximum number of Set-of-Mark boxes to draw (avoid clutter).
const MAX_MARKS: usize = 60;

/// Enumerate actionable elements of `pid`, draw numbered boxes for those visible
/// in `view`, and return the mark list with screenshot-pixel centers.
fn build_marks(img: &mut image::RgbImage, view: ViewFrame, pid: i32) -> Vec<Mark> {
    let (sw, sh) = (img.width() as f64, img.height() as f64);
    let mut marks = Vec::new();
    for el in crate::tools::elements::actionable_elements(pid, 400) {
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
            role: el.role,
            label: el.label,
            x: px,
            y: py,
        });
        if marks.len() >= MAX_MARKS {
            break;
        }
    }
    marks
}

/// Build a content filter + target dims for the *main* display — the same one
/// `display::geometry::primary_display` (CGDisplay::main) maps click coordinates
/// against. SCK's `displays()` order is not guaranteed to put the main display
/// first, so on a multi-monitor setup `.first()` could capture a different
/// screen than clicks target; match by display id.
fn main_display_filter() -> Result<(SCContentFilter, TargetDims), String> {
    let content = SCShareableContent::get().map_err(|e| format!("SCShareableContent::get: {e}"))?;
    let displays = content.displays();
    let main_id = core_graphics::display::CGDisplay::main().id;
    let display = displays
        .iter()
        .find(|d| d.display_id() == main_id)
        .or_else(|| displays.first())
        .ok_or_else(|| "no displays found".to_string())?;

    let target = compute_target_dims(display.width(), display.height());
    let filter = SCContentFilter::create()
        .with_display(display)
        .with_excluding_windows(&[])
        .build();
    Ok((filter, target))
}

/// Capture `filter` at `target` dims into an in-memory RGB image (no overlays).
fn capture_rgb_via(filter: SCContentFilter, target: TargetDims) -> Result<image::RgbImage, String> {
    let config = SCStreamConfiguration::new()
        .with_width(target.width)
        .with_height(target.height);

    let image = SCScreenshotManager::capture_image(&filter, &config)
        .map_err(|e| format!("capture_image: {e}"))?;

    let img_w = image.width() as u32;
    let img_h = image.height() as u32;
    let rgba = image.rgba_data().map_err(|e| format!("rgba_data: {e}"))?;
    let rgb = rgba_to_rgb(&rgba, img_w as usize, img_h as usize);
    image::RgbImage::from_raw(img_w, img_h, rgb)
        .ok_or_else(|| "captured buffer size did not match dimensions".to_string())
}

/// Encode an RGB image as base64 JPEG (quality 80).
fn encode_jpeg_base64(img: &image::RgbImage) -> Result<String, String> {
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80)
        .encode(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("encode: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

/// Convert RGBA raw bytes to RGB (dropping alpha channel).
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
