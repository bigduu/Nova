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
mod semantic;
mod target;
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

use crate::platform::{
    ElementHandle, UiReadError, UiSnapshot, UiSnapshotOptions, UiTarget, UiTree,
};

/// The macOS [`UiTree`]: Set-of-Mark discovery + query-driven AX actions,
/// forwarding onto the moved `discover`/`actions`/`debug`/`model`/`warmth`
/// free functions — none of their logic changed, only how it's reached.
pub struct MacUiTree;

impl UiTree for MacUiTree {
    fn resolve_target(
        &self,
        query: Option<&str>,
        preferred_pid: Option<i32>,
        deadline: std::time::Instant,
    ) -> Result<UiTarget, UiReadError> {
        target::resolve_target(query, preferred_pid, deadline)
    }

    fn read_snapshot(
        &self,
        target: &UiTarget,
        options: UiSnapshotOptions,
    ) -> Result<UiSnapshot, UiReadError> {
        let (mut snapshot, handles) = semantic::read_snapshot(target, options)?;
        for (collected, handle) in snapshot.nodes.iter_mut().zip(handles) {
            if let Some((handle, anchor)) = handle {
                collected.handle = Some(Box::new(MacElementHandle::for_semantic(handle, anchor)));
            }
        }
        Ok(snapshot)
    }

    fn collect_actionable(
        &self,
        pid: i32,
        max: usize,
        clip: Option<(f64, f64, f64, f64)>,
    ) -> Vec<(UiElement, Box<dyn ElementHandle>)> {
        discover::collect_actionable(pid, max, clip)
            .into_iter()
            .map(|(el, handle)| {
                let desired_center = Some(el.center());
                (
                    el,
                    Box::new(MacElementHandle::with_center(handle, desired_center))
                        as Box<dyn ElementHandle>,
                )
            })
            .collect()
    }

    fn ax_click(
        &self,
        pid: i32,
        query: &str,
        deadline: std::time::Instant,
    ) -> Result<String, String> {
        actions::ax_click(pid, query, deadline)
    }

    fn ax_set_value(
        &self,
        pid: i32,
        query: &str,
        value: &str,
        deadline: std::time::Instant,
    ) -> Result<String, String> {
        actions::ax_set_value(pid, query, value, deadline)
    }

    fn ax_focus(
        &self,
        pid: i32,
        query: &str,
        deadline: std::time::Instant,
    ) -> Result<String, String> {
        actions::ax_focus(pid, query, deadline)
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
struct MacElementHandle {
    inner: model::AxHandle,
    center_basis: CenterBasis,
}

#[derive(Debug, Clone)]
enum CenterBasis {
    /// Legacy screenshot marks already have an independent capture frame.
    StaticOffset((f64, f64)),
    /// Semantic reads recompute the global anchor at action time so moving a
    /// WebKit window cannot leave a stale offset behind.
    WindowAnchor(semantic::SemanticAnchor),
    /// No independent global anchor: semantic activation is still allowed,
    /// but coordinate fallback fails closed.
    Unavailable,
}

impl MacElementHandle {
    fn for_semantic(handle: model::AxHandle, anchor: Option<semantic::SemanticAnchor>) -> Self {
        Self {
            inner: handle,
            center_basis: anchor
                .map(CenterBasis::WindowAnchor)
                .unwrap_or(CenterBasis::Unavailable),
        }
    }

    fn with_center(handle: model::AxHandle, desired_center: Option<(f64, f64)>) -> Self {
        let offset = match (desired_center, handle.current_center()) {
            (Some(desired), Some(raw)) => (desired.0 - raw.0, desired.1 - raw.1),
            _ => (0.0, 0.0),
        };
        Self {
            inner: handle,
            center_basis: CenterBasis::StaticOffset(offset),
        }
    }
}

impl ElementHandle for MacElementHandle {
    fn prepare_for_action(&self, deadline: std::time::Instant) -> Result<(), String> {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or_else(|| "AX action deadline elapsed".to_string())?;
        let timeout = remaining.as_secs_f32().clamp(0.05, 0.5);
        if let Some(pid) = attrs::ax_pid(self.inner.element()) {
            accessibility::AXUIElement::application(pid)
                .set_messaging_timeout(timeout)
                .map_err(|error| format!("failed to configure AX action timeout: {error:?}"))?;
        }
        if let CenterBasis::WindowAnchor(anchor) = &self.center_basis {
            let _ = anchor.window.element().set_messaging_timeout(timeout);
        }
        Ok(())
    }

    fn click(&self) -> Result<&'static str, String> {
        self.inner.click()
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive()
    }

    fn current_center(&self) -> Option<(f64, f64)> {
        match &self.center_basis {
            CenterBasis::StaticOffset(offset) => self
                .inner
                .current_center()
                .map(|center| (center.0 + offset.0, center.1 + offset.1)),
            CenterBasis::WindowAnchor(anchor) => {
                let global = target::global_window_bounds(anchor.window_id)?;
                let lift = walk::CoordLift::derive(
                    anchor.window.element(),
                    global.as_tuple(),
                    Some(anchor.window_id),
                )?;
                let rect = attrs::element_rect(self.inner.element())?;
                let rect = walk::CoordLift::lift(Some(lift), rect);
                Some((rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0))
            }
            CenterBasis::Unavailable => None,
        }
    }

    /// Relocated from `server.rs::click_cached_mark`'s inline web-click branch
    /// — behavior is unchanged, just encapsulated behind
    /// the trait so the caller no longer needs to know this is web content in a
    /// scriptable browser. Gated on BOTH the element living under an
    /// `AXWebArea` (`model::web_click_point` returns `None` otherwise) AND the
    /// owning app being a scriptable browser (`webclick::browser_for_pid`), so
    /// native chrome and non-browser apps keep the reliable AX/coordinate path.
    fn try_web_click(
        &self,
        pid: i32,
        label: &str,
        deadline: std::time::Instant,
    ) -> Option<Result<String, String>> {
        // `px,py` is the element's center RELATIVE to its web area, read in raw
        // AX coords — not derived from a cached (possibly view-local-lifted)
        // mark center, which would aim the click off-page on WKWebView windows.
        let (px, py) = model::web_click_point(self.inner.element())?;
        let browser = webclick::browser_for_pid(pid)?;
        match webclick::js_click_at(&browser, px, py, label, deadline) {
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
