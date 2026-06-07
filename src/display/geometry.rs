/// Display geometry — logical/physical dimensions of the primary display.
///
/// Click and cursor coordinates live in the *global logical point* space that
/// CoreGraphics (CGEvent / CGWarpMouseCursorPosition) uses, so we read the
/// primary display's geometry from `CGDisplay` rather than ScreenCaptureKit.
/// Screen-recording *permission* is a separate concern, detected via
/// ScreenCaptureKit (see [`screen_recording_available`]).
use crate::display::scaling::{compute_target_dims, screen_to_logical};
use crate::types::{DisplayInfo, ScreenCoord};
use core_graphics::display::CGDisplay;
use screencapturekit::shareable_content::SCShareableContent;

/// Get the primary display's geometry in logical points, with the real
/// backing-scale factor (2.0 on Retina). Logical dims are the coordinate
/// space mouse events are posted in.
pub fn primary_display() -> DisplayInfo {
    let main = CGDisplay::main();
    let bounds = main.bounds();
    let logical_w = bounds.size.width;
    let logical_h = bounds.size.height;
    // pixels_wide/high report the physical backing resolution; the ratio to
    // logical points is the backing-scale factor.
    let scale_factor = if logical_w > 0.0 {
        main.pixels_wide() as f64 / logical_w
    } else {
        1.0
    };
    DisplayInfo {
        id: main.id,
        width: logical_w as u32,
        height: logical_h as u32,
        scale_factor,
        is_primary: true,
    }
}

/// Convert a coordinate from screenshot space (what the LLM sees) to the
/// global logical point coordinates used to post mouse events.
///
/// Screenshots are deterministically derived from the primary display
/// ([`compute_target_dims`]), so the screenshot dimensions can be recomputed
/// here without the server having to remember the last capture.
pub fn screen_to_logical_coords(x: f64, y: f64) -> (f64, f64) {
    let display = primary_display();
    let dims = compute_target_dims(display.width, display.height);
    let logical = screen_to_logical(
        ScreenCoord { x, y },
        (dims.width as f64, dims.height as f64),
        &display,
    );
    (logical.x, logical.y)
}

/// Whether Screen Recording permission is granted (and capture is possible).
///
/// `SCShareableContent::get` fails (or yields no displays) when the host app
/// has not been granted Screen Recording in System Settings, which is exactly
/// the signal we want — unlike `CGDisplay`, which works without permission.
pub fn screen_recording_available() -> bool {
    match SCShareableContent::get() {
        Ok(content) => !content.displays().is_empty(),
        Err(_) => false,
    }
}

/// List all available displays (via ScreenCaptureKit; requires permission).
pub fn list_displays() -> Vec<DisplayInfo> {
    let Ok(content) = SCShareableContent::get() else {
        return vec![];
    };
    content
        .displays()
        .iter()
        .enumerate()
        .map(|(i, d)| DisplayInfo {
            id: d.display_id(),
            width: d.width(),
            height: d.height(),
            scale_factor: 1.0,
            is_primary: i == 0,
        })
        .collect()
}
