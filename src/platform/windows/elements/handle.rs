//! [`WinElementHandle`] — Windows' `crate::platform::ElementHandle`: a
//! `Send`+`Clone`-able wrapper around a live `IUIAutomationElement`, driving
//! clicks through the UI Automation pattern ladder. The direct analog of
//! macOS's `mac::elements::model::AxHandle`/`AXPress`, with `Invoke` playing
//! the role `AXPress` plays there.

use super::automation::{
    build_actionable_condition, configure_deadline, ensure_com_mta, invoke_pattern, pattern_for,
};
use crate::platform::ElementHandle;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, TreeScope_Descendants, UIA_CONTROLTYPE_ID,
};

/// How far up to look for a click-ish ancestor when the element itself AND
/// its descendants (see [`find_actionable_descendant`]) expose no pattern —
/// mirrors macOS `model::ANCESTOR_CLICK_DEPTH`.
const ANCESTOR_CLICK_DEPTH: usize = 4;

/// A live UI Automation element, kept across a `marks` screenshot and a later
/// `click_mark`. `role`/`label` are copies captured once at discovery time,
/// used ONLY for a cheap, infallible [`Debug`] impl — every real trait method
/// below re-reads the LIVE element, never these cached strings.
pub struct WinElementHandle {
    automation: IUIAutomation,
    element: IUIAutomationElement,
    role: String,
    label: String,
}

// SAFETY: `IUIAutomationElement` is a COM interface pointer living in the
// process's single Multi-Threaded Apartment. Every entry point that touches
// one — `WinUiTree::collect_actionable` (which constructs these) and every
// method below — calls `ensure_com_mta()` first, joining that SAME MTA on
// whichever tokio blocking-pool thread happens to run it. Within one MTA, COM
// permits direct, unmarshaled use of an interface pointer from any thread
// that has also joined it (no proxy/stub hop, unlike moving a pointer between
// an MTA and an STA) — so a handle discovered on one blocking-pool thread and
// clicked later on a different one (exactly what `server.rs::click_cached_mark`
// does, both ends wrapped in `spawn_blocking`) is sound. This mirrors the
// reasoning macOS's `AxHandle` documents for its own unsafe `Send` impl over
// a Core Foundation object.
unsafe impl Send for WinElementHandle {}

impl Clone for WinElementHandle {
    fn clone(&self) -> Self {
        WinElementHandle {
            automation: self.automation.clone(),
            // Cloning a windows-rs interface wrapper is an `AddRef`, not a
            // raw-pointer copy — always sound, any thread. Its mirror, the
            // implicit `Drop` (a COM `Release`), is likewise apartment-
            // agnostic: `AddRef`/`Release` just adjust a refcount and never
            // marshal across an apartment boundary, so dropping a handle on a
            // thread that never joined the MTA is sound — which matters
            // because `server.rs`'s `set_marks`/`cache.clear()` releases
            // cached handles from a plain async worker that never calls
            // `ensure_com_mta()`. (Only the actual UIA METHOD calls —
            // click/is_alive/current_center — require the MTA join, and each
            // does it itself.)
            element: self.element.clone(),
            role: self.role.clone(),
            label: self.label.clone(),
        }
    }
}

impl std::fmt::Debug for WinElementHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WinElementHandle({} {:?})", self.role, self.label)
    }
}

impl WinElementHandle {
    pub(super) fn new(
        automation: IUIAutomation,
        element: IUIAutomationElement,
        role: String,
        label: String,
    ) -> Self {
        WinElementHandle {
            automation,
            element,
            role,
            label,
        }
    }
}

/// First descendant (anywhere in the subtree) exposing a click-ish pattern —
/// ONE bounded `FindFirst` COM call rather than macOS's manual depth-capped
/// recursion (`model::first_descendant_action`'s `DESCENDANT_CLICK_DEPTH`):
/// UI Automation's own tree walk is what evaluates the condition, so there's
/// no equivalent "how deep do I recurse myself" knob to set here — see the
/// module/PR doc for why this is simpler on Windows than on macOS's AX API.
fn find_actionable_descendant(
    automation: &IUIAutomation,
    el: &IUIAutomationElement,
) -> Option<IUIAutomationElement> {
    let condition = build_actionable_condition(automation).ok()?;
    // SAFETY: one bounded, documented COM call; `condition` is valid for its
    // duration and the automation client has an action deadline configured.
    unsafe { el.FindFirst(TreeScope_Descendants, &condition) }.ok()
}

