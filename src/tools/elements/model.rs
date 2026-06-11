//! The element value types and the live AX handle used for index-based clicking.
//!
//! [`UiElement`] is the plain, `Send` description that becomes a numbered mark.
//! [`AxHandle`] wraps the live `AXUIElement` so a later `click_mark` can drive the
//! control straight through its AX action (true background, no cursor), with a
//! coordinate-click fallback. [`CachedElement`] pairs the two with the mark number.

use super::attrs::{ax_label, ax_pair, ax_role, click_action_for, element_rect};
use super::walk::child_elements;
use accessibility::{AXAttribute, AXUIElement};
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;

/// An actionable UI element with its frame in global logical points.
#[derive(Debug, Clone)]
pub struct UiElement {
    pub role: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl UiElement {
    /// Center of the element in global logical points.
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// AX roles worth offering as click targets.
pub(crate) fn is_actionable(role: &str) -> bool {
    matches!(
        role,
        "AXButton"
            | "AXLink"
            | "AXTextField"
            | "AXTextArea"
            | "AXCheckBox"
            | "AXRadioButton"
            | "AXPopUpButton"
            | "AXMenuButton"
            | "AXMenuItem"
            | "AXTab"
            | "AXDisclosureTriangle"
            | "AXComboBox"
            | "AXSlider"
            | "AXStepper"
            | "AXSegment"
            | "AXToolbarButton"
            // a list/table row (e.g. an email in a mail list) — a common target
            // whose press often lives on a descendant; AxHandle::click finds it.
            | "AXRow"
    )
}

/// Whether an element is worth offering as a click target: either its role is in
/// the allowlist, OR it actually exposes a click-like AX action. The action-based
/// half is what makes web/Electron content (links, list rows, custom controls
/// that report a generic role but respond to AXPress) show up — a fixed role
/// allowlist alone misses most of a web page. The `||` short-circuits so the
/// extra `action_names()` round-trip only happens for non-allowlisted roles.
pub(crate) fn is_target(el: &AXUIElement, role: &str) -> bool {
    is_actionable(role) || click_action_for(el).is_some()
}

/// Extract a [`UiElement`] from an AX node (given its already-read `role`) if it
/// is a target and laid out on screen.
pub(crate) fn element_info_with_role(el: &AXUIElement, role: &str) -> Option<UiElement> {
    if !is_target(el, role) {
        return None;
    }
    let (x, y, width, height) = element_rect(el)?;
    if width < 1.0 || height < 1.0 {
        return None;
    }
    Some(UiElement {
        role: role.to_string(),
        label: ax_label(el),
        x,
        y,
        width,
        height,
    })
}

/// How deep to look for a clickable descendant when an element itself exposes no
/// action (a mail row → its inner link is usually 1–3 levels down).
const DESCENDANT_CLICK_DEPTH: usize = 4;
/// How far up to look for a clickable ancestor (a cell → its row → a link wrapper).
const ANCESTOR_CLICK_DEPTH: usize = 4;

/// First descendant (within `depth`) that exposes a click-like action.
fn first_descendant_action(el: &AXUIElement, depth: usize) -> Option<(AXUIElement, &'static str)> {
    if depth == 0 {
        return None;
    }
    for child in child_elements(el, &ax_role(el)) {
        if let Some(action) = click_action_for(&child) {
            return Some((child, action));
        }
        if let Some(found) = first_descendant_action(&child, depth - 1) {
            return Some(found);
        }
    }
    None
}

/// First ancestor (within `depth`) that exposes a click-like action.
fn first_ancestor_action(el: &AXUIElement, depth: usize) -> Option<(AXUIElement, &'static str)> {
    let mut cur = el.attribute(&AXAttribute::parent()).ok()?;
    for _ in 0..depth {
        if let Some(action) = click_action_for(&cur) {
            return Some((cur, action));
        }
        cur = cur.attribute(&AXAttribute::parent()).ok()?;
    }
    None
}

/// A live `AXUIElement` handle wrapped so it can be cached in the server's
/// (async, `Arc`-shared) state between a `marks` screenshot and a later
/// `click_mark`. The model picks an element by its mark NUMBER; we keep the real
/// handle so the click drives the control through its AX action (true
/// background, no coordinates), falling back to a coordinate click only if the
/// action is unsupported.
///
/// SAFETY (`Send`): `AXUIElement` is an atomically reference-counted Core
/// Foundation object, and the AX *client* calls used here (`AXUIElementPerform
/// Action`, `…CopyActionNames`, `…SetAttributeValue`) are safe to invoke from a
/// background thread. Every access goes through the server's `Mutex` and runs on
/// a `spawn_blocking` worker, so a handle is never touched concurrently — moving
/// it across threads and using it under that lock is sound. We deliberately do
/// NOT assert `Sync` (there is no shared concurrent access); `Send` is all the
/// cache needs.
pub struct AxHandle(pub(crate) AXUIElement);

// SAFETY: see the `AxHandle` doc comment — access is serialized behind the
// server's mutex and the AX client calls are thread-safe.
unsafe impl Send for AxHandle {}

impl Clone for AxHandle {
    fn clone(&self) -> Self {
        // CFType clone is a CFRetain — thread-safe and cheap.
        AxHandle(self.0.clone())
    }
}

impl std::fmt::Debug for AxHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AxHandle({} {:?})", ax_role(&self.0), ax_label(&self.0))
    }
}

