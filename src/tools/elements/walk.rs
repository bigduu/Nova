//! A bounded depth-first walk of an app's accessibility tree that follows every
//! child-bearing attribute (not just `AXChildren`) and culls subtrees outside the
//! visible window. This is the "native chrome" half of mark discovery; the web
//! content half is the hit-test pass (see [`super::hittest`]).

use super::attrs::{ax_label, element_array, element_rect, rects_intersect, Rect};
use super::model::{is_target, UiElement};
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
///
/// `MAX_NODES` also bounds total work for the path-based cycle guard (which may
/// re-walk a subtree shared across the DAG — e.g. WebKit's duplicate `AXWebArea`
/// roughly doubles the web tree). The budget is sized with headroom for that
/// re-walk so a normal page still reaches all of its UNIQUE elements rather than
/// exhausting the budget on duplicates; a normal page visits far fewer.
const MAX_NODES: usize = 6000;
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

/// Lifts a WKWebView window's view-local coordinates back into global screen space.
///
/// A WKWebView (every Tauri app, Safari, other WebKit hosts) often reports its
/// ENTIRE window subtree — window, scroll area, sidebar, page content — in
/// view-LOCAL coordinates (origin near `0,0`), even though the OS window-list API
/// reports that same window in correct GLOBAL screen coordinates. The two flip
/// between captures and can even disagree within one tree. A naive walk therefore
/// marks content at unusable off-screen positions, and the global off-screen cull
/// then throws it away — leaving only whatever happened to come back global (the
/// exact Bodhi/Tauri symptom: just the native chrome, none of the page).
///
/// The OS window rect (`clip`) is ground truth. Anchoring on the AXWindow's own
/// AX-reported origin vs `clip` yields the precise local→global offset (including
/// any title-bar inset), applied PER ELEMENT so a frame WebKit already reports
/// globally is left untouched.
#[derive(Clone, Copy)]
struct CoordLift {
    /// Local→global translation: `global = local + off`.
    off: (f64, f64),
    /// The window's true global origin (from `clip`). A frame whose origin sits
    /// left of / above this is in local space and must be lifted; one at/after it
    /// is already global and is left alone.
    gx: f64,
    gy: f64,
}

impl CoordLift {
    /// Derive the lift for a window from its AX-reported frame and the OS window
    /// rect. A zero offset (AX already agrees with the window list) makes [`lift`]
    /// a no-op.
    ///
    /// `clip` is the ONE captured window's rect, but the walk roots at the app and
    /// reaches every window it owns — so only the captured window may be anchored
    /// on `clip`; lifting a different window onto it would slide that window's
    /// elements into `clip` and escape the cull. The captured window is the one
    /// whose SIZE matches `clip` (AX reports the right size even when its origin is
    /// view-local); others return `None` and keep their own coords to be culled.
    fn derive(win: &AXUIElement, clip: Rect) -> Option<CoordLift> {
        let raw = element_rect(win)?;
        if (raw.2 - clip.2).abs() > 2.0 || (raw.3 - clip.3).abs() > 2.0 {
            return None;
        }
        Some(CoordLift {
            off: (clip.0 - raw.0, clip.1 - raw.1),
            gx: clip.0,
            gy: clip.1,
        })
    }

    /// Lift `r` into global coords if it sits in the window's local space.
    fn lift(lift: Option<CoordLift>, r: Rect) -> Rect {
        match lift {
            Some(c) if c.off != (0.0, 0.0) && (r.0 < c.gx - 1.0 || r.1 < c.gy - 1.0) => {
                (r.0 + c.off.0, r.1 + c.off.1, r.2, r.3)
            }
            _ => r,
        }
    }
}

