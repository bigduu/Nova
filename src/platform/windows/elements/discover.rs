//! `WinUiTree::collect_actionable` — real UI Automation discovery.
//!
//! Unlike macOS's manual, depth-capped `AXUIElement` tree recursion
//! (`mac::elements::walk`), this drives discovery through ONE
//! `IUIAutomationElement::FindAllBuildCache` call per (retry) attempt: UI
//! Automation's own implementation walks the provider's tree server-side and
//! evaluates the actionable condition as it goes, returning just the matching
//! set — see `automation::build_actionable_condition`'s doc for why this also
//! sidesteps macOS's per-path cycle-guard problem (there is no manual
//! recursion here for a cycle to hide in).
//!
//! # Why `AutomationElementMode_Full` (the default), not `_None`
//!
//! `IUIAutomationCacheRequest::AutomationElementMode` can be set to `_None` to
//! make returned elements cache-data-only (no live provider reference) — a
//! valid micro-optimization when a caller only ever reads cached properties.
//! We do NOT set it: `WinElementHandle::click`/`is_alive`/`current_center` all
//! need a LIVE reference (fresh property reads, `GetCurrentPatternAs` to
//! actually invoke a pattern) at click time, which can be long after
//! discovery. The perf-critical part the crate/PR doc calls out — batching
//! every property into one `FindAllBuildCache` round trip instead of N
//! per-property calls — is independent of element mode; keeping the default
//! `Full` mode costs nothing extra there while keeping `click_mark` correct.
use super::automation::{
    build_actionable_condition, build_cache_request, control_type_name, with_automation,
};
use super::handle::WinElementHandle;
use super::UiElement;
use crate::platform::ElementHandle;
use std::time::Duration;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationCacheRequest, IUIAutomationCondition, IUIAutomationElement,
    TreeScope_Descendants, UIA_CONTROLTYPE_ID,
};

type Rect = (f64, f64, f64, f64);

/// Named alias for `collect_actionable_inner`'s return type — the bare
/// `Result<Vec<(UiElement, Box<dyn ElementHandle>)>, String>` trips clippy's
/// `type_complexity` lint (the SAME shape unwrapped, as the `UiTree` trait
/// method returns it, stays just under the threshold; wrapping it in a
/// `Result` for this private fallible helper is what pushes it over).
type ActionableElements = Vec<(UiElement, Box<dyn ElementHandle>)>;

fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && a.0 + a.2 > b.0 && a.1 < b.1 + b.3 && a.1 + a.3 > b.1
}

/// Defensive cap on raw `FindAll` matches processed per call, in case the
/// actionable condition still matches an unexpectedly huge set on a
/// pathological tree. The real caps that matter for what the MODEL sees are
/// upstream (`collect_actionable(pid, 400, ..)` from `build_marks`, further
/// trimmed to `MAX_MARKS = 150`); this just bounds this loop's own cost.
const MAX_CANDIDATES: usize = 2000;

/// Cold Electron/Chromium-hosted UIA providers (WebView2, CEF-backed apps)
/// sometimes haven't finished exposing their tree on the FIRST `FindAll`
/// right after a window appears — retry briefly rather than reporting empty,
/// mirroring macOS `discover::collect_actionable`'s bounded (`<2`-attempt)
/// retry for the exact same "cold Chromium tree" reason. NOT a background
/// thread/heartbeat — see `WinUiTree::keep_warm`'s doc for why Windows needs
/// no such thing.
const COLD_RETRY_ATTEMPTS: u32 = 2;
const COLD_RETRY_DELAY: Duration = Duration::from_millis(350);

/// Discover actionable elements for `pid`, clipped to `clip` (global-logical,
/// i.e. already-unscaled-by-DPI-awareness rect) when given. Empty (never a
/// panic) on any failure — permission issues, no matching window, or a COM
/// error all degrade to "no marks", exactly like a macOS app with no
/// Accessibility tree.
pub fn collect_actionable(
    pid: i32,
    max: usize,
    clip: Option<Rect>,
) -> Vec<(UiElement, Box<dyn ElementHandle>)> {
    // A test or other caller reaching this directly (bypassing `main()`)
    // still needs unscaled coordinates — see `platform::windows`'s doc.
    super::super::ensure_dpi_awareness();
    match collect_actionable_inner(pid, max, clip) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(target: "nova::uia", "collect_actionable(pid={pid}) failed: {e}");
            Vec::new()
        }
    }
}

