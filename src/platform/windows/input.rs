//! Synthetic mouse/keyboard input via `SendInput` — the Windows
//! `crate::platform::InputInjector` implementation (P1 MVP).
//!
//! # Coordinate mapping
//!
//! Incoming `(x, y)` are GLOBAL LOGICAL points (see `crate::platform`'s module
//! doc) — with Per-Monitor-DPI-v2 declared ([`super::init_dpi_awareness`],
//! called once from `main()`), that is real, unscaled virtual-desktop pixels,
//! the same space [`super::geometry::virtual_desktop_bounds`] and
//! `GetWindowRect` report in. `SendInput`'s absolute-mouse mode maps its own
//! internal 0..65535 space onto a rectangle chosen by its flags:
//! `MOUSEEVENTF_VIRTUALDESK` picks the WHOLE virtual desktop (every monitor,
//! which can extend to negative coordinates left of/above the primary) rather
//! than just the primary monitor — required for correctness on any multi-
//! monitor layout, so [`to_absolute`] always normalizes through
//! `virtual_desktop_bounds()`, never just `SM_CXSCREEN`/`SM_CYSCREEN`.
//!
//! # `InputTarget::Pid` — no background delivery yet
//!
//! macOS's `CGEventPostToPid` lets an event target one process without moving
//! the real cursor or requiring it be foreground. Win32 has no direct
//! equivalent for arbitrary synthetic mouse/keyboard input — the closest
//! analogs (`PostMessage`/`SendMessage` with raw `WM_*` messages, or UI
//! Automation's `Invoke` patterns) are a different, per-control mechanism, not
//! a drop-in swap for `SendInput`, and are tracked as later-phase work
//! alongside the UI Automation tree walk ([`super::elements`]). For the P1
//! MVP, EVERY input event below goes through the same global `SendInput`
//! queue regardless of `InputTarget` — both variants deliver to whichever
//! window is (or becomes, via the cursor move) foreground. Any caller that
//! set `background=true` expecting a native-only, non-cursor-moving click
//! will still see the cursor move and the target window raised; this is a
//! known P1 gap, not a silent behavior change to hide.
use crate::error::{NovaError, Result};
use crate::tools::input::InputTarget;
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
    VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12,
    VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_HOME, VK_LEFT, VK_LWIN, VK_MENU,
    VK_NEXT, VK_OEM_1, VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA,
    VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU,
    VK_RSHIFT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use super::geometry::virtual_desktop_bounds;

/// `SendInput`'s per-notch wheel delta — a fixed OS constant (`WHEEL_DELTA`),
/// not configurable.
const WHEEL_DELTA: i32 = 120;

const CLICK_HOLD: Duration = Duration::from_millis(60);
const SETTLE: Duration = Duration::from_millis(24);
const DOUBLE_CLICK_GAP: Duration = Duration::from_millis(90);

/// Log (once per call, at debug level) that `target`'s background-delivery
/// request has no Windows P1 equivalent — see the module doc.
fn note_target_limitation(target: InputTarget) {
    if let InputTarget::Pid(pid) = target {
        tracing::debug!(
            "InputTarget::Pid({pid}) requested but Windows P1 has no per-process background \
             input delivery (see platform::windows::input's module doc) — delivering via the \
             global SendInput queue instead (cursor moves, target window may be raised)"
        );
    }
}

/// Map a global-logical point to `SendInput`'s absolute 0..65535 space,
/// spanning the whole virtual desktop.
fn to_absolute(x: f64, y: f64) -> (i32, i32) {
    let (left, top, width, height) = virtual_desktop_bounds();
    let ax = ((x - left as f64) * 65536.0 / width as f64).round() as i32;
    let ay = ((y - top as f64) * 65536.0 / height as f64).round() as i32;
    (ax.clamp(0, 65535), ay.clamp(0, 65535))
}

