//! Top-level mark discovery: combine the native-chrome tree walk with the web
//! hit-test pass, warming the Chromium tree until it stabilizes, and return the
//! actionable elements with their live handles.

use super::attrs::Rect;
use super::geometry;
use super::hittest::hit_test_elements;
use super::model::{AxHandle, UiElement};
use super::walk::Walk;
use super::warmth::enable_web_accessibility;
use accessibility::AXUIElement;
use std::time::Duration;

/// Cold Chromium web trees finish materializing within a few seconds; this caps
/// how long the warm-until-stable loop waits on a COLD first capture. Warm
/// captures plateau and exit long before this.
const COLD_MATERIALIZE_DEADLINE: Duration = Duration::from_millis(5000);
/// Pause between hit-test passes while waiting for the tree to finish building.
const WARM_STEP: Duration = Duration::from_millis(300);
/// Grid spacing (px) for the hit-test pass — must be smaller than a target row.
const HIT_STEP: f64 = 30.0;
/// Fraction of the window width treated as the left-edge native chrome (sidebar):
/// an actionable element past this is "real content". Used to decide whether a
/// web app exposed any semantic content or needs the geometry row fallback.
const CONTENT_REGION_FROM_LEFT: f64 = 0.2;

/// Collect up to `max` actionable elements from the application with `pid`.
/// Returns an empty vec if Accessibility permission is missing or the app
/// exposes no tree — callers degrade gracefully.
pub fn actionable_elements(pid: i32, max: usize) -> Vec<UiElement> {
    collect_actionable(pid, max, None)
        .into_iter()
        .map(|(el, _)| el)
        .collect()
}

