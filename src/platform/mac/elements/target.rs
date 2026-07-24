//! Accessibility-only app/window targeting for `ax_read`.
//!
//! This deliberately does not call `WindowManager` or ScreenCaptureKit. It
//! enumerates running GUI applications through NSWorkspace, then resolves their
//! windows from AX attributes. Therefore semantic reads remain available with
//! Accessibility granted and Screen Recording denied.

use super::attrs::{
    ax_bool, ax_pid, ax_role, ax_title, ax_window_id, element_array, element_attribute,
    element_rect, process_is_trusted,
};
use crate::platform::{UiBounds, UiReadError, UiReadErrorKind, UiTarget};
use accessibility::AXUIElement;
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::geometry::CGRect;
use core_graphics::window::{
    create_description_from_array, create_window_list, kCGNullWindowID,
    kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
};
use objc2_app_kit::NSWorkspace;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Debug, Clone)]
struct RunningApp {
    pid: i32,
    name: String,
    bundle_id: String,
}

struct WindowCandidate {
    title: String,
    raw_frame: Option<UiBounds>,
    window_id: Option<u32>,
    global_frame: Option<UiBounds>,
}

/// Frontmost application followed by all running GUI applications.
fn workspace_apps() -> (Option<RunningApp>, Vec<RunningApp>) {
    let workspace = NSWorkspace::sharedWorkspace();
    let frontmost = workspace.frontmostApplication().and_then(|app| {
        let pid = app.processIdentifier();
        (pid > 0).then(|| RunningApp {
            pid,
            name: app
                .localizedName()
                .map(|name| name.to_string())
                .unwrap_or_default(),
            bundle_id: app
                .bundleIdentifier()
                .map(|identifier| identifier.to_string())
                .unwrap_or_default(),
        })
    });
    let applications = workspace.runningApplications();
    let out = applications
        .iter()
        .filter_map(|app| {
            let pid = app.processIdentifier();
            (pid > 0).then(|| RunningApp {
                pid,
                name: app
                    .localizedName()
                    .map(|name| name.to_string())
                    .unwrap_or_default(),
                bundle_id: app
                    .bundleIdentifier()
                    .map(|identifier| identifier.to_string())
                    .unwrap_or_default(),
            })
        })
        .collect();
    (frontmost, out)
}

fn app_query_rank(app_name: &str, bundle_id: &str, query: &str) -> u8 {
    let app_name = app_name.to_lowercase();
    let bundle_id = bundle_id.to_lowercase();
    let bundle_leaf = bundle_id.rsplit('.').next().unwrap_or_default();
    if app_name == query || bundle_id == query || bundle_leaf == query {
        0
    } else if app_name.contains(query) || bundle_id.contains(query) {
        1
    } else {
        2
    }
}

/// Stable-rank explicit queries so an exact application name wins over a
/// background helper whose longer name merely contains the same text. The
/// bundle identifier's final component is also an exact alias, so a
/// locale-independent query such as "Finder" resolves localized "访达"
/// (`com.apple.finder`) before Finder Sync extensions.
///
/// Focus/frontmost order is preserved within each rank. Applications with no
/// name match remain candidates because the query may instead name a window.
fn rank_explicit_query_candidates(
    candidate_pids: &mut [i32],
    names: &HashMap<i32, String>,
    bundle_ids: &HashMap<i32, String>,
    query: Option<&str>,
) {
    let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
        return;
    };
    let query = query.to_lowercase();
    let ranks: HashMap<i32, u8> = names
        .iter()
        .map(|(&pid, name)| {
            let bundle_id = bundle_ids.get(&pid).map_or("", String::as_str);
            (pid, app_query_rank(name, bundle_id, &query))
        })
        .collect();
    candidate_pids.sort_by_key(|pid| ranks.get(pid).copied().unwrap_or(2));
}

fn deadline_error() -> UiReadError {
    UiReadError::new(
        UiReadErrorKind::TimedOut,
        "Accessibility target resolution exceeded its deadline",
    )
}

fn configure_timeout(app: &AXUIElement, deadline: Instant) -> Result<(), UiReadError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(deadline_error)?;
    // AX messaging timeout is per application element. Keep each provider call
    // short enough that the resolver can move to another candidate before the
    // request-wide deadline expires.
    app.set_messaging_timeout(remaining.as_secs_f32().clamp(0.05, 0.5))
        .map_err(|error| {
            UiReadError::new(
                UiReadErrorKind::BackendFailure,
                format!("failed to set AX provider timeout: {error:?}"),
            )
        })
}

fn bounds(element: &AXUIElement) -> Option<UiBounds> {
    element_rect(element).map(|(x, y, width, height)| UiBounds {
        x,
        y,
        width,
        height,
    })
}

fn dictionary_number(
    dictionary: &CFDictionary<CFString, core_foundation::base::CFType>,
    key: &str,
) -> Option<i64> {
    dictionary
        .find(CFString::new(key))
        .and_then(|value| value.downcast::<CFNumber>())
        .and_then(|number| number.to_i64())
}