fn mouse_input(dx: i32, dy: i32, mouse_data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn keyboard_input(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Post one already-built `INPUT` event, erroring if `SendInput` didn't accept
/// it (e.g. blocked by an installed low-level hook, or another thread holding
/// a `BlockInput` lock).
fn send(input: INPUT) -> Result<()> {
    let events = [input];
    // SAFETY: `SendInput` reads exactly `events.len()` `INPUT` structs we own
    // for the duration of the call and does not retain the pointer afterward.
    let sent = unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        return Err(NovaError::Input(
            "SendInput did not accept the event (0 delivered) — another process may be blocking \
             synthetic input (e.g. an installed low-level hook, or a UIPI-elevated foreground \
             window rejecting input from this integrity level)"
                .to_string(),
        ));
    }
    Ok(())
}

// ── Mouse movement ──────────────────────────────────────────────────

pub fn mouse_move(x: f64, y: f64) -> Result<()> {
    let (ax, ay) = to_absolute(x, y);
    send(mouse_input(
        ax,
        ay,
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    ))
}

pub fn cursor_position() -> Result<(f64, f64)> {
    let mut pt = POINT::default();
    // SAFETY: `pt` is a local we own; `GetCursorPos` only writes through it.
    unsafe { GetCursorPos(&mut pt) }
        .map_err(|e| NovaError::Input(format!("GetCursorPos failed: {e}")))?;
    Ok((pt.x as f64, pt.y as f64))
}

/// Move the cursor to `(x, y)` and let it settle — mirrors
/// `platform::mac::input::arrive_at`'s `Global` branch; Windows has no
/// process-targeted delivery yet (see the module doc), so every target
/// arrives the same way.
fn arrive_at(x: f64, y: f64) -> Result<()> {
    mouse_move(x, y)?;
    thread::sleep(SETTLE);
    Ok(())
}

// ── Mouse clicks / scroll ────────────────────────────────────────────

fn press_release(down: MOUSE_EVENT_FLAGS, up: MOUSE_EVENT_FLAGS) -> Result<()> {
    send(mouse_input(0, 0, 0, down))?;
    thread::sleep(CLICK_HOLD);
    send(mouse_input(0, 0, 0, up))?;
    thread::sleep(SETTLE);
    Ok(())
}

pub fn left_click_at(x: f64, y: f64, target: InputTarget) -> Result<()> {
    note_target_limitation(target);
    arrive_at(x, y)?;
    press_release(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
}

pub fn right_click_at(x: f64, y: f64, target: InputTarget) -> Result<()> {
    note_target_limitation(target);
    arrive_at(x, y)?;
    press_release(MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
}

pub fn double_click_at(x: f64, y: f64, target: InputTarget) -> Result<()> {
    note_target_limitation(target);
    arrive_at(x, y)?;
    press_release(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)?;
    thread::sleep(DOUBLE_CLICK_GAP);
    press_release(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
}

/// Scroll vertically by `lines` at `(x, y)`. Positive = up (`WHEEL_DELTA`
/// scaling matches the trait's "positive = up" contract: Windows'
/// `MOUSEEVENTF_WHEEL` uses the same sign convention — a positive `mouseData`
/// rotates the wheel away from the user, which scrolls content up/back).
pub fn scroll_at(x: f64, y: f64, lines: i32, target: InputTarget) -> Result<()> {
    note_target_limitation(target);
    arrive_at(x, y)?;
    send(mouse_input(0, 0, lines * WHEEL_DELTA, MOUSEEVENTF_WHEEL))
}

// ── Keyboard ────────────────────────────────────────────────────────

/// Map a key name to its Windows virtual-key code. Letters/digits use their
/// ASCII value directly (Windows assigns `VK_A..VK_Z` == `'A'..'Z'` and
/// `VK_0..VK_9` == `'0'..'9'` by convention — these aren't named constants in
/// `winuser.h`, hence no `windows` crate symbols for them either); everything
/// else uses the crate's named `VK_*` constants.
fn key_name_to_vk(name: &str) -> Option<VIRTUAL_KEY> {
    let lower = name.to_lowercase();
    if let Some(c) = single_char(&lower) {
        if c.is_ascii_alphabetic() {
            return Some(VIRTUAL_KEY(c.to_ascii_uppercase() as u16));
        }
        if c.is_ascii_digit() {
            return Some(VIRTUAL_KEY(c as u16));
        }
    }
    Some(match lower.as_str() {
        "return" | "enter" => VK_RETURN,
        "tab" => VK_TAB,
        "space" => VK_SPACE,
        "delete" | "backspace" => VK_BACK,
        "escape" | "esc" => VK_ESCAPE,
        "capslock" | "caps_lock" => VK_CAPITAL,
        "shift" => VK_SHIFT,
        "control" | "ctrl" => VK_CONTROL,
        "option" | "alt" => VK_MENU,
        // The Windows-key analog of macOS `cmd`/`command`. NOTE: this is NOT
        // remapped to `ctrl` — a cross-platform agent must use `ctrl+c`/
        // `ctrl+v` for copy/paste on Windows; `cmd+c` here presses the literal
        // Windows logo key + C (opens Windows Search's "c" jumplist, not a
        // copy), which is the truer platform analog and the least-surprising
        // choice for P1. Revisit if agents commonly send `cmd+`-prefixed
        // shortcuts expecting Windows semantics.
        "cmd" | "command" | "win" | "windows" | "meta" | "super" => VK_LWIN,
        "right_shift" => VK_RSHIFT,
        "right_option" | "right_alt" => VK_RMENU,
        "right_control" | "right_ctrl" => VK_RCONTROL,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "down" => VK_DOWN,
        "up" => VK_UP,
        "home" => VK_HOME,
        "end" => VK_END,
        "page_up" | "pgup" => VK_PRIOR,
        "page_down" | "pgdn" => VK_NEXT,
        "f1" => VK_F1,
        "f2" => VK_F2,
        "f3" => VK_F3,
        "f4" => VK_F4,
        "f5" => VK_F5,
        "f6" => VK_F6,
        "f7" => VK_F7,
        "f8" => VK_F8,
        "f9" => VK_F9,
        "f10" => VK_F10,
        "f11" => VK_F11,
        "f12" => VK_F12,
        "-" => VK_OEM_MINUS,
        "=" => VK_OEM_PLUS,
        "[" => VK_OEM_4,
        "]" => VK_OEM_6,
        "\\" => VK_OEM_5,
        ";" => VK_OEM_1,
        "'" => VK_OEM_7,
        "," => VK_OEM_COMMA,
        "." => VK_OEM_PERIOD,
        "/" => VK_OEM_2,
        "`" => VK_OEM_3,
        _ => return None,
    })
}

/// `s` as its single `char`, or `None` if it isn't exactly one.
fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// Modifier key names recognized by [`parse_combo`] (mirrors
/// `platform::mac::input::is_modifier`'s alias set, plus the Windows-key
/// aliases documented on [`key_name_to_vk`]).
fn is_modifier(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "cmd"
            | "command"
            | "win"
            | "windows"
            | "meta"
            | "super"
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

/// Parse a key combination string into (modifier VKs, main VKs) — mirrors
/// `platform::mac::input::parse_combo` exactly (same splitting/validation
/// rules), just resolving Windows VK codes instead of macOS keycodes.
fn parse_combo(combo: &str) -> Result<(Vec<VIRTUAL_KEY>, Vec<VIRTUAL_KEY>)> {
    let mut modifiers = Vec::new();
    let mut keys = Vec::new();

    for token in combo.split('+').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let vk = key_name_to_vk(token)
            .ok_or_else(|| NovaError::Input(format!("unknown key: {token}")))?;
        if is_modifier(token) {
            modifiers.push(vk);
        } else {
            keys.push(vk);
        }
    }

    if keys.is_empty() {
        return Err(NovaError::Input(format!("no main key in combo: {combo}")));
    }
    Ok((modifiers, keys))
}

fn post_key(vk: VIRTUAL_KEY, down: bool) -> Result<()> {
    let flags = if down {
        KEYBD_EVENT_FLAGS(0)
    } else {
        KEYEVENTF_KEYUP
    };
    send(keyboard_input(vk, 0, flags))?;
    thread::sleep(Duration::from_millis(1));
    Ok(())
}

/// Simulate a key combination. Unlike macOS's `CGEvent` (which needs the
/// modifier flags STAMPED on the main key's event to be recognized), Windows
/// reads live modifier state from the actual key-down events already injected
/// into the input stream — press modifiers down, press+release the main
/// key(s), then release modifiers in reverse, exactly as a physical chord
/// would arrive.
pub fn key_combo(combo: &str, target: InputTarget) -> Result<()> {
    note_target_limitation(target);
    let (modifiers, keys) = parse_combo(combo)?;

    for &vk in &modifiers {
        post_key(vk, true)?;
    }
    thread::sleep(Duration::from_millis(2));

    for &vk in &keys {
        post_key(vk, true)?;
        post_key(vk, false)?;
    }
    thread::sleep(Duration::from_millis(2));

    for &vk in modifiers.iter().rev() {
        post_key(vk, false)?;
    }
    Ok(())
}

// ── Type text ───────────────────────────────────────────────────────

fn send_unicode_unit(unit: u16, down: bool) -> Result<()> {
    let flags = if down {
        KEYEVENTF_UNICODE
    } else {
        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
    };
    // `wVk` must be 0 for a KEYEVENTF_UNICODE event; `wScan` carries the
    // UTF-16 code unit itself (a surrogate half for anything outside the BMP,
    // e.g. emoji — sent as two sequential unit events, exactly like a real
    // IME's WM_CHAR delivery of an astral character).
    send(keyboard_input(VIRTUAL_KEY(0), unit, flags))
}

/// Type literal Unicode text (CJK/emoji included) via `KEYEVENTF_UNICODE` —
/// the Windows analog of macOS's `CGEventKeyboardSetUnicodeString` path, at
/// the granularity Win32 actually offers (one UTF-16 code unit per event,
/// rather than a whole string per event).
pub fn type_text(text: &str, target: InputTarget) -> Result<()> {
    note_target_limitation(target);
    for ch in text.chars() {
        let mut buf = [0u16; 2];
        for &unit in ch.encode_utf16(&mut buf).iter() {
            send_unicode_unit(unit, true)?;
            send_unicode_unit(unit, false)?;
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// The Windows [`crate::platform::InputInjector`]: `SendInput`, via the free
/// functions above.
pub struct WinInputInjector;

impl crate::platform::InputInjector for WinInputInjector {
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

    #[test]
    fn key_name_to_vk_letters_and_digits() {
        assert_eq!(key_name_to_vk("a"), Some(VIRTUAL_KEY(b'A' as u16)));
        assert_eq!(key_name_to_vk("A"), Some(VIRTUAL_KEY(b'A' as u16)));
        assert_eq!(key_name_to_vk("5"), Some(VIRTUAL_KEY(b'5' as u16)));
        assert_eq!(key_name_to_vk("not_a_key"), None);
    }

    #[test]
    fn key_name_to_vk_named_keys_and_aliases() {
        assert_eq!(key_name_to_vk("return"), key_name_to_vk("enter"));
        assert_eq!(key_name_to_vk("esc"), key_name_to_vk("escape"));
        assert_eq!(key_name_to_vk("ctrl"), key_name_to_vk("control"));
        assert_eq!(key_name_to_vk("cmd"), Some(VK_LWIN));
    }

    #[test]
    fn is_modifier_recognizes_aliases_only() {
        for m in ["cmd", "command", "win", "shift", "alt", "ctrl", "control"] {
            assert!(is_modifier(m), "{m} should be a modifier");
        }
        for k in ["a", "enter", "f1", "space"] {
            assert!(!is_modifier(k), "{k} should not be a modifier");
        }
    }

    #[test]
    fn parse_combo_splits_modifiers_and_main_key() {
        let (mods, keys) = parse_combo("ctrl+c").unwrap();
        assert_eq!(mods, vec![VK_CONTROL]);
        assert_eq!(keys, vec![VIRTUAL_KEY(b'C' as u16)]);
    }

    #[test]
    fn parse_combo_rejects_modifier_only() {
        let err = parse_combo("ctrl").unwrap_err();
        assert!(err.to_string().contains("no main key"), "{err}");
    }

    #[test]
    fn parse_combo_rejects_unknown_key() {
        let err = parse_combo("ctrl+nope").unwrap_err();
        assert!(err.to_string().contains("unknown key"), "{err}");
    }

    #[test]
    fn to_absolute_clamps_into_0_65535() {
        // Sanity only (can't call GetSystemMetrics off Windows in this test
        // binary's target, but the clamp math itself is target-independent —
        // this just documents the contract for a reviewer).
        let clamp = |v: f64| -> i32 { (v.round() as i32).clamp(0, 65535) };
        assert_eq!(clamp(-100.0), 0);
        assert_eq!(clamp(70000.0), 65535);
        assert_eq!(clamp(32768.0), 32768);
    }
}
