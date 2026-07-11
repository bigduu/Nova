//! Input injection via CoreGraphics `CGEvent` — the macOS
//! [`crate::platform::InputInjector`] implementation.
//!
//! All coordinates are in macOS logical points.
//!
//! Events can be delivered two ways (see [`InputTarget`]):
//! - `Global`: posted to the HID event stream — the OS routes it to the
//!   frontmost app and the real cursor moves. Works anywhere but needs the
//!   target foreground and takes over the user's mouse/keyboard.
//! - `Pid`: posted directly to a process via `CGEventPostToPid` — the global
//!   cursor is not moved and the app usually need not be frontmost (as close to
//!   "background" input as macOS allows). Apps that do their own event handling
//!   may ignore it, so it is best-effort for those.
//!
//! This is a MOVE from the old `src/tools/input.rs` (part of the
//! platform-abstraction split — see PARALLEL_PLAN.md): the mechanics below are
//! unchanged in substance (every timing constant, quirk, and comment
//! preserved); only the delivery target enum ([`InputTarget`]) stayed behind in
//! `crate::tools::input`, since `src/tools/batch.rs` and `src/server.rs` name
//! it without needing anything platform-specific.
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use std::thread;
use std::time::Duration;

use crate::error::{NovaError, Result};
use crate::tools::input::InputTarget;

// ── Event source & delivery target ──────────────────────────────────

/// Create a private event source. Using `Private` state means the events
/// are treated as if they came from a physical device.
fn event_source() -> Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| NovaError::Input("failed to create event source".into()))
}

/// Post an already-built event to the chosen target.
fn post_event(event: &CGEvent, target: InputTarget) {
    match target {
        InputTarget::Global => event.post(CGEventTapLocation::HID),
        InputTarget::Pid(pid) => event.post_to_pid(pid),
    }
}

// ── Timing ──────────────────────────────────────────────────────────
//
// Synthetic input that fires too fast is unreliable: a sub-millisecond
// down→up is often dropped or not seen as a real click, hover states never
// update, and a double-click whose two presses are nearly instantaneous is not
// recognized. These intervals are deliberately human-scale.

/// How long a click holds the button down (down→up). ~1ms is too fast for many
/// controls (web/Electron especially) to register; this is a human-scale press.
const CLICK_HOLD: Duration = Duration::from_millis(60);
/// Settle time after the cursor arrives (before pressing) and after releasing,
/// so the receiving app processes each phase before the next.
const SETTLE: Duration = Duration::from_millis(24);
/// Gap between the two presses of a double-click. Must be under the system
/// double-click interval (~250ms default) but long enough to be two distinct
/// presses.
const DOUBLE_CLICK_GAP: Duration = Duration::from_millis(90);
/// Delay between interpolated steps of a cursor move (≈ a smooth glide).
const MOVE_STEP_DELAY: Duration = Duration::from_millis(6);

// ── Mouse movement ──────────────────────────────────────────────────

/// Move the real (global) mouse cursor to `(x, y)` along a short interpolated
/// path, posting `mouseMoved` events at each step so hover states update — a
/// bare warp jumps the cursor but posts no movement, leaving hover-driven UI
/// (buttons, menus, web `:hover`) cold. Ends exactly on the target.
///
/// Used only for `Global` delivery; `Pid` clicks carry their own location and
/// deliberately leave the user's cursor where it is.
pub fn mouse_move(x: f64, y: f64) -> Result<()> {
    // Start from the current cursor; if we can't read it, jump straight there.
    let (sx, sy) = cursor_position().unwrap_or((x, y));
    let dist = (x - sx).hypot(y - sy);
    // ~1 step per 40px so longer moves glide rather than jump; clamp the count.
    let steps = ((dist / 40.0).ceil() as i32).clamp(1, 24);

    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let pt = CGPoint {
            x: sx + (x - sx) * t,
            y: sy + (y - sy) * t,
        };
        if let Ok(ev) = make_mouse_event(CGEventType::MouseMoved, CGMouseButton::Left, pt, None) {
            ev.post(CGEventTapLocation::HID);
        }
        thread::sleep(MOVE_STEP_DELAY);
    }

    // Snap to the exact target so the final position is pixel-accurate.
    CGDisplay::warp_mouse_cursor_position(CGPoint { x, y })
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

/// Build a mouse event at `pos`, optionally stamping the click-state field
/// (1 = single, 2 = the second press of a double-click). macOS double-click
/// recognition reads this field, so it must be set for an app to see a real
/// double-click rather than two separate single clicks.
fn make_mouse_event(
    event_type: CGEventType,
    button: CGMouseButton,
    pos: CGPoint,
    click_state: Option<i64>,
) -> Result<CGEvent> {
    let source = event_source()?;
    let event = CGEvent::new_mouse_event(source, event_type, pos, button)
        .map_err(|_| NovaError::Input("failed to create mouse event".into()))?;
    if let Some(state) = click_state {
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, state);
    }
    Ok(event)
}

