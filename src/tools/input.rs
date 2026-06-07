/// Input tools — mouse and keyboard control via CoreGraphics CGEvent.
///
/// All coordinates are in macOS logical points.
/// Events are posted to the HID (Human Interface Device) level
/// so applications receive them as normal input events.
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use std::thread;
use std::time::Duration;

use crate::error::{NovaError, Result};

// ── Event source ────────────────────────────────────────────────────

/// Create a private event source. Using `Private` state means the events
/// are treated as if they came from a physical device.
fn event_source() -> Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| NovaError::Input("failed to create event source".into()))
}

// ── Mouse movement ──────────────────────────────────────────────────

/// Move the mouse cursor to the given logical coordinates.
pub fn mouse_move(x: f64, y: f64) -> Result<()> {
    let point = CGPoint { x, y };
    CGDisplay::warp_mouse_cursor_position(point)
        .map_err(|e| NovaError::Input(format!("warp_mouse_cursor_position: {e:?}")))
}

/// Get the current cursor position in logical coordinates.
pub fn cursor_position() -> Result<(f64, f64)> {
    let e = CGEvent::new(event_source()?)
        .map_err(|_| NovaError::Input("failed to create query event".into()))?;
    let pos = e.location();
    Ok((pos.x, pos.y))
}

// ── Mouse clicks ────────────────────────────────────────────────────

/// Create and post a mouse event at the given position.
fn post_mouse_event(
    event_type: CGEventType,
    button: CGMouseButton,
    pos: CGPoint,
) -> Result<()> {
    let source = event_source()?;
    let event = CGEvent::new_mouse_event(source, event_type, pos, button)
        .map_err(|_| NovaError::Input("failed to create mouse event".into()))?;
    event.post(CGEventTapLocation::HID);
    // Small delay to let the event be processed
    thread::sleep(Duration::from_millis(1));
    Ok(())
}

/// Left click at the current position.
pub fn left_click() -> Result<()> {
    let (x, y) = cursor_position()?;
    let pos = CGPoint { x, y };
    post_mouse_event(CGEventType::LeftMouseDown, CGMouseButton::Left, pos)?;
    post_mouse_event(CGEventType::LeftMouseUp, CGMouseButton::Left, pos)
}

/// Right click at the current position.
pub fn right_click() -> Result<()> {
    let (x, y) = cursor_position()?;
    let pos = CGPoint { x, y };
    post_mouse_event(CGEventType::RightMouseDown, CGMouseButton::Right, pos)?;
    post_mouse_event(CGEventType::RightMouseUp, CGMouseButton::Right, pos)
}

/// Double click at the current position.
pub fn double_click() -> Result<()> {
    let (x, y) = cursor_position()?;
    let pos = CGPoint { x, y };
    // First click
    post_mouse_event(CGEventType::LeftMouseDown, CGMouseButton::Left, pos)?;
    post_mouse_event(CGEventType::LeftMouseUp, CGMouseButton::Left, pos)?;
    // Double-click interval (~200ms is standard)
    thread::sleep(Duration::from_millis(50));
    // Second click
    post_mouse_event(CGEventType::LeftMouseDown, CGMouseButton::Left, pos)?;
    post_mouse_event(CGEventType::LeftMouseUp, CGMouseButton::Left, pos)
}

/// Left click at specific coordinates.
pub fn left_click_at(x: f64, y: f64) -> Result<()> {
    mouse_move(x, y)?;
    thread::sleep(Duration::from_millis(10));
    left_click()
}

/// Right click at specific coordinates.
pub fn right_click_at(x: f64, y: f64) -> Result<()> {
    mouse_move(x, y)?;
    thread::sleep(Duration::from_millis(10));
    right_click()
}

/// Press left mouse button down (no release until mouse_up is called).
pub fn left_mouse_down() -> Result<()> {
    let (x, y) = cursor_position()?;
    let pos = CGPoint { x, y };
    post_mouse_event(CGEventType::LeftMouseDown, CGMouseButton::Left, pos)
}

/// Release left mouse button.
pub fn left_mouse_up() -> Result<()> {
    let (x, y) = cursor_position()?;
    let pos = CGPoint { x, y };
    post_mouse_event(CGEventType::LeftMouseUp, CGMouseButton::Left, pos)
}

