// `broker` (the shared capture daemon), `stream` (the persistent
// ScreenCaptureKit stream it owns), and `init_core_graphics` (its CoreGraphics
// bootstrap) moved to `crate::platform::mac::capture` — they are the mac-only
// implementation of `crate::platform::ScreenCapture`. `screenshot` and
// `overlay` stay here: they are the OS-neutral "finish a raw capture" layer
// (overlays, Set-of-Mark, JPEG encode) that CONSUMES a capture, not part of
// the capability itself. See PARALLEL_PLAN.md.
pub mod overlay;
pub mod screenshot;