impl AxHandle {
    /// Drive the control through the Accessibility tree (no cursor movement).
    /// Tries the element's own click action first, then a descendant's — a list
    /// row or web container often delegates its press to an inner button/link, so
    /// without this the click would needlessly fall back to a coordinate click.
    /// Returns the action performed, or an error if nothing in the subtree is
    /// clickable.
    pub fn click(&self) -> Result<&'static str, String> {
        if let Some(action) = click_action_for(&self.0) {
            self.0
                .perform_action(&CFString::new(action))
                .map_err(|e| format!("{action} failed: {}", super::attrs::ax_err(&e)))?;
            return Ok(action);
        }
        // A list row / generic web container often delegates its press to an
        // inner control (descendant) or a wrapping link (ancestor) — check both
        // before giving up, so the click stays on the cursor-free AX path.
        if let Some((el, action)) = first_descendant_action(&self.0, DESCENDANT_CLICK_DEPTH)
            .or_else(|| first_ancestor_action(&self.0, ANCESTOR_CLICK_DEPTH))
        {
            el.perform_action(&CFString::new(action)).map_err(|e| {
                format!("{action} on relative failed: {}", super::attrs::ax_err(&e))
            })?;
            return Ok(action);
        }
        Err("element (and its descendants/ancestors) expose no click action".to_string())
    }

    /// Whether this handle still points at a live, laid-out element. After a page
    /// refresh or navigation Chromium destroys and rebuilds its AX tree, leaving
    /// cached handles dangling; a dangling handle stops reporting a role/frame.
    /// The click path checks this so a stale mark reports "re-capture" instead of
    /// silently pressing the wrong thing (or a destroyed node).
    pub fn is_alive(&self) -> bool {
        // A live element still answers for its role; a destroyed one returns an
        // error (or empty). Require a non-empty role AND a real frame so a
        // re-used-but-collapsed node doesn't read as live.
        if ax_role(&self.0).is_empty() {
            return false;
        }
        matches!(ax_pair(&self.0, "AXSize", accessibility_sys::kAXValueTypeCGSize),
            Some((w, h)) if w >= 1.0 && h >= 1.0)
    }

    /// This handle's current center in global logical points, if still laid out —
    /// used to re-validate a cached mark against where it now sits.
    pub fn current_center(&self) -> Option<(f64, f64)> {
        let (x, y, w, h) = element_rect(&self.0)?;
        Some((x + w / 2.0, y + h / 2.0))
    }

    /// Borrow the underlying element (for the diagnostics layer).
    pub(crate) fn element(&self) -> &AXUIElement {
        &self.0
    }
}

/// A marked actionable element kept for index-based clicking. Pairs the mark
/// NUMBER with the live AX handle and the element's global-logical center (the
/// coordinate-click fallback target).
#[derive(Debug, Clone)]
pub struct CachedElement {
    pub number: u32,
    pub handle: AxHandle,
    /// Global logical center — the fallback click point if the AX action fails.
    pub center: (f64, f64),
    pub role: String,
    pub label: String,
    /// Owning process — used to raise the app before a coordinate fallback so
    /// the click lands on the target instead of merely focusing the window.
    pub pid: i32,
}

/// Bring the application with `pid` to the front (so a subsequent coordinate
/// click actually hits its content rather than just activating the window —
/// the "first click only focuses" problem). Best-effort via AX `AXFrontmost`.
pub fn raise_app(pid: i32) {
    let app = AXUIElement::application(pid);
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string("AXFrontmost"));
    let _ = app.set_attribute(&attr, CFBoolean::true_value().as_CFType());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actionable_roles_recognized() {
        assert!(is_actionable("AXButton"));
        assert!(is_actionable("AXLink"));
        assert!(is_actionable("AXRow"));
        assert!(!is_actionable("AXGroup"));
        assert!(!is_actionable("AXStaticText"));
    }

    #[test]
    fn center_is_midpoint() {
        let e = UiElement {
            role: "AXButton".into(),
            label: "OK".into(),
            x: 100.0,
            y: 200.0,
            width: 80.0,
            height: 40.0,
        };
        assert_eq!(e.center(), (140.0, 220.0));
    }
}