/// First ancestor (within [`ANCESTOR_CLICK_DEPTH`] levels) exposing a
/// click-ish pattern — mirrors macOS `model::first_ancestor_action`, climbing
/// via `IUIAutomationTreeWalker::GetParentElement` since `FindFirst`/`FindAll`
/// don't support an ancestor `TreeScope`.
fn find_actionable_ancestor(
    automation: &IUIAutomation,
    el: &IUIAutomationElement,
    depth: usize,
) -> Option<IUIAutomationElement> {
    // SAFETY: `ControlViewWalker`/`GetParentElement` are standard, bounded
    // per-call COM operations; `depth` bounds the climb and the client timeout
    // bounds each provider transaction.
    unsafe {
        let walker = automation.ControlViewWalker().ok()?;
        let mut cur = el.clone();
        for _ in 0..depth {
            cur = walker.GetParentElement(&cur).ok()?;
            if pattern_for(&cur).is_some() {
                return Some(cur);
            }
        }
        None
    }
}

impl ElementHandle for WinElementHandle {
    fn prepare_for_action(&self, deadline: std::time::Instant) -> Result<(), String> {
        ensure_com_mta();
        if std::time::Instant::now() >= deadline {
            return Err("UI Automation action deadline elapsed".to_string());
        }
        configure_deadline(&self.automation, deadline);
        Ok(())
    }

    /// Drive the control through UI Automation's pattern ladder (see
    /// `automation::pattern_for`/`invoke_pattern`): try the element's own
    /// pattern first, then a descendant's, then an ancestor's — a list row or
    /// web container often delegates its action to an inner control or a
    /// wrapping link, exactly like macOS's `AxHandle::click`.
    fn click(&self) -> Result<&'static str, String> {
        ensure_com_mta();
        if let Some(action) = pattern_for(&self.element) {
            invoke_pattern(&self.element, action)?;
            return Ok(action);
        }
        if let Some(descendant) = find_actionable_descendant(&self.automation, &self.element) {
            if let Some(action) = pattern_for(&descendant) {
                invoke_pattern(&descendant, action)?;
                return Ok(action);
            }
        }
        if let Some(ancestor) =
            find_actionable_ancestor(&self.automation, &self.element, ANCESTOR_CLICK_DEPTH)
        {
            if let Some(action) = pattern_for(&ancestor) {
                invoke_pattern(&ancestor, action)?;
                return Ok(action);
            }
        }
        Err(format!(
            "{} {:?} (and its descendants/ancestors) expose no Invoke/Toggle/SelectionItem/\
             ExpandCollapse pattern",
            self.role, self.label
        ))
    }

    /// Whether this handle still points at a live element. A
    /// destroyed/disconnected element (e.g. after a page navigation rebuilds
    /// the tree) errors on this live read (`UIA_E_ELEMENTNOTAVAILABLE`/
    /// `RPC_E_DISCONNECTED`) rather than panicking; treated as not alive.
    fn is_alive(&self) -> bool {
        ensure_com_mta();
        // SAFETY: reading a live property never mutates the element. Geometry
        // is optional for semantic activation; current_center separately
        // requires a real rectangle for coordinate fallback.
        unsafe { self.element.CurrentControlType() }
            .map(|control_type| control_type != UIA_CONTROLTYPE_ID(0))
            .unwrap_or(false)
    }

    fn current_center(&self) -> Option<(f64, f64)> {
        ensure_com_mta();
        // SAFETY: see `is_alive`.
        let r = unsafe { self.element.CurrentBoundingRectangle() }.ok()?;
        let (w, h) = ((r.right - r.left) as f64, (r.bottom - r.top) as f64);
        if w < 1.0 || h < 1.0 {
            return None;
        }
        Some((r.left as f64 + w / 2.0, r.top as f64 + h / 2.0))
    }

    /// Always `None` on Windows — UI Automation's `Invoke` pattern already
    /// fires web content's own handlers straight off the accessibility tree
    /// (Chromium/WebView2 expose real DOM elements as `Invoke`/`Toggle`/etc.
    /// providers), unlike macOS's `AXPress`, which is a silent no-op on most
    /// web content and needs the browser-JS detour (`mac::elements::webclick`)
    /// to actually fire a click. There is no Windows analog of that detour —
    /// see the crate/PR doc for the full rationale; this is a deliberate
    /// permanent `None`, not a placeholder.
    fn try_web_click(
        &self,
        _pid: i32,
        _label: &str,
        _deadline: std::time::Instant,
    ) -> Option<Result<String, String>> {
        None
    }

    fn clone_box(&self) -> Box<dyn ElementHandle> {
        Box::new(self.clone())
    }
}