fn collect_actionable_inner(
    pid: i32,
    max: usize,
    clip: Option<Rect>,
) -> Result<ActionableElements, String> {
    let Some(hwnd) = resolve_target_hwnd(pid, clip) else {
        // No on-screen window for `pid` (or it just closed) — degrade empty,
        // never an error (mirrors the trait's documented contract).
        return Ok(Vec::new());
    };

    with_automation(|automation| {
        // SAFETY: `hwnd` was just resolved from a live `EnumWindows` pass.
        let root = unsafe { automation.ElementFromHandle(hwnd) }
            .map_err(|e| format!("ElementFromHandle failed: {e}"))?;
        let cache = build_cache_request(automation)
            .map_err(|e| format!("CreateCacheRequest failed: {e}"))?;
        let condition = build_actionable_condition(automation)
            .map_err(|e| format!("build_actionable_condition failed: {e}"))?;

        let mut candidates = find_all(&root, &condition, &cache)?;
        let mut attempts = 0;
        while candidates.is_empty() && attempts < COLD_RETRY_ATTEMPTS {
            std::thread::sleep(COLD_RETRY_DELAY);
            candidates = find_all(&root, &condition, &cache)?;
            attempts += 1;
        }

        let mut out: ActionableElements = Vec::new();
        // Frame-based dedupe (mirrors macOS's exact-frame dedupe in
        // `discover::collect_actionable`): `FindAll` can hand back more than
        // one COM proxy for what is visually the same control.
        let mut seen: std::collections::HashSet<(i64, i64, i64, i64)> =
            std::collections::HashSet::new();
        for el in candidates.into_iter().take(MAX_CANDIDATES) {
            if out.len() >= max {
                break;
            }
            let Some((ui, key)) = to_ui_element(&el, clip) else {
                continue;
            };
            if !seen.insert(key) {
                continue;
            }
            let handle =
                WinElementHandle::new(automation.clone(), el, ui.role.clone(), ui.label.clone());
            out.push((ui, Box::new(handle) as Box<dyn ElementHandle>));
        }
        Ok(out)
    })
}

fn find_all(
    root: &IUIAutomationElement,
    condition: &IUIAutomationCondition,
    cache: &IUIAutomationCacheRequest,
) -> Result<Vec<IUIAutomationElement>, String> {
    // SAFETY: one bounded, documented COM call; `condition`/`cache` are valid
    // for its whole duration (both owned by the caller, held across retries).
    let array = unsafe { root.FindAllBuildCache(TreeScope_Descendants, condition, cache) }
        .map_err(|e| format!("FindAllBuildCache failed: {e}"))?;
    // SAFETY: `array` was just returned above.
    let len = unsafe { array.Length() }.map_err(|e| format!("ElementArray::Length failed: {e}"))?;
    let mut out = Vec::with_capacity(len.max(0) as usize);
    for i in 0..len {
        // A single bad index is skipped rather than aborting the whole batch
        // — best-effort, matches the trait's "degrade gracefully" contract.
        // SAFETY: `i` is in `0..len`, per `array`'s own reported length.
        if let Ok(el) = unsafe { array.GetElement(i) } {
            out.push(el);
        }
    }
    Ok(out)
}

/// Convert a matched, cached element into the neutral [`UiElement`] plus a
/// dedupe key (its rounded frame). `None` when the element is offscreen, has
/// no usable size, or (with a `clip`) doesn't intersect it.
///
/// Every property read below was explicitly added to the `CacheRequest` in
/// `automation::build_cache_request`, so each `Cached*` call here is a LOCAL
/// read of the already-fetched cache blob from the ONE `FindAllBuildCache`
/// call above — no additional RPC per element (the perf-critical property of
/// this whole design).
fn to_ui_element(
    el: &IUIAutomationElement,
    clip: Option<Rect>,
) -> Option<(UiElement, (i64, i64, i64, i64))> {
    // SAFETY: every accessor below reads a property this module's
    // `CacheRequest` explicitly requested — see `build_cache_request`.
    let offscreen = unsafe { el.CachedIsOffscreen() }
        .map(|b| b.as_bool())
        .unwrap_or(false);
    if offscreen {
        return None;
    }
    let rect = unsafe { el.CachedBoundingRectangle() }.ok()?;
    let (x, y) = (rect.left as f64, rect.top as f64);
    let (w, h) = (
        (rect.right - rect.left) as f64,
        (rect.bottom - rect.top) as f64,
    );
    if w < 1.0 || h < 1.0 {
        return None;
    }
    if let Some(clip) = clip {
        if !rects_intersect((x, y, w, h), clip) {
            return None;
        }
    }
    let name = unsafe { el.CachedName() }
        .map(|b| b.to_string())
        .unwrap_or_default();
    let control_type = unsafe { el.CachedControlType() }.unwrap_or(UIA_CONTROLTYPE_ID(0));
    let role = control_type_name(control_type);
    let key = (x as i64, y as i64, w as i64, h as i64);
    Some((
        UiElement {
            role,
            label: name,
            // Preserve Set-of-Mark/read_ui's existing compact behavior. Rich
            // values and secure redaction belong to the semantic snapshot's
            // separately budgeted two-pass cache.
            value: String::new(),
            x,
            y,
            width: w,
            height: h,
        },
        key,
    ))
}

