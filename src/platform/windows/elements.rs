//! Set-of-Mark element discovery — Windows STUB (P1).
//!
//! Real UI-tree walking on Windows belongs to Microsoft UI Automation (the
//! `IUIAutomation` COM API) — a P2 scope (see the crate's Windows-port plan).
//! For P1, [`WinUiTree`] satisfies `crate::platform::UiTree` so the crate
//! (and every tool handler that calls `crate::platform::ui_tree()`) compiles
//! and links, but every method returns a clean "not yet implemented" error (or
//! the empty-results degrade the trait already documents for "no accessibility
//! tree") rather than faking success — screenshots on Windows simply come back
//! with `marks` empty, exactly like a macOS app with no Accessibility tree.
//!
//! [`UiElement`] and [`CachedElement`] ARE real types (not stubs): they are
//! the plain, platform-neutral value types `crate::tools::elements` re-exports
//! on this OS (mirroring `platform::mac::elements::{UiElement, CachedElement}`
//! exactly, field-for-field) — `platform::mod.rs`'s `UiTree` trait signature
//! and `capture/screenshot.rs`'s mark-building reference
//! `crate::tools::elements::{UiElement, CachedElement}` unconditionally (OS-
//! neutral code), so these must exist on every supported OS even while
//! `collect_actionable` itself never constructs one on Windows yet.
use crate::platform::{ElementHandle, UiTree};

/// An actionable UI element with its frame in global logical points.
/// Field-for-field identical to `platform::mac::elements::model::UiElement`.
#[derive(Debug, Clone)]
pub struct UiElement {
    pub role: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl UiElement {
    /// Center of the element in global logical points.
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// A marked actionable element kept for index-based clicking. Field-for-field
/// identical to `platform::mac::elements::model::CachedElement`.
#[derive(Debug, Clone)]
pub struct CachedElement {
    pub number: u32,
    pub handle: Box<dyn ElementHandle>,
    pub center: (f64, f64),
    pub role: String,
    pub label: String,
    pub pid: i32,
}

/// The Windows [`UiTree`] STUB: no UI Automation walk yet (P2). Every method
/// degrades cleanly rather than panicking or faking a result.
pub struct WinUiTree;

impl UiTree for WinUiTree {
    fn collect_actionable(
        &self,
        _pid: i32,
        _max: usize,
        _clip: Option<(f64, f64, f64, f64)>,
    ) -> Vec<(crate::tools::elements::UiElement, Box<dyn ElementHandle>)> {
        // Degrades exactly like a macOS app with no Accessibility tree: an
        // empty mark list, never an error (screenshot/marks callers already
        // handle "0 actionable elements" as a normal outcome).
        Vec::new()
    }

    fn ax_click(&self, _pid: i32, _query: &str) -> Result<String, String> {
        Err(
            "ax_click is not yet implemented on Windows (UI Automation tree walking is tracked \
             for a later phase); use left_click with coordinates from a screenshot instead"
                .to_string(),
        )
    }

    fn ax_set_value(&self, _pid: i32, _query: &str, _value: &str) -> Result<String, String> {
        Err(
            "ax_set_value is not yet implemented on Windows (UI Automation tree walking is \
             tracked for a later phase); click the field and use type_text instead"
                .to_string(),
        )
    }

    fn ax_focus(&self, _pid: i32, _query: &str) -> Result<String, String> {
        Err(
            "ax_focus is not yet implemented on Windows (UI Automation tree walking is tracked \
             for a later phase); use left_click with coordinates from a screenshot instead"
                .to_string(),
        )
    }

    fn raise_app(&self, pid: i32) {
        // Best-effort: reuse the WindowManager's foreground logic so the
        // coordinate-click fallback (server.rs::click_cached_mark) still has
        // SOMETHING to call even though marks are never produced on Windows
        // today (this only ever runs if a future caller manually constructs a
        // CachedElement) — cheap to wire up now, and free of new surface. The
        // bool (whether a window was actually raised) is irrelevant to a
        // best-effort pre-click raise, so it is intentionally discarded.
        let _ = crate::platform::windows::window::raise_pid(pid);
    }

    fn dump_tree(&self, _pid: i32, _max_nodes: usize) -> String {
        "UI Automation tree dump is not yet implemented on Windows (tracked for a later phase)"
            .to_string()
    }

    fn keep_warm(&self, _pid: i32) {
        // No warm-tree concept without a UI Automation walk yet — no-op.
    }

    fn clear_warm(&self) {
        // No warm-tree concept without a UI Automation walk yet — no-op.
    }
}
