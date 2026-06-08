//! Set-of-Mark element discovery via the macOS Accessibility (AX) API.
//!
//! Walks an application's accessibility tree and collects actionable UI elements
//! (buttons, links, fields, …) with their on-screen frames. The server draws
//! numbered boxes on these and hands the model a list with each element's center,
//! so it can pick a target by its labeled mark instead of estimating raw pixel
//! coordinates — the most reliable way to ground clicks.
//!
//! Requires the host process to have Accessibility permission; degrades to an
//! empty list otherwise (never errors).

use accessibility::{AXAttribute, AXUIElement, TreeVisitor, TreeWalker, TreeWalkerFlow};
use accessibility_sys::{kAXValueTypeCGPoint, kAXValueTypeCGSize, AXValueGetValue, AXValueRef};
use core_foundation::base::{CFType, TCFType};
use core_foundation::string::CFString;
use std::cell::RefCell;
use std::ffi::c_void;

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
fn is_actionable(role: &str) -> bool {
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
    )
}

/// Read an AXValue attribute holding two `f64` (a CGPoint or CGSize) via the raw
/// `AXValueGetValue` FFI. CGPoint is laid out `{x, y}` and CGSize `{width,
/// height}`, so both fill a `[f64; 2]` in order.
fn ax_pair(el: &AXUIElement, name: &'static str, value_type: u32) -> Option<(f64, f64)> {
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(name));
    let value = el.attribute(&attr).ok()?;
    let ax_ref = value.as_CFTypeRef() as AXValueRef;
    let mut out = [0.0f64; 2];
    // SAFETY: `ax_ref` is the AXValue returned for this attribute; we ask for the
    // matching value type and provide a correctly-sized destination buffer.
    let ok = unsafe { AXValueGetValue(ax_ref, value_type, out.as_mut_ptr() as *mut c_void) };
    ok.then_some((out[0], out[1]))
}

/// Extract a [`UiElement`] from an AX node if it is actionable and laid out.
fn element_info(el: &AXUIElement) -> Option<UiElement> {
    let role = el.attribute(&AXAttribute::role()).ok()?.to_string();
    if !is_actionable(&role) {
        return None;
    }
    let (x, y) = ax_pair(el, "AXPosition", kAXValueTypeCGPoint)?;
    let (width, height) = ax_pair(el, "AXSize", kAXValueTypeCGSize)?;
    if width < 1.0 || height < 1.0 {
        return None;
    }
    let label = el
        .attribute(&AXAttribute::title())
        .ok()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            el.attribute(&AXAttribute::description())
                .ok()
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    Some(UiElement {
        role,
        label,
        x,
        y,
        width,
        height,
    })
}

/// Visitor that accumulates actionable elements, capped at `max`.
struct Collector {
    out: RefCell<Vec<UiElement>>,
    max: usize,
}

impl TreeVisitor for Collector {
    fn enter_element(&self, element: &AXUIElement) -> TreeWalkerFlow {
        if self.out.borrow().len() >= self.max {
            return TreeWalkerFlow::Exit;
        }
        if let Some(info) = element_info(element) {
            self.out.borrow_mut().push(info);
        }
        TreeWalkerFlow::Continue
    }

    fn exit_element(&self, _element: &AXUIElement) {}
}

/// Collect up to `max` actionable elements from the application with `pid`.
/// Returns an empty vec if Accessibility permission is missing or the app
/// exposes no tree — callers degrade gracefully.
pub fn actionable_elements(pid: i32, max: usize) -> Vec<UiElement> {
    let app = AXUIElement::application(pid);
    let collector = Collector {
        out: RefCell::new(Vec::new()),
        max,
    };
    TreeWalker::new().walk(&app, &collector);

    // AX trees often expose the same control at the same frame more than once;
    // collapse exact-frame duplicates so marks don't stack.
    let mut out = collector.out.into_inner();
    let mut seen = std::collections::HashSet::new();
    out.retain(|e| seen.insert((e.x as i64, e.y as i64, e.width as i64, e.height as i64)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actionable_roles_recognized() {
        assert!(is_actionable("AXButton"));
        assert!(is_actionable("AXLink"));
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
