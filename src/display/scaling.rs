/// Coordinate scaling — convert between screenshot space and logical display coordinates.
///
/// Screenshots are resized to fit within a max dimension (typically 1280×768)
/// to keep payloads small for LLMs. This module converts coordinates between
/// the resized screenshot space and the actual display logical coordinates.
use crate::types::{DisplayInfo, LogicalCoord, ScreenCoord};

/// Target dimensions for resized screenshots.
#[derive(Debug, Clone, Copy)]
pub struct TargetDims {
    pub width: u32,
    pub height: u32,
}

/// Long-edge cap for a FULL-DISPLAY capture. Kept modest: a full screen is an
/// overview, and this value is recomputed at click time from the logical display
/// size (see `geometry::screen_to_logical_coords`), so it must stay a pure
/// function of the display — don't make it depend on the model or surface.
pub const MAX_DIMENSION: u32 = 1280;

/// Long-edge cap for a SINGLE-WINDOW capture. Higher than the display cap because
/// a window is the working surface where text/controls must be legible, and the
/// window path now feeds PHYSICAL (Retina) pixels — at 1568 a small 2× window is
/// captured sharp instead of at 1×. 1568 is the universal image cap (no model
/// re-downscales it), ~2.0MP, so it never wastes pixels. Window clicks map via
/// the capture's own `ViewFrame`, so a per-surface cap here is coordinate-safe.
pub const WINDOW_MAX_DIMENSION: u32 = 1568;

/// Long-edge cap for a `zoom_region` capture — the surface whose whole point is
/// reading fine detail. Opus 4.7+/4.8 accept up to 2576px / 3.75MP at 1:1
/// coordinates; 2200 stays under the area cap for typical aspect ratios. On an
/// older model the API simply re-downscales to 1568 (wasted bytes, not a
/// correctness issue). Region captures already use physical pixels and their own
/// `ViewFrame`, so this is coordinate-safe.
pub const REGION_MAX_DIMENSION: u32 = 2200;

/// Compute the target dimensions that fit within [`MAX_DIMENSION`] (the
/// full-display cap), preserving the original aspect ratio.
pub fn compute_target_dims(display_width: u32, display_height: u32) -> TargetDims {
    compute_target_dims_capped(display_width, display_height, MAX_DIMENSION)
}

/// Like [`compute_target_dims`] but with an explicit long-edge cap, for callers
/// (window / region) that pick their own budget. Never upscales.
pub fn compute_target_dims_capped(width: u32, height: u32, max_dim: u32) -> TargetDims {
    let max_edge = width.max(height);
    if max_edge <= max_dim {
        return TargetDims { width, height };
    }
    // Past here `max_edge > max_dim >= 0`, so `max_edge >= 1` — the divisor below
    // can't be zero.

    let scale = max_dim as f64 / max_edge as f64;
    TargetDims {
        width: (width as f64 * scale).round().max(1.0) as u32,
        height: (height as f64 * scale).round().max(1.0) as u32,
    }
}

