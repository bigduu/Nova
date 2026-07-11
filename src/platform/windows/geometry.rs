//! Display / virtual-desktop geometry (Windows).
//!
//! With Per-Monitor-DPI-v2 declared ([`super::init_dpi_awareness`]), every
//! Win32 geometry query below already returns real, unscaled screen pixels —
//! unlike macOS, there is no separate "logical points vs physical backing
//! pixels" distinction to reconcile here (see the module doc on `platform::mac
//! ::geometry` for that split): one pixel space serves both the "global
//! logical points" the `crate::platform` traits document and the raw capture
//! grab.
use crate::display::scaling::compute_target_dims;
use crate::display::view::ViewFrame;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Logical (== physical, PMv2-aware) size of the PRIMARY monitor — what
/// `screenshot`'s full-display capture and `zoom_region`'s default frame use.
pub fn primary_display_size() -> (f64, f64) {
    // SAFETY: GetSystemMetrics is an argless-per-call, side-effect-free Win32
    // query; safe from any thread.
    let w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    (w.max(1) as f64, h.max(1) as f64)
}

/// Bounding rect of the WHOLE virtual desktop (every monitor, which may extend
/// to negative coordinates left/above the primary) as `(left, top, width,
/// height)`. `SendInput`'s absolute-coordinate mode
/// (`MOUSEEVENTF_VIRTUALDESK`) maps its 0..65535 space across exactly this
/// rect, so mouse moves/clicks must normalize through it — see
/// `platform::windows::input`.
pub fn virtual_desktop_bounds() -> (i32, i32, i32, i32) {
    // SAFETY: same as above — argless Win32 metrics queries.
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    }
}

/// The [`ViewFrame`] for a full-display screenshot of the primary monitor —
/// the default coordinate frame when no window has been captured yet. Mirrors
/// `platform::mac::geometry::display_view_frame`.
pub fn display_view_frame() -> ViewFrame {
    let (w, h) = primary_display_size();
    let dims = compute_target_dims(w as u32, h as u32);
    ViewFrame {
        origin: (0.0, 0.0),
        region: (w, h),
        screenshot: (dims.width as f64, dims.height as f64),
    }
}
