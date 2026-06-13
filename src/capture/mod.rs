pub mod broker;
pub mod overlay;
pub mod screenshot;
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
