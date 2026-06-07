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

/// Maximum dimension for screenshot resizing (constrains to ~1MP).
const MAX_DIMENSION: u32 = 1280;

/// Compute the target dimensions that fit within the max dimension,
/// preserving the original aspect ratio.
pub fn compute_target_dims(display_width: u32, display_height: u32) -> TargetDims {
    let max_edge = display_width.max(display_height);
    if max_edge <= MAX_DIMENSION {
        return TargetDims {
            width: display_width,
            height: display_height,
        };
    }

    let scale = MAX_DIMENSION as f64 / max_edge as f64;
    TargetDims {
        width: (display_width as f64 * scale).round() as u32,
        height: (display_height as f64 * scale).round() as u32,
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
