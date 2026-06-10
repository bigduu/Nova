//! Fallback discovery for div-rendered web pages that expose NO semantic AX
//! roles (QQ-mail and similar SPAs): probing proves the content is just a wall of
//! actionless `AXGroup`/`AXStaticText`/`AXImage`, because the clickable rows are
//! plain `<div>`s with JS handlers and the page sets no ARIA. Chromium only
//! synthesizes pressable roles for those under a registered screen-reader AT
//! (Homerow/VoiceOver), which our enable signal does not flip on.
//!
//! But the rows ARE in the tree — as labeled, row-shaped `AXGroup`s with real
//! frames. So when the AX-first pass finds nothing in a web area, we mark those
//! row containers by GEOMETRY and let `click_mark` coordinate-click their center
//! (a real synthesized mouse event triggers the JS handler). This is gated to the
//! empty-web-area case, so semantic pages (GitHub) never hit it.

use super::attrs::{ax_label, ax_role, ax_value_string, element_rect, point_in_rect, Rect};
use super::hittest::element_at_position;
use super::model::{AxHandle, UiElement};
use super::walk::child_elements;
use accessibility::{AXAttribute, AXUIElement};

/// A row candidate must span at least this fraction of the window width (rows run
/// nearly the full content width; sidebar items and inline spans are far narrower
/// and get rejected, so we needn't know where the content area starts).
const ROW_MIN_WIDTH_FRAC: f64 = 0.5;
/// Plausible height band for a single list/mail row (one to a few text lines).
/// Excludes inline spans (too short) and the whole list container (too tall).
const ROW_MIN_HEIGHT: f64 = 18.0;
const ROW_MAX_HEIGHT: f64 = 170.0;
/// Vertical grid spacing — smaller than a row so each row gets sampled.
const PROBE_STEP: f64 = 20.0;

/// Climb from a hit leaf to the row-shaped container that holds it: the first
/// ancestor (within 8 levels) that is nearly content-wide and one-row tall. The
/// container itself is an unlabeled `AXGroup` (div-rendered pages put no label on
/// it), so we synthesize a label from its descendant text. `None` if no ancestor
/// looks like a row — so non-row hits (toolbar icons, page chrome) are dropped.
fn row_container(el: &AXUIElement, min_width: f64) -> Option<(AXUIElement, Rect, String)> {
    let mut cur = el.clone();
    for _ in 0..8 {
        if let Some(r) = element_rect(&cur) {
            if is_row_shaped(r, min_width) {
                let label = synthesize_label(&cur);
                return Some((cur, r, label));
            }
        }
        cur = cur.attribute(&AXAttribute::parent()).ok()?;
    }
    None
}

/// Whether a frame looks like a single list/mail row: nearly content-wide and one
/// to a few text lines tall. Excludes inline spans/icons (too short or narrow) and
/// the whole list container (too tall).
fn is_row_shaped((_, _, w, h): Rect, min_width: f64) -> bool {
    w >= min_width && (ROW_MIN_HEIGHT..=ROW_MAX_HEIGHT).contains(&h)
}

/// How many raw text fragments to gather before deduping, how many DISTINCT ones
/// to keep in the label, and the overall length cap — enough to identify a row
/// (sender + subject) without dragging in the whole preview. Div-rendered pages
/// often emit the same text several times (avatar alt, name, tooltip), so we
/// gather generously then dedup.
const LABEL_GATHER: usize = 12;
const LABEL_KEEP_DISTINCT: usize = 4;
const LABEL_MAX_CHARS: usize = 90;

/// Build a human label for an unlabeled row from its descendant `AXStaticText`s
/// (div-rendered pages keep the text in `AXValue`, not a title). Order-preserving
/// and deduped so a row whose sender is repeated reads "sender subject …", not
/// "sender sender sender sender".
fn synthesize_label(row: &AXUIElement) -> String {
    let mut parts: Vec<String> = Vec::new();
    collect_text(row, 0, &mut parts);

    let mut kept: Vec<String> = Vec::new();
    for p in parts {
        let p = p.trim().to_string();
        if p.is_empty() || kept.iter().any(|k| k == &p) {
            continue;
        }
        kept.push(p);
        if kept.len() >= LABEL_KEEP_DISTINCT {
            break;
        }
    }
    kept.join(" ").chars().take(LABEL_MAX_CHARS).collect::<String>().trim().to_string()
}

fn collect_text(el: &AXUIElement, depth: usize, parts: &mut Vec<String>) {
    if depth >= 5 || parts.len() >= LABEL_GATHER {
        return;
    }
    let role = ax_role(el);
    if role == "AXStaticText" {
        let t = {
            let v = ax_value_string(el);
            if v.is_empty() {
                ax_label(el)
            } else {
                v
            }
        };
        if !t.trim().is_empty() {
            parts.push(t);
        }
    }
    for child in child_elements(el, &role) {
        if parts.len() >= LABEL_GATHER {
            break;
        }
        collect_text(&child, depth + 1, parts);
    }
}

/// Mark row-shaped labeled containers across `clip` by geometry, skipping samples
/// inside `covered` (what the tree walk already marked — the native sidebar) and
/// capping at `max`. Returns `(UiElement, AxHandle)` whose click falls through to
/// a coordinate click at the row center (the divs expose no AX action).
pub(crate) fn geometry_rows(clip: Rect, covered: &[Rect], max: usize) -> Vec<(UiElement, AxHandle)> {
    let (cx, cy, cw, ch) = clip;
    let min_width = cw * ROW_MIN_WIDTH_FRAC;
    let system_wide = AXUIElement::system_wide();
    let mut seen: std::collections::HashSet<(i64, i64, i64, i64)> =
        std::collections::HashSet::new();
    let mut out: Vec<(UiElement, AxHandle)> = Vec::new();

    // Probe down the middle of the content area (one column is enough — every row
    // spans it), stepping finer than a row so none is skipped.
    let probe_x = cx + cw * 0.5;
    let mut y = cy + PROBE_STEP / 2.0;
    while y < cy + ch {
        if covered.iter().any(|&r| point_in_rect(probe_x, y, r)) {
            y += PROBE_STEP;
            continue;
        }
        if let Some(hit) = element_at_position(&system_wide, probe_x, y) {
            if let Some((el, (rx, ry, rw, rh), label)) = row_container(&hit, min_width) {
                let key = (rx as i64, ry as i64, rw as i64, rh as i64);
                if seen.insert(key) {
                    out.push((
                        UiElement {
                            role: "AXGroup".to_string(),
                            label,
                            x: rx,
                            y: ry,
                            width: rw,
                            height: rh,
                        },
                        AxHandle(el),
                    ));
                    if out.len() >= max {
                        break;
                    }
                }
            }
        }
        y += PROBE_STEP;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_shape_accepts_wide_short_rejects_else() {
        let min_w = 860.0; // 50% of a 1720-wide window
        // A real QQ-mail email row: 1218x26.
        assert!(is_row_shaped((2195.0, 183.0, 1218.0, 26.0), min_w));
        // The whole list container: too tall.
        assert!(!is_row_shaped((2192.0, 137.0, 1224.0, 1292.0), min_w));
        // An inline text span / toolbar chip: too narrow.
        assert!(!is_row_shaped((2438.0, 101.0, 814.0, 29.0), min_w));
        // A hairline separator: too short.
        assert!(!is_row_shaped((2195.0, 183.0, 1218.0, 4.0), min_w));
    }
}
