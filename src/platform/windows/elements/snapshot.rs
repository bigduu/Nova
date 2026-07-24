//! Capture-free Windows UI Automation semantic reads.
//!
//! Target resolution uses the existing `EnumWindows`-backed window manager;
//! no GDI/WGC/screenshot path is involved. Snapshot discovery uses one cached
//! classification batch plus a non-password-only Value batch, followed by
//! local filtering, deterministic ordering, redaction, and hard
//! node/character/deadline budgets.

use super::automation::{
    build_nonsecure_snapshot_condition, build_snapshot_cache_request, build_snapshot_condition,
    cached_actions, cached_bool_property, cached_i32_property, cached_node_value,
    cached_password_state, configure_deadline, control_type_name, is_actionable_control_type,
    with_automation_raw,
};
use super::handle::WinElementHandle;
use crate::platform::{
    CollectedUiNode, ElementHandle, UiBounds, UiNode, UiNodeStates, UiPartialReason,
    UiReadCoverage, UiReadError, UiReadErrorKind, UiReadMode, UiSnapshot, UiSnapshotOptions,
    UiTarget, WindowHandle,
};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::c_void;
use std::time::Instant;
use windows::core::Error as WinError;
use windows::Win32::Foundation::{E_ACCESSDENIED, HWND};
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationCacheRequest, IUIAutomationCondition, IUIAutomationElement,
    TreeScope_Descendants, UIA_ExpandCollapseExpandCollapseStatePropertyId,
    UIA_SelectionItemIsSelectedPropertyId, UIA_ToggleToggleStatePropertyId, UIA_CONTROLTYPE_ID,
    UIA_E_ELEMENTNOTAVAILABLE, UIA_E_NOTSUPPORTED, UIA_E_TIMEOUT,
};

/// A defensive local-processing bound. UIA's `FindAllBuildCache` itself has no
/// count parameter; the public budgets below remain the authoritative output
/// limits.
const MAX_RAW_CANDIDATES: usize = 10_000;
/// Bound identity comparisons when a pathological provider returns thousands
/// of indistinguishable proxies. Once exhausted we preserve remaining nodes
/// and report partial coverage instead of risking an unbounded quadratic pass.
const MAX_IDENTITY_COMPARISONS: usize = 50_000;

#[derive(Debug, PartialEq, Eq, Hash)]
struct IdentityBucketKey {
    control_type: i32,
    name: String,
    bounds: (i32, i32, i32, i32),
}

fn is_usable_window(window: &WindowHandle) -> bool {
    window.pid > 0
        && window.is_visible
        && window.width > 0.0
        && window.height > 0.0
        && window.id != 0
}

fn query_matches(window: &WindowHandle, query: &str) -> bool {
    window.title.to_lowercase().contains(query) || window.app_name.to_lowercase().contains(query)
}

/// Pick the first maximum-area item, preserving EnumWindows' front-to-back
/// order as the deterministic tiebreaker.
fn largest_window(windows: Vec<WindowHandle>) -> Option<WindowHandle> {
    let mut windows = windows.into_iter();
    let first = windows.next()?;
    Some(windows.fold(first, |best, candidate| {
        if candidate.width * candidate.height > best.width * best.height {
            candidate
        } else {
            best
        }
    }))
}

fn target_from_window(window: WindowHandle) -> UiTarget {
    UiTarget {
        pid: window.pid,
        app_name: window.app_name,
        window_title: window.title,
        window_id: (window.id != 0).then_some(window.id),
        bounds: (window.width > 0.0 && window.height > 0.0).then_some(UiBounds {
            x: window.x,
            y: window.y,
            width: window.width,
            height: window.height,
        }),
    }
}

