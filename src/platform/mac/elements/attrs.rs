//! Low-level reads off an `AXUIElement`: role, label, frame, supported actions,
//! plus the geometry primitives the walk and hit-test share. This is the only
//! layer that touches the raw `AXValueGetValue` / `AXUIElementCopyAttributeValue`
//! FFI, so the `unsafe` is contained here.

use accessibility::{AXAttribute, AXUIElement, Error as AxError};
use accessibility_sys::{
    error_string, kAXErrorSuccess, kAXValueTypeCGPoint, kAXValueTypeCGSize, AXIsProcessTrusted,
    AXUIElementCopyAttributeValue, AXUIElementGetPid, AXUIElementRef, AXValueGetValue, AXValueRef,
};
use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use std::ffi::c_void;

/// A global-logical rectangle `(x, y, width, height)`.
pub(crate) type Rect = (f64, f64, f64, f64);

extern "C" {
    /// Private but long-stable Accessibility API (used by yabai, Hammerspoon,
    /// and friends): writes the element's owning `CGWindowID`. Returns
    /// `kAXErrorSuccess` (0) on success. Linked from the same framework that
    /// provides the rest of the AX FFI.
    fn _AXUIElementGetWindow(element: AXUIElementRef, identifier: *mut u32) -> i32;
}

/// The `CGWindowID` of AX window element `el` (same id space as
/// ScreenCaptureKit's `SCWindow::window_id`), via `_AXUIElementGetWindow`. Lets
/// mark discovery match an AX window node to the captured window EXACTLY rather
/// than by frame size. `None` if `el` is not a window or the call fails.
pub(crate) fn ax_window_id(el: &AXUIElement) -> Option<u32> {
    let mut id: u32 = 0;
    // SAFETY: `as_concrete_TypeRef` is a live `AXUIElementRef`; the call only
    // writes one `u32` through our out-pointer and returns an `AXError`.
    let err = unsafe { _AXUIElementGetWindow(el.as_concrete_TypeRef(), &mut id) };
    (err == kAXErrorSuccess && id != 0).then_some(id)
}

/// Click-like AX actions, in preference order. Different controls expose
/// different ones — a button has `AXPress`, a list row / file often only has
/// `AXOpen` or `AXPick` — so we perform whichever the element actually supports
/// rather than assuming `AXPress` (which fails on rows with a cryptic code).
pub(crate) const CLICK_ACTIONS: &[&str] = &["AXPress", "AXOpen", "AXPick", "AXConfirm"];

/// Read an AXValue attribute holding two `f64` (a CGPoint or CGSize) via the raw
/// `AXValueGetValue` FFI. CGPoint is laid out `{x, y}` and CGSize `{width,
/// height}`, so both fill a `[f64; 2]` in order.
pub(crate) fn ax_pair(el: &AXUIElement, name: &'static str, value_type: u32) -> Option<(f64, f64)> {
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(name));
    let value = el.attribute(&attr).ok()?;
    let ax_ref = value.as_CFTypeRef() as AXValueRef;
    let mut out = [0.0f64; 2];
    // SAFETY: `ax_ref` is the AXValue returned for this attribute; we ask for the
    // matching value type and provide a correctly-sized destination buffer.
    let ok = unsafe { AXValueGetValue(ax_ref, value_type, out.as_mut_ptr() as *mut c_void) };
    ok.then_some((out[0], out[1]))
}

/// Read an AX attribute that holds an array of elements (children / rows /
/// contents / visible-children) by name. Empty on any failure or non-array.
pub(crate) fn element_array(el: &AXUIElement, name: &str) -> Vec<AXUIElement> {
    let attr = CFString::new(name);
    let mut value: CFTypeRef = std::ptr::null();
    // SAFETY: standard AX copy-attribute call; on success `value` is a +1
    // reference we take ownership of below.
    let err = unsafe {
        AXUIElementCopyAttributeValue(
            el.as_concrete_TypeRef(),
            attr.as_concrete_TypeRef(),
            &mut value,
        )
    };
    if err != kAXErrorSuccess || value.is_null() {
        return Vec::new();
    }
    // These attributes are arrays of AXUIElementRef by definition. SAFETY: wrap
    // the owned (+1) CFArrayRef and collect the elements out of it.
    let array: CFArray<AXUIElement> =
        unsafe { CFArray::wrap_under_create_rule(value as CFArrayRef) };
    array.into_iter().map(|e| e.clone()).collect()
}

