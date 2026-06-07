/// Screenshot capture — captures the primary display using ScreenCaptureKit.
///
/// Returns raw RGBA pixel data along with the capture dimensions.
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

/// Capture the primary display as a JPEG screenshot.
///
/// Returns base64-encoded JPEG data ready for MCP ImageContent.
/// The image is resized to fit within 1280px max dimension.
pub fn capture_display() -> Result<ScreenshotResult, String> {
    // Get shareable content and pick the primary display
    let content = SCShareableContent::get().map_err(|e| format!("SCShareableContent::get: {e}"))?;
    let displays = content.displays();
    let display = displays
        .first()
        .ok_or_else(|| "no displays found".to_string())?;

    let display_width = display.width();
    let display_height = display.height();

    // Compute target dimensions (max 1280px on longest edge)
    let (target_w, target_h) = {
        let max_edge = display_width.max(display_height);
        if max_edge <= 1280 {
            (display_width, display_height)
        } else {
            let scale = 1280.0 / max_edge as f64;
            (
                (display_width as f64 * scale).round() as u32,
                (display_height as f64 * scale).round() as u32,
            )
        }
    };

    // Build content filter for the display
    let filter = SCContentFilter::create()
        .with_display(display)
        .with_excluding_windows(&[])
        .build();

    // Configure the capture
    let config = SCStreamConfiguration::new()
        .with_width(target_w)
        .with_height(target_h);

    // Capture the image
    let image = SCScreenshotManager::capture_image(&filter, &config)
        .map_err(|e| format!("capture_image: {e}"))?;

    let img_w = image.width() as u32;
    let img_h = image.height() as u32;

    // Get RGBA raw data
    let rgba = image
        .rgba_data()
        .map_err(|e| format!("rgba_data: {e}"))?;

    // Convert RGBA to RGB JPEG using the image crate
    let jpeg_bytes = rgb_to_jpeg(&rgba, img_w, img_h)
        .map_err(|e| format!("encode: {e}"))?;

    // Base64 encode
    let base64_image = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);

    Ok(ScreenshotResult {
        base64_image,
        width: img_w,
        height: img_h,
    })
}

/// Encode raw RGBA pixel data as a JPEG image with quality 80.
fn rgb_to_jpeg(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, image::ImageError> {
    // Convert RGBA to RGB (discard alpha — CG renders opaque anyway)
    let rgb = rgba_to_rgb(rgba, width as usize, height as usize);
    let mut buf = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80);
    encoder.encode(&rgb, width, height, image::ExtendedColorType::Rgb8)?;
    Ok(buf)
}

/// Convert RGBA raw bytes to RGB (dropping alpha channel).
pub(crate) fn rgba_to_rgb(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let pixel_count = width * height;
    let mut rgb = Vec::with_capacity(pixel_count * 3);
    for i in 0..pixel_count {
        let offset = i * 4;
        rgb.push(rgba[offset]);     // R
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