pub(super) fn resolve_target(
    query: Option<&str>,
    preferred_pid: Option<i32>,
    deadline: Instant,
) -> Result<UiTarget, UiReadError> {
    super::super::ensure_dpi_awareness();
    if Instant::now() >= deadline {
        return Err(UiReadError::new(
            UiReadErrorKind::TimedOut,
            "UI Automation target-resolution deadline elapsed before EnumWindows",
        ));
    }
    let windows = crate::platform::windows::window::list_windows().map_err(|error| {
        UiReadError::new(
            UiReadErrorKind::BackendFailure,
            format!("EnumWindows target discovery failed: {error}"),
        )
    })?;
    if Instant::now() >= deadline {
        return Err(UiReadError::new(
            UiReadErrorKind::TimedOut,
            "UI Automation target-resolution deadline elapsed during EnumWindows",
        ));
    }
    let query = query
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_lowercase);
    let mut candidates: Vec<_> = windows
        .into_iter()
        .filter(is_usable_window)
        .filter(|window| {
            query
                .as_deref()
                .is_none_or(|query| query_matches(window, query))
        })
        .collect();

    let preferred_matched =
        preferred_pid.is_some_and(|pid| candidates.iter().any(|window| window.pid == pid));
    if preferred_matched {
        if let Some(pid) = preferred_pid {
            candidates.retain(|window| window.pid == pid);
        }
    }

    // An explicit app/window query (or pid) should resolve the main window,
    // not a tiny PiP/tool window. A bare request means "frontmost" and keeps
    // EnumWindows' first usable titled window.
    let selected = if query.is_some() || preferred_matched {
        largest_window(candidates)
    } else {
        candidates
            .into_iter()
            .find(|window| !window.title.is_empty())
    }
    .ok_or_else(|| {
        let selector = query
            .as_deref()
            .map(|query| format!(" matching {query:?}"))
            .unwrap_or_default();
        UiReadError::new(
            UiReadErrorKind::TargetNotFound,
            format!("no on-screen window{selector} found via EnumWindows"),
        )
    })?;

    if Instant::now() >= deadline {
        return Err(UiReadError::new(
            UiReadErrorKind::TimedOut,
            "UI Automation target-resolution deadline elapsed while selecting a window",
        ));
    }
    Ok(target_from_window(selected))
}

/// Revalidate the opaque HWND just before UIA reads it. This prevents a stale
/// numeric handle from being reused for an unrelated window after the original
/// target closes.
fn live_target(target: &UiTarget) -> Result<(HWND, UiTarget), UiReadError> {
    let windows = crate::platform::windows::window::list_windows().map_err(|error| {
        UiReadError::new(
            UiReadErrorKind::BackendFailure,
            format!("EnumWindows target revalidation failed: {error}"),
        )
    })?;
    let mut matches: Vec<_> = windows
        .into_iter()
        .filter(is_usable_window)
        .filter(|window| window.pid == target.pid)
        .filter(|window| {
            target
                .window_id
                .is_none_or(|window_id| window.id == window_id)
        })
        .collect();
    if target.window_id.is_none() && !target.window_title.is_empty() {
        let exact_title: Vec<_> = matches
            .iter()
            .filter(|window| window.title == target.window_title)
            .cloned()
            .collect();
        if !exact_title.is_empty() {
            matches = exact_title;
        }
    }
    let window = largest_window(matches).ok_or_else(|| {
        UiReadError::new(
            UiReadErrorKind::TargetNotFound,
            format!(
                "target window is no longer available (pid={}, id={:?}, title={:?})",
                target.pid, target.window_id, target.window_title
            ),
        )
    })?;
    let hwnd = HWND(window.id as usize as *mut c_void);
    Ok((hwnd, target_from_window(window)))
}

fn map_uia_error(stage: &str, error: WinError, deadline: Instant) -> UiReadError {
    let code = error.code();
    let raw = code.0 as u32;
    let kind = if code == E_ACCESSDENIED {
        UiReadErrorKind::PermissionDenied
    } else if raw == UIA_E_TIMEOUT || Instant::now() >= deadline {
        UiReadErrorKind::TimedOut
    } else if raw == UIA_E_ELEMENTNOTAVAILABLE {
        UiReadErrorKind::TargetNotFound
    } else if raw == UIA_E_NOTSUPPORTED {
        UiReadErrorKind::NoSemanticTree
    } else {
        UiReadErrorKind::BackendFailure
    };
    UiReadError::new(kind, format!("{stage} failed: {error}"))
}

