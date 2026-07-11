//! End-to-end tests that actually POST input events (CGEvent) to the macOS
//! window server, plus the application/window introspection tools.
//!
//! These have real side effects — they move the cursor, click, scroll, and type
//! into whatever window is focused — so they are all `#[ignore]` and meant to be
//! run deliberately on a machine where that is acceptable:
//!
//!     cargo test --test e2e_input -- --ignored
//!
//! Where a test can verify the system actually observed the event (mouse
//! position), it asserts; where it cannot without a controlled target app, it is
//! a smoke test that proves the posting path returns `Ok` and restores state.

use nova::platform::mac::input::{
    cursor_position, double_click_at, key_combo, left_click_at, mouse_move, right_click_at,
    scroll_at, type_text,
};
use nova::tools::input::InputTarget;
use std::thread::sleep;
use std::time::Duration;

mod common;

/// The strongest input e2e: post real cursor-move events and read the position
/// back from the window server. This proves the move path reaches the OS *and*
/// that the logical coordinate space round-trips. Restores the original cursor.
#[test]
#[ignore = "posts real mouse-move events; moves the cursor"]
fn mouse_move_roundtrips_through_cursor_position() {
    let original = cursor_position().expect("read initial cursor position");

    for (tx, ty) in [(200.0, 200.0), (640.0, 360.0), (123.0, 456.0)] {
        mouse_move(tx, ty).expect("mouse_move should post");
        sleep(Duration::from_millis(25));
        let (cx, cy) = cursor_position().expect("read cursor position");
        assert!(
            (cx - tx).abs() <= 2.0 && (cy - ty).abs() <= 2.0,
            "moved to ({tx}, {ty}) but window server reports ({cx}, {cy})"
        );
    }

    // Restore.
    let _ = mouse_move(original.0, original.1);
}

/// Smoke test for the click-posting paths. Clicks the bottom-right corner —
/// empty desktop on a default setup, so a left/double click just deselects.
/// (Right-click would open the desktop context menu, so it is exercised then
/// dismissed with Escape.) Verifies each path returns `Ok`. Restores the cursor.
#[test]
#[ignore = "posts real click events on the desktop"]
fn click_events_post_without_error() {
    let display = nova::display::geometry::primary_display();
    let original = cursor_position().unwrap_or((0.0, 0.0));
    let (x, y) = (display.width as f64 - 2.0, display.height as f64 - 2.0);

    left_click_at(x, y, InputTarget::Global).expect("left_click_at should post");
    sleep(Duration::from_millis(50));
    double_click_at(x, y, InputTarget::Global).expect("double_click_at should post");
    sleep(Duration::from_millis(50));
    right_click_at(x, y, InputTarget::Global).expect("right_click_at should post");
    sleep(Duration::from_millis(50));
    // Dismiss any context menu the right-click opened.
    let _ = key_combo("escape", InputTarget::Global);

    let _ = mouse_move(original.0, original.1);
}

/// Smoke test for the scroll-posting path (vertical, both directions). Scrolls
/// whatever is under the cursor; verifies the event constructs and posts.
#[test]
#[ignore = "posts real scroll events"]
fn scroll_events_post_without_error() {
    let (x, y) = cursor_position().unwrap_or((400.0, 400.0));
    scroll_at(x, y, 3, InputTarget::Global).expect("scroll up should post");
    sleep(Duration::from_millis(20));
    scroll_at(x, y, -3, InputTarget::Global).expect("scroll down should post");
}

/// Smoke test for keyboard text entry. Types into the focused window, so it is
/// intentionally benign (a few letters) and `#[ignore]`. Verifies the full
/// type_text path — including the shifted-symbol map — posts without error.
#[test]
#[ignore = "types into the focused window"]
fn type_text_posts_without_error() {
    // Exercises ASCII (lower/upper/digit/symbol) AND non-ASCII (CJK) — the
    // Unicode path must handle characters with no key on a US layout.
    type_text("Nova7@ 中文", InputTarget::Global).expect("type_text should post");
}

// ── application ─────────────────────────────────────────────────────

/// `list_applications` should enumerate real installed apps via Spotlight,
/// sorted and pointing at `.app` bundles. Tolerant of a Spotlight-less CI host
/// (asserts the invariants only when results are present).
#[test]
fn list_applications_returns_app_bundles() {
    let apps = nova::tools::application::list_applications().expect("list_applications should Ok");

    if apps.is_empty() {
        eprintln!("no Spotlight results (headless/CI?); skipping invariant checks");
        return;
    }

    for app in &apps {
        assert!(!app.name.is_empty(), "app name must not be empty");
        assert!(
            app.path.ends_with(".app"),
            "expected a .app bundle, got {}",
            app.path
        );
    }

    // Verify the sort contract (case-insensitive by name).
    let mut sorted = apps.clone();
    sorted.sort_by_key(|a| a.name.to_lowercase());
    let names: Vec<_> = apps.iter().map(|a| a.name.to_lowercase()).collect();
    let sorted_names: Vec<_> = sorted.iter().map(|a| a.name.to_lowercase()).collect();
    assert_eq!(
        names, sorted_names,
        "list_applications must be sorted by name"
    );
}

/// `open_application` against a stable system app. Launches/focuses it, so
/// `#[ignore]`. Verifies the launch path returns `Ok`.
#[test]
#[ignore = "launches/focuses a real application"]
fn open_application_launches_system_app() {
    nova::tools::application::open_application("System Settings")
        .expect("open_application should launch a known app");
}

// ── window ──────────────────────────────────────────────────────────

/// `list_windows` enumerates on-screen windows via ScreenCaptureKit (needs
/// Screen Recording permission). Verifies every returned window satisfies the
/// module's invariants (non-empty title, finite geometry).
#[test]
#[ignore = "requires Screen Recording permission in System Settings"]
fn list_windows_returns_titled_windows() {
    common::use_isolated_capture_daemon();
    let windows = nova::tools::window::list_windows().expect("list_windows should Ok");

    for w in &windows {
        assert!(
            !w.title.is_empty(),
            "windows without titles are filtered out"
        );
        assert!(
            w.width.is_finite() && w.height.is_finite(),
            "window geometry must be finite"
        );
    }
}