/// Read an AX attribute known to contain one `AXUIElement`.
pub(crate) fn element_attribute(el: &AXUIElement, name: &str) -> Option<AXUIElement> {
    let attr = CFString::new(name);
    let mut value: CFTypeRef = std::ptr::null();
    // SAFETY: standard AX copy-attribute call. The named attributes used by
    // callers (`AXFocusedApplication`, `AXFocusedWindow`, `AXMainWindow`) are
    // specified to return an AXUIElementRef.
    let err = unsafe {
        AXUIElementCopyAttributeValue(
            el.as_concrete_TypeRef(),
            attr.as_concrete_TypeRef(),
            &mut value,
        )
    };
    if err != kAXErrorSuccess || value.is_null() {
        return None;
    }
    // SAFETY: `value` is the +1 AXUIElementRef described above; transfer that
    // ownership into the safe wrapper.
    Some(unsafe { AXUIElement::wrap_under_create_rule(value as accessibility_sys::AXUIElementRef) })
}

/// Process id owning an AX element.
pub(crate) fn ax_pid(el: &AXUIElement) -> Option<i32> {
    let mut pid = 0i32;
    // SAFETY: the call only writes one pid_t into our local.
    let err = unsafe { AXUIElementGetPid(el.as_concrete_TypeRef(), &mut pid) };
    (err == kAXErrorSuccess && pid > 0).then_some(pid)
}

/// Whether the current Nova process has the macOS Accessibility TCC grant.
pub(crate) fn process_is_trusted() -> bool {
    // SAFETY: argument-free process trust query.
    unsafe { AXIsProcessTrusted() }
}

/// Read an arbitrary string-valued AX attribute.
pub(crate) fn ax_string(el: &AXUIElement, name: &'static str) -> String {
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(name));
    el.attribute(&attr)
        .ok()
        .and_then(|v| v.downcast_into::<CFString>())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Read an arbitrary boolean-ish AX attribute. Cocoa controls use either
/// CFBoolean or CFNumber for these states depending on the role/provider.
pub(crate) fn ax_bool(el: &AXUIElement, name: &'static str) -> Option<bool> {
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(name));
    let value = el.attribute(&attr).ok()?;
    if value.instance_of::<CFBoolean>() {
        return value.downcast_into::<CFBoolean>().map(bool::from);
    }
    if value.instance_of::<CFNumber>() {
        return value
            .downcast_into::<CFNumber>()
            .and_then(|number| number.to_i64())
            .map(|number| number != 0);
    }
    None
}

/// Render an accessibility error with its symbolic name (e.g.
/// `kAXErrorActionUnsupported (-25206)`) instead of a bare numeric code.
pub(crate) fn ax_err(e: &AxError) -> String {
    match e {
        AxError::Ax(code) => format!("{} ({code})", error_string(*code)),
        other => format!("{other:?}"),
    }
}

/// Best human label for an element: title, else description.
pub(crate) fn ax_label(el: &AXUIElement) -> String {
    let title = ax_title(el);
    if !title.is_empty() {
        return title;
    }
    ax_description(el)
}

pub(crate) fn ax_title(el: &AXUIElement) -> String {
    ax_string(el, "AXTitle")
}

pub(crate) fn ax_description(el: &AXUIElement) -> String {
    ax_string(el, "AXDescription")
}

pub(crate) fn ax_help(el: &AXUIElement) -> String {
    ax_string(el, "AXHelp")
}

/// The element's `AXValue` rendered as a string, if it holds one (e.g. an
/// `AXStaticText`'s text, which div-rendered pages put here rather than in the
/// title/description). Empty if absent or not string-convertible.
pub(crate) fn ax_value_string(el: &AXUIElement) -> String {
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string("AXValue"));
    el.attribute(&attr)
        .ok()
        .and_then(|v| v.downcast_into::<CFString>())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The element's user-visible VALUE for a text-like control, else empty.