/// Pixel-free CoreGraphics window metadata is the independent global anchor
/// for AX providers that report a window subtree in view-local coordinates.
/// This does not call ScreenCaptureKit or copy any pixels. If metadata is
/// unavailable (including a privacy-restricted provider), callers fail closed
/// by omitting coordinate bounds while retaining semantic activation.
pub(super) fn global_window_bounds(window_id: u32) -> Option<UiBounds> {
    let ids = create_window_list(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )?;
    let descriptions = create_description_from_array(ids)?;
    for dictionary in &descriptions {
        if dictionary_number(&dictionary, "kCGWindowNumber") != Some(i64::from(window_id)) {
            continue;
        }
        let raw_bounds = dictionary.find(CFString::new("kCGWindowBounds"))?;
        let bounds_dictionary = raw_bounds.downcast::<CFDictionary>()?;
        let rect = CGRect::from_dict_representation(&bounds_dictionary)?;
        if rect.size.width < 1.0 || rect.size.height < 1.0 {
            return None;
        }
        return Some(UiBounds {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        });
    }
    None
}

fn app_name_fallback(pid: i32) -> String {
    crate::platform::mac::geometry::proc_path(pid)
        .and_then(|path| {
            path.split('/')
                .find(|component| component.ends_with(".app"))
                .map(|component| component.trim_end_matches(".app").to_string())
        })
        .unwrap_or_else(|| format!("pid {pid}"))
}

fn app_windows(app: &AXUIElement, deadline: Instant) -> Result<Vec<AXUIElement>, UiReadError> {
    let mut windows = Vec::new();
    let mut seen = HashSet::new();
    for attribute in ["AXFocusedWindow", "AXMainWindow"] {
        if Instant::now() >= deadline {
            return Err(deadline_error());
        }
        if let Some(candidate) = element_attribute(app, attribute) {
            let key = candidate.as_concrete_TypeRef() as usize;
            if seen.insert(key) {
                windows.push(candidate);
            }
        }
    }
    if Instant::now() >= deadline {
        return Err(deadline_error());
    }
    for candidate in element_array(app, "AXWindows") {
        let key = candidate.as_concrete_TypeRef() as usize;
        if seen.insert(key) {
            windows.push(candidate);
        }
    }
    Ok(windows)
}

