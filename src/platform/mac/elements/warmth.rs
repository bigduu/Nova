//! Keeping a Chromium/Electron app's FULL accessibility tree built.
//!
//! Chromium gates its rich semantic tree (real `AXButton` / `AXLink` / `AXPress`,
//! not actionless `AXGroup`s) behind an assistive-technology signal, and — this
//! is the crux — only keeps it built while it believes a live AT is connected.
//! From cold the tree materializes over ~2-3s; if no AT keeps poking it, Chromium
//! reaps it back to a geometry-only skeleton to save memory. A one-shot process
//! therefore sees the tree flicker; a long-lived one that re-asserts the enable
//! periodically (a "keep-warm heartbeat") holds it open the way Homerow does.
//!
//! [`enable_web_accessibility`] is the single enable signal; [`TreeWarmer`] is the
//! background heartbeat the server runs for the lifetime of the process.

use accessibility::{AXAttribute, AXUIElement};
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// How often the warmer re-asserts the enable signal on the current target. Must
/// be short enough that Chromium never decides the AT went away and reaps the
/// tree, but long enough to be negligible (a couple of cheap attribute sets).
const HEARTBEAT: Duration = Duration::from_millis(600);

/// Coax a Chromium/Electron app (Arc, Chrome, VS Code, Slack, …) into building
/// and exposing its WEB accessibility tree. Chromium gates the web tree behind
/// an assistive-technology signal; different builds watch different private
/// attributes, so we set BOTH `AXManualAccessibility` and
/// `AXEnhancedUserInterface` (the older VoiceOver trigger). Without this, those
/// apps expose only native chrome (toolbar/tabs) and none of the page content.
///
/// Returns whether `AXManualAccessibility` was ACCEPTED: native apps reject this
/// private attribute, so a success marks the target as a Chromium/Electron web
/// view — whose tree materializes asynchronously, telling the discovery code it
/// must retry while the page tree builds. Best-effort and idempotent.
pub(crate) fn enable_web_accessibility(app: &AXUIElement) -> bool {
    let manual = AXAttribute::<CFType>::new(&CFString::from_static_string("AXManualAccessibility"));
    let accepted = app
        .set_attribute(&manual, CFBoolean::true_value().as_CFType())
        .is_ok();
    let enhanced =
        AXAttribute::<CFType>::new(&CFString::from_static_string("AXEnhancedUserInterface"));
    let _ = app.set_attribute(&enhanced, CFBoolean::true_value().as_CFType());
    accepted
}

/// Shared state between the public handle and the heartbeat thread.
struct Shared {
    /// The app whose web tree we currently keep warm (`None` ⇒ idle). Updated by
    /// `warm`; read by the heartbeat each tick.
    target: Mutex<Option<i32>>,
    /// Cleared to stop the heartbeat thread (used on shutdown / in tests).
    running: AtomicBool,
}

/// A background heartbeat that holds one Chromium/Electron app's full
/// accessibility tree built, so captures don't each pay the ~2-3s cold
/// materialization. The server points it at the most-recently-captured app via
/// [`TreeWarmer::warm`]; the first capture of an app warms it, and every capture
/// after stays warm (and therefore fast) — the Homerow model.
///
/// Cheap and non-disruptive: it only ever re-asserts an idempotent enable
/// attribute on a single app, never moves focus or input.
pub struct TreeWarmer {
    shared: Arc<Shared>,
}

impl TreeWarmer {
    /// Start the heartbeat thread. It idles (no target) until [`warm`] is called.
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            target: Mutex::new(None),
            running: AtomicBool::new(true),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("nova-ax-warmer".to_string())
            .spawn(move || run(worker))
            .expect("spawn nova-ax-warmer thread");
        TreeWarmer { shared }
    }

    /// Point the heartbeat at `pid` (the app whose tree to keep warm). Idempotent;
    /// switching apps just changes which one stays warm.
    pub fn warm(&self, pid: i32) {
        if let Ok(mut t) = self.shared.target.lock() {
            *t = Some(pid);
        }
        // Provider RPCs stay on the dedicated heartbeat thread. Callers such as
        // an async MCP handler only update this pid and never block their
        // executor on an unbounded AX message.
    }

    /// The app currently kept warm, if any.
    pub fn target(&self) -> Option<i32> {
        self.shared.target.lock().ok().and_then(|t| *t)
    }

    /// Stop keeping any app warm (heartbeat goes idle but the thread lives).
    pub fn clear(&self) {
        if let Ok(mut t) = self.shared.target.lock() {
            *t = None;
        }
    }

    /// Stop the heartbeat thread entirely (shutdown / tests).
    pub fn stop(&self) {
        self.shared.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for TreeWarmer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The process-wide keep-warm heartbeat, started on first use. The MCP server is
/// long-lived and there is exactly one desktop to keep warm, so a single shared
/// warmer is the natural shape — and it avoids threading a handle through every
/// caller. `warmer().warm(pid)` after a capture keeps that app's tree warm.
pub fn warmer() -> &'static TreeWarmer {
    static WARMER: OnceLock<TreeWarmer> = OnceLock::new();
    WARMER.get_or_init(TreeWarmer::start)
}

fn run(shared: Arc<Shared>) {
    while shared.running.load(Ordering::Relaxed) {
        let target = shared.target.lock().ok().and_then(|t| *t);
        if let Some(pid) = target {
            let app = AXUIElement::application(pid);
            // A wedged provider must not wedge the process-wide warmer thread
            // forever. The next heartbeat can retry independently.
            let _ = app.set_messaging_timeout(0.25);
            enable_web_accessibility(&app);
        }
        std::thread::sleep(HEARTBEAT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmer_tracks_and_clears_target() {
        let w = TreeWarmer::start();
        assert_eq!(w.target(), None);
        w.warm(4242);
        assert_eq!(w.target(), Some(4242));
        w.warm(99); // switching just changes the target
        assert_eq!(w.target(), Some(99));
        w.clear();
        assert_eq!(w.target(), None);
        w.stop();
    }
}