// ── Click and drag ──────────────────────────────────────────────────

/// Click and drag from start to end, with optional animation step count.
pub fn left_click_drag(start: (f64, f64), end: (f64, f64), steps: Option<u32>) -> Result<()> {
    let steps = steps.unwrap_or(20);
    let (sx, sy) = start;
    let (ex, ey) = end;

    // Move to start and press
    mouse_move(sx, sy)?;
    thread::sleep(Duration::from_millis(5));
    left_mouse_down()?;
    thread::sleep(Duration::from_millis(5));

    // Animate to end
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let x = sx + (ex - sx) * t;
        let y = sy + (ey - sy) * t;
        mouse_move(x, y)?;
        thread::sleep(Duration::from_millis(5));
    }

    // Release at end
    left_mouse_up()
}

// ── Scrolling ───────────────────────────────────────────────────────

/// Scroll by the given number of lines.
/// Positive lines = up (content moves up, scrollbar moves down).
/// Negative lines = down.
pub fn scroll(lines: i32) -> Result<()> {
    let source = event_source()?;
    let event = CGEvent::new_scroll_event(
        source,
        ScrollEventUnit::LINE,
        2,      // wheel_count: 2 axes (vertical only for now)
        0,      // wheel1: horizontal (unused)
        lines,  // wheel2: vertical
        0,      // wheel3: unused
    )
    .map_err(|_| NovaError::Input("failed to create scroll event".into()))?;
    event.post(CGEventTapLocation::HID);
    Ok(())
}

/// Horizontal scroll by the given number of lines.
pub fn scroll_horizontal(lines: i32) -> Result<()> {
    let source = event_source()?;
    let event = CGEvent::new_scroll_event(
        source,
        ScrollEventUnit::LINE,
        2,
        lines, // wheel1: horizontal
        0,     // wheel2: vertical (unused)
        0,
    )
    .map_err(|_| NovaError::Input("failed to create scroll event".into()))?;
    event.post(CGEventTapLocation::HID);
    Ok(())
}

// ── Keyboard ────────────────────────────────────────────────────────

/// Map common key names to macOS key codes.
/// See: <https://web.archive.org/web/20100501161453/http://www.classicteck.com/rbarticles/mackeyboard.php>
fn key_name_to_code(name: &str) -> Option<u16> {
    match name.to_lowercase().as_str() {
        "a" => Some(0), "s" => Some(1), "d" => Some(2), "f" => Some(3),
        "h" => Some(4), "g" => Some(5), "z" => Some(6), "x" => Some(7),
        "c" => Some(8), "v" => Some(9), "b" => Some(11), "q" => Some(12),
        "w" => Some(13), "e" => Some(14), "r" => Some(15), "y" => Some(16),
        "t" => Some(17), "1" => Some(18), "2" => Some(19), "3" => Some(20),
        "4" => Some(21), "6" => Some(22), "5" => Some(23), "=" => Some(24),
        "9" => Some(25), "7" => Some(26), "-" => Some(27), "8" => Some(28),
        "0" => Some(29), "]" => Some(30), "o" => Some(31), "u" => Some(32),
        "[" => Some(33), "i" => Some(34), "p" => Some(35), "l" => Some(37),
        "j" => Some(38), "'" => Some(39), "k" => Some(40), ";" => Some(41),
        "\\" => Some(42), "," => Some(43), "/" => Some(44), "n" => Some(45),
        "m" => Some(46), "." => Some(47), "`" => Some(50),
        "return" | "enter" => Some(36),
        "tab" => Some(48),
        "space" => Some(49),
        "delete" | "backspace" => Some(51),
        "escape" | "esc" => Some(53),
        "cmd" | "command" => Some(55),
        "shift" => Some(56),
        "capslock" | "caps_lock" => Some(57),
        "option" | "alt" => Some(58),
        "control" | "ctrl" => Some(59),
        "right_shift" => Some(60),
        "right_option" | "right_alt" => Some(61),
        "right_control" | "right_ctrl" => Some(62),
        "fn" | "function" => Some(63),
        "f1" => Some(122), "f2" => Some(120), "f3" => Some(99),
        "f4" => Some(118), "f5" => Some(96), "f6" => Some(97),
        "f7" => Some(98), "f8" => Some(100), "f9" => Some(101),
        "f10" => Some(109), "f11" => Some(103), "f12" => Some(111),
        "left" => Some(123), "right" => Some(124),
        "down" => Some(125), "up" => Some(126),
        "home" => Some(115), "end" => Some(119),
        "page_up" | "pgup" => Some(116),
        "page_down" | "pgdn" => Some(121),
        _ => None,
    }
}

