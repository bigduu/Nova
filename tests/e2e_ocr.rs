//! End-to-end test for Apple Vision OCR on a real screen capture.
//!
//! Needs Screen Recording permission, so it is `#[ignore]`d by default. Run:
//!   cargo test --test e2e_ocr -- --ignored --nocapture
//!
//! macOS only (Apple Vision via `platform::mac::ocr`; the Windows OCR path is
//! a P2/P3 stub, see `platform::windows::ocr`).
#![cfg(target_os = "macos")]

use nova::capture::screenshot::encode_raw_capture;
use nova::platform::mac::capture::stream::StreamCapturer;
use nova::platform::OcrMode;

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
    // OCR's production path encodes the clean raw frame exactly once; it does
    // not create an MCP base64 screenshot and decode it again.
    let shot = with_timeout(15, "encode_raw_capture", move || encode_raw_capture(raw))
        .expect("encode raw OCR capture");

    let (w, h, jpeg) = (shot.width, shot.height, shot.jpeg);
    let lines = with_timeout(35, "Vision OCR Auto", move || {
        nova::platform::mac::ocr::recognize_with_mode(
            &jpeg,
            w,
            h,
            &["zh-Hans", "en-US"],
            OcrMode::Auto,
        )
    })
    .expect("OCR should not error");

    eprintln!("OCR found {} lines on a {}x{} capture", lines.len(), w, h);
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
                && l.center.0 <= w as f64
                && l.center.1 >= 0.0
                && l.center.1 <= h as f64,
            "line center {:?} outside the {}x{} image",
            l.center,
            w,
            h
        );
    }
}