/// Convert a coordinate in screenshot space to macOS logical coordinates.
///
/// `screenshot_dims` is the (width, height) of the actual screenshot image.
/// `display` is the display where the screenshot was captured.
pub fn screen_to_logical(
    coord: ScreenCoord,
    screenshot_dims: (f64, f64),
    display: &DisplayInfo,
) -> LogicalCoord {
    // Guard against a degenerate (zero-sized) screenshot, which would otherwise
    // produce inf/NaN coordinates and warp the cursor somewhere nonsensical.
    if screenshot_dims.0 <= 0.0 || screenshot_dims.1 <= 0.0 {
        return LogicalCoord {
            x: coord.x,
            y: coord.y,
        };
    }
    let scale_x = display.width as f64 / screenshot_dims.0;
    let scale_y = display.height as f64 / screenshot_dims.1;
    LogicalCoord {
        x: coord.x * scale_x,
        y: coord.y * scale_y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_target_dims_no_resize_small_screen() {
        let dims = compute_target_dims(800, 600);
        assert_eq!(dims.width, 800);
        assert_eq!(dims.height, 600);
    }

    #[test]
    fn compute_target_dims_resizes_4k_to_1280() {
        let dims = compute_target_dims(3840, 2160);
        assert_eq!(dims.width, 1280);
        assert_eq!(dims.height, 720);
    }

    #[test]
    fn compute_target_dims_resizes_vertical_display() {
        // Vertical portrait display (1080x1920)
        let dims = compute_target_dims(1080, 1920);
        assert_eq!(dims.height, 1280); // height is max edge
        assert_eq!(dims.width, 720);
    }

    #[test]
    fn compute_target_dims_exact_max_edge() {
        let dims = compute_target_dims(1280, 1024);
        // 1280 is not > 1280, so no resize
        assert_eq!(dims.width, 1280);
        assert_eq!(dims.height, 1024);
    }

    #[test]
    fn compute_target_dims_preserves_aspect_ratio() {
        let dims = compute_target_dims(2560, 1440);
        // 2560/1440 = 16/9 => 1280/720 = 16/9
        assert_eq!(dims.width, 1280);
        assert_eq!(dims.height, 720);
    }

    #[test]
    fn capped_window_keeps_retina_detail() {
        // A 1000×700pt window on a 2× display: feed PHYSICAL pixels (2000×1400),
        // capped to the window budget (1568) — far sharper than the old 1× path
        // that produced a 1000-wide image.
        let dims = compute_target_dims_capped(2000, 1400, WINDOW_MAX_DIMENSION);
        assert_eq!(dims.width, 1568);
        assert_eq!(dims.height, 1098); // 1400 * 1568/2000
    }

    #[test]
    fn capped_region_uses_larger_budget() {
        // A native 3000×1800 zoom region maps to the 2200 long-edge budget.
        let dims = compute_target_dims_capped(3000, 1800, REGION_MAX_DIMENSION);
        assert_eq!(dims.width, 2200);
        assert_eq!(dims.height, 1320);
    }

    #[test]
    fn capped_never_upscales() {
        // Below the cap, dimensions pass through unchanged (no upscaling a small
        // window to fill the budget).
        let dims = compute_target_dims_capped(800, 600, WINDOW_MAX_DIMENSION);
        assert_eq!(dims.width, 800);
        assert_eq!(dims.height, 600);
    }

    #[test]
    fn screen_to_logical_scales_coordinates() {
        let display = DisplayInfo {
            id: 1,
            width: 3840,
            height: 2160,
            scale_factor: 1.0,
            is_primary: true,
        };
        // Screenshot was resized to 1280x720
        let coord = ScreenCoord { x: 640.0, y: 360.0 };
        let logical = screen_to_logical(coord, (1280.0, 720.0), &display);
        // x: 640 * (3840/1280) = 640 * 3 = 1920
        // y: 360 * (2160/720) = 360 * 3 = 1080
        assert_eq!(logical.x, 1920.0);
        assert_eq!(logical.y, 1080.0);
    }

    #[test]
    fn screen_to_logical_zero_dims_returns_input_not_nan() {
        let display = DisplayInfo {
            id: 1,
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
            is_primary: true,
        };
        let coord = ScreenCoord { x: 100.0, y: 200.0 };
        let logical = screen_to_logical(coord, (0.0, 0.0), &display);
        // Must not be inf/NaN — falls back to the input coordinate.
        assert!(logical.x.is_finite() && logical.y.is_finite());
        assert_eq!(logical.x, 100.0);
        assert_eq!(logical.y, 200.0);
    }

    #[test]
    fn screen_to_logical_identity_when_same_size() {
        let display = DisplayInfo {
            id: 1,
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
            is_primary: true,
        };
        let coord = ScreenCoord { x: 100.0, y: 200.0 };
        let logical = screen_to_logical(coord, (1920.0, 1080.0), &display);
        assert_eq!(logical.x, 100.0);
        assert_eq!(logical.y, 200.0);
    }
}
