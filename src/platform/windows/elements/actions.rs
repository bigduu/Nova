//! Query-driven Accessibility-style actions (Windows): find the unique element
//! whose control-type name / `Name` property contains `query`
//! (case-insensitive) among "actionable or focusable" candidates, then drive
//! it directly — no coordinates, no cursor movement, and the app need not be
//! frontmost. Same contract as macOS's `mac::elements::actions` (see
//! `crate::platform::UiTree::ax_click` &c.'s doc), implemented over UI
//! Automation instead of the AX API.
//!
//! # Scope: the frontmost window only (a known P2 simplification)
//!
//! macOS's `find_matching` walks from the whole APPLICATION element, reaching
//! every window it owns. UI Automation has no equivalent "whole app" root
//! (`ElementFromHandle` is per-HWND) — this searches `pid`'s FRONTMOST
//! on-screen window only (`window::first_hwnd_for_pid`), matching the
//! existing single-window focus model `server.rs`'s `target_pid`/marks cache
//! already assumes. A query that only matches a background secondary window
//! of a multi-window app won't be found; tracked as a follow-up rather than
//! blocking P2 (multi-window `ax_click` targeting is a rare case in practice).
use super::automation::{
    build_cache_request, build_queryable_condition, configure_deadline, control_type_name,
    invoke_pattern, pattern_for, value_pattern, with_automation,
};
use windows::core::BSTR;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, TreeScope_Descendants,
};

/// Walk `pid`'s frontmost window for the unique element whose control-type
/// name or `Name` property contains `query` (case-insensitive) among
/// "actionable or focusable" candidates (see
/// `automation::build_queryable_condition`). Ambiguous substring queries fail
/// closed instead of silently acting on provider order's first match.
fn find_matching(
    pid: i32,
    query: &str,
    deadline: std::time::Instant,
) -> Result<(IUIAutomation, IUIAutomationElement), String> {
    // Every `WinUiTree` entry point establishes DPI awareness (see
    // `platform::windows`'s doc), so a caller reaching one before `main()`'s
    // init still gets unscaled coordinates. These query actions never return a
    // `BoundingRectangle` (they act by pattern/focus, not position), so it has
    // no coordinate impact TODAY — but keeping the guard here makes the "every
    // entry point calls it" invariant literally true, so a future `window.rs`
    // refactor that starts reading rects can't silently break it. Idempotent +
    // cheap (a `Once` load after the first call).
    super::super::ensure_dpi_awareness();
    let query = query.trim();
    if query.is_empty() {
        return Err("route=uia; ambiguity: element query must not be empty".to_string());
    }
    let query_lower = query.to_lowercase();
    let Some(hwnd) = crate::platform::windows::window::first_hwnd_for_pid(pid) else {
        return Err(format!("no on-screen window found for pid {pid}"));
    };
    with_automation(|automation| {
        if std::time::Instant::now() >= deadline {
            return Err("route=uia; query action deadline elapsed".to_string());
        }
        configure_deadline(automation, deadline);
        // SAFETY: `hwnd` just resolved from a live `EnumWindows` pass.
        let root = unsafe { automation.ElementFromHandle(hwnd) }.map_err(|e| e.to_string())?;
        let cache = build_cache_request(automation).map_err(|e| e.to_string())?;
        let condition = build_queryable_condition(automation).map_err(|e| e.to_string())?;
        // SAFETY: one bounded, documented COM call.
        let elements = unsafe { root.FindAllBuildCache(TreeScope_Descendants, &condition, &cache) }
            .map_err(|e| format!("FindAllBuildCache failed: {e}"))?;
        let len = unsafe { elements.Length() }.map_err(|e| e.to_string())?;
        let mut matches = Vec::new();
        for i in 0..len {
            if std::time::Instant::now() >= deadline {
                return Err("route=uia; query action exceeded its bounded deadline".to_string());
            }
            let Ok(el) = (unsafe { elements.GetElement(i) }) else {
                continue;
            };
            let name = unsafe { el.CachedName() }
                .map(|b| b.to_string())
                .unwrap_or_default();
            let ct = unsafe { el.CachedControlType() }
                .unwrap_or(windows::Win32::UI::Accessibility::UIA_CONTROLTYPE_ID(0));
            let role = control_type_name(ct);
            let haystack = format!("{role} {name}").to_lowercase();
            if haystack.contains(&query_lower) {
                matches.push((el, role, name));
            }
        }
        match matches.len() {
            0 => Err(format!(
                "route=uia; no element matching {query:?} found in pid {pid}'s frontmost window"
            )),
            1 => matches
                .pop()
                .map(|(element, _, _)| (automation.clone(), element))
                .ok_or_else(|| "route=uia; matched element disappeared".to_string()),
            count => {
                let examples = matches
                    .iter()
                    .take(5)
                    .map(|(_, role, name)| format!("{role} {name:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(format!(
                    "route=uia; ambiguity: element query {query:?} matched {count} UIA elements \
                     ({examples}); refine the query or use a fresh ax_read node id"
                ))
            }
        }
    })
}

/// Click the element in `pid` matching `query`, performing whichever
/// click-ish pattern it actually supports (Invoke/Toggle/SelectionItem/
/// ExpandCollapse — see `automation::pattern_for`).
pub fn ax_click(pid: i32, query: &str, deadline: std::time::Instant) -> Result<String, String> {
    let (automation, el) = find_matching(pid, query, deadline)?;
    configure_deadline(&automation, deadline);
    let action = pattern_for(&el).ok_or_else(|| {
        format!(
            "element matching {query:?} exposes no click-ish pattern (Invoke/Toggle/\
             SelectionItem/ExpandCollapse)"
        )
    })?;
    invoke_pattern(&el, action)?;
    Ok(format!(
        "route=uia; performed {action} on element matching {query:?}"
    ))
}

/// Set the value (`ValuePattern::SetValue`) of the element in `pid` matching
/// `query` to `value` — e.g. fill a text field directly, without focusing or
/// typing.
pub fn ax_set_value(
    pid: i32,
    query: &str,
    value: &str,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let (automation, el) = find_matching(pid, query, deadline)?;
    configure_deadline(&automation, deadline);
    let pattern = value_pattern(&el)
        .map_err(|e| format!("element matching {query:?} has no ValuePattern: {e}"))?;
    let value_b = BSTR::from(value);
    // SAFETY: documented COM call; `value_b` is kept alive for its duration.
    unsafe { pattern.SetValue(&value_b) }
        .map_err(|e| format!("SetValue on {query:?} failed: {e}"))?;
    Ok(format!(
        "route=uia; set value of element matching {query:?}"
    ))
}

/// Move keyboard focus (`IUIAutomationElement::SetFocus`) to the element in
/// `pid` matching `query`.
pub fn ax_focus(pid: i32, query: &str, deadline: std::time::Instant) -> Result<String, String> {
    let (automation, el) = find_matching(pid, query, deadline)?;
    configure_deadline(&automation, deadline);
    // SAFETY: documented COM call on a live element reference.
    unsafe { el.SetFocus() }.map_err(|e| format!("SetFocus on {query:?} failed: {e}"))?;
    Ok(format!("route=uia; focused element matching {query:?}"))
}