/// Post a full press→release of `button` at `pos`: hold for [`CLICK_HOLD`] so
/// the control registers it, stamping `click_state` on both phases, then settle.
fn press_release(
    down: CGEventType,
    up: CGEventType,
    button: CGMouseButton,
    pos: CGPoint,
    target: InputTarget,
    click_state: i64,
) -> Result<()> {
    let d = make_mouse_event(down, button, pos, Some(click_state))?;
    post_event(&d, target);
    thread::sleep(CLICK_HOLD);
    let u = make_mouse_event(up, button, pos, Some(click_state))?;
    post_event(&u, target);
    thread::sleep(SETTLE);
    Ok(())
}

/// For `Global` delivery, glide the real cursor to `(x, y)` (so hover updates
/// and the click lands under the pointer) and let it settle. For a `Pid`
/// target, leave the cursor alone — the click event carries its own location.
fn arrive_at(x: f64, y: f64, target: InputTarget) -> Result<()> {
    if target.is_global() {
        mouse_move(x, y)?;
        thread::sleep(SETTLE);
    }
    Ok(())
}

/// Left click at specific coordinates, delivered to `target`.
pub fn left_click_at(x: f64, y: f64, target: InputTarget) -> Result<()> {
    let pos = CGPoint { x, y };
    arrive_at(x, y, target)?;
    press_release(
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGMouseButton::Left,
        pos,
        target,
        1,
    )
}

/// Right click at specific coordinates, delivered to `target`.
pub fn right_click_at(x: f64, y: f64, target: InputTarget) -> Result<()> {
    let pos = CGPoint { x, y };
    arrive_at(x, y, target)?;
    press_release(
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGMouseButton::Right,
        pos,
        target,
        1,
    )
}

/// Double click at specific coordinates, delivered to `target`. The two presses
/// carry click-state 1 then 2 — the field macOS uses to recognize a double
/// click — and are spaced under the system double-click interval.
pub fn double_click_at(x: f64, y: f64, target: InputTarget) -> Result<()> {
    let pos = CGPoint { x, y };
    arrive_at(x, y, target)?;
    press_release(
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGMouseButton::Left,
        pos,
        target,
        1,
    )?;
    thread::sleep(DOUBLE_CLICK_GAP);
    press_release(
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGMouseButton::Left,
        pos,
        target,
        2,
    )
}

// ── Scrolling ───────────────────────────────────────────────────────

