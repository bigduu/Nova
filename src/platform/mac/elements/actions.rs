//! Driving native-app controls directly through the AX tree by a text query: no
//! coordinates, no cursor movement, and the app need not be frontmost. These work
//! for apps that expose an accessibility tree (most native Cocoa apps); apps with
//! no tree (many Electron / custom-rendered apps) return "no element matching",
//! and the caller should fall back to screenshot + click.

use super::attrs::{ax_err, ax_label, ax_role, click_action_for};
use super::warmth::enable_web_accessibility;
use accessibility::{AXAttribute, AXUIElement, TreeVisitor, TreeWalker, TreeWalkerFlow};
use accessibility_sys::{kAXFocusedAttribute, kAXValueAttribute};
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;

/// Walk `pid`'s tree and require exactly one element whose role/label contains
/// `query` (case-insensitive) and for which `accept` yields a value.
///
/// Query actions are a legacy convenience. They must never silently choose the
/// first of several substring matches; the generation-safe ax_read +
/// ax_activate protocol is preferred for exact actions.
fn find_unique_matching<T: 'static>(
    pid: i32,
    query: &str,
    deadline: std::time::Instant,
    accept: impl Fn(&AXUIElement) -> Option<T> + 'static,
) -> Result<(AXUIElement, T), String> {
    struct Finder<T, F> {
        query: String,
        accept: F,
        found: RefCell<Vec<(AXUIElement, T, String)>>,
        seen: RefCell<HashSet<usize>>,
        deadline: std::time::Instant,
        timed_out: Cell<bool>,
    }
    impl<T: 'static, F: Fn(&AXUIElement) -> Option<T>> TreeVisitor for Finder<T, F> {
        fn enter_element(&self, element: &AXUIElement) -> TreeWalkerFlow {
            if std::time::Instant::now() >= self.deadline {
                self.timed_out.set(true);
                return TreeWalkerFlow::Exit;
            }
            if self.found.borrow().len() >= 2 {
                return TreeWalkerFlow::Exit;
            }
            let hay = format!("{} {}", ax_role(element), ax_label(element)).to_lowercase();
            if !self.query.is_empty() && hay.contains(&self.query) {
                if let Some(payload) = (self.accept)(element) {
                    use core_foundation::base::TCFType;
                    let identity = element.as_concrete_TypeRef() as usize;
                    if self.seen.borrow_mut().insert(identity) {
                        self.found.borrow_mut().push((
                            element.clone(),
                            payload,
                            format!("{} {:?}", ax_role(element), ax_label(element)),
                        ));
                    }
                    if self.found.borrow().len() >= 2 {
                        return TreeWalkerFlow::Exit;
                    }
                }
            }
            TreeWalkerFlow::Continue
        }
        fn exit_element(&self, _element: &AXUIElement) {}
    }

    let app = AXUIElement::application(pid);
    let remaining = deadline
        .checked_duration_since(std::time::Instant::now())
        .ok_or_else(|| "route=ax; accessibility query deadline elapsed".to_string())?;
    let _ = app.set_messaging_timeout(remaining.as_secs_f32().clamp(0.05, 0.5));
    // Browser/Electron apps only expose their web tree once asked; it then
    // builds asynchronously over ~2-3s. `enable_web_accessibility` returns
    // whether the app accepted the web-AX enable (i.e. it IS a Chromium/Electron
    // view); for those, retry with backoff so a cold tree gets time to
    // materialize. A native app exposes its whole tree on the first walk, so it
    // gets a single attempt — a genuinely-missing query there still fails fast
    // instead of stalling ~2s.
    let web_capable = enable_web_accessibility(&app);
    let finder = Finder {
        query: query.to_lowercase(),
        accept,
        found: RefCell::new(Vec::new()),
        seen: RefCell::new(HashSet::new()),
        deadline,
        timed_out: Cell::new(false),
    };
    let backoffs_ms: &[u64] = if web_capable {
        &[0, 300, 500, 1000]
    } else {
        &[0]
    };
    for &delay in backoffs_ms {
        if delay > 0 {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                finder.timed_out.set(true);
                break;
            }
            std::thread::sleep(std::cmp::min(
                std::time::Duration::from_millis(delay),
                remaining,
            ));
        }
        TreeWalker::new().walk(&app, &finder);
        if finder.timed_out.get() {
            break;
        }
        // One complete walk has already seen every currently exposed match.
        // Stop as soon as it found anything; repeating a warm-tree walk can
        // rebuild the same logical control under a new AX object identity and
        // would otherwise manufacture a false ambiguity.
        if !finder.found.borrow().is_empty() {
            break;
        }
    }
    if finder.timed_out.get() {
        return Err("route=ax; accessibility query exceeded its bounded deadline".to_string());
    }
    let mut found = finder.found.into_inner();
    match found.len() {
        0 => Err(format!("no accessible element matching {query:?}")),
        1 => {
            let (element, payload, _) = found.remove(0);
            Ok((element, payload))
        }
        _ => Err(format!(
            "ambiguous accessibility query {query:?}; at least two candidates match: {}. Run \
             ax_read and activate an exact snapshot-local node_id instead",
            found
                .iter()
                .map(|(_, _, description)| description.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn configure_action_timeout(
    element: &AXUIElement,
    deadline: std::time::Instant,
) -> Result<(), String> {
    let remaining = deadline
        .checked_duration_since(std::time::Instant::now())
        .ok_or_else(|| "route=ax; accessibility action deadline elapsed".to_string())?;
    element
        .set_messaging_timeout(remaining.as_secs_f32().clamp(0.05, 0.5))
        .map_err(|error| format!("route=ax; failed to configure AX action timeout: {error:?}"))
}

/// Click the element in `pid` matching `query`, performing whichever click-like
/// action it actually supports (AXPress/AXOpen/AXPick/AXConfirm).
pub fn ax_click(pid: i32, query: &str, deadline: std::time::Instant) -> Result<String, String> {
    let (el, action) = find_unique_matching(pid, query, deadline, click_action_for)?;
    configure_action_timeout(&el, deadline)?;
    el.perform_action(&CFString::new(action))
        .map_err(|e| format!("{action} on {query:?} failed: {}", ax_err(&e)))?;
    Ok(format!(
        "route=ax performed {action} on element matching {query:?}"
    ))
}

/// Set the value (AXValue) of the element in `pid` matching `query` to `value`
/// — e.g. fill a text field directly, without focusing or typing.
pub fn ax_set_value(
    pid: i32,
    query: &str,
    value: &str,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let (el, _) = find_unique_matching(pid, query, deadline, |_| Some(()))?;
    configure_action_timeout(&el, deadline)?;
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXValueAttribute));
    el.set_attribute(&attr, CFString::new(value).as_CFType())
        .map_err(|e| format!("set AXValue on {query:?} failed: {}", ax_err(&e)))?;
    Ok(format!("route=ax set value of element matching {query:?}"))
}

/// Move keyboard focus (AXFocused) to the element in `pid` matching `query`.
pub fn ax_focus(pid: i32, query: &str, deadline: std::time::Instant) -> Result<String, String> {
    let (el, _) = find_unique_matching(pid, query, deadline, |_| Some(()))?;
    configure_action_timeout(&el, deadline)?;
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXFocusedAttribute));
    el.set_attribute(&attr, CFBoolean::true_value().as_CFType())
        .map_err(|e| format!("set AXFocused on {query:?} failed: {}", ax_err(&e)))?;
    Ok(format!("route=ax focused element matching {query:?}"))
}
