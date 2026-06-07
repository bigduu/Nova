//! End-to-end tests for the non-screenshot interaction surface.
//!
//! These avoid Screen Recording permission:
//! - coordinate mapping uses CoreGraphics `CGDisplay` (no permission needed)
//! - clipboard uses pbcopy/pbpaste
//!
//! Run with: `cargo test --test e2e_interaction`

use nova::display::geometry::{primary_display, screen_to_logical_coords};

/// The center of the screenshot must map to the center of the logical display,
/// and a screenshot-space corner must map to the display corner. This is the
/// core invariant that was previously broken (clicks used raw screenshot
/// coordinates with no scaling).
#[test]
fn screenshot_center_maps_to_display_center() {
    let display = primary_display();
    if display.width == 0 || display.height == 0 {
        // Headless/virtual display with no geometry — nothing to assert.
        eprintln!("no usable display geometry; skipping");
        return;
    }

    // Recreate the screenshot dims the capture path produces.
    let dims = nova::display::scaling::compute_target_dims(display.width, display.height);
    let (sw, sh) = (dims.width as f64, dims.height as f64);

    // Center of the screenshot → center of the logical display.
    let (cx, cy) = screen_to_logical_coords(sw / 2.0, sh / 2.0);
    let (ex, ey) = (display.width as f64 / 2.0, display.height as f64 / 2.0);
    assert!(
        (cx - ex).abs() <= 1.0 && (cy - ey).abs() <= 1.0,
        "center mapped to ({cx:.1}, {cy:.1}), expected (~{ex:.1}, ~{ey:.1})"
    );

    // Origin maps to origin.
    let (ox, oy) = screen_to_logical_coords(0.0, 0.0);
    assert_eq!((ox, oy), (0.0, 0.0));

    // Far corner maps to the logical extent.
    let (fx, fy) = screen_to_logical_coords(sw, sh);
    assert!(
        (fx - display.width as f64).abs() <= 1.0 && (fy - display.height as f64).abs() <= 1.0,
        "corner mapped to ({fx:.1}, {fy:.1}), expected (~{}, ~{})",
        display.width,
        display.height
    );
}

/// Writing then reading the clipboard should round-trip, including text with
/// shifted symbols and unicode (clipboard is byte-exact, unlike keystroke typing).
#[test]
fn clipboard_round_trips() {
    use nova::tools::clipboard::{read_clipboard, write_clipboard};

    // Snapshot and restore the user's clipboard so the test is non-destructive.
    let original = read_clipboard().unwrap_or_default();

    let payload = "Nova e2e: user.name@example.com — 中文 🚀";
    write_clipboard(payload).expect("write_clipboard should succeed");
    let read_back = read_clipboard().expect("read_clipboard should succeed");
    assert_eq!(read_back, payload);

    // Best-effort restore.
    let _ = write_clipboard(&original);
}
