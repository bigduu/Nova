//! End-to-end smoke test: drive a real browser task through nova's own tools and
//! read the result back, exercising the whole stack the way an agent does —
//! `open_application` → locate the address bar via Set-of-Mark (Accessibility) →
//! MOUSE-click it → type the URL → persistent-stream window capture → Vision OCR.
//!
//! This is the canonical "does nova actually work on this machine" check. It is
//! `#[ignore]`d because it needs a real GUI session, a network connection, and —
//! crucially — the test RUNNER (terminal / IDE) must be granted BOTH:
//!   • Screen Recording  (capture + window enumeration + marks), and
//!   • Accessibility     (so synthesized mouse/keys actually reach Safari).
//! Under Bodhi those grants come from the host app; from `cargo test` you must
//! grant them to whatever runs the test. Run:
//!   cargo test --test e2e_safari_google -- --ignored --nocapture
//!
//! Note: navigation uses the MOUSE to focus the address bar (this machine's ⌘L is
//! remapped), located through the Accessibility tree rather than a hard-coded
//! pixel, with a geometric top-of-window fallback.
//!
//! macOS only — drives Safari specifically through `platform::mac::*` free
//! functions directly (capture stream, CGEvent input, Vision OCR).
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use base64::Engine;
use nova::capture::screenshot::{finish_capture, CaptureOptions};
use nova::platform::mac::capture::stream::StreamCapturer;
use nova::platform::mac::input::{key_combo, left_click_at, type_text};
use nova::platform::mac::ocr::recognize;
use nova::tools::application::open_application;
use nova::tools::input::InputTarget;
use nova::tools::screenshot::ScreenshotImage;
use nova::tools::window::list_windows;
use nova::types::WindowInfo;

mod common;
use common::with_timeout;

/// Poll `list_windows()` until `pred` holds or `secs` elapse.
fn wait_for_windows(secs: u64, pred: impl Fn(&[WindowInfo]) -> bool) -> Option<Vec<WindowInfo>> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Ok(ws) = list_windows() {
            if pred(&ws) {
                return Some(ws);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn safari_window(ws: &[WindowInfo]) -> Option<&WindowInfo> {
    ws.iter()
        .find(|w| w.app_name.to_lowercase().contains("safari"))
}

fn title_containing<'a>(ws: &'a [WindowInfo], needle: &str) -> Option<&'a WindowInfo> {
    let n = needle.to_lowercase();
    ws.iter().find(|w| w.title.to_lowercase().contains(&n))
}

/// Capture the Safari window (with marks) and return the finished image. Each
/// blocking step is time-bounded so a wedge fails the test instead of hanging.
fn shoot_safari() -> ScreenshotImage {
    let raw = with_timeout(8, "Safari window capture (stream)", || {
        StreamCapturer::new().capture_window("Safari")
    })
    .expect("capture the Safari window via the live stream");
    with_timeout(15, "finish_capture (overlays + AX marks)", move || {
        finish_capture(
            raw,
            CaptureOptions {
                grid: false,
                marks: true,
            },
        )
    })
    .expect("finish_capture")
    .into()
}

/// Global-logical point to click to focus the address bar. Prefer the topmost
/// text field the Accessibility walk found (AX roles are NOT localized, so this
/// is layout/language independent); fall back to the top-center of the window.
fn address_bar_point(shot: &ScreenshotImage, win: &WindowInfo) -> (f64, f64) {
    let topmost_field = shot
        .marks
        .iter()
        .filter(|m| m.role.to_lowercase().contains("textfield"))
        .min_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
    match topmost_field {
        Some(m) => {
            // marks are in screenshot-pixel space; map back to global logical.
            let (lx, ly) = shot.view.to_logical(m.x, m.y);
            eprintln!(
                "[safari] address bar via AX text field {:?} -> ({lx:.0}, {ly:.0})",
                m.label
            );
            (lx, ly)
        }
        None => {
            let p = (win.x + win.width * 0.5, win.y + 40.0);
            eprintln!(
                "[safari] no AX text field marked; geometric fallback -> ({:.0}, {:.0})",
                p.0, p.1
            );
            p
        }
    }
}