fn find_all(
    root: &IUIAutomationElement,
    condition: &IUIAutomationCondition,
    cache: &IUIAutomationCacheRequest,
) -> windows::core::Result<(Vec<IUIAutomationElement>, bool)> {
    // SAFETY: a single bounded UIA request; the automation client's
    // transaction timeout is configured immediately before this call.
    let array = unsafe { root.FindAllBuildCache(TreeScope_Descendants, condition, cache) }?;
    // SAFETY: `array` was returned by UIA above.
    let length = unsafe { array.Length() }?;
    let mut provider_partial = false;
    let mut elements = Vec::with_capacity((length.max(0) as usize).min(MAX_RAW_CANDIDATES));
    for index in 0..length {
        if elements.len() >= MAX_RAW_CANDIDATES {
            provider_partial = true;
            break;
        }
        // SAFETY: index lies in the array's reported range. One transient bad
        // provider element does not discard the rest of a useful snapshot.
        match unsafe { array.GetElement(index) } {
            Ok(element) => elements.push(element),
            Err(_) => provider_partial = true,
        }
    }
    Ok((elements, provider_partial))
}

fn value_without_read(element: &IUIAutomationElement) -> crate::platform::UiNodeValue {
    match cached_password_state(element) {
        Some(true) | None => crate::platform::UiNodeValue::Redacted,
        Some(false) => crate::platform::UiNodeValue::Absent,
    }
}

/// Match the provider-side non-password value batch back onto the first-pass
/// elements. `CompareElements` validates identity so a dynamic tree can never
/// shift one control's value onto a different node.
fn attach_safe_values(
    automation: &IUIAutomation,
    elements: Vec<IUIAutomationElement>,
    safe_value_elements: &[IUIAutomationElement],
) -> (
    Vec<(IUIAutomationElement, crate::platform::UiNodeValue)>,
    bool,
) {
    let mut safe_index = 0usize;
    let mut provider_partial = false;
    let mut out = Vec::with_capacity(elements.len());
    for element in elements {
        let value = match cached_password_state(&element) {
            Some(true) | None => value_without_read(&element),
            Some(false) => {
                let Some(safe_element) = safe_value_elements.get(safe_index) else {
                    provider_partial = true;
                    out.push((element, crate::platform::UiNodeValue::Absent));
                    continue;
                };
                // SAFETY: both elements came from the two immediately adjacent
                // UIA batches rooted at the same live HWND.
                match unsafe { automation.CompareElements(&element, safe_element) } {
                    Ok(equal) if equal.as_bool() => {
                        safe_index += 1;
                        cached_node_value(safe_element)
                    }
                    Ok(_) | Err(_) => {
                        // Keep the safe element for the next base node. If the
                        // tree changed, absence is safer than mis-association.
                        provider_partial = true;
                        crate::platform::UiNodeValue::Absent
                    }
                }
            }
        };
        out.push((element, value));
    }
    if safe_index != safe_value_elements.len() {
        provider_partial = true;
    }
    (out, provider_partial)
}

fn identity_bucket_key(element: &IUIAutomationElement) -> IdentityBucketKey {
    // SAFETY: these properties are present in the first-pass cache.
    let control_type = unsafe { element.CachedControlType() }
        .map(|control_type| control_type.0)
        .unwrap_or_default();
    let name = unsafe { element.CachedName() }
        .map(|name| name.to_string())
        .unwrap_or_default();
    let bounds = unsafe { element.CachedBoundingRectangle() }
        .map(|bounds| (bounds.left, bounds.top, bounds.right, bounds.bottom))
        .unwrap_or((i32::MIN, i32::MIN, i32::MIN, i32::MIN));
    IdentityBucketKey {
        control_type,
        name,
        bounds,
    }
}

