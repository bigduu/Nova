//! Screen/window pixel capture — the Windows `crate::platform::ScreenCapture`
//! implementation (P1 MVP).
//!
//! - Display/region capture: GDI `BitBlt` screen-scrape.
//! - Window capture: `PrintWindow(PW_RENDERFULLCONTENT)` first (correctly
//!   captures occluded/off-screen-but-open content on apps that support it),
//!   falling back to a `BitBlt` of the window's on-screen rect if
//!   `PrintWindow` itself fails (some older/non-DWM-aware apps ignore it) —
//!   the fallback only sees the unoccluded, on-screen portion, the classic
//!   screen-scrape limitation.
//!
//! Unlike macOS's ScreenCaptureKit (which can be asked to stream an already-
//! downscaled frame), GDI always hands back native-resolution pixels — so,
//! uniquely among the two platforms, capture here does its own
//! `image::imageops::resize` down to the model's pixel budget
//! ([`crate::display::scaling`]'s `*_MAX_DIMENSION` constants) after the grab.
//!
//! One pixel space throughout (see `super::geometry`'s module doc): with
//! Per-Monitor-DPI-v2 declared, `GetWindowRect`/`GetSystemMetrics`/`BitBlt`
//! all agree on real, unscaled screen pixels — no separate physical-vs-
//! logical reconciliation needed here, unlike macOS's Retina split.
use crate::capture::screenshot::RawCapture;
use crate::display::scaling::{
    compute_target_dims, compute_target_dims_capped, TargetDims, REGION_MAX_DIMENSION,
    WINDOW_MAX_DIMENSION,
};
use crate::display::view::ViewFrame;
use crate::platform::WindowHandle;
use std::ffi::c_void;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
    SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::PW_RENDERFULLCONTENT;

use super::geometry::primary_display_size;

