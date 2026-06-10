//! A bounded depth-first walk of an app's accessibility tree that follows every
//! child-bearing attribute (not just `AXChildren`) and culls subtrees outside the
//! visible window. This is the "native chrome" half of mark discovery; the web
//! content half is the hit-test pass (see [`super::hittest`]).

use super::attrs::{element_array, element_rect, rects_intersect, Rect};
use super::model::{element_info_with_role, UiElement};
use crate::tools::elements::attrs::ax_role;
use accessibility::AXUIElement;

/// Attributes that hold an element's children, in the order we follow them.
/// Tables, outlines, lists and WEB content expose their real children via
/// `AXRows` / `AXContents` / `AXVisibleChildren` rather than `AXChildren` — the
/// stock tree walker follows only `AXChildren`, which is exactly why marks never
/// reached web page content. Following all four is the penetration fix.
const CHILD_ATTRS: &[&str] = &["AXChildren", "AXRows", "AXContents", "AXVisibleChildren"];

/// For a big scrollable container (a mail list with thousands of rows) `AXChildren`
/// / `AXRows` return EVERY row, including the thousands off-screen — pulling and
/// walking those is what blew the capture timeout. For these roles prefer
/// `AXVisibleChildren` (just what's on screen) first, then `AXRows`, and keep
/// `AXChildren` only as a LAST-resort fallback (some web lists expose their rows
/// solely under `AXChildren`); the per-node cap bounds the cost.
const LIST_CHILD_ATTRS: &[&str] = &["AXVisibleChildren", "AXRows", "AXContents", "AXChildren"];

fn child_attrs_for(role: &str) -> &'static [&'static str] {
    match role {
        "AXOutline" | "AXList" | "AXTable" | "AXBrowser" | "AXGrid" => LIST_CHILD_ATTRS,
        _ => CHILD_ATTRS,
    }
}

/// Hard caps so a deep/wide web tree can't stall the (default-on) marks walk:
/// bound total nodes visited, recursion depth, and children taken per node (a
/// single container can list thousands). The 20s capture timeout is the outer
/// backstop; these keep a normal page well under it.
const MAX_NODES: usize = 3000;
pub(crate) const MAX_DEPTH: usize = 80;
const MAX_CHILDREN_PER_NODE: usize = 400;

/// All distinct children of `el` (whose role is `role`), gathered across the
/// child-bearing attributes appropriate for that role. The same node frequently
/// appears under several attributes, so dedupe by ref identity — an O(1) hash on
/// the pointer, NOT an O(n²) CFEqual scan (that scan over a thousands-row list is
/// what stalled the walk). Capped at [`MAX_CHILDREN_PER_NODE`].
pub(crate) fn child_elements(el: &AXUIElement, role: &str) -> Vec<AXUIElement> {
    use core_foundation::base::TCFType;
    let mut out: Vec<AXUIElement> = Vec::new();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for name in child_attrs_for(role) {
        for child in element_array(el, name) {
            if seen.insert(child.as_concrete_TypeRef() as usize) {
                out.push(child);
                if out.len() >= MAX_CHILDREN_PER_NODE {
                    return out;
                }
            }
        }
    }
    out
}

/// A bounded depth-first walk that collects actionable elements with their live
/// handles, following every child-bearing attribute (not just `AXChildren`).
pub(crate) struct Walk {
    pub(crate) out: Vec<(UiElement, AXUIElement)>,
    visited: usize,
    /// Element pointers already entered, so a CYCLIC tree (e.g. an app that lists
    /// itself as its own child — a real Chromium/Electron AX glitch we've hit)
    /// can't send the walk into an 80-deep self-recursion that burns the whole
    /// node budget before reaching the actual windows.
    seen_ptrs: std::collections::HashSet<usize>,
    max: usize,
    /// Visible window rectangle. Any element whose frame lies ENTIRELY outside
    /// this is skipped together with its subtree — which prunes the hundreds of
    /// scrolled-off rows (e.g. Arc's sidebar collection) that otherwise exhaust
    /// the node budget before the walk reaches the visible main content.
    clip: Option<Rect>,
    /// Whether an `AXWebArea` was seen — i.e. this is a browser/Electron view.
    pub(crate) saw_web_area: bool,
    /// Actionable elements found underneath a web area (0 ⇒ the web tree is
    /// present but still building, so a retry is worthwhile).
    pub(crate) web_actionable: usize,
}

impl Walk {
    pub(crate) fn run(root: &AXUIElement, max: usize, clip: Option<Rect>) -> Walk {
        let mut w = Walk {
            out: Vec::new(),
            visited: 0,
            seen_ptrs: std::collections::HashSet::new(),
            max,
            clip,
            saw_web_area: false,
            web_actionable: 0,
        };
        w.recurse(root, 0, false);
        w
    }

    fn recurse(&mut self, el: &AXUIElement, depth: usize, in_web: bool) {
        use core_foundation::base::TCFType;
        if self.visited >= MAX_NODES || self.out.len() >= self.max || depth >= MAX_DEPTH {
            return;
        }
        // Cycle guard: never re-enter an element we've already walked, so a
        // self-referential subtree can't loop until the budget is exhausted.
        if !self.seen_ptrs.insert(el.as_concrete_TypeRef() as usize) {
            return;
        }
        self.visited += 1;
        let role = ax_role(el);
        // Off-screen cull: a real-framed element entirely outside the visible
        // window is skipped along with its whole subtree.
        if let (Some(clip), Some(r)) = (self.clip, element_rect(el)) {
            if r.2 >= 1.0 && r.3 >= 1.0 && !rects_intersect(r, clip) {
                return;
            }
        }
        let in_web = in_web || role == "AXWebArea";
        if role == "AXWebArea" {
            self.saw_web_area = true;
        }
        if let Some(info) = element_info_with_role(el, &role) {
            if in_web {
                self.web_actionable += 1;
            }
            self.out.push((info, el.clone()));
        }
        for child in child_elements(el, &role) {
            if self.visited >= MAX_NODES || self.out.len() >= self.max {
                break;
            }
            self.recurse(&child, depth + 1, in_web);
        }
    }
}
