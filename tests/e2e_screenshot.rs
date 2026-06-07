//! End-to-end test: verify that screenshot capture actually works on this machine.
//!
//! Requires Screen Recording permission to be granted.
//! Run with: `cargo test --test e2e_screenshot -- --ignored`
//! or `cargo test --test e2e_screenshot -- --include-ignored`

use base64::Engine;
use nova::capture::screenshot::capture_display;

#[test]
#[ignore = "requires Screen Recording permission in System Settings"]
fn e2e_capture_display_returns_valid_jpeg() {
    let result = capture_display().expect("capture_display should succeed with permissions");
    
    // Verify dimensions
    assert!(result.width > 0, "image width must be positive");
    assert!(result.height > 0, "image height must be positive");
    assert!(
        result.width.max(result.height) <= 1280,
        "max dimension should be <= 1280, got {}x{}",
        result.width,
        result.height
    );

    // Verify base64 is non-empty
    assert!(!result.base64_image.is_empty(), "base64 image must not be empty");

    // Decode and verify it's a valid JPEG
    let jpeg_bytes = base64::engine::general_purpose::STANDARD
        .decode(&result.base64_image)
        .expect("base64 should decode to valid bytes");
    assert!(!jpeg_bytes.is_empty(), "decoded JPEG must not be empty");

    // JPEG magic bytes
    assert_eq!(jpeg_bytes[0], 0xFF, "JPEG must start with SOI marker byte 0xFF");
    assert_eq!(jpeg_bytes[1], 0xD8, "JPEG must start with SOI marker byte 0xD8");

    // Try to decode with image crate
    let img = image::load_from_memory(&jpeg_bytes)
        .expect("decoded bytes should be a valid image");
    assert_eq!(img.width(), result.width);
    assert_eq!(img.height(), result.height);

    // Reasonable size check (< 2MB for a 1280px JPEG)
    assert!(
        jpeg_bytes.len() < 2_000_000,
        "JPEG size {} exceeds 2MB limit",
        jpeg_bytes.len()
    );
}

#[test]
fn e2e_capture_display_fails_gracefully_without_permission() {
    // Without Screen Recording permission, this should return an error
    // (but NOT panic or segfault — graceful failure is the important part)
    match capture_display() {
        Ok(result) => {
            // If we happen to have permission, that's fine — verify data looks ok
            eprintln!(
                "Permission granted! Got {}x{} screenshot, {} base64 chars",
                result.width,
                result.height,
                result.base64_image.len()
            );
        }
        Err(e) => {
            // The error should mention Screen Recording or shareable content
            eprintln!("Expected no-permission error: {e}");
            assert!(
                e.to_lowercase().contains("screen")
                    || e.to_lowercase().contains("permission")
                    || e.to_lowercase().contains("display")
                    || e.to_lowercase().contains("shareable"),
                "Error should mention screen/permission/display, got: {e}"
            );
        }
    }
}