/// Remove repeated COM proxies for the same provider element without using
/// frame/text equality as identity. Distinct text nodes may legitimately share
/// every visible property and rectangle, so only UIA `CompareElements=true`
/// collapses a candidate.
fn dedupe_provider_proxies(
    automation: &IUIAutomation,
    elements: Vec<(IUIAutomationElement, crate::platform::UiNodeValue)>,
) -> (
    Vec<(IUIAutomationElement, crate::platform::UiNodeValue)>,
    bool,
) {
    let mut out: Vec<(IUIAutomationElement, crate::platform::UiNodeValue)> =
        Vec::with_capacity(elements.len());
    let mut buckets: HashMap<IdentityBucketKey, Vec<usize>> = HashMap::new();
    let mut comparisons = 0usize;
    let mut provider_partial = false;

    for (element, value) in elements {
        let key = identity_bucket_key(&element);
        let bucket = buckets.entry(key).or_default();
        let mut duplicate = false;
        for &existing_index in bucket.iter() {
            if comparisons >= MAX_IDENTITY_COMPARISONS {
                provider_partial = true;
                break;
            }
            comparisons += 1;
            // SAFETY: both elements came from one immediately preceding UIA
            // snapshot rooted at the same live HWND.
            match unsafe { automation.CompareElements(&out[existing_index].0, &element) } {
                Ok(equal) if equal.as_bool() => {
                    duplicate = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => provider_partial = true,
            }
        }
        if !duplicate {
            let index = out.len();
            out.push((element, value));
            bucket.push(index);
        }
    }
    (out, provider_partial)
}

fn cached_bounds(element: &IUIAutomationElement) -> Option<UiBounds> {
    // SAFETY: BoundingRectangle is included in the cache request.
    let rect = unsafe { element.CachedBoundingRectangle() }.ok()?;
    let width = (rect.right - rect.left) as f64;
    let height = (rect.bottom - rect.top) as f64;
    (width > 0.0 && height > 0.0).then_some(UiBounds {
        x: rect.left as f64,
        y: rect.top as f64,
        width,
        height,
    })
}

fn checked_state(element: &IUIAutomationElement) -> Option<bool> {
    match cached_i32_property(element, UIA_ToggleToggleStatePropertyId) {
        Some(0) => Some(false), // ToggleState_Off
        Some(1) => Some(true),  // ToggleState_On
        // Indeterminate and unsupported cannot be represented faithfully by
        // the neutral bool, so remain absent rather than lying.
        _ => None,
    }
}

fn expanded_state(element: &IUIAutomationElement) -> Option<bool> {
    match cached_i32_property(element, UIA_ExpandCollapseExpandCollapseStatePropertyId) {
        Some(0) => Some(false),          // Collapsed
        Some(1) | Some(2) => Some(true), // Expanded / PartiallyExpanded
        _ => None,                       // LeafNode / unsupported
    }
}

fn is_click_action(action: &str) -> bool {
    matches!(
        action,
        "Invoke" | "Toggle" | "SelectionItem" | "ExpandCollapse"
    )
}

fn node_from_cached(
    automation: &IUIAutomation,
    element: IUIAutomationElement,
    value: crate::platform::UiNodeValue,
    mode: UiReadMode,
) -> Option<CollectedUiNode> {
    // SAFETY: IsOffscreen is included in the cache request.
    if unsafe { element.CachedIsOffscreen() }
        .map(|value| value.as_bool())
        .unwrap_or(false)
    {
        return None;
    }
    // SAFETY: all specialized accessors below read properties included in the
    // one cache request; there is no per-property provider RPC here.
    let name = unsafe { element.CachedName() }
        .map(|value| value.to_string())
        .unwrap_or_default();
    let description = unsafe { element.CachedHelpText() }
        .map(|value| value.to_string())
        .unwrap_or_default();
    let control_type = unsafe { element.CachedControlType() }.unwrap_or(UIA_CONTROLTYPE_ID(0));
    let role = control_type_name(control_type);
    let actions = cached_actions(&element);
    let actionable =
        is_actionable_control_type(control_type) || actions.iter().any(|a| is_click_action(a));
    let readable =
        !name.is_empty() || !description.is_empty() || !value.as_filter_text().is_empty();
    let include = match mode {
        UiReadMode::Interactive => actionable,
        UiReadMode::Content => readable,
        UiReadMode::All => actionable || readable,
    };
    if !include {
        return None;
    }

    let node = UiNode {
        role: role.clone(),
        name: name.clone(),
        description,
        value,
        actions,
        states: UiNodeStates {
            // SAFETY: both specialized properties are cached.
            enabled: unsafe { element.CachedIsEnabled() }
                .ok()
                .map(|value| value.as_bool()),
            focused: unsafe { element.CachedHasKeyboardFocus() }
                .ok()
                .map(|value| value.as_bool()),
            selected: cached_bool_property(&element, UIA_SelectionItemIsSelectedPropertyId),
            checked: checked_state(&element),
            expanded: expanded_state(&element),
        },
        bounds: cached_bounds(&element),
        // FindAllBuildCache returns a flat provider array. Zero is the honest
        // neutral depth when ancestry was not requested; it must not invent a
        // hierarchy from geometry or list order.
        depth: 0,
        actionable,
    };
    let handle = actionable.then(|| {
        Box::new(WinElementHandle::new(
            automation.clone(),
            element,
            role,
            name,
        )) as Box<dyn ElementHandle>
    });
    Some(CollectedUiNode { node, handle })
}

fn compare_nodes(left: &(usize, CollectedUiNode), right: &(usize, CollectedUiNode)) -> Ordering {
    match (left.1.node.bounds, right.1.node.bounds) {
        (Some(a), Some(b)) => {
            a.y.total_cmp(&b.y)
                .then_with(|| a.x.total_cmp(&b.x))
                .then_with(|| a.height.total_cmp(&b.height))
                .then_with(|| a.width.total_cmp(&b.width))
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
    .then_with(|| left.1.node.role.cmp(&right.1.node.role))
    .then_with(|| left.1.node.name.cmp(&right.1.node.name))
    // Same-frame text nodes are intentionally NOT deduplicated. Their provider
    // order is the final deterministic tiebreaker.
    .then_with(|| left.0.cmp(&right.0))
}

fn node_char_count(node: &UiNode) -> usize {
    node.role.chars().count()
        + node.name.chars().count()
        + node.description.chars().count()
        + node.value.as_filter_text().chars().count()
        + node
            .actions
            .iter()
            .map(|action| action.chars().count())
            .sum::<usize>()
}

pub(super) fn read_snapshot(
    target: &UiTarget,
    options: UiSnapshotOptions,
) -> Result<UiSnapshot, UiReadError> {
    super::super::ensure_dpi_awareness();
    if Instant::now() >= options.deadline {
        return Err(UiReadError::new(
            UiReadErrorKind::TimedOut,
            "UI Automation semantic-read deadline elapsed before discovery",
        ));
    }
    let (hwnd, live_target) = live_target(target)?;
    if Instant::now() >= options.deadline {
        return Err(UiReadError::new(
            UiReadErrorKind::TimedOut,
            "UI Automation semantic-read deadline elapsed during target resolution",
        ));
    }

    let (elements, mut provider_partial, value_deadline_partial, automation) =
        with_automation_raw(|automation| {
            configure_deadline(automation, options.deadline);
            // SAFETY: hwnd was revalidated against a fresh EnumWindows pass.
            let root = unsafe { automation.ElementFromHandle(hwnd) }?;
            let cache = build_snapshot_cache_request(automation, false)?;
            let condition = build_snapshot_condition(automation, options.mode)?;
            let (elements, mut provider_partial) = find_all(&root, &condition, &cache)?;

            if Instant::now() >= options.deadline {
                let elements = elements
                    .into_iter()
                    .map(|element| {
                        let value = value_without_read(&element);
                        (element, value)
                    })
                    .collect();
                return Ok((elements, provider_partial, true, automation.clone()));
            }

            // The second batch filters passwords provider-side before its
            // cache request asks for Value.
            configure_deadline(automation, options.deadline);
            let safe_values = (|| {
                let cache = build_snapshot_cache_request(automation, true)?;
                let condition = build_nonsecure_snapshot_condition(automation, options.mode)?;
                find_all(&root, &condition, &cache)
            })();
            match safe_values {
                Ok((safe_elements, safe_partial)) => {
                    provider_partial |= safe_partial;
                    let (elements, identity_partial) =
                        attach_safe_values(automation, elements, &safe_elements);
                    provider_partial |= identity_partial;
                    Ok((elements, provider_partial, false, automation.clone()))
                }
                Err(error) => {
                    let deadline_partial = error.code().0 as u32 == UIA_E_TIMEOUT
                        || Instant::now() >= options.deadline;
                    let elements = elements
                        .into_iter()
                        .map(|element| {
                            let value = value_without_read(&element);
                            (element, value)
                        })
                        .collect();
                    Ok((elements, true, deadline_partial, automation.clone()))
                }
            }
        })
        .map_err(|error| map_uia_error("UI Automation snapshot", error, options.deadline))?;
    let (elements, identity_partial) = dedupe_provider_proxies(&automation, elements);
    provider_partial |= identity_partial;

    if elements.is_empty() && !provider_partial {
        return Err(UiReadError::new(
            UiReadErrorKind::NoSemanticTree,
            format!(
                "window {:?} exposes no {} UI Automation descendants",
                live_target.window_title,
                options.mode.as_str()
            ),
        ));
    }

    let mut candidates = Vec::new();
    let mut partial_reason = if value_deadline_partial {
        Some(UiPartialReason::Deadline)
    } else {
        provider_partial.then_some(UiPartialReason::ProviderPartial)
    };
    for (provider_index, (element, value)) in elements.into_iter().enumerate() {
        if Instant::now() >= options.deadline {
            partial_reason = Some(UiPartialReason::Deadline);
            break;
        }
        if let Some(node) = node_from_cached(&automation, element, value, options.mode) {
            candidates.push((provider_index, node));
        }
    }
    candidates.sort_by(compare_nodes);

    let mut nodes = Vec::new();
    let mut used_chars = 0usize;
    for (_, candidate) in candidates {
        if Instant::now() >= options.deadline {
            partial_reason = Some(UiPartialReason::Deadline);
            break;
        }
        if nodes.len() >= options.max_nodes {
            partial_reason.get_or_insert(UiPartialReason::NodeLimit);
            break;
        }
        let candidate_chars = node_char_count(&candidate.node);
        if candidate_chars > options.max_chars.saturating_sub(used_chars) {
            partial_reason.get_or_insert(UiPartialReason::CharacterLimit);
            break;
        }
        used_chars += candidate_chars;
        nodes.push(candidate);
    }

    let coverage = if partial_reason.is_some() {
        UiReadCoverage::Partial
    } else if nodes.is_empty() {
        UiReadCoverage::Empty
    } else {
        UiReadCoverage::Complete
    };
    if nodes.is_empty() && partial_reason == Some(UiPartialReason::Deadline) {
        return Err(UiReadError::new(
            UiReadErrorKind::TimedOut,
            "UI Automation semantic-read deadline elapsed before any node was collected",
        ));
    }
    Ok(UiSnapshot {
        target: live_target,
        nodes,
        coverage,
        truncated: partial_reason.is_some(),
        partial_reason,
    })
}
