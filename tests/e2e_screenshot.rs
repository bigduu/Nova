//! End-to-end test: verify that screenshot capture actually works on this machine.
//!
//! Requires Screen Recording permission to be granted.
//! Run with: `cargo test --test e2e_screenshot -- --ignored`
//! or `cargo test --test e2e_screenshot -- --include-ignored`

use base64::Engine;
use nova::capture::screenshot::{finish_capture, CaptureOptions};
use nova::platform::mac::capture::stream::StreamCapturer;
use nova::tools::screenshot::ScreenshotImage;

mod common;
use common::with_timeout;

/// Capture the display via the live stream path (the real one nova uses) and
/// finish it into a `ScreenshotImage`, mirroring what the server produces. Each
/// blocking step is time-bounded so a wedge fails the test instead of hanging.
fn capture_display_shot() -> ScreenshotImage {
    let raw = with_timeout(12, "stream capture_display", || {
        StreamCapturer::new().capture_display()
    })
    .expect("stream capture_display should succeed with permissions");
    with_timeout(15, "finish_capture", move || {
        finish_capture(
            raw,
            CaptureOptions {
                grid: false,
                marks: false,
            },
        )
    })
    .expect("finish_capture")
    .into()
}

#[test]
#[ignore = "requires Screen Recording permission in System Settings"]
fn e2e_capture_display_returns_valid_jpeg() {
    let result = capture_display_shot();

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
    assert!(
        !result.base64_data.is_empty(),
        "base64 image must not be empty"
    );

    // Decode and verify it's a valid JPEG
    let jpeg_bytes = base64::engine::general_purpose::STANDARD
        .decode(&result.base64_data)
        .expect("base64 should decode to valid bytes");
    assert!(!jpeg_bytes.is_empty(), "decoded JPEG must not be empty");

    // JPEG magic bytes
    assert_eq!(
        jpeg_bytes[0], 0xFF,
        "JPEG must start with SOI marker byte 0xFF"
    );
    assert_eq!(
        jpeg_bytes[1], 0xD8,
        "JPEG must start with SOI marker byte 0xD8"
    );

    // Try to decode with image crate
    let img = image::load_from_memory(&jpeg_bytes).expect("decoded bytes should be a valid image");
    assert_eq!(img.width(), result.width);
    assert_eq!(img.height(), result.height);

    // Reasonable size check (< 2MB for a 1280px JPEG)
    assert!(
        jpeg_bytes.len() < 2_000_000,
        "JPEG size {} exceeds 2MB limit",
        jpeg_bytes.len()
    );
}

/// The screenshot the agent sees and the click coordinate space MUST be derived
/// from the same target-dimension computation, otherwise clicks land in the
/// wrong place. This verifies the live capture matches `compute_target_dims` for
/// the real primary display — the contract the coordinate-conversion path relies
/// on. Requires Screen Recording permission.
#[test]
#[ignore = "requires Screen Recording permission in System Settings"]
fn e2e_capture_dims_match_target_dims_contract() {
    use nova::display::scaling::compute_target_dims;
    use nova::platform::mac::geometry::primary_display;

    let display = primary_display();
    let expected = compute_target_dims(display.width, display.height);

    let result = capture_display_shot();

    assert_eq!(
        (result.width, result.height),
        (expected.width, expected.height),
        "screenshot dims {}x{} diverged from compute_target_dims {}x{} \
         (logical display {}x{}); click coordinate mapping would be wrong",
        result.width,
        result.height,
        expected.width,
        expected.height,
        display.width,
        display.height,
    );
}

/// Single-window capture should produce a valid image and a view frame whose
/// region matches the window's logical size, so clicks map back correctly.
/// Smoke test: picks the first reasonably-sized on-screen window.
#[test]
#[ignore = "requires Screen Recording permission in System Settings"]
fn e2e_window_screenshot_produces_view_frame() {
    use nova::tools::window::list_windows;

    common::use_isolated_capture_daemon();
    let windows = list_windows().expect("list_windows");
    let Some(w) = windows
        .iter()
        .find(|w| !w.app_name.is_empty() && w.width > 80.0 && w.height > 80.0)
    else {
        eprintln!("no suitable window to capture; skipping");
        return;
    };

    let app = w.app_name.clone();
    let raw = with_timeout(12, "stream capture_window", move || {
        StreamCapturer::new().capture_window(&app)
    })
    .expect("window screenshot");
    let shot: ScreenshotImage = with_timeout(15, "finish_capture", move || {
        finish_capture(
            raw,
            CaptureOptions {
                grid: false,
                marks: false,
            },
        )
    })
    .expect("finish_capture")
    .into();
    assert!(shot.width > 0 && shot.height > 0, "empty window capture");
    assert!(
        shot.view.region.0 > 0.0 && shot.view.region.1 > 0.0,
        "view frame region must be positive"
    );
    // The image must fit within the 1280px cap like display captures.
    assert!(shot.width.max(shot.height) <= 1280);
}
