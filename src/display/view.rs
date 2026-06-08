//! The active "view frame" — how the pixels of the most recent screenshot map
//! back to global macOS logical points (where mouse events are posted).
//!
//! A full-display screenshot and a single-window screenshot have different
//! coordinate frames (origin + size). Storing the frame of the last capture
//! lets the model keep working in "the pixel space of the image it just saw"
//! while clicks still land on the right physical spot — which is what makes
//! single-window screenshots usable (and they cut both LLM context and the
//! downscaling that hurts coordinate precision).

/// Maps a screenshot-pixel coordinate to a global logical point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewFrame {
    /// Global logical top-left of the captured region (points).
    pub origin: (f64, f64),
    /// Logical size of the captured region (points).
    pub region: (f64, f64),
    /// Screenshot image dimensions (pixels).
    pub screenshot: (f64, f64),
}

impl ViewFrame {
    /// Convert a coordinate in screenshot-image pixels to global logical points.
    pub fn to_logical(&self, x: f64, y: f64) -> (f64, f64) {
        // Degenerate screenshot: translate by origin only, never inf/NaN.
        if self.screenshot.0 <= 0.0 || self.screenshot.1 <= 0.0 {
            return (self.origin.0 + x, self.origin.1 + y);
        }
        (
            self.origin.0 + x / self.screenshot.0 * self.region.0,
            self.origin.1 + y / self.screenshot.1 * self.region.1,
        )
    }

    /// Inverse of [`to_logical`](Self::to_logical): map a global logical point
    /// to this image's pixel space. Used to place Set-of-Mark overlays for AX
    /// elements (whose frames are in global logical points) onto the screenshot.
    pub fn to_screenshot(&self, lx: f64, ly: f64) -> (f64, f64) {
        if self.region.0 <= 0.0 || self.region.1 <= 0.0 {
            return (lx - self.origin.0, ly - self.origin.1);
        }
        (
            (lx - self.origin.0) / self.region.0 * self.screenshot.0,
            (ly - self.origin.1) / self.region.1 * self.screenshot.1,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_display_frame_scales_like_before() {
        // 1280x536 screenshot of a 3440x1440 display at origin (0,0).
        let f = ViewFrame {
            origin: (0.0, 0.0),
            region: (3440.0, 1440.0),
            screenshot: (1280.0, 536.0),
        };
        let (x, y) = f.to_logical(640.0, 268.0);
        assert!((x - 1720.0).abs() < 0.01); // 640/1280 * 3440
        assert!((y - 720.0).abs() < 0.01); // 268/536 * 1440
    }

    #[test]
    fn window_frame_adds_origin_offset() {
        // A 800x600 window at global (1000, 200), screenshot 800x600 (no resize).
        let f = ViewFrame {
            origin: (1000.0, 200.0),
            region: (800.0, 600.0),
            screenshot: (800.0, 600.0),
        };
        // Center of the window image -> center of the window in global coords.
        assert_eq!(f.to_logical(400.0, 300.0), (1400.0, 500.0));
        // Top-left of the image -> the window's origin.
        assert_eq!(f.to_logical(0.0, 0.0), (1000.0, 200.0));
    }

    #[test]
    fn to_screenshot_is_inverse_of_to_logical() {
        let f = ViewFrame {
            origin: (100.0, 50.0),
            region: (1600.0, 1000.0),
            screenshot: (1280.0, 800.0),
        };
        // A logical point round-trips back to the same screenshot pixel.
        let (lx, ly) = f.to_logical(640.0, 400.0);
        let (sx, sy) = f.to_screenshot(lx, ly);
        assert!((sx - 640.0).abs() < 1e-6 && (sy - 400.0).abs() < 1e-6);
        // The region origin maps to the image origin.
        assert_eq!(f.to_screenshot(100.0, 50.0), (0.0, 0.0));
    }

    #[test]
    fn region_in_image_px_resolves_to_logical_rect() {
        // A window view: 800x600 window at global (1000, 200), shown 1:1.
        let f = ViewFrame {
            origin: (1000.0, 200.0),
            region: (800.0, 600.0),
            screenshot: (800.0, 600.0),
        };
        // Model asks to zoom image-rect [100,100, 200,150]; this is how the
        // server resolves it to a global-logical rectangle.
        let (tlx, tly) = f.to_logical(100.0, 100.0);
        let (brx, bry) = f.to_logical(100.0 + 200.0, 100.0 + 150.0);
        assert_eq!((tlx, tly), (1100.0, 300.0));
        assert_eq!((brx - tlx, bry - tly), (200.0, 150.0));
    }

    #[test]
    fn zero_screenshot_is_finite() {
        let f = ViewFrame {
            origin: (10.0, 20.0),
            region: (100.0, 100.0),
            screenshot: (0.0, 0.0),
        };
        let (x, y) = f.to_logical(5.0, 5.0);
        assert!(x.is_finite() && y.is_finite());
        assert_eq!((x, y), (15.0, 25.0));
    }
}
