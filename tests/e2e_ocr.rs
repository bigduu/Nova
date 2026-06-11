//! End-to-end test for Apple Vision OCR on a real screen capture.
//!
//! Needs Screen Recording permission, so it is `#[ignore]`d by default. Run:
//!   cargo test --test e2e_ocr -- --ignored --nocapture

use base64::Engine;
use nova::tools::screenshot::take_screenshot;

#[test]
#[ignore = "captures the display; needs Screen Recording permission"]
fn ocr_recognizes_text_on_the_display() {
    // Capture the whole display without overlays.
    let shot = take_screenshot(false, false).expect("capture display");
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(&shot.base64_data)
        .expect("decode jpeg");

    let lines = nova::ocr::recognize(&jpeg, shot.width, shot.height, &["zh-Hans", "en-US"])
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
