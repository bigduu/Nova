//! Set-of-Mark element discovery — Windows P2: Microsoft UI Automation
//! (`IUIAutomation`, the `windows::Win32::UI::Accessibility` bindings).
//!
//! This upgrades Windows nova from pure-coordinate clicking to the same
//! semantic, mark-numbered clicking macOS has always had: [`WinUiTree::collect_actionable`]
//! walks a live app's UI Automation tree for actionable controls, and
//! [`super::elements::handle::WinElementHandle`] (re-exported as the crate's
//! `ElementHandle` for this OS) drives a click straight through the control's
//! own Automation pattern (`Invoke`/`Toggle`/`SelectionItem`/`ExpandCollapse`)
//! — no cursor movement, works in the background, and — unlike macOS's
//! `AXPress` — fires real DOM click handlers on Chromium/WebView2-hosted web
//! content directly, with no browser-JS detour needed (see `handle.rs`'s
//! `try_web_click` doc).
//!
//! Module layout (mirrors `platform::mac::elements`'s low → high split):
//! - [`automation`] — thread-local COM/MTA join + `IUIAutomation` instance,
//!   `CacheRequest`/`Condition` builders, the pattern-availability ladder
//! - [`handle`]      — [`handle::WinElementHandle`], the live element handle
//! - [`discover`]    — the real `collect_actionable` (`FindAllBuildCache` walk)
//!   + the diagnostic `dump_tree`
//! - [`actions`]     — query-driven `ax_click`/`ax_set_value`/`ax_focus`
//!
//! [`UiElement`]/[`CachedElement`] below are unchanged from the P1 stub (plain
//! data, field-for-field identical to macOS's) — only [`WinUiTree`]'s methods
//! and [`handle::WinElementHandle`] are new.
mod actions;
pub(crate) mod automation;
mod discover;
mod handle;

pub use handle::WinElementHandle;

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

/// The Windows [`UiTree`]: Set-of-Mark discovery via UI Automation, forwarding
/// onto the `discover`/`actions` free functions — mirrors `mac::elements::MacUiTree`'s
/// thin-forwarder shape.
pub struct WinUiTree;

impl UiTree for WinUiTree {
    fn collect_actionable(
        &self,
        pid: i32,
        max: usize,
        clip: Option<(f64, f64, f64, f64)>,
    ) -> Vec<(UiElement, Box<dyn ElementHandle>)> {
        discover::collect_actionable(pid, max, clip)
    }

    fn ax_click(&self, pid: i32, query: &str) -> Result<String, String> {
        actions::ax_click(pid, query)
    }

    fn ax_set_value(&self, pid: i32, query: &str, value: &str) -> Result<String, String> {
        actions::ax_set_value(pid, query, value)
    }

    fn ax_focus(&self, pid: i32, query: &str) -> Result<String, String> {
        actions::ax_focus(pid, query)
    }

    fn raise_app(&self, pid: i32) {
        // Unchanged from the P1 stub: best-effort, reuses `WindowManager`'s
        // foreground logic. The bool (whether a window was actually raised)
        // is irrelevant to a best-effort pre-click raise, so it's discarded.
        let _ = crate::platform::windows::window::raise_pid(pid);
    }

    fn dump_tree(&self, pid: i32, max_nodes: usize) -> String {
        discover::dump_tree(pid, max_nodes)
    }

    /// No-op — see `discover::COLD_RETRY_ATTEMPTS`'s doc for the closest
    /// Windows analog. Unlike macOS's Chromium/WebKit AX bridge, which reaps
    /// its full semantic tree back to a geometry-only skeleton once nothing
    /// polls it (requiring `mac::elements::warmth::TreeWarmer`'s active
    /// keep-alive), UI Automation providers materialize their tree on the
    /// `WM_GETOBJECT` message a client's first UIA call already sends — there
    /// is no "warm" vs "cold-and-decaying" state to maintain BETWEEN captures,
    /// only a possible one-time delay on a provider's FIRST ever UIA query
    /// after launch, which `discover.rs`'s bounded retry-with-sleep already
    /// covers. Verified empirically against VS Code (Electron/Chromium) and
    /// Explorer in the VM — see the PR body's smoke-test evidence.
    fn keep_warm(&self, _pid: i32) {}

    /// See [`Self::keep_warm`] — nothing to clear.
    fn clear_warm(&self) {}
}
