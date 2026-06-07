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
fn post_mouse_event(event_type: CGEventType, button: CGMouseButton, pos: CGPoint) -> Result<()> {
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

/// Scroll vertically by the given number of lines.
/// Positive lines = up (content moves up, scrollbar moves down).
/// Negative lines = down.
///
/// CGEvent scroll wheels are ordered (wheel1 = vertical/y, wheel2 =
/// horizontal/x, wheel3 = z), so the vertical delta goes in wheel1.
pub fn scroll(lines: i32) -> Result<()> {
    let source = event_source()?;
    let event = CGEvent::new_scroll_event(
        source,
        ScrollEventUnit::LINE,
        2,     // wheel_count
        lines, // wheel1: vertical
        0,     // wheel2: horizontal (unused)
        0,     // wheel3: unused
    )
    .map_err(|_| NovaError::Input("failed to create scroll event".into()))?;
    event.post(CGEventTapLocation::HID);
    Ok(())
}

/// Scroll horizontally by the given number of lines.
/// Positive lines = right, negative = left. (wheel2 = horizontal/x axis.)
pub fn scroll_horizontal(lines: i32) -> Result<()> {
    let source = event_source()?;
    let event = CGEvent::new_scroll_event(
        source,
        ScrollEventUnit::LINE,
        2,     // wheel_count
        0,     // wheel1: vertical (unused)
        lines, // wheel2: horizontal
        0,     // wheel3: unused
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
        "a" => Some(0),
        "s" => Some(1),
        "d" => Some(2),
        "f" => Some(3),
        "h" => Some(4),
        "g" => Some(5),
        "z" => Some(6),
        "x" => Some(7),
        "c" => Some(8),
        "v" => Some(9),
        "b" => Some(11),
        "q" => Some(12),
        "w" => Some(13),
        "e" => Some(14),
        "r" => Some(15),
        "y" => Some(16),
        "t" => Some(17),
        "1" => Some(18),
        "2" => Some(19),
        "3" => Some(20),
        "4" => Some(21),
        "6" => Some(22),
        "5" => Some(23),
        "=" => Some(24),
        "9" => Some(25),
        "7" => Some(26),
        "-" => Some(27),
        "8" => Some(28),
        "0" => Some(29),
        "]" => Some(30),
        "o" => Some(31),
        "u" => Some(32),
        "[" => Some(33),
        "i" => Some(34),
        "p" => Some(35),
        "l" => Some(37),
        "j" => Some(38),
        "'" => Some(39),
        "k" => Some(40),
        ";" => Some(41),
        "\\" => Some(42),
        "," => Some(43),
        "/" => Some(44),
        "n" => Some(45),
        "m" => Some(46),
        "." => Some(47),
        "`" => Some(50),
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
        "f1" => Some(122),
        "f2" => Some(120),
        "f3" => Some(99),
        "f4" => Some(118),
        "f5" => Some(96),
        "f6" => Some(97),
        "f7" => Some(98),
        "f8" => Some(100),
        "f9" => Some(101),
        "f10" => Some(109),
        "f11" => Some(103),
        "f12" => Some(111),
        "left" => Some(123),
        "right" => Some(124),
        "down" => Some(125),
        "up" => Some(126),
        "home" => Some(115),
        "end" => Some(119),
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
        "cmd"
            | "command"
            | "shift"
            | "option"
            | "alt"
            | "control"
            | "ctrl"
            | "right_shift"
            | "right_option"
            | "right_alt"
            | "right_control"
            | "right_ctrl"
    )
}

/// Parse a key combination string into (modifier keycodes, main keycodes).
///
/// Splits on `+`, classifies each token as modifier or main key, and resolves
/// keycodes. Errors on an unknown key or when no main (non-modifier) key is
/// present (e.g. `"cmd"` alone). Empty tokens (from a trailing `+`) are ignored.
fn parse_combo(combo: &str) -> Result<(Vec<u16>, Vec<u16>)> {
    let mut modifiers: Vec<u16> = Vec::new();
    let mut keys: Vec<u16> = Vec::new();

    for token in combo.split('+').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let code = key_name_to_code(token)
            .ok_or_else(|| NovaError::Input(format!("unknown key: {token}")))?;
        if is_modifier(token) {
            modifiers.push(code);
        } else {
            keys.push(code);
        }
    }

    if keys.is_empty() {
        return Err(NovaError::Input(format!("no main key in combo: {combo}")));
    }

    Ok((modifiers, keys))
}