/// A bounded depth-first walk that collects actionable elements with their live
/// handles, following every child-bearing attribute (not just `AXChildren`).
pub(crate) struct Walk {
    pub(crate) out: Vec<(UiElement, AXUIElement)>,
    visited: usize,
    /// Pointers of the elements on the CURRENT recursion path (ancestors of the
    /// node being visited). Re-entering one means the tree is cyclic (an app that
    /// lists itself as its own child — a real Chromium/WebKit AX glitch we've hit),
    /// which would otherwise self-recurse 80 deep and burn the node budget. Only
    /// ancestors are excluded, so a node legitimately shared by several parents is
    /// still reached via each — entries are removed on the way back up.
    path: std::collections::HashSet<usize>,
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
            path: std::collections::HashSet::new(),
            max,
            clip,
            saw_web_area: false,
            web_actionable: 0,
        };
        w.recurse(root, 0, false, None);
        w
    }

    fn recurse(&mut self, el: &AXUIElement, depth: usize, in_web: bool, lift: Option<CoordLift>) {
        use core_foundation::base::TCFType;
        if self.visited >= MAX_NODES || self.out.len() >= self.max || depth >= MAX_DEPTH {
            return;
        }
        // Cycle guard: skip only an element already on the CURRENT path. A
        // self-referential subtree (an app that lists itself as its own child — a
        // real Chromium/WebKit AX glitch) still can't loop forever, but an element
        // legitimately reachable from several parents IS visited via its real
        // path. A global visited-set was wrong here: WebKit exposes page content
        // under more than one container (a duplicate `AXWebArea`, plus the same
        // node under several child attributes), so a global set marked the content
        // "seen" while walking the sidebar and then dropped it — leaving Tauri/
        // WebKit windows with only their chrome marked. `MAX_NODES` bounds any
        // re-walk of a shared subtree; duplicate marks collapse downstream by frame.
        let ptr = el.as_concrete_TypeRef() as usize;
        if !self.path.insert(ptr) {
            return;
        }
        self.visited += 1;
        let role = ax_role(el);
        // At the window, anchor on the OS window rect: WebKit-hosted windows
        // (Tauri/Safari) report their whole subtree in view-local coords, so this
        // captures the local→global offset for everything below.
        let lift = match (lift, self.clip) {
            (None, Some(clip)) if role == "AXWindow" => CoordLift::derive(el, clip),
            _ => lift,
        };
        let in_web = in_web || role == "AXWebArea";
        if role == "AXWebArea" {
            self.saw_web_area = true;
        }
        // This element's frame, lifted into global coords when the window exposes
        // its content in view-local space — used for BOTH the cull and the stored
        // mark, so a page element is culled and positioned by where it is drawn.
        let rect = element_rect(el).map(|r| CoordLift::lift(lift, r));
        // Off-screen cull: a real-framed element entirely outside the visible
        // window is skipped along with its whole subtree.
        let culled = matches!(
            (self.clip, rect),
            (Some(clip), Some(r)) if r.2 >= 1.0 && r.3 >= 1.0 && !rects_intersect(r, clip)
        );
        if !culled {
            if is_target(el, &role) {
                if let Some(r) = rect {
                    if r.2 >= 1.0 && r.3 >= 1.0 {
                        if in_web {
                            self.web_actionable += 1;
                        }
                        self.out.push((
                            UiElement {
                                role: role.clone(),
                                label: ax_label(el),
                                x: r.0,
                                y: r.1,
                                width: r.2,
                                height: r.3,
                            },
                            el.clone(),
                        ));
                    }
                }
            }
            for child in child_elements(el, &role) {
                if self.visited >= MAX_NODES || self.out.len() >= self.max {
                    break;
                }
                self.recurse(&child, depth + 1, in_web, lift);
            }
        }
        // Leave the path so siblings can reach a node shared across the DAG.
        self.path.remove(&ptr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A WebKit window on a SECONDARY display: AX reports it (and its whole
    // subtree) in view-local coords with origin (0,30), while the OS window list
    // places it at (3439,1408). The lift must map local content back to where the
    // window actually is — verified against frames captured live from Bodhi.
    fn lift_secondary() -> CoordLift {
        // off = clip_origin - window_ax_origin = (3439,1408) - (0,30)
        CoordLift {
            off: (3439.0, 1378.0),
            gx: 3439.0,
            gy: 1408.0,
        }
    }

    #[test]
    fn lifts_view_local_content_to_global() {
        let c = Some(lift_secondary());
        // Input box reported local (303,1259) → global (3742,2637).
        assert_eq!(
            CoordLift::lift(c, (303.0, 1259.0, 976.0, 81.0)),
            (3742.0, 2637.0, 976.0, 81.0)
        );
        // The window's own local origin (0,30) → the OS origin (3439,1408).
        assert_eq!(
            CoordLift::lift(c, (0.0, 30.0, 1720.0, 1409.0)),
            (3439.0, 1408.0, 1720.0, 1409.0)
        );
    }

    #[test]
    fn leaves_already_global_frames_untouched() {
        let c = Some(lift_secondary());
        // A frame WebKit already reports at/after the global origin is not a
        // local coordinate — never double-shift it.
        assert_eq!(
            CoordLift::lift(c, (3449.0, 1697.0, 248.0, 35.0)),
            (3449.0, 1697.0, 248.0, 35.0)
        );
    }

    #[test]
    fn zero_offset_is_a_noop() {
        // Primary display: AX agrees with the window list, so nothing shifts.
        let c = Some(CoordLift {
            off: (0.0, 0.0),
            gx: 0.0,
            gy: 30.0,
        });
        assert_eq!(
            CoordLift::lift(c, (303.0, 1259.0, 976.0, 81.0)),
            (303.0, 1259.0, 976.0, 81.0)
        );
        assert_eq!(CoordLift::lift(None, (1.0, 2.0, 3.0, 4.0)), (1.0, 2.0, 3.0, 4.0));
    }
}
