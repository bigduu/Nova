//! macOS screen/window pixel capture — the shared capture daemon ([`broker`])
//! and the persistent ScreenCaptureKit stream it owns ([`stream`]) — behind
//! [`crate::platform::ScreenCapture`].
//!
//! This is a MOVE of the old `src/capture/{broker,stream}.rs` (unchanged in
//! substance — same daemon architecture, wedge-avoidance, downscaling; only
//! their home and the trait wiring below are new). `src/capture/screenshot.rs`
//! and `src/capture/overlay.rs` deliberately STAY at the crate root: they are
//! the OS-neutral "finish a raw capture" layer (overlays, Set-of-Mark, JPEG
//! encode) that CONSUMES a [`crate::capture::screenshot::RawCapture`], not
//! part of the capture CAPABILITY itself. See the platform-abstraction move plan for the pattern
//! the other subsystems follow.
//!
//! Window enumeration (`broker::CaptureRequest::Windows` / `WireWindow`) also
//! lives here rather than under a `WindowManager` impl: it rides the SAME
//! daemon socket as pixel capture (see [`broker`]'s module doc on why —
//! replayd keys client identity by executable path, so a second same-binary
//! process holding its own `SCShareableContent` connection is a storm
//! participant same as a second capture stream would be). `tools::window`
//! calls `broker::shared_client().windows()` directly for this reason; a
//! future `WindowManager` impl should wrap the SAME `CaptureClient` singleton
//! rather than open a second connection.

pub mod broker;
pub mod stream;

/// Force CoreGraphics / the window-server connection to initialize once, before
/// any ScreenCaptureKit call.
///
/// nova is launched as a subprocess by its MCP host (bamboo), which does not set
/// up a GUI/window-server session the way an app launched from the Dock or an
/// interactive shell does. ScreenCaptureKit's `SCScreenshotManager::capture_image`
/// then talks to `replayd` over an XPC connection that was never bootstrapped —
/// which manifests as either a `CGS_REQUIRE_INIT` SIGABRT (window/region paths,
/// which touch more CoreGraphics APIs) or a capture that hangs forever in
/// replayd-connection churn (plain path). Calling `CGMainDisplayID()` first forces
/// CoreGraphics to establish that connection cleanly.
///
/// This mirrors `sc_initialize_core_graphics()` that the screencapturekit crate's
/// own tests, examples and benches all call before capturing. The symbol is
/// provided by the statically-linked Swift bridge; we just have to invoke it once
/// at startup (it is idempotent / cheap — a single `CGMainDisplayID` call).
pub fn init_core_graphics() {
    extern "C" {
        // Defined in screencapturekit's Swift bridge (Core.swift):
        // `@_cdecl("sc_initialize_core_graphics")` → calls `CGMainDisplayID()`.
        fn sc_initialize_core_graphics();
    }
    // SAFETY: the symbol takes no args, returns nothing, and merely forces CG
    // initialization; safe to call any number of times from any thread.
    unsafe { sc_initialize_core_graphics() }
}

/// The macOS [`crate::platform::ScreenCapture`]: pixel capture via the shared
/// per-user capture daemon ([`broker`]) — a thin forwarder building the
/// matching [`broker::CaptureRequest`] and delegating to
/// [`broker::shared_client`]. See `broker`'s module doc for why every capture,
/// across every nova process, MUST funnel through that one daemon connection.
pub struct MacScreenCapture;

impl crate::platform::ScreenCapture for MacScreenCapture {
    fn capture_display(&self) -> Result<crate::capture::screenshot::RawCapture, String> {
        broker::shared_client().capture(&broker::CaptureRequest::Display)
    }

    fn capture_window(
        &self,
        query: &str,
    ) -> Result<crate::capture::screenshot::RawCapture, String> {
        broker::shared_client().capture(&broker::CaptureRequest::Window {
            query: query.to_string(),
        })
    }

    fn capture_region(
        &self,
        rect: (f64, f64, f64, f64),
    ) -> Result<crate::capture::screenshot::RawCapture, String> {
        broker::shared_client().capture(&broker::CaptureRequest::Region { rect })
    }
}