///
/// Role-gated so the common marks (buttons, links, rows) pay NO extra AX
/// round-trip — only text-bearing controls are read, and only for them is
/// `AXValue` a meaningful CFString. A text-only consumer (`read_ui`) needs this
/// to know what a field already contains WITHOUT taking a screenshot; without
/// it, reading a form would still force an image. Deliberately excludes
/// checkbox/radio roles: their `AXValue` is a `CFNumber` (0/1) that
/// [`ax_value_string`] can't stringify, so surfacing "checked" state is left to
/// a follow-up rather than emitting a misleading empty string here.
pub(crate) fn value_for_role(el: &AXUIElement, role: &str) -> String {
    let subrole = ax_subrole(el);
    if is_secure_field(el, role, &subrole) {
        return "[REDACTED]".to_string();
    }
    match role {
        "AXTextField" | "AXSecureTextField" | "AXTextArea" | "AXComboBox" | "AXStaticText" => {
            ax_value_string(el)
        }
        _ => String::new(),
    }
}

/// Password/secure controls must be classified before their value is read.
pub(crate) fn is_secure_field(el: &AXUIElement, role: &str, subrole: &str) -> bool {
    role == "AXSecureTextField"
        || subrole == "AXSecureTextField"
        || role.to_ascii_lowercase().contains("secure")
        || subrole.to_ascii_lowercase().contains("secure")
        || ax_bool(el, "AXProtectedContent") == Some(true)
}

/// The element's AX role (e.g. `AXButton`), or empty if it exposes none.
pub(crate) fn ax_role(el: &AXUIElement) -> String {
    el.attribute(&AXAttribute::role())
        .ok()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The element's subrole (e.g. `AXStandardWindow`), if any.
pub(crate) fn ax_subrole(el: &AXUIElement) -> String {
    el.attribute(&AXAttribute::subrole())
        .ok()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The action names this element supports, e.g. `["AXPress", "AXShowMenu"]`.
pub(crate) fn ax_actions(el: &AXUIElement) -> Vec<String> {
    el.action_names()
        .ok()
        .map(|a: CFArray<CFString>| a.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

/// The first click-like action this element actually supports, if any.
pub(crate) fn click_action_for(el: &AXUIElement) -> Option<&'static str> {
    let available = ax_actions(el);
    CLICK_ACTIONS
        .iter()
        .copied()
        .find(|a| available.iter().any(|x| x == a))
}

/// This element's frame in global logical points, if it exposes one.
pub(crate) fn element_rect(el: &AXUIElement) -> Option<Rect> {
    let (x, y) = ax_pair(el, "AXPosition", kAXValueTypeCGPoint)?;
    let (w, h) = ax_pair(el, "AXSize", kAXValueTypeCGSize)?;
    Some((x, y, w, h))
}

/// Whether two rectangles overlap.
pub(crate) fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && a.0 + a.2 > b.0 && a.1 < b.1 + b.3 && a.1 + a.3 > b.1
}

/// Whether point `(px, py)` falls inside rectangle `r`.
pub(crate) fn point_in_rect(px: f64, py: f64, r: Rect) -> bool {
    px >= r.0 && px < r.0 + r.2 && py >= r.1 && py < r.1 + r.3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_intersection() {
        assert!(rects_intersect(
            (0.0, 0.0, 10.0, 10.0),
            (5.0, 5.0, 10.0, 10.0)
        ));
        assert!(!rects_intersect(
            (0.0, 0.0, 10.0, 10.0),
            (20.0, 20.0, 5.0, 5.0)
        ));
    }

    #[test]
    fn point_inside_rect() {
        let r = (10.0, 10.0, 20.0, 20.0);
        assert!(point_in_rect(15.0, 15.0, r));
        assert!(!point_in_rect(30.0, 30.0, r)); // on the exclusive far edge
        assert!(!point_in_rect(5.0, 15.0, r));
    }
}