/// Resolve `query` (case-insensitive substring of title or app name) to an
/// on-screen window — the LARGEST-area match, mirroring
/// `tools::window::pid_for_window`'s policy (a query like "Arc" can match
/// several windows, and the real main window is usually the largest one).
/// Kept as its own resolver rather than reusing `tools::window::pid_for_window`
/// because capture needs the window's raw `HWND` (carried in `WindowHandle::id`
/// on Windows — see `platform::windows::window::list_windows`'s doc), which
/// that neutral pid+rect helper doesn't expose.
fn resolve_window(query: &str) -> Result<WindowHandle, String> {
    let q = query.to_lowercase();
    crate::platform::window_manager()
        .list_windows()
        .map_err(|e| format!("list_windows failed while resolving {query:?}: {e}"))?
        .into_iter()
        .filter(|w| {
            w.pid > 0
                && !w.title.is_empty()
                && (w.title.to_lowercase().contains(&q) || w.app_name.to_lowercase().contains(&q))
        })
        .max_by(|a, b| {
            (a.width * a.height)
                .partial_cmp(&(b.width * b.height))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .ok_or_else(|| format!("no on-screen window matching {query:?}"))
}

/// Create a `w`x`h` 32bpp memory bitmap compatible with the desktop, let
/// `draw` render into it, then read the result back as RGB pixels. Centralizes
/// the create/select/cleanup dance shared by the `BitBlt` (display/region) and
/// `PrintWindow` (single window) paths below — only what fills the bitmap
/// differs between them.
fn render_to_rgb(
    w: i32,
    h: i32,
    draw: impl FnOnce(HDC) -> windows::core::Result<()>,
) -> Result<image::RgbImage, String> {
    if w <= 0 || h <= 0 {
        return Err(format!("capture size is non-positive ({w}x{h})"));
    }
    // SAFETY: `None` requests the whole-desktop DC, a documented valid arg.
    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        return Err("GetDC(desktop) returned an invalid DC".to_string());
    }
    let result = (|| -> Result<image::RgbImage, String> {
        // SAFETY: `screen_dc` was just validated non-invalid above.
        let mem_dc = unsafe { CreateCompatibleDC(screen_dc) };
        if mem_dc.is_invalid() {
            return Err("CreateCompatibleDC failed".to_string());
        }
        let out = (|| -> Result<image::RgbImage, String> {
            // SAFETY: `screen_dc` is valid; `w`/`h` were validated positive above.
            let bmp = unsafe { CreateCompatibleBitmap(screen_dc, w, h) };
            if bmp.is_invalid() {
                return Err("CreateCompatibleBitmap failed".to_string());
            }
            let out = (|| -> Result<image::RgbImage, String> {
                // SAFETY: `mem_dc`/`bmp` are both valid and freshly created
                // above; `SelectObject` returns the DC's previous (stock)
                // bitmap, restored below before either handle is deleted.
                let old = unsafe { SelectObject(mem_dc, bmp) };
                let pixels = draw(mem_dc)
                    .map_err(|e| format!("{e}"))
                    .and_then(|_| read_bitmap_rgb(mem_dc, bmp, w, h));
                // SAFETY: restores `mem_dc`'s original bitmap before `bmp`/
                // `mem_dc` are deleted, so neither delete call touches a DC
                // that still references the other's handle.
                unsafe {
                    let _ = SelectObject(mem_dc, old);
                }
                pixels
            })();
            // SAFETY: `bmp` is no longer selected into any DC (restored above).
            unsafe {
                let _ = DeleteObject(bmp);
            }
            out
        })();
        // SAFETY: `mem_dc` has no bitmap selected into it at this point.
        unsafe {
            let _ = DeleteDC(mem_dc);
        }
        out
    })();
    // SAFETY: releases exactly the DC obtained above, for the same (desktop) target.
    unsafe {
        let _ = ReleaseDC(None, screen_dc);
    }
    result
}

/// Read a `w`x`h` 32bpp top-down DIB out of `bmp` (selected into `dc`) as RGB
/// pixels — GDI's native 32bpp layout is `B,G,R,X` per pixel, so this also
/// does the channel reorder `image::RgbImage` needs.
fn read_bitmap_rgb(dc: HDC, bmp: HBITMAP, w: i32, h: i32) -> Result<image::RgbImage, String> {
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative height requests a TOP-DOWN DIB (row 0 = top), so no
            // manual vertical flip is needed after the copy.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
    // SAFETY: `dc`/`bmp` are valid, compatible handles from the caller; `buf`
    // is sized exactly for a top-down 32bpp DIB of `w`x`h`, matching `bmi`.
    let lines = unsafe {
        GetDIBits(
            dc,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    if lines == 0 {
        return Err("GetDIBits copied 0 scanlines".to_string());
    }
    let mut rgb = Vec::with_capacity((w as usize) * (h as usize) * 3);
    for px in buf.chunks_exact(4) {
        rgb.push(px[2]); // R
        rgb.push(px[1]); // G
        rgb.push(px[0]); // B
    }
    image::RgbImage::from_raw(w as u32, h as u32, rgb)
        .ok_or_else(|| "failed to build an RgbImage from the captured pixels".to_string())
}

/// `BitBlt`-capture the screen rectangle at global `(x, y)`, size `w`x`h`.
fn capture_screen_rect(x: i32, y: i32, w: i32, h: i32) -> Result<image::RgbImage, String> {
    render_to_rgb(w, h, |mem_dc| unsafe {
        // A second desktop DC as the BitBlt source. Independent of
        // `render_to_rgb`'s own screen DC (each GetDC/ReleaseDC pair is
        // self-contained), which keeps this closure's signature identical to
        // the PrintWindow path's below (`HDC` in, `Result<()>` out).
        let src_dc = GetDC(None);
        let r = BitBlt(mem_dc, 0, 0, w, h, src_dc, x, y, SRCCOPY);
        let _ = ReleaseDC(None, src_dc);
        r
    })
}

/// `PrintWindow`-capture `hwnd`, falling back to a `BitBlt` screen-scrape of
/// its on-screen rect `(x, y, w, h)` if `PrintWindow` itself fails.
fn capture_window_pixels(
    hwnd: HWND,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<image::RgbImage, String> {
    let printed = render_to_rgb(w, h, |mem_dc| unsafe {
        PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).ok()
    });
    match printed {
        Ok(img) => Ok(img),
        Err(e) => {
            tracing::debug!(
                "PrintWindow failed ({e}); falling back to a BitBlt screen-scrape (only sees the \
                 unoccluded, on-screen portion of the window)"
            );
            capture_screen_rect(x, y, w, h)
        }
    }
}

/// Downscale `image` to `target` if it isn't already that size — GDI/
/// `PrintWindow` always hand back native pixels, so (unlike ScreenCaptureKit
/// on macOS, which can be asked to stream already at the target size) every
/// Windows capture path needs this explicit step.
fn downscale(image: image::RgbImage, target: TargetDims) -> image::RgbImage {
    if image.width() == target.width && image.height() == target.height {
        return image;
    }
    image::imageops::resize(
        &image,
        target.width,
        target.height,
        image::imageops::FilterType::Lanczos3,
    )
}

/// Capture the whole primary display.
pub fn capture_display() -> Result<RawCapture, String> {
    let (w, h) = primary_display_size();
    let native = capture_screen_rect(0, 0, w as i32, h as i32)?;
    let dims = compute_target_dims(w as u32, h as u32);
    Ok(RawCapture {
        image: downscale(native, dims),
        view: ViewFrame {
            origin: (0.0, 0.0),
            region: (w, h),
            screenshot: (dims.width as f64, dims.height as f64),
        },
        window_pid: None,
    })
}

/// Capture a single on-screen window matching `query`.
pub fn capture_window(query: &str) -> Result<RawCapture, String> {
    let win = resolve_window(query)?;
    // SAFETY: reconstructs the `HWND` from the id `platform::windows::window
    // ::list_windows` stashed there (the raw HWND value) — see that module's
    // `WindowHandle::id` doc for why this round-trip is sound.
    let hwnd = HWND(win.id as usize as *mut c_void);
    let (x, y, w, h) = (
        win.x as i32,
        win.y as i32,
        win.width as i32,
        win.height as i32,
    );
    let native = capture_window_pixels(hwnd, x, y, w, h)?;
    let dims = compute_target_dims_capped(w as u32, h as u32, WINDOW_MAX_DIMENSION);
    Ok(RawCapture {
        image: downscale(native, dims),
        view: ViewFrame {
            origin: (win.x, win.y),
            region: (win.width, win.height),
            screenshot: (dims.width as f64, dims.height as f64),
        },
        window_pid: Some(win.pid),
    })
}

/// Capture exactly the rectangle `(x, y, w, h)` in global logical points.
pub fn capture_region(rect: (f64, f64, f64, f64)) -> Result<RawCapture, String> {
    let (x, y, w, h) = rect;
    if w <= 0.0 || h <= 0.0 {
        return Err("region has zero size".to_string());
    }
    let native = capture_screen_rect(x as i32, y as i32, w as i32, h as i32)?;
    let dims = compute_target_dims_capped(w as u32, h as u32, REGION_MAX_DIMENSION);
    Ok(RawCapture {
        image: downscale(native, dims),
        view: ViewFrame {
            origin: (x, y),
            region: (w, h),
            screenshot: (dims.width as f64, dims.height as f64),
        },
        window_pid: None,
    })
}

/// The Windows [`crate::platform::ScreenCapture`]: GDI `BitBlt`/`PrintWindow`,
/// via the free functions above.
pub struct WinScreenCapture;

impl crate::platform::ScreenCapture for WinScreenCapture {
    fn capture_display(&self) -> Result<RawCapture, String> {
        capture_display()
    }

    fn capture_window(&self, query: &str) -> Result<RawCapture, String> {
        capture_window(query)
    }

    fn capture_region(&self, rect: (f64, f64, f64, f64)) -> Result<RawCapture, String> {
        capture_region(rect)
    }
}
