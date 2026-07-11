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
use std::cell::RefCell;

/// Walk `pid`'s tree and return the first element whose role/label contains
/// `query` (case-insensitive) and for which `accept` yields a value.
fn find_matching<T: 'static>(
    pid: i32,
    query: &str,
    accept: impl Fn(&AXUIElement) -> Option<T> + 'static,
) -> Option<(AXUIElement, T)> {
    struct Finder<T, F> {
        query: String,
        accept: F,
        found: RefCell<Option<(AXUIElement, T)>>,
    }
    impl<T: 'static, F: Fn(&AXUIElement) -> Option<T>> TreeVisitor for Finder<T, F> {
        fn enter_element(&self, element: &AXUIElement) -> TreeWalkerFlow {
            if self.found.borrow().is_some() {
                return TreeWalkerFlow::Exit;
            }
            let hay = format!("{} {}", ax_role(element), ax_label(element)).to_lowercase();
            if !self.query.is_empty() && hay.contains(&self.query) {
                if let Some(payload) = (self.accept)(element) {
                    *self.found.borrow_mut() = Some((element.clone(), payload));
                    return TreeWalkerFlow::Exit;
                }
            }
            TreeWalkerFlow::Continue
        }
        fn exit_element(&self, _element: &AXUIElement) {}
    }

    let app = AXUIElement::application(pid);
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
        found: RefCell::new(None),
    };
    let backoffs_ms: &[u64] = if web_capable {
        &[0, 300, 500, 1000]
    } else {
        &[0]
    };
    for &delay in backoffs_ms {
        if delay > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay));
        }
        TreeWalker::new().walk(&app, &finder);
        if finder.found.borrow().is_some() {
            break;
        }
    }
    finder.found.into_inner()
}

/// Click the element in `pid` matching `query`, performing whichever click-like
/// action it actually supports (AXPress/AXOpen/AXPick/AXConfirm).
pub fn ax_click(pid: i32, query: &str) -> Result<String, String> {
    let (el, action) = find_matching(pid, query, click_action_for)
        .ok_or_else(|| format!("no clickable accessible element matching {query:?}"))?;
    el.perform_action(&CFString::new(action))
        .map_err(|e| format!("{action} on {query:?} failed: {}", ax_err(&e)))?;
    Ok(format!("performed {action} on element matching {query:?}"))
}

/// Set the value (AXValue) of the element in `pid` matching `query` to `value`
/// — e.g. fill a text field directly, without focusing or typing.
pub fn ax_set_value(pid: i32, query: &str, value: &str) -> Result<String, String> {
    let (el, _) = find_matching(pid, query, |_| Some(()))
        .ok_or_else(|| format!("no accessible element matching {query:?}"))?;
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXValueAttribute));
    el.set_attribute(&attr, CFString::new(value).as_CFType())
        .map_err(|e| format!("set AXValue on {query:?} failed: {}", ax_err(&e)))?;
    Ok(format!("set value of element matching {query:?}"))
}

/// Move keyboard focus (AXFocused) to the element in `pid` matching `query`.
pub fn ax_focus(pid: i32, query: &str) -> Result<String, String> {
    let (el, _) = find_matching(pid, query, |_| Some(()))
        .ok_or_else(|| format!("no accessible element matching {query:?}"))?;
    let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXFocusedAttribute));
    el.set_attribute(&attr, CFBoolean::true_value().as_CFType())
        .map_err(|e| format!("set AXFocused on {query:?} failed: {}", ax_err(&e)))?;
    Ok(format!("focused element matching {query:?}"))
}