/// Scroll vertically by `lines` at logical position `(x, y)`, delivered to
/// `target`. Positive lines = up (content moves up), negative = down.
///
/// For `Global`, the cursor is moved to `(x, y)` first so the scroll lands on
/// the intended view. For `Pid`, the event's location is set to `(x, y)` as a
/// hint (the app scrolls the view there / its focused scroller) without moving
/// the user's cursor.
pub fn scroll_at(x: f64, y: f64, lines: i32, target: InputTarget) -> Result<()> {
    arrive_at(x, y, target)?;
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
    if let InputTarget::Pid(_) = target {
        event.set_location(CGPoint { x, y });
    }
    post_event(&event, target);
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

/// The `CGEventFlags` bit a modifier keycode contributes. Synthesized key events
/// do NOT inherit a global modifier state, so for an app to recognize e.g.
/// `cmd+c` the COMMAND flag must be set on the `c` event itself — posting a bare
/// keycode-55 (command) key-down does not do that. `key_combo` ORs these together
/// and stamps them on every event it posts.
fn modifier_flag(keycode: u16) -> CGEventFlags {
    match keycode {
        55 => CGEventFlags::CGEventFlagCommand,
        56 | 60 => CGEventFlags::CGEventFlagShift, // shift, right_shift
        58 | 61 => CGEventFlags::CGEventFlagAlternate, // option, right_option
        59 | 62 => CGEventFlags::CGEventFlagControl, // control, right_control
        63 => CGEventFlags::CGEventFlagSecondaryFn, // fn
        _ => CGEventFlags::empty(),
    }
}

/// Post a key down or up event to `target`, with `flags` stamped on it (the
/// active modifier bits the receiving app reads to recognize the chord).
fn post_key(keycode: u16, keydown: bool, flags: CGEventFlags, target: InputTarget) -> Result<()> {
    let source = event_source()?;
    let event = CGEvent::new_keyboard_event(source, keycode, keydown)
        .map_err(|_| NovaError::Input("failed to create keyboard event".into()))?;
    event.set_flags(flags);
    post_event(&event, target);
    thread::sleep(Duration::from_millis(1));
    Ok(())
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

/// Simulate a key combination (e.g., "cmd+c", "shift+tab", "ctrl+cmd+f")
/// delivered to `target`.
pub fn key_combo(combo: &str, target: InputTarget) -> Result<()> {
    let (modifiers, keys) = parse_combo(combo)?;

    // The cumulative modifier flag set. This — not the bare modifier key-down
    // events — is what makes an app see `cmd+c` rather than a plain `c`.
    let flags = modifiers
        .iter()
        .fold(CGEventFlags::empty(), |acc, &c| acc | modifier_flag(c));

    // Press modifiers (each carries the cumulative flags so the held state is
    // coherent as more modifiers go down).
    for &code in &modifiers {
        post_key(code, true, flags, target)?;
    }
    thread::sleep(Duration::from_millis(2));

    // Press+release each main key WITH the modifier flags stamped on it.
    for &code in &keys {
        post_key(code, true, flags, target)?;
        post_key(code, false, flags, target)?;
    }
    thread::sleep(Duration::from_millis(2));

    // Release modifiers (reverse order), flags clearing back to empty.
    for &code in modifiers.iter().rev() {
        post_key(code, false, CGEventFlags::empty(), target)?;
    }

    Ok(())
}

// ── Type text ───────────────────────────────────────────────────────

/// Type a string into the focused element of `target`, supporting ANY Unicode
/// (including CJK / emoji) via `CGEventKeyboardSetUnicodeString`.
///
/// Unlike a keycode-based approach, this carries the literal character payload
/// on each key event, so it does not depend on the active keyboard layout and
/// can produce characters (e.g. 中文) that have no key on a US layout. Each
/// character is sent as its own key-down/key-up pair for broad app compatibility.
pub fn type_text(text: &str, target: InputTarget) -> Result<()> {
    for ch in text.chars() {
        let mut buf = [0u16; 2];
        let utf16: &[u16] = ch.encode_utf16(&mut buf);

        let down = CGEvent::new_keyboard_event(event_source()?, 0, true)
            .map_err(|_| NovaError::Input("failed to create keyboard event".into()))?;
        down.set_string_from_utf16_unchecked(utf16);
        post_event(&down, target);

        let up = CGEvent::new_keyboard_event(event_source()?, 0, false)
            .map_err(|_| NovaError::Input("failed to create keyboard event".into()))?;
        up.set_string_from_utf16_unchecked(utf16);
        post_event(&up, target);

        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// The macOS [`crate::platform::InputInjector`]: CoreGraphics `CGEvent`
/// posting, via the free functions above (unchanged from the pre-move
/// `tools::input` free functions — this is a thin forwarder, same shape as
/// `MacOcrEngine` in `platform/mac/ocr.rs`).
pub struct MacInputInjector;

impl crate::platform::InputInjector for MacInputInjector {
    fn mouse_move(&self, x: f64, y: f64) -> Result<()> {
        mouse_move(x, y)
    }

    fn cursor_position(&self) -> Result<(f64, f64)> {
        cursor_position()
    }

    fn left_click_at(&self, x: f64, y: f64, target: InputTarget) -> Result<()> {
        left_click_at(x, y, target)
    }

    fn right_click_at(&self, x: f64, y: f64, target: InputTarget) -> Result<()> {
        right_click_at(x, y, target)
    }

    fn double_click_at(&self, x: f64, y: f64, target: InputTarget) -> Result<()> {
        double_click_at(x, y, target)
    }

    fn scroll_at(&self, x: f64, y: f64, lines: i32, target: InputTarget) -> Result<()> {
        scroll_at(x, y, lines, target)
    }

    fn key_combo(&self, combo: &str, target: InputTarget) -> Result<()> {
        key_combo(combo, target)
    }

    fn type_text(&self, text: &str, target: InputTarget) -> Result<()> {
        type_text(text, target)
    }
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

    // ── modifier flags ──────────────────────────────────────────────

    #[test]
    fn modifier_flag_maps_modifier_keycodes() {
        // The chord only works if the modifier keycode contributes its flag —
        // this is the bit that was missing (bare keycodes set no flag).
        assert_eq!(modifier_flag(55), CGEventFlags::CGEventFlagCommand);
        assert_eq!(modifier_flag(59), CGEventFlags::CGEventFlagControl);
        assert_eq!(modifier_flag(60), CGEventFlags::CGEventFlagShift); // right_shift
        assert_eq!(modifier_flag(58), CGEventFlags::CGEventFlagAlternate);
        // A non-modifier (main) key contributes nothing.
        assert_eq!(modifier_flag(8), CGEventFlags::empty()); // 'c'
    }

    #[test]
    fn parsed_combo_folds_into_expected_flags() {
        let (mods, _keys) = parse_combo("ctrl+cmd+f").unwrap();
        let flags = mods
            .iter()
            .fold(CGEventFlags::empty(), |acc, &c| acc | modifier_flag(c));
        assert!(flags.contains(CGEventFlags::CGEventFlagCommand));
        assert!(flags.contains(CGEventFlags::CGEventFlagControl));
        assert!(!flags.contains(CGEventFlags::CGEventFlagShift));
    }
}
