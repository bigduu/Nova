//! Windows implementations of the `crate::platform` capability traits (P1 MVP).
//!
//! Every `windows` crate (Win32 metadata bindings) use in this crate belongs
//! under this module (enforced by the dependency gating in `Cargo.toml` — the
//! `windows` crate isn't even fetched off Windows). Each submodule below
//! implements one `crate::platform` trait for one subsystem, mirroring
//! `platform::mac`'s layout:
//!
//! - [`input`]     — `InputInjector` via `SendInput`
//! - [`capture`]   — `ScreenCapture` via GDI `BitBlt` / `PrintWindow`
//! - [`window`]    — `WindowManager` via `EnumWindows` / `ShellExecuteW`
//! - [`clipboard`] — `Clipboard` via `OpenClipboard`/`CF_UNICODETEXT`
//! - [`elements`]  — `UiTree` STUB (P2: UI Automation) + the `UiElement`/
//!                   `CachedElement` value types `crate::tools::elements`
//!                   re-exports on Windows
//! - [`ocr`]       — `OcrEngine` STUB (P3: Windows.Media.Ocr)
//! - [`geometry`]  — shared display/virtual-desktop geometry helpers used by
//!                   `input`/`capture`/`server.rs`'s default view frame
//!
//! # DPI awareness (read before touching coordinates)
//!
//! Every coordinate the `crate::platform` traits pass around (window frames,
//! click points, capture rects) is documented as "global logical points". On
//! Windows those are only equal to real screen pixels if the process has
//! declared **Per-Monitor-DPI-v2** awareness — otherwise Windows silently
//! virtualizes/scales coordinates for an "unaware" process and every capture/
//! click computed here would be off by the active scale factor on anything
//! but a 100%-scaled display. [`init_dpi_awareness`] declares PMv2 as early as
//! possible in `main()` (before any window/display query) so `GetWindowRect`,
//! `GetSystemMetrics`, `SendInput`'s absolute coordinates, and GDI capture all
//! agree on one undistorted pixel space — the same invariant macOS gets for
//! free via `CGEvent`'s logical-point space. A future packaged build should
//! ALSO declare this in the executable's manifest (`dpiAwareness` /
//! `PerMonitorV2`) as belt-and-suspenders — the manifest wins if present, but
//! the API call covers the common case of running the raw `.exe` with no
//! manifest, which is how `cargo build`/`cargo run` ship it today.
pub mod capture;
pub mod clipboard;
pub mod elements;
pub mod geometry;
pub mod input;
pub mod ocr;
pub mod window;

/// Opt this process into Per-Monitor-DPI-v2 awareness. Must run before any
/// `GetWindowRect`/`GetSystemMetrics`/`SendInput` call — see the module doc.
/// Idempotent; a failure (e.g. already set by a manifest) is logged, not fatal.
pub fn init_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    // SAFETY: takes a well-known constant context handle, no pointers of ours
    // involved; safe to call any number of times from any thread (Microsoft
    // recommends calling it once, as early as possible, before any UI/DPI
    // query — exactly what `main()` does).
    let ok = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if let Err(e) = ok {
        tracing::warn!(
            "SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2) failed: {e} — coordinates may be \
             scaled if the display isn't at 100%; this is expected if the executable's manifest \
             already declares a DPI awareness (the manifest wins and this call then no-ops with an \
             error, which is fine)"
        );
    }
}