/// Like [`actionable_elements`] but also returns each element's live AX handle,
/// so the server can cache them and later click by mark number (driving the AX
/// action directly, with a coordinate fallback).
pub fn collect_actionable(pid: i32, max: usize, clip: Option<Rect>) -> Vec<(UiElement, AxHandle)> {
    let app = AXUIElement::application(pid);
    let web_capable = enable_web_accessibility(&app);

    // The captured window's `CGWindowID`, so the view-local→global coordinate
    // lift fires for EXACTLY that window and not a same-sized sibling (the walk
    // roots at the app and reaches every window it owns). `None` for a full-
    // display walk or when it can't be resolved — the lift then falls back to
    // matching the window by frame size.
    let target_window = clip.and_then(|c| crate::tools::window::window_id_for_rect(pid, c));

    // Walk from the APP element (it reaches every window's content), but with
    // `clip` set to the visible window rect the off-screen cull throws away
    // background tabs and scrolled-off rows — so the budget actually reaches the
    // visible page. (Rooting at the focused window instead does NOT help: a
    // browser's web content often hangs off a sibling element, and the sidebar's
    // own off-screen collection still eats the budget without the cull.)
    let mut walk = Walk::run(&app, max, clip, target_window);
    // Chromium/Electron build their web tree ASYNCHRONOUSLY after the enable
    // signal, so a freshly-loaded page comes back with its native chrome but no
    // web area yet. Retry briefly when a web-capable app hasn't materialized its
    // AXWebArea (gating on `walk.out.is_empty()` instead missed this: the chrome
    // buttons are already in `out`, so the page silently marked chrome-only), or
    // when the web area is present but still empty. Once web content is found we
    // stop, so a warm capture pays nothing.
    let mut attempts = 0;
    while attempts < 2
        && ((web_capable && !walk.saw_web_area) || (walk.saw_web_area && walk.web_actionable == 0))
    {
        std::thread::sleep(Duration::from_millis(350));
        let retry = Walk::run(&app, max, clip, target_window);
        if retry.out.len() > walk.out.len() {
            walk = retry;
        }
        attempts += 1;
    }

    // AX trees often expose the same control at the same frame more than once;
    // collapse exact-frame duplicates so marks don't stack.
    let mut walked = walk.out;
    let mut seen = std::collections::HashSet::new();
    walked.retain(|(e, _)| seen.insert((e.x as i64, e.y as i64, e.width as i64, e.height as i64)));
    let mut result: Vec<(UiElement, AxHandle)> = walked
        .into_iter()
        .map(|(el, h)| (el, AxHandle(h)))
        .collect();

    // Hit-test pass: sample the visible window and pick up actionable elements
    // the tree walk couldn't reach (e.g. web rows Chromium buries under a
    // generic AXGroup skeleton). Skip samples inside what the walk already
    // covered, and merge deduped by frame.
    //
    // Because this process is long-lived (the MCP server) we can hold the AT
    // connection: re-assert the enable and re-hit-test until the actionable count
    // STABILIZES (plateaus) or the cold-materialization deadline, keeping the
    // richest pass. A warm tree plateaus after one extra pass, so a warm capture
    // stays cheap; only a cold first capture pays the full materialization wait.
    if let Some(clip) = clip {
        let covered: Vec<Rect> = result
            .iter()
            .map(|(e, _)| (e.x, e.y, e.width, e.height))
            .collect();
        let best = warm_until_stable(
            &app,
            clip,
            &covered,
            web_capable,
            max.saturating_sub(result.len()),
        );
        for (el, handle) in best {
            if result.len() >= max {
                break;
            }
            if seen.insert((el.x as i64, el.y as i64, el.width as i64, el.height as i64)) {
                result.push((el, handle));
            }
        }

        // Geometry fallback for div-rendered SPAs (QQ-mail and the like): if NO
        // actionable element landed in the content region — the main area past the
        // left-edge native chrome — then the page exposes no semantic AX (its rows
        // are plain `<div>`s). Mark the row-shaped containers by geometry and let
        // click_mark coordinate-click them. Gated this way, semantic pages (which
        // fill the content region with real AXLink/AXButton marks) never reach it.
        let content_x_min = clip.0 + clip.2 * CONTENT_REGION_FROM_LEFT;
        let has_content_marks = result.iter().any(|(e, _)| e.x >= content_x_min);
        if web_capable && !has_content_marks && result.len() < max {
            for (el, handle) in
                geometry::geometry_rows(clip, &covered, max.saturating_sub(result.len()))
            {
                if result.len() >= max {
                    break;
                }
                if seen.insert((el.x as i64, el.y as i64, el.width as i64, el.height as i64)) {
                    result.push((el, handle));
                }
            }
        }
    }
    result
}

/// Re-hit-test the content area, re-asserting the web-AX enable between passes,
/// until the actionable count plateaus (two non-growing passes), the cap is
/// reached, or the cold deadline expires. Returns the richest pass seen.
fn warm_until_stable(
    app: &AXUIElement,
    clip: Rect,
    covered: &[Rect],
    web_capable: bool,
    remaining: usize,
) -> Vec<(UiElement, AxHandle)> {
    let mut best: Vec<(UiElement, AxHandle)> = Vec::new();
    let start = std::time::Instant::now();
    let mut stable = 0u32;
    loop {
        let hits = hit_test_elements(clip, HIT_STEP, covered);
        if hits.len() > best.len() {
            best = hits;
            stable = 0;
        } else {
            stable += 1;
        }
        let timed_out = start.elapsed() >= COLD_MATERIALIZE_DEADLINE;
        // A plateau only counts once we've actually FOUND something: a cold web
        // tree returns 0 for its first passes, and treating that empty run as a
        // plateau would bail before the tree materializes (the bug that left
        // QQ-mail's rows unmarked). For a web-capable app keep probing — the
        // hit-testing itself drives Chromium to build the tree — until content
        // appears and stabilizes, or the deadline.
        let plateaued = stable >= 2 && !best.is_empty();
        if !web_capable || plateaued || best.len() >= remaining || timed_out {
            break;
        }
        // Keep the connection warm so Chromium finishes building the tree.
        enable_web_accessibility(app);
        std::thread::sleep(WARM_STEP);
    }
    best
}