/// Simulate a key combination (e.g., "cmd+c", "shift+tab", "ctrl+cmd+f").
pub fn key_combo(combo: &str) -> Result<()> {
    let (modifiers, keys) = parse_combo(combo)?;

    // Press modifiers
    for &code in &modifiers {
        post_key(code, true)?;
    }
    thread::sleep(Duration::from_millis(2));

    // Press and release main keys
    for &code in &keys {
        tap_key(code)?;
    }
    thread::sleep(Duration::from_millis(2));

    // Release modifiers (reverse order)
    for &code in modifiers.iter().rev() {
        post_key(code, false)?;
    }

    Ok(())
}

/// Hold a key down for N milliseconds, then release.
pub fn hold_key(key: &str, duration_ms: u64) -> Result<()> {
    let code =
        key_name_to_code(key).ok_or_else(|| NovaError::Input(format!("unknown key: {key}")))?;

    post_key(code, true)?;
    thread::sleep(Duration::from_millis(duration_ms));
    post_key(code, false)
}

// ── Type text ───────────────────────────────────────────────────────

/// Map a printable character to a (keycode, needs_shift) pair on a US layout.
///
/// Covers letters, digits, whitespace, and the ASCII punctuation reachable from
/// the US keyboard — including the shifted symbols (`@ : ? "` …) that an agent
/// routinely needs to type emails, URLs, and JSON. Returns `None` for anything
/// not directly typable (e.g. non-ASCII), which the caller surfaces as an error.
fn char_to_keystroke(ch: char) -> Option<(u16, bool)> {
    // Whitespace and direct keys.
    match ch {
        ' ' => return Some((49, false)),         // space
        '\n' | '\r' => return Some((36, false)), // return
        '\t' => return Some((48, false)),        // tab
        _ => {}
    }

    // Uppercase letters: lowercase keycode + shift.
    if ch.is_ascii_uppercase() {
        return key_name_to_code(&ch.to_ascii_lowercase().to_string()).map(|c| (c, true));
    }

    // Shifted symbols map onto an unshifted base key + shift.
    let shifted_base = match ch {
        '!' => Some("1"),
        '@' => Some("2"),
        '#' => Some("3"),
        '$' => Some("4"),
        '%' => Some("5"),
        '^' => Some("6"),
        '&' => Some("7"),
        '*' => Some("8"),
        '(' => Some("9"),
        ')' => Some("0"),
        '_' => Some("-"),
        '+' => Some("="),
        '{' => Some("["),
        '}' => Some("]"),
        '|' => Some("\\"),
        ':' => Some(";"),
        '"' => Some("'"),
        '<' => Some(","),
        '>' => Some("."),
        '?' => Some("/"),
        '~' => Some("`"),
        _ => None,
    };
    if let Some(base) = shifted_base {
        return key_name_to_code(base).map(|c| (c, true));
    }

    // Everything else (lowercase letters, digits, and unshifted punctuation
    // like `- = [ ] ; ' \ , . / ` `) resolves directly.
    key_name_to_code(&ch.to_string()).map(|c| (c, false))
}