/// The HWND to walk: `pid`'s window matching `clip` exactly (a few px
/// tolerance — `platform::windows::window::hwnd_for_rect`, the Windows analog
/// of macOS's `window_id_for_rect`/`CoordLift::derive` anchoring), falling
/// back to `pid`'s frontmost on-screen window when no `clip` is given (or
/// none matches closely enough). Unlike macOS, no coordinate LIFT is needed
/// once the right window is found — see the crate/PR doc's coordinate-
/// contract note: `BoundingRectangle` already reports the same global/
/// physical pixel space `GetWindowRect` (and hence `clip`) uses, because P1
/// declared Per-Monitor-DPI-v2 process-wide.
fn resolve_target_hwnd(pid: i32, clip: Option<Rect>) -> Option<HWND> {
    if let Some(clip) = clip {
        if let Some(hwnd) = crate::platform::windows::window::hwnd_for_rect(pid, clip) {
            return Some(hwnd);
        }
    }
    crate::platform::windows::window::first_hwnd_for_pid(pid)
}

/// Diagnostic-only flat dump of `pid`'s UI Automation "control view" (the
/// same reduced, human-meaningful subset Narrator/Inspect.exe show by
/// default) — NOT a tree-indented walk like macOS's `debug::dump_tree`
/// (`FindAll`'s result is a flat array with no parent/depth info attached).
/// Not on the marks/`click_mark` hot path.
pub fn dump_tree(pid: i32, max_nodes: usize) -> String {
    super::super::ensure_dpi_awareness();
    match dump_tree_inner(pid, max_nodes) {
        Ok(s) => s,
        Err(e) => format!("UI Automation tree dump for pid {pid} failed: {e}"),
    }
}

fn dump_tree_inner(pid: i32, max_nodes: usize) -> Result<String, String> {
    let Some(hwnd) = crate::platform::windows::window::first_hwnd_for_pid(pid) else {
        return Ok(format!("no on-screen window found for pid {pid}"));
    };
    with_automation(|automation: &IUIAutomation| {
        // SAFETY: `hwnd` just resolved from a live `EnumWindows` pass.
        let root = unsafe { automation.ElementFromHandle(hwnd) }.map_err(|e| e.to_string())?;
        let cache = build_cache_request(automation).map_err(|e| e.to_string())?;
        // The "control view" condition — same reduced set Narrator/Inspect.exe
        // default to — rather than every raw element (text runs, decorative
        // panes, ...), which on a complex app would make this diagnostic both
        // slow and unreadable.
        let condition = unsafe { automation.ControlViewCondition() }.map_err(|e| e.to_string())?;
        let elements = find_all(&root, &condition, &cache)?;

        let mut out = String::new();
        let mut n = 0usize;
        for el in elements.iter().take(max_nodes) {
            n += 1;
            let name = unsafe { el.CachedName() }
                .map(|b| b.to_string())
                .unwrap_or_default();
            let ct = unsafe { el.CachedControlType() }.unwrap_or(UIA_CONTROLTYPE_ID(0));
            let role = control_type_name(ct);
            let frame = unsafe { el.CachedBoundingRectangle() }
                .map(|r| {
                    format!(
                        " @({},{} {}x{})",
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top
                    )
                })
                .unwrap_or_default();
            out.push_str(&format!("{role} {name:?}{frame}\n"));
        }
        out.push_str(&format!(
            "\n[{n} nodes dumped (flat UI Automation \"control view\" — not tree-indented; \
             diagnostic only){}]\n",
            if elements.len() > max_nodes {
                ", TRUNCATED"
            } else {
                ""
            }
        ));
        Ok(out)
    })
}
