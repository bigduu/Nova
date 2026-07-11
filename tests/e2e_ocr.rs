//! End-to-end test for Apple Vision OCR on a real screen capture.
//!
//! Needs Screen Recording permission, so it is `#[ignore]`d by default. Run:
//!   cargo test --test e2e_ocr -- --ignored --nocapture

use base64::Engine;
use nova::capture::screenshot::{finish_capture, CaptureOptions};
use nova::platform::mac::capture::stream::StreamCapturer;
use nova::tools::screenshot::ScreenshotImage;

mod common;
use common::with_timeout;

#[test]
#[ignore = "captures the display; needs Screen Recording permission"]
fn ocr_recognizes_text_on_the_display() {
    // Capture the whole display (via the live stream) without overlays. Each
    // blocking step is time-bounded so a wedge fails the test instead of hanging.
    let raw = with_timeout(12, "stream capture_display", || {
        StreamCapturer::new().capture_display()
    })
    .expect("capture display");
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
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(&shot.base64_data)
        .expect("decode jpeg");

    let (w, h) = (shot.width, shot.height);
    let lines = with_timeout(20, "Vision OCR", move || {
        nova::platform::mac::ocr::recognize(&jpeg, w, h, &["zh-Hans", "en-US"])
    })
    .expect("OCR should not error");

    eprintln!(
        "OCR found {} lines on a {}x{} capture",
        lines.len(),
        shot.width,
        shot.height
    );
    for l in lines.iter().take(15) {
        eprintln!(
            "  {:?} @ ({:.0}, {:.0}) conf={:.2}",
            l.text, l.center.0, l.center.1, l.confidence
        );
    }

    // A normal desktop always shows text (menu bar: app name, clock, status
    // items). If this is empty, Vision OCR isn't actually running.
    assert!(
        !lines.is_empty(),
        "expected to recognize some on-screen text (menu bar etc.)"
    );
    // Every center must land inside the image (coordinate mapping sanity).
    for l in &lines {
        assert!(
            l.center.0 >= 0.0
                && l.center.0 <= shot.width as f64
                && l.center.1 >= 0.0
                && l.center.1 <= shot.height as f64,
            "line center {:?} outside the {}x{} image",
            l.center,
            shot.width,
            shot.height
        );
    }
}