/// Type a string of text into the currently focused element.
///
/// This sends raw keyboard events for each character (US layout). For long
/// strings or non-ASCII text, prefer setting the clipboard and pasting.
pub fn type_text(text: &str) -> Result<()> {
    let shift = key_name_to_code("shift").expect("shift keycode is defined");
    for ch in text.chars() {
        let (code, needs_shift) = char_to_keystroke(ch)
            .ok_or_else(|| NovaError::Input(format!("cannot type character: {ch:?}")))?;
        if needs_shift {
            post_key(shift, true)?;
            thread::sleep(Duration::from_millis(1));
            tap_key(code)?;
            post_key(shift, false)?;
        } else {
            tap_key(code)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── key_name_to_code / is_modifier ──────────────────────────────

    #[test]
    fn key_name_to_code_is_case_insensitive_and_aliased() {
        assert_eq!(key_name_to_code("a"), Some(0));
        assert_eq!(key_name_to_code("A"), Some(0));
        assert_eq!(key_name_to_code("Return"), key_name_to_code("enter"));
        assert_eq!(key_name_to_code("esc"), key_name_to_code("escape"));
        assert_eq!(key_name_to_code("ctrl"), key_name_to_code("control"));
        assert_eq!(key_name_to_code("not_a_key"), None);
    }

    #[test]
    fn is_modifier_recognizes_modifier_aliases_only() {
        for m in [
            "cmd", "command", "shift", "option", "alt", "ctrl", "control",
        ] {
            assert!(is_modifier(m), "{m} should be a modifier");
        }
        for k in ["a", "enter", "f1", "space"] {
            assert!(!is_modifier(k), "{k} should not be a modifier");
        }
    }

    // ── parse_combo ─────────────────────────────────────────────────

    #[test]
    fn parse_combo_splits_modifiers_and_main_key() {
        let (mods, keys) = parse_combo("cmd+c").unwrap();
        assert_eq!(mods, vec![55]); // cmd
        assert_eq!(keys, vec![8]); // c
    }

    #[test]
    fn parse_combo_supports_multiple_modifiers() {
        let (mods, keys) = parse_combo("ctrl+cmd+f").unwrap();
        assert_eq!(mods, vec![59, 55]); // ctrl, cmd
        assert_eq!(keys, vec![3]); // f
    }

    #[test]
    fn parse_combo_is_whitespace_and_trailing_plus_tolerant() {
        let (mods, keys) = parse_combo(" shift + tab +").unwrap();
        assert_eq!(mods, vec![56]); // shift
        assert_eq!(keys, vec![48]); // tab
    }

    #[test]
    fn parse_combo_rejects_modifier_only() {
        let err = parse_combo("cmd").unwrap_err();
        assert!(err.to_string().contains("no main key"), "{err}");
    }

    #[test]
    fn parse_combo_rejects_unknown_key() {
        let err = parse_combo("cmd+nope").unwrap_err();
        assert!(err.to_string().contains("unknown key"), "{err}");
    }

    // ── char_to_keystroke ───────────────────────────────────────────

    #[test]
    fn char_to_keystroke_letters_and_case() {
        assert_eq!(char_to_keystroke('a'), Some((0, false)));
        assert_eq!(char_to_keystroke('A'), Some((0, true)));
    }

    #[test]
    fn char_to_keystroke_digits_unshifted() {
        assert_eq!(char_to_keystroke('1'), Some((18, false)));
        assert_eq!(char_to_keystroke('0'), Some((29, false)));
    }

    #[test]
    fn char_to_keystroke_whitespace() {
        assert_eq!(char_to_keystroke(' '), Some((49, false)));
        assert_eq!(char_to_keystroke('\n'), Some((36, false)));
        assert_eq!(char_to_keystroke('\t'), Some((48, false)));
    }

    #[test]
    fn char_to_keystroke_shifted_symbols() {
        // These are exactly the chars the old type_text could not produce.
        assert_eq!(char_to_keystroke('@'), Some((19, true))); // shift+2
        assert_eq!(char_to_keystroke('!'), Some((18, true))); // shift+1
        assert_eq!(char_to_keystroke(':'), Some((41, true))); // shift+;
        assert_eq!(char_to_keystroke('?'), Some((44, true))); // shift+/
        assert_eq!(char_to_keystroke('"'), Some((39, true))); // shift+'
        assert_eq!(char_to_keystroke('_'), Some((27, true))); // shift+-
    }

    #[test]
    fn char_to_keystroke_unshifted_punctuation() {
        assert_eq!(char_to_keystroke('-'), Some((27, false)));
        assert_eq!(char_to_keystroke('.'), Some((47, false)));
        assert_eq!(char_to_keystroke(';'), Some((41, false)));
        assert_eq!(char_to_keystroke('/'), Some((44, false)));
    }

    #[test]
    fn char_to_keystroke_covers_a_realistic_email() {
        // Regression guard: "user.name@example.com" must be fully typable.
        for ch in "user.name@example.com".chars() {
            assert!(char_to_keystroke(ch).is_some(), "cannot type {ch:?}");
        }
    }

    #[test]
    fn char_to_keystroke_rejects_non_ascii() {
        assert_eq!(char_to_keystroke('é'), None);
        assert_eq!(char_to_keystroke('中'), None);
        assert_eq!(char_to_keystroke('€'), None);
    }
}
