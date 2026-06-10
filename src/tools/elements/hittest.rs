//! Element discovery by HIT-TESTING a grid of points across the visible window.
//! Each sample asks the window server what is actually rendered at a pixel, then
//! climbs to its actionable wrapper — so this reaches web content the parent→
//! child tree walk can't (Chromium buries page content under a generic AXGroup
//! skeleton, and the DFS budget is eaten by native chrome before it gets there).

use super::attrs::{ax_role, point_in_rect, Rect};
use super::model::{element_info_with_role, is_target, AxHandle, UiElement};
use accessibility::{AXAttribute, AXUIElement};
use accessibility_sys::{kAXErrorSuccess, AXUIElementCopyElementAtPosition, AXUIElementRef};
use core_foundation::base::TCFType;

/// The deepest accessible element at global screen point `(x, y)`, via AX
/// hit-testing. This reaches elements the parent→child tree walk can't: the
/// hit-test asks the window server what is actually rendered at that pixel.
pub(crate) fn element_at_position(
    system_wide: &AXUIElement,
    x: f64,
    y: f64,
) -> Option<AXUIElement> {
    let mut el: AXUIElementRef = std::ptr::null_mut();
    // SAFETY: standard AX hit-test; on success `el` is a +1 reference we own.
    let err = unsafe {
        AXUIElementCopyElementAtPosition(
            system_wide.as_concrete_TypeRef(),
            x as f32,
            y as f32,
            &mut el,
        )
    };
    if err != kAXErrorSuccess || el.is_null() {
        return None;
    }
    Some(unsafe { AXUIElement::wrap_under_create_rule(el) })
}

/// `el` if it is an actionable target, else its nearest actionable ancestor
/// (bounded). Hit-testing returns the deepest element (usually static text); the
/// clickable target is the row / link / button wrapping it.
pub(crate) fn actionable_self_or_ancestor(el: &AXUIElement) -> Option<AXUIElement> {
    let mut cur = el.clone();
    for _ in 0..8 {
        let role = ax_role(&cur);
        if is_target(&cur, &role) {
            return Some(cur);
        }
        cur = cur.attribute(&AXAttribute::parent()).ok()?;
    }
    None
}

/// Discover actionable elements by hit-testing a grid of points across `clip`
/// (the visible window rect, global logical). `step` px between samples (must be
/// smaller than a target row to land on each one).
///
/// The samples are INDEPENDENT, so the grid's rows are split across a small pool
/// of worker threads — each makes its own system-wide handle and AX calls, so
/// there is no shared state, and the per-sample mach round-trips overlap instead
/// of running back-to-back. Results are merged and deduped by frame.
///
/// `covered` are frames already found by the tree walk (the native chrome /
/// sidebar); samples landing inside them are skipped so we only spend hit-tests
/// on the region the walk couldn't reach.
pub(crate) fn hit_test_elements(
    clip: Rect,
    step: f64,
    covered: &[Rect],
) -> Vec<(UiElement, AxHandle)> {
    let (cx, cy, cw, ch) = clip;

    // The y of each sample row.
    let mut rows = Vec::new();
    let mut y = cy + step / 2.0;
    while y < cy + ch {
        rows.push(y);
        y += step;
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let chunk = rows.len().div_ceil(workers).max(1);

    // One worker per row-band. Each scans its rows and returns the actionable
    // elements it found (deduped within the band).
    let bands: Vec<Vec<(UiElement, AxHandle)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = rows
            .chunks(chunk)
            .map(|band| {
                let band: Vec<f64> = band.to_vec();
                scope.spawn(move || {
                    let system_wide = AXUIElement::system_wide();
                    let mut seen: std::collections::HashSet<(i64, i64, i64, i64)> =
                        std::collections::HashSet::new();
                    let mut out = Vec::new();
                    for &yy in &band {
                        let mut x = cx + step / 2.0;
                        while x < cx + cw {
                            // Skip samples the tree walk already covered.
                            if covered.iter().any(|&r| point_in_rect(x, yy, r)) {
                                x += step;
                                continue;
                            }
                            if let Some(hit) = element_at_position(&system_wide, x, yy) {
                                if let Some(target) = actionable_self_or_ancestor(&hit) {
                                    let role = ax_role(&target);
                                    if let Some(info) = element_info_with_role(&target, &role) {
                                        let key = (
                                            info.x as i64,
                                            info.y as i64,
                                            info.width as i64,
                                            info.height as i64,
                                        );
                                        if seen.insert(key) {
                                            out.push((info, AxHandle(target)));
                                        }
                                    }
                                }
                            }
                            x += step;
                        }
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });

    // Merge bands, deduping by frame across band boundaries.
    let mut seen: std::collections::HashSet<(i64, i64, i64, i64)> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for band in bands {
        for (info, handle) in band {
            let key = (
                info.x as i64,
                info.y as i64,
                info.width as i64,
                info.height as i64,
            );
            if seen.insert(key) {
                out.push((info, handle));
            }
        }
    }
    out
}
