/// Coordinate in screenshot space (resized, typically 1280×768 max dimension).
/// LLMs see and interact with this coordinate system.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScreenCoord {
    pub x: f64,
    pub y: f64,
}

/// Coordinate in macOS logical points (the native coordinate space).
/// Used for CoreGraphics event posting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalCoord {
    pub x: f64,
    pub y: f64,
}

/// Physical pixel coordinate (Retina-aware).
/// Used for pixel-level operations like screenshot regions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalCoord {
    pub x: u32,
    pub y: u32,
}

/// A display's geometry and scale factor.
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub is_primary: bool,
}

/// A window's metadata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WindowInfo {
    pub title: String,
    pub app_name: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub is_visible: bool,
}

impl ScreenCoord {
    /// Convert screenshot-space coordinates to macOS logical coordinates.
    pub fn to_logical(&self, screenshot_size: (f64, f64), display: &DisplayInfo) -> LogicalCoord {
        let scale_x = display.width as f64 / screenshot_size.0;
        let scale_y = display.height as f64 / screenshot_size.1;
        LogicalCoord {
            x: self.x * scale_x,
            y: self.y * scale_y,
        }
    }
}
