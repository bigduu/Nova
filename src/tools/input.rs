/// Input tools — mouse and keyboard control via CoreGraphics CGEvent.
///
/// All coordinates are in macOS logical points (LogicalCoord).
/// CGEvent posting uses the CGEventPost API through core-graphics bindings.
use crate::error::Result;
use crate::types::LogicalCoord;

/// Move the mouse cursor to the given logical coordinates.
pub fn mouse_move(pos: LogicalCoord) -> Result<()> {
    // TODO: implement with CGEvent
    let _ = pos;
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Perform a left mouse click at the current cursor position.
pub fn left_click() -> Result<()> {
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Perform a right mouse click at the current cursor position.
pub fn right_click() -> Result<()> {
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Perform a left double-click at the current cursor position.
pub fn double_click() -> Result<()> {
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Click and drag from start to end, with optional animation step count.
pub fn left_click_drag(start: LogicalCoord, end: LogicalCoord, steps: Option<u32>) -> Result<()> {
    let _ = (start, end, steps);
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Press left mouse button down (no release until mouse_up is called).
pub fn left_mouse_down() -> Result<()> {
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Release left mouse button.
pub fn left_mouse_up() -> Result<()> {
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Scroll by the given number of lines (positive = up, negative = down).
pub fn scroll(lines: i32) -> Result<()> {
    let _ = lines;
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Horizontal scroll by the given number of lines.
pub fn scroll_horizontal(lines: i32) -> Result<()> {
    let _ = lines;
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Get the current cursor position in logical coordinates.
pub fn cursor_position() -> Result<(f64, f64)> {
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Simulate a key combination (e.g., "Cmd+C").
pub fn key_combo(key: &str) -> Result<()> {
    let _ = key;
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Hold a key down for N milliseconds.
pub fn hold_key(key: &str, duration_ms: u64) -> Result<()> {
    let _ = (key, duration_ms);
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}

/// Type a string of text into the currently focused element.
pub fn type_text(text: &str) -> Result<()> {
    let _ = text;
    Err(crate::error::NovaError::Input("not yet implemented".into()))
}