/// Post a key down or up event.
fn post_key(keycode: u16, keydown: bool) -> Result<()> {
    let source = event_source()?;
    let event = CGEvent::new_keyboard_event(source, keycode, keydown)
        .map_err(|_| NovaError::Input("failed to create keyboard event".into()))?;
    event.post(CGEventTapLocation::HID);
    thread::sleep(Duration::from_millis(1));
    Ok(())
}

/// Press and release a single key.
fn tap_key(keycode: u16) -> Result<()> {
    post_key(keycode, true)?;
    post_key(keycode, false)
}

/// Check if a key name is a modifier key.
fn is_modifier(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "cmd" | "command" | "shift" | "option" | "alt" | "control" | "ctrl"
            | "right_shift" | "right_option" | "right_alt" | "right_control" | "right_ctrl"
    )
}

/// Simulate a key combination (e.g., "cmd+c", "shift+tab", "ctrl+cmd+f").
pub fn key_combo(combo: &str) -> Result<()> {
    let parts: Vec<&str> = combo.split('+').map(|s| s.trim()).collect();
    let mut modifiers: Vec<&str> = Vec::new();
    let mut keys: Vec<&str> = Vec::new();

    for &k in &parts {
        if is_modifier(k) {
            modifiers.push(k);
        } else {
            keys.push(k);
        }
    }

    if keys.is_empty() {
        return Err(NovaError::Input(format!("no main key in combo: {combo}")));
    }

    // Press modifiers
    for &m in &modifiers {
        if let Some(code) = key_name_to_code(m) {
            post_key(code, true)?;
        }
    }
    thread::sleep(Duration::from_millis(2));

    // Press and release main keys
    for &k in &keys {
        if let Some(code) = key_name_to_code(k) {
            tap_key(code)?;
        } else {
            return Err(NovaError::Input(format!("unknown key: {k}")));
        }
    }
    thread::sleep(Duration::from_millis(2));

    // Release modifiers (reverse order)
    for &m in modifiers.iter().rev() {
        if let Some(code) = key_name_to_code(m) {
            post_key(code, false)?;
        }
    }

    Ok(())
}

/// Hold a key down for N milliseconds, then release.
pub fn hold_key(key: &str, duration_ms: u64) -> Result<()> {
    let code = key_name_to_code(key)
        .ok_or_else(|| NovaError::Input(format!("unknown key: {key}")))?;

    post_key(code, true)?;
    thread::sleep(Duration::from_millis(duration_ms));
    post_key(code, false)
}

// ── Type text ───────────────────────────────────────────────────────

/// Type a string of text into the currently focused element.
///
/// This sends raw keyboard events for each character.
/// For more reliable text input, consider using the accessibility API
/// or clipboard paste approach for long strings.
pub fn type_text(text: &str) -> Result<()> {
    for ch in text.chars() {
        let key_str = match ch {
            ' ' => "space",
            '\n' | '\r' => "return",
            '\t' => "tab",
            _ => {
                // For uppercase letters, we need shift
                if ch.is_ascii_uppercase() {
                    let lower = ch.to_ascii_lowercase();
                    let code = key_name_to_code(&lower.to_string())
                        .ok_or_else(|| NovaError::Input(format!("cannot type: {ch}")))?;
                    // Press shift
                    if let Some(shift_code) = key_name_to_code("shift") {
                        post_key(shift_code, true)?;
                        thread::sleep(Duration::from_millis(1));
                        tap_key(code)?;
                        post_key(shift_code, false)?;
                    }
                    continue;
                } else {
                    &ch.to_string()
                }
            }
        };
        if let Some(code) = key_name_to_code(key_str) {
            tap_key(code)?;
        } else {
            return Err(NovaError::Input(format!("cannot type character: {ch}")));
        }
    }
    Ok(())
}
