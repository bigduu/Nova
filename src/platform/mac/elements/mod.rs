//! Set-of-Mark element discovery via the macOS Accessibility (AX) API.
//!
//! Walks an application's accessibility tree (and hit-tests the visible window)
//! to collect actionable UI elements — buttons, links, fields, list rows — with
//! their on-screen frames. The server draws numbered boxes on these and hands the
//! model a list with each element's center, so it can pick a target by its
//! labeled mark instead of estimating raw pixel coordinates — the most reliable
//! way to ground clicks.
//!
//! Requires the host process to have Accessibility permission; degrades to an
//! empty list otherwise (never errors).
//!
//! Module layout (low → high level):
//! - [`attrs`]   — raw AX attribute/action reads + geometry primitives (the FFI)
//! - [`model`]   — `UiElement`, the live `AxHandle`, `CachedElement`, targeting
//! - [`walk`]    — the bounded native-chrome tree walk
//! - [`hittest`] — the web-content hit-test pass
//! - [`warmth`]  — enabling + keeping Chromium's full tree warm ([`TreeWarmer`])
//! - [`discover`]— top-level `collect_actionable` combining walk + warm hit-test
//! - [`actions`] — query-driven `ax_click` / `ax_set_value` / `ax_focus`
//! - [`debug`]   — `--dump-ax` / `--hit-dump` / `--ax-warm` developer diagnostics
//! - [`webclick`]— background web-content clicking via browser JS (AppleScript)
//!
//! This module moved here (from `src/tools/elements/`) unchanged in substance —
//! every quirk/comment preserved, only its home and its trait wiring changed;
//! see the platform-abstraction move plan. [`MacUiTree`] and [`MacElementHandle`] at the bottom of
//! this file are the new pieces: thin forwarders onto the moved free
//! functions/methods, exactly like `MacOcrEngine` in `platform/mac/ocr.rs`.
//! `UiElement`/`CachedElement` stay reachable at the stable
//! `crate::tools::elements` path via a thin re-export there (see
//! `src/tools/elements.rs`) — `platform/mod.rs`'s `UiTree` trait references
//! `crate::tools::elements::UiElement` directly, and `CachedElement` is cached
//! server-side across a `marks` screenshot and a later `click_mark`.

mod attrs;
mod discover;
mod geometry;
mod hittest;
mod model;
mod walk;
mod warmth;

pub mod actions;
// `hit_dump`/`ax_warm_probe` are developer diagnostics only (not part of the
// `UiTree` trait / MCP tool surface) — `src/main.rs`'s `--hit-dump`/`--ax-warm`
// debug CLI calls them directly via this path, bypassing the trait, exactly
// like the capture agent's `--selftest` reaching `platform::mac::capture::*`
// straight. `dump_tree` (which IS a trait method) is also defined here but
// reached through `crate::platform::ui_tree().dump_tree(...)` everywhere else.
pub mod debug;
pub(crate) mod webclick;

// Public surface carried over unchanged from the old `tools/elements/mod.rs`
// — every real call site now goes through
// `crate::platform::ui_tree()` / the `crate::tools::elements` re-export shim
// below instead of these paths directly, but the re-exports stay so
// `actionable_elements`/`TreeWarmer::target` etc. (which have no in-crate
// caller today, same as before the move) don't flip from "reachable via a
// public path" to genuine dead code.
pub use actions::{ax_click, ax_focus, ax_set_value};
pub use discover::{actionable_elements, collect_actionable};
pub use model::{raise_app, web_area_origin, web_click_point, AxHandle, CachedElement, UiElement};
pub use warmth::{warmer, TreeWarmer};

use crate::platform::{ElementHandle, UiTree};

/// The macOS [`UiTree`]: Set-of-Mark discovery + query-driven AX actions,
/// forwarding onto the moved `discover`/`actions`/`debug`/`model`/`warmth`
/// free functions — none of their logic changed, only how it's reached.
pub struct MacUiTree;

impl UiTree for MacUiTree {
    fn collect_actionable(
        &self,
        pid: i32,
        max: usize,
        clip: Option<(f64, f64, f64, f64)>,
    ) -> Vec<(UiElement, Box<dyn ElementHandle>)> {
        discover::collect_actionable(pid, max, clip)
            .into_iter()
            .map(|(el, handle)| {
                (
                    el,
                    Box::new(MacElementHandle(handle)) as Box<dyn ElementHandle>,
                )
            })
            .collect()
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
        model::raise_app(pid)
    }

    fn dump_tree(&self, pid: i32, max_nodes: usize) -> String {
        debug::dump_tree(pid, max_nodes)
    }

    fn keep_warm(&self, pid: i32) {
        warmth::warmer().warm(pid)
    }

    fn clear_warm(&self) {
        warmth::warmer().clear()
    }
}

/// The macOS [`ElementHandle`]: an object-safe wrapper around [`model::AxHandle`].
///
/// A thin newtype rather than `impl ElementHandle for AxHandle` directly, so
/// `model.rs` (and its own hermetic tests) stays an unchanged move from
/// `tools/elements/model.rs`; the trait wiring — including the NEW
/// `try_web_click`, see below — lives here instead, alongside `MacUiTree`.
#[derive(Debug, Clone)]
struct MacElementHandle(model::AxHandle);

impl ElementHandle for MacElementHandle {
    fn click(&self) -> Result<&'static str, String> {
        self.0.click()
    }

    fn is_alive(&self) -> bool {
        self.0.is_alive()
    }

    fn current_center(&self) -> Option<(f64, f64)> {
        self.0.current_center()
    }

    /// Relocated from `server.rs::click_cached_mark`'s inline web-click branch
    /// — behavior is unchanged, just encapsulated behind
    /// the trait so the caller no longer needs to know this is web content in a
    /// scriptable browser. Gated on BOTH the element living under an
    /// `AXWebArea` (`model::web_click_point` returns `None` otherwise) AND the
    /// owning app being a scriptable browser (`webclick::browser_for_pid`), so
    /// native chrome and non-browser apps keep the reliable AX/coordinate path.
    fn try_web_click(&self, pid: i32, label: &str) -> Option<Result<String, String>> {
        // `px,py` is the element's center RELATIVE to its web area, read in raw
        // AX coords — not derived from a cached (possibly view-local-lifted)
        // mark center, which would aim the click off-page on WKWebView windows.
        let (px, py) = model::web_click_point(self.0.element())?;
        let browser = webclick::browser_for_pid(pid)?;
        match webclick::js_click_at(&browser, px, py, label) {
            Ok(desc) => Some(Ok(format!("{} in-page JS [{desc}]", browser.name()))),
            // JS unavailable (Automation / "allow JS from Apple Events" off) or
            // the point was empty — the caller falls through to AX, then the
            // coordinate path.
            Err(e) => Some(Err(e)),
        }
    }

    fn clone_box(&self) -> Box<dyn ElementHandle> {
        Box::new(self.clone())
    }
}