fn target_from_app(
    pid: i32,
    app_name: &str,
    bundle_id: &str,
    query: Option<&str>,
    deadline: Instant,
) -> Result<Option<UiTarget>, UiReadError> {
    let app = AXUIElement::application(pid);
    configure_timeout(&app, deadline)?;
    if ax_role(&app).is_empty() {
        return Ok(None);
    }
    let windows = app_windows(&app, deadline)?;
    let had_windows = !windows.is_empty();
    let query = query.map(str::trim).filter(|value| !value.is_empty());
    let query_lower = query.map(str::to_lowercase);
    let app_matches = query_lower
        .as_ref()
        .is_some_and(|query| app_query_rank(app_name, bundle_id, query) < 2);

    let mut candidates: Vec<WindowCandidate> = windows
        .into_iter()
        .filter_map(|window| {
            if Instant::now() >= deadline
                || ax_bool(&window, "AXMinimized") == Some(true)
                || ax_bool(&window, "AXHidden") == Some(true)
            {
                return None;
            }
            let title = ax_title(&window);
            let title_matches = query_lower
                .as_ref()
                .is_some_and(|query| title.to_lowercase().contains(query));
            if query.is_some() && !app_matches && !title_matches {
                return None;
            }
            let raw_frame = bounds(&window);
            if raw_frame.is_some_and(|frame| frame.width < 1.0 || frame.height < 1.0) {
                return None;
            }
            let window_id = ax_window_id(&window);
            let global_frame = window_id.and_then(global_window_bounds);
            Some(WindowCandidate {
                title,
                raw_frame,
                window_id,
                global_frame,
            })
        })
        .collect();

    if Instant::now() >= deadline {
        return Err(deadline_error());
    }

    // AXFocusedWindow/AXMainWindow were inserted first. For explicit app-name
    // matches, however, choose the largest real window so a tiny palette/PiP
    // does not win just because it happened to be focused.
    if query.is_some() && app_matches {
        candidates.sort_by(|a, b| {
            let area_a = a
                .global_frame
                .or(a.raw_frame)
                .map_or(0.0, |frame| frame.width * frame.height);
            let area_b = b
                .global_frame
                .or(b.raw_frame)
                .map_or(0.0, |frame| frame.width * frame.height);
            area_b
                .partial_cmp(&area_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    if let Some(candidate) = candidates.into_iter().next() {
        return Ok(Some(UiTarget {
            pid,
            app_name: app_name.to_string(),
            window_title: candidate.title,
            window_id: candidate.window_id.map(u64::from),
            bounds: candidate.global_frame,
        }));
    }

    // Windowless apps can still expose useful application-level AX content.
    // Only return them when the app name itself was the explicit match, or when
    // this is the preferred/focused no-query candidate.
    if !had_windows && ax_bool(&app, "AXHidden") != Some(true) && (query.is_none() || app_matches) {
        return Ok(Some(UiTarget {
            pid,
            app_name: app_name.to_string(),
            window_title: String::new(),
            window_id: None,
            bounds: None,
        }));
    }
    Ok(None)
}

fn focused_pid(deadline: Instant) -> Option<i32> {
    if Instant::now() >= deadline {
        return None;
    }
    let system = AXUIElement::system_wide();
    let remaining = deadline.saturating_duration_since(Instant::now());
    let _ = system.set_messaging_timeout(remaining.as_secs_f32().clamp(0.05, 0.5));
    element_attribute(&system, "AXFocusedApplication").and_then(|app| ax_pid(&app))
}

pub(super) fn resolve_target(
    query: Option<&str>,
    preferred_pid: Option<i32>,
    deadline: Instant,
) -> Result<UiTarget, UiReadError> {
    if !process_is_trusted() {
        return Err(UiReadError::new(
            UiReadErrorKind::PermissionDenied,
            "macOS Accessibility permission is not granted to the process hosting Nova; grant \
             Accessibility and retry. Do not substitute screenshot/OCR for a missing grant.",
        ));
    }

    if Instant::now() >= deadline {
        return Err(deadline_error());
    }
    let (frontmost, applications) = workspace_apps();
    let names: HashMap<i32, String> = applications
        .iter()
        .map(|app| (app.pid, app.name.clone()))
        .collect();
    let bundle_ids: HashMap<i32, String> = applications
        .iter()
        .map(|app| (app.pid, app.bundle_id.clone()))
        .collect();
    let mut candidate_pids = Vec::new();
    if let Some(pid) = preferred_pid.filter(|pid| names.contains_key(pid)) {
        candidate_pids.push(pid);
    }
    if let Some(pid) = focused_pid(deadline) {
        candidate_pids.push(pid);
    }
    if let Some(app) = frontmost {
        candidate_pids.push(app.pid);
    }
    candidate_pids.extend(applications.iter().map(|app| app.pid));
    rank_explicit_query_candidates(&mut candidate_pids, &names, &bundle_ids, query);

    let mut seen = HashSet::new();
    for pid in candidate_pids {
        if Instant::now() >= deadline {
            return Err(deadline_error());
        }
        if !seen.insert(pid) {
            continue;
        }
        let app_name = names
            .get(&pid)
            .cloned()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| app_name_fallback(pid));
        let bundle_id = bundle_ids.get(&pid).map_or("", String::as_str);
        if let Some(target) = target_from_app(pid, &app_name, bundle_id, query, deadline)? {
            return Ok(target);
        }
    }

    Err(UiReadError::new(
        UiReadErrorKind::TargetNotFound,
        match query {
            Some(query) => format!("no running AX application/window matches {query:?}"),
            None => "no focused or preferred AX application is available".to_string(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::{app_query_rank, rank_explicit_query_candidates};
    use std::collections::HashMap;

    #[test]
    fn locale_independent_bundle_alias_beats_background_helper_substring() {
        let names = HashMap::from([
            (10, "Setapp Finder Integration".to_string()),
            (20, "Terminal".to_string()),
            (30, "访达".to_string()),
        ]);
        let bundle_ids = HashMap::from([
            (
                10,
                "com.setapp.DesktopClient.SetappAgent.FinderSyncExt".to_string(),
            ),
            (20, "com.apple.Terminal".to_string()),
            (30, "com.apple.finder".to_string()),
        ]);
        let mut candidates = vec![10, 20, 30];

        rank_explicit_query_candidates(&mut candidates, &names, &bundle_ids, Some("finder"));

        assert_eq!(candidates, vec![30, 10, 20]);
        assert_eq!(app_query_rank("访达", "com.apple.finder", "finder"), 0);
    }

    #[test]
    fn partial_app_matches_keep_their_existing_focus_order() {
        let names = HashMap::from([
            (10, "Setapp Finder Integration".to_string()),
            (20, "Terminal".to_string()),
            (30, "iBoysoft Finder Integration".to_string()),
        ]);
        let bundle_ids = HashMap::new();
        let mut candidates = vec![30, 20, 10];

        rank_explicit_query_candidates(&mut candidates, &names, &bundle_ids, Some("Finder"));

        assert_eq!(candidates, vec![30, 10, 20]);
    }

    #[test]
    fn absent_or_blank_query_preserves_focus_order() {
        let names = HashMap::from([(10, "Finder".to_string()), (20, "Terminal".to_string())]);
        let bundle_ids = HashMap::new();

        for query in [None, Some("  ")] {
            let mut candidates = vec![20, 10];
            rank_explicit_query_candidates(&mut candidates, &names, &bundle_ids, query);
            assert_eq!(candidates, vec![20, 10]);
        }
    }
}