#[test]
#[ignore = "drives Safari live; needs a GUI session, network, + Screen Recording AND Accessibility granted to the test runner"]
fn safari_opens_google_and_nova_reads_the_homepage() {
    common::use_isolated_capture_daemon();
    // 1. Open Safari and wait for it to put a window on screen.
    open_application("Safari").expect("open Safari");
    let ws0 = wait_for_windows(10, |ws| safari_window(ws).is_some())
        .expect("Safari never showed a window — installed? allowed to launch?");
    let win = safari_window(&ws0).unwrap().clone();
    eprintln!(
        "[safari] window up: {:?} ({}x{})",
        win.title, win.width as i32, win.height as i32
    );
    std::thread::sleep(Duration::from_millis(1200));

    // 2. Locate the address bar from the Accessibility tree and MOUSE-click it
    //    (⌘L is remapped on this machine), then type the URL and press Return.
    let pre = shoot_safari();
    eprintln!(
        "[safari] pre-nav capture {}x{}, {} marked elements",
        pre.width,
        pre.height,
        pre.marks.len()
    );
    let (ax, ay) = address_bar_point(&pre, &win);
    left_click_at(ax, ay, InputTarget::Global).expect("mouse-click the address bar");
    std::thread::sleep(Duration::from_millis(500));
    type_text("google.com", InputTarget::Global).expect("type url");
    key_combo("return", InputTarget::Global).expect("return");

    // 3. Wait for the page to load — Safari sets the window/tab title to "Google".
    let ws = wait_for_windows(25, |ws| title_containing(ws, "google").is_some()).expect(
        "Safari never showed a window titled \"Google\" within 25s — navigation didn't take. \
         Usually the mouse-click/keystrokes aren't reaching Safari: grant Accessibility to the \
         process running this test (or run under Bodhi). Also check the network.",
    );
    let gw = title_containing(&ws, "google").unwrap();
    eprintln!(
        "[safari] Google window on screen: {:?} ({}x{})",
        gw.title, gw.width as i32, gw.height as i32
    );
    std::thread::sleep(Duration::from_millis(1500));

    // 4. Capture the loaded homepage through the persistent-stream path + marks.
    let shot = shoot_safari();
    assert!(shot.width > 0 && shot.height > 0, "empty Safari capture");
    eprintln!(
        "[safari] homepage capture {}x{} px, {} actionable elements marked",
        shot.width,
        shot.height,
        shot.marks.len()
    );
    for m in shot.marks.iter().take(20) {
        eprintln!("  mark [{}] {} {:?}", m.number, m.role, m.label);
    }

    // 5. Read the homepage text with Vision OCR (what an agent does to "see" it).
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(&shot.base64_data)
        .expect("decode captured jpeg");
    let (w, h) = (shot.width, shot.height);
    let lines = with_timeout(20, "Vision OCR", move || {
        recognize(&jpeg, w, h, &["en-US", "zh-Hans"])
    })
    .expect("OCR should not error");
    eprintln!(
        "[safari] OCR recognized {} text lines on the Google homepage:",
        lines.len()
    );
    for l in lines.iter().take(40) {
        eprintln!("  {:?} @ ({:.0}, {:.0})", l.text, l.center.0, l.center.1);
    }

    // 6. Assertions: navigation worked (Google title) and nova actually read the
    //    page (surfaced marked controls or OCR text — a blank read means the
    //    capture / AX / OCR pipeline is broken even though a window exists).
    assert!(
        gw.title.to_lowercase().contains("google"),
        "expected the Safari title to contain \"Google\""
    );
    assert!(
        !lines.is_empty() || !shot.marks.is_empty(),
        "nova read NO content from the Google homepage (no OCR text, no marked elements) — \
         the capture / Accessibility / OCR pipeline is failing"
    );
}
