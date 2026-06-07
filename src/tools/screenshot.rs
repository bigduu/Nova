/// Screenshot tool — capture the primary display or a specific window.
///
/// Uses Apple's ScreenCaptureKit via the `screencapturekit` crate.
use crate::error::Result;
use crate::types::{DisplayInfo, WindowInfo};
use base64::Engine;

/// Capture the primary display and return a base64-encoded PNG image.
pub async fn capture_display(display: &DisplayInfo) -> Result<String> {
    // TODO: implement with screencapturekit
    let _ = display;
    Err(crate::error::NovaError::Screenshot(
        "not yet implemented".into(),
    ))
}

/// Capture a specific window and return a base64-encoded PNG image.
pub async fn capture_window(window: &WindowInfo) -> Result<String> {
    // TODO: implement with screencapturekit
    let _ = window;
    Err(crate::error::NovaError::Screenshot(
        "not yet implemented".into(),
    ))
}

/// High-resolution capture of a specific screen region.
pub async fn capture_region(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<String> {
    // TODO: implement with screencapturekit zoom
    let _ = (x, y, width, height);
    Err(crate::error::NovaError::Screenshot(
        "not yet implemented".into(),
    ))
}

/// Resize an image to fit within max_width × max_height, preserving aspect ratio.
#[allow(dead_code)]
fn resize_image(
    data: &[u8],
    max_width: u32,
    max_height: u32,
) -> Result<Vec<u8>> {
    use image::GenericImageView;
    let img = image::load_from_memory(data).map_err(|e| {
        crate::error::NovaError::Screenshot(format!("failed to decode image: {e}"))
    })?;
    let (w, h) = img.dimensions();
    let scale = (max_width as f64 / w as f64).min(max_height as f64 / h as f64).min(1.0);
    if scale >= 1.0 {
        return Ok(data.to_vec());
    }
    let new_w = (w as f64 * scale) as u32;
    let new_h = (h as f64 * scale) as u32;
    let resized = img.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3);
    let mut buf = std::io::Cursor::new(Vec::new());
    resized
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| crate::error::NovaError::Screenshot(format!("encode failed: {e}")))?;
    Ok(buf.into_inner())
}

/// Convert raw image bytes to base64 data URI.
pub fn to_base64_data_uri(data: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(data)
    )
}
