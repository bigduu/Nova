//! Screen/window pixel capture — the Windows `crate::platform::ScreenCapture`
//! implementation.
//!
//! - Display/region capture: GDI `BitBlt` screen-scrape (unchanged since P1 —
//!   no black-bitmap bug here; see [`wgc`]'s module doc for why WGC is out of
//!   scope for these two).
//! - Window capture (P4): [`wgc`] (Windows.Graphics.Capture) is now the
//!   PRIMARY path, falling back to the P1 `PrintWindow(PW_RENDERFULLCONTENT)`
//!   / `BitBlt` ladder ([`print_window_only`] / [`capture_window_pixels`]) on
//!   ANY WGC failure (old Windows, no DWM session, a protected/DRM surface,
//!   or WGC simply timing out on its first frame). WGC asks the DWM
//!   compositor directly for the window's actual composited output, so it
//!   sees real pixels regardless of how the app renders — see [`wgc`]'s
//!   module doc for the full pipeline and why this was necessary:
//!   `PrintWindow` returns `TRUE` (success) yet hands back an all-black
//!   bitmap for hardware-accelerated browsers/Electron/games, because the
//!   composited frame never reaches the GDI-visible surface — a failure mode
//!   this module cannot even detect (a black `PrintWindow` result and a
//!   legitimately dark window look identical), so P1 could only fall back
//!   from an `Err`, which this bug never produces.
//!
//! Unlike macOS's ScreenCaptureKit (which can be asked to stream an already-
//! downscaled frame), Windows always hands back native-resolution pixels — so,
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
    HGDIOBJ, SRCCOPY,
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
    // Each GDI resource is held in an RAII guard so cleanup happens in exactly
    // reverse acquisition order (Rust drops locals last-declared-first):
    // selection restore → bitmap delete → mem-DC delete → screen-DC release.
    // That ordering matters — `bmp` must be de-selected from `mem_dc` before
    // either is freed — and RAII guarantees it even on the early-return
    // (`?`) paths, without the nested try/finally closures clippy flags.
    let screen_dc = ScreenDc::get()?;
    let mem_dc = MemDc::create_compatible(screen_dc.0)?;
    // SAFETY: `screen_dc` is valid; `w`/`h` were validated positive above.
    let bmp = MemBitmap::create_compatible(screen_dc.0, w, h)?;
    // SAFETY: `mem_dc`/`bmp` are both valid; the guard restores `mem_dc`'s
    // previous (stock) bitmap on drop, before `bmp`/`mem_dc` are freed.
    let _selection = SelectGuard::select(mem_dc.0, bmp.0);
    draw(mem_dc.0)
        .map_err(|e| format!("{e}"))
        .and_then(|()| read_bitmap_rgb(mem_dc.0, bmp.0, w, h))
}

// ── GDI RAII guards ─────────────────────────────────────────────────
//
// One-field newtypes whose Drop frees the wrapped handle, so `render_to_rgb`
// reads as a flat acquire-then-use sequence with `?` early-returns instead of
// a nested create/select/cleanup ladder.

/// The whole-desktop device context (`GetDC(None)` / `ReleaseDC`).
struct ScreenDc(HDC);
impl ScreenDc {
    fn get() -> Result<Self, String> {
        // SAFETY: `None` requests the whole-desktop DC, a documented valid arg.
        let dc = unsafe { GetDC(None) };
        if dc.is_invalid() {
            return Err("GetDC(desktop) returned an invalid DC".to_string());
        }
        Ok(Self(dc))
    }
}
impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: releases exactly the DC this guard obtained, same (desktop) target.
        unsafe {
            let _ = ReleaseDC(None, self.0);
        }
    }
}

/// An in-memory device context (`CreateCompatibleDC` / `DeleteDC`).
struct MemDc(HDC);
impl MemDc {
    fn create_compatible(screen_dc: HDC) -> Result<Self, String> {
        // SAFETY: `screen_dc` is a valid DC from `ScreenDc::get`.
        let dc = unsafe { CreateCompatibleDC(screen_dc) };
        if dc.is_invalid() {
            return Err("CreateCompatibleDC failed".to_string());
        }
        Ok(Self(dc))
    }
}
impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: dropped AFTER the SelectGuard restored the DC's stock bitmap,
        // so no caller-owned bitmap is selected into it at deletion.
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

/// A memory bitmap (`CreateCompatibleBitmap` / `DeleteObject`).
struct MemBitmap(HBITMAP);
impl MemBitmap {
    fn create_compatible(screen_dc: HDC, w: i32, h: i32) -> Result<Self, String> {
        // SAFETY: `screen_dc` is valid; `w`/`h` were validated positive by the caller.
        let bmp = unsafe { CreateCompatibleBitmap(screen_dc, w, h) };
        if bmp.is_invalid() {
            return Err("CreateCompatibleBitmap failed".to_string());
        }
        Ok(Self(bmp))
    }
}
impl Drop for MemBitmap {
    fn drop(&mut self) {
        // SAFETY: dropped AFTER the SelectGuard de-selected this bitmap from
        // the mem DC, so it is not selected into any DC at deletion.
        unsafe {
            let _ = DeleteObject(self.0);
        }
    }
}

/// Selects `obj` into `dc` and restores the DC's previous object on drop.
struct SelectGuard {
    dc: HDC,
    old: HGDIOBJ,
}
impl SelectGuard {
    fn select(dc: HDC, bmp: HBITMAP) -> Self {
        // SAFETY: `dc`/`bmp` are valid, freshly created; `SelectObject` returns
        // the DC's previous (stock) object, which `drop` restores. `HBITMAP`
        // satisfies `SelectObject`'s `Param<HGDIOBJ>` bound directly (it
        // `CanInto<HGDIOBJ>`), so no explicit conversion is needed.
        let old = unsafe { SelectObject(dc, bmp) };
        Self { dc, old }
    }
}
impl Drop for SelectGuard {
    fn drop(&mut self) {
        // SAFETY: restores the DC's original object before the bitmap/DC guards
        // (which drop after this one) free their handles.
        unsafe {
            let _ = SelectObject(self.dc, self.old);
        }
    }
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
    render_to_rgb(w, h, |mem_dc| {
        // A second desktop DC as the BitBlt source, null-checked before use
        // (like every other handle in this file). Independent of
        // `render_to_rgb`'s own screen DC (each GetDC/ReleaseDC pair is
        // self-contained), which keeps this closure's `HDC -> Result<()>`
        // shape identical to the PrintWindow path below.
        // SAFETY: `None` requests the whole-desktop DC; `ReleaseDC` below
        // releases exactly it, and `BitBlt` reads from a validated source DC.
        let src_dc = unsafe { GetDC(None) };
        if src_dc.is_invalid() {
            return Err(windows::core::Error::from_win32());
        }
        let r = unsafe { BitBlt(mem_dc, 0, 0, w, h, src_dc, x, y, SRCCOPY) };
        unsafe {
            let _ = ReleaseDC(None, src_dc);
        }
        r
    })
}

/// `PrintWindow(PW_RENDERFULLCONTENT)`-capture `hwnd` ONLY — no `BitBlt`
/// fallback. Split out of [`capture_window_pixels`] so [`capture_probe`] (the
/// WGC smoke-test diagnostic) can demonstrate the black-bitmap bug directly:
/// `PrintWindow` returns `TRUE`/`Ok` for a GPU-composited window (the bug case
/// below), so [`capture_window_pixels`]'s `Err`-triggered `BitBlt` fallback
/// never fires for it either — this function's raw result IS what a caller
/// would see before P4, and reproduces the bug on demand.
fn print_window_only(hwnd: HWND, w: i32, h: i32) -> Result<image::RgbImage, String> {
    render_to_rgb(w, h, |mem_dc| unsafe {
        PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).ok()
    })
}

/// `PrintWindow`-capture `hwnd`, falling back to a `BitBlt` screen-scrape of
/// its on-screen rect `(x, y, w, h)` if `PrintWindow` itself fails. The P1
/// window-capture path, now the FALLBACK behind [`wgc::capture_window`] (see
/// this module's doc) — [`crate::platform::windows::capture::capture_window`]
/// only reaches this when WGC itself failed.
///
/// KNOWN GAP: some GPU-composited surfaces (hardware-accelerated browsers/
/// games, certain Electron apps) return `PrintWindow == TRUE` yet render a
/// BLACK or partially-blank bitmap — the content never reaches the GDI DC. We
/// can't distinguish that from a legitimately dark window here (a black-pixel
/// heuristic has real false positives), so this fallback does NOT try to
/// detect it — which is exactly why P4 made [`wgc::capture_window`] the
/// PRIMARY path instead of trying to patch this one.
fn capture_window_pixels(
    hwnd: HWND,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<image::RgbImage, String> {
    match print_window_only(hwnd, w, h) {
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

/// Capture a single on-screen window matching `query`. Tries
/// [`wgc::capture_window`] (Windows.Graphics.Capture) FIRST — see this
/// module's doc for why that must be the primary path — falling back to the
/// P1 `PrintWindow`/`BitBlt` ladder ([`capture_window_pixels`]) on any WGC
/// failure.
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
    let native = match wgc::capture_window(hwnd) {
        Ok(img) => img,
        Err(e) => {
            tracing::debug!(
                "Windows.Graphics.Capture failed ({e}); falling back to PrintWindow/BitBlt (may \
                 return a black bitmap on a GPU-composited surface — see this module's doc)"
            );
            capture_window_pixels(hwnd, x, y, w, h)?
        }
    };
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

/// The Windows [`crate::platform::ScreenCapture`]: WGC/`PrintWindow`/`BitBlt`,
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

// ── `--capture-probe` diagnostic (P4 WGC smoke test) ──────────────────
//
// Link ≠ working: the whole point of WGC is that it returns real pixels where
// `PrintWindow` silently returns a black bitmap, so the only convincing proof
// is a runtime, non-black-pixel measurement — not just "it compiled" or "it
// didn't error". `main.rs`'s hidden `--capture-probe <window-title>` flag
// calls [`capture_probe`] and prints its report; no MCP round trip needed.

/// Per-channel pixel statistics — the evidence a capture path returned real
/// content (high variance, non-near-zero mean) vs. the `PrintWindow` bug
/// (uniform black: mean ≈ 0, variance ≈ 0).
struct PixelStats {
    mean: (f64, f64, f64),
    variance: (f64, f64, f64),
}

impl std::fmt::Display for PixelStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mean=(R{:.2} G{:.2} B{:.2}) variance=(R{:.2} G{:.2} B{:.2}) stddev=(R{:.2} G{:.2} B{:.2})",
            self.mean.0,
            self.mean.1,
            self.mean.2,
            self.variance.0,
            self.variance.1,
            self.variance.2,
            self.variance.0.sqrt(),
            self.variance.1.sqrt(),
            self.variance.2.sqrt(),
        )
    }
}

/// Compute per-channel mean/variance over every pixel in `img` — two full
/// passes (mean, then sum-of-squared-deviations), which is plenty fast for a
/// one-shot diagnostic image (not a hot path).
fn pixel_stats(img: &image::RgbImage) -> PixelStats {
    let n = (img.width() as f64) * (img.height() as f64);
    if n <= 0.0 {
        return PixelStats {
            mean: (0.0, 0.0, 0.0),
            variance: (0.0, 0.0, 0.0),
        };
    }
    let mut sum = (0f64, 0f64, 0f64);
    for px in img.pixels() {
        sum.0 += px[0] as f64;
        sum.1 += px[1] as f64;
        sum.2 += px[2] as f64;
    }
    let mean = (sum.0 / n, sum.1 / n, sum.2 / n);
    let mut sq_dev = (0f64, 0f64, 0f64);
    for px in img.pixels() {
        sq_dev.0 += (px[0] as f64 - mean.0).powi(2);
        sq_dev.1 += (px[1] as f64 - mean.1).powi(2);
        sq_dev.2 += (px[2] as f64 - mean.2).powi(2);
    }
    PixelStats {
        mean,
        variance: (sq_dev.0 / n, sq_dev.1 / n, sq_dev.2 / n),
    }
}

/// Save `img` as a JPEG next to the system temp dir, returning the path — so
/// a human can eyeball the probe's output alongside its printed stats.
fn save_probe_jpeg(img: &image::RgbImage, tag: &str) -> Result<std::path::PathBuf, String> {
    let path = std::env::temp_dir().join(format!("nova-capture-probe-{tag}.jpg"));
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90)
        .encode(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("jpeg encode failed: {e}"))?;
    std::fs::write(&path, &buf).map_err(|e| format!("write {path:?} failed: {e}"))?;
    Ok(path)
}

/// `--capture-probe <query>`: resolve `query` to a window, then run BOTH
/// capture paths against the SAME live window and report pixel stats for
/// each — the strongest possible evidence for the P4 fix. Two variants,
/// deliberately NOT sharing an early return, so a failure in one still lets
/// the other run and report:
/// - "PrintWindow-only" ([`print_window_only`], no `BitBlt` fallback): the
///   pre-P4 behavior. On a GPU-composited window this is expected to report
///   mean≈0 / variance≈0 (uniform black) — `PrintWindow` returns `Ok`, so the
///   old code's `Err`-triggered fallback never rescues it either.
/// - "WGC" ([`wgc::capture_window`]): the new primary path. Expected to
///   report a high-variance, non-near-zero mean on the same window.
pub fn capture_probe(query: &str) -> Result<String, String> {
    use std::fmt::Write as _;

    let win = resolve_window(query)?;
    // SAFETY: same reconstruction `capture_window` uses — see its comment.
    let hwnd = HWND(win.id as usize as *mut c_void);
    let (w, h) = (win.width as i32, win.height as i32);

    let mut report = String::new();
    let _ = writeln!(
        report,
        "[capture-probe] {query:?} -> {:?} @({:.0},{:.0} {:.0}x{:.0})",
        win.title, win.x, win.y, win.width, win.height
    );

    match print_window_only(hwnd, w, h) {
        Ok(img) => {
            let stats = pixel_stats(&img);
            let _ = writeln!(
                report,
                "PrintWindow-only: OK {}x{} {stats}",
                img.width(),
                img.height()
            );
            match save_probe_jpeg(&img, "printwindow") {
                Ok(path) => {
                    let _ = writeln!(report, "  saved {}", path.display());
                }
                Err(e) => {
                    let _ = writeln!(report, "  (failed to save jpeg: {e})");
                }
            }
        }
        Err(e) => {
            let _ = writeln!(report, "PrintWindow-only: FAILED: {e}");
        }
    }

    match wgc::capture_window(hwnd) {
        Ok(img) => {
            let stats = pixel_stats(&img);
            let _ = writeln!(report, "WGC: OK {}x{} {stats}", img.width(), img.height());
            match save_probe_jpeg(&img, "wgc") {
                Ok(path) => {
                    let _ = writeln!(report, "  saved {}", path.display());
                }
                Err(e) => {
                    let _ = writeln!(report, "  (failed to save jpeg: {e})");
                }
            }
        }
        Err(e) => {
            let _ = writeln!(report, "WGC: FAILED: {e}");
        }
    }

    Ok(report)
}

// ── Windows.Graphics.Capture (WGC) — P4 primary window-capture path ──────
mod wgc {
    //! Windows.Graphics.Capture window capture — see `super`'s (this file's)
    //! module doc for the "why" (the GPU-composited black-bitmap bug this
    //! fixes). This module is the "how": HWND -> `GraphicsCaptureItem` ->
    //! D3D11 device -> free-threaded frame pool/session -> one composited
    //! frame -> CPU-readable RGB pixels.
    //!
    //! # Threading model
    //!
    //! [`capture_window`] runs synchronously on whatever thread calls it —
    //! `server.rs` already runs every `ScreenCapture` entry point inside a
    //! `tokio::task::spawn_blocking` (see `platform::windows::elements::
    //! automation`'s module doc for the identical reasoning applied to UI
    //! Automation), so this function is free to block that thread. What it
    //! blocks ON is the interesting part: `Direct3D11CaptureFramePool::
    //! CreateFreeThreaded` (NOT `Create`, which needs a `DispatcherQueue`/
    //! message loop this thread doesn't run) raises its `FrameArrived` event
    //! on an INTERNAL WinRT threadpool thread, not ours. So the calling
    //! thread starts the session, then blocks on a bounded
    //! `std::sync::mpsc::sync_channel(1)` that the `FrameArrived` handler (on
    //! that other thread) delivers the decoded frame — or an error — into.
    //! This is deliberately NOT a naked `TryGetNextFrame()` poll loop (races
    //! the compositor, wastes CPU) and NOT routed through a `DispatcherQueue`
    //! (this process runs no per-thread UI message pump for one to dispatch
    //! on) — the free-threaded pool + event + channel combination is the one
    //! that needs neither.
    //!
    //! # COM apartment
    //!
    //! WinRT activation (`factory::<GraphicsCaptureItem, _>()`,
    //! `CreateForWindow`, `CreateDirect3D11DeviceFromDXGIDevice`) needs this
    //! thread joined to the process's Multi-Threaded Apartment, exactly like
    //! UI Automation (`platform::windows::elements::automation::
    //! ensure_com_mta`, which an in-flight, concurrently-developed PR is
    //! promoting to `pub(crate)` on this same file tree). To avoid a merge
    //! race with that PR, THIS module keeps its own temporary, private
    //! `CoInitializeEx(MULTITHREADED)` join below rather than depending on
    //! that helper's new visibility before it lands — it is a deliberate,
    //! reviewed duplicate, not an oversight, and is expected to be collapsed
    //! into the shared helper in a follow-up once both PRs are merged.
    //!
    //! # The RowPitch gotcha (read before touching [`frame_to_rgb`])
    //!
    //! The staging texture's mapped row stride (`D3D11_MAPPED_SUBRESOURCE::
    //! RowPitch`) is USUALLY NOT `width * 4` — the driver pads each row to its
    //! own alignment. Indexing the mapped buffer as one contiguous
    //! `width * 4 * height` block (instead of `row * RowPitch` per row) reads
    //! garbage/shifted pixels past the first row on most GPUs. This is widely
    //! considered THE #1 WGC integration bug; [`frame_to_rgb`] below indexes
    //! every row by `RowPitch` and only reads the first `width * 4` bytes of
    //! each.
    //!
    //! # Border / DRM (deliberately NOT handled)
    //!
    //! WGC draws a thin yellow capture-indicator border around the captured
    //! window by default. Suppressing it (`IGraphicsCaptureSession3::
    //! SetIsBorderRequired(false)`) requires an MSIX packaging identity plus a
    //! user consent prompt that an unpackaged nova.exe cannot provide, so this
    //! module never calls it — the border is a cosmetic system overlay drawn
    //! OUTSIDE the captured content; it does not appear in the captured
    //! pixels/coordinates. A DRM/protected window going black under WGC is
    //! likewise an OS-level content-protection limit, not a bug here — it
    //! just means this function returns non-representative pixels, same as
    //! `PrintWindow` would; a real screenshot of a real window is still
    //! produced.
    use std::sync::mpsc;
    use std::time::Duration;
    use windows::core::Interface;
    use windows::Foundation::TypedEventHandler;
    use windows::Graphics::Capture::{
        Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
        GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
        D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

    /// How long to wait for WGC's first composited frame before giving up and
    /// letting the caller fall back to `PrintWindow`/`BitBlt`. Cold start
    /// (device/pool/session stand-up, DWM handing over the first composited
    /// frame) can take a few compositor vsyncs on a loaded system; 3s is
    /// generous headroom without hanging a `screenshot` call for long.
    const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(3);

    thread_local! {
        /// TEMPORARY duplicate of `platform::windows::elements::automation`'s
        /// `COM_MTA_JOINED` — see this module's doc for why it isn't reused
        /// as-is yet. Same idempotent-per-thread, never-`CoUninitialize`d
        /// design: join is a per-thread refcount bump, and these are
        /// long-lived tokio blocking-pool threads, so leaking the join until
        /// thread exit (OS/CRT-cleaned) is simpler and sound.
        static COM_MTA_JOINED: () = {
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
            // SAFETY: `None`/`COINIT_MULTITHREADED` are documented, pointer-free
            // arguments; safe to call from any thread, any number of times.
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr.is_err() {
                tracing::warn!(
                    "CoInitializeEx(COINIT_MULTITHREADED) returned {hr:?} on this thread — WGC \
                     calls here may fail (see platform::windows::capture::wgc's module doc)"
                );
            }
        };
    }

    /// Join this thread to the process's Multi-Threaded Apartment. MUST run
    /// before any WinRT/D3D11 activation below.
    fn ensure_com_mta() {
        COM_MTA_JOINED.with(|_| {});
    }

    /// The D3D11 device/context this module creates once per capture, plus
    /// the WinRT-wrapped handle to the SAME device the frame pool needs.
    struct D3d11Device {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        winrt_device: IDirect3DDevice,
    }

    /// `D3D11CreateDevice(D3D_DRIVER_TYPE_HARDWARE, BGRA_SUPPORT)` -> QI
    /// `IDXGIDevice` -> `CreateDirect3D11DeviceFromDXGIDevice` -> cast to the
    /// WinRT `IDirect3DDevice` the frame pool is parameterized on.
    /// `D3D11_CREATE_DEVICE_BGRA_SUPPORT` is mandatory — the frame pool's
    /// `B8G8R8A8UIntNormalized` pixel format requires a BGRA-capable device.
    fn create_d3d_device() -> Result<D3d11Device, String> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        // SAFETY: `None`/`None` (adapter/software module) select the default
        // hardware adapter; no feature-level array requests the driver's
        // best-supported level; the two `Some(&mut _)` out-params are valid,
        // freshly-declared locals.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| format!("D3D11CreateDevice failed: {e}"))?;
        let device = device.ok_or("D3D11CreateDevice returned a null device")?;
        let context = context.ok_or("D3D11CreateDevice returned a null immediate context")?;

        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|e| format!("QI ID3D11Device -> IDXGIDevice failed: {e}"))?;
        // SAFETY: `dxgi_device` is the live device just created above.
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
            .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice failed: {e}"))?;
        let winrt_device: IDirect3DDevice = inspectable
            .cast()
            .map_err(|e| format!("cast IInspectable -> IDirect3DDevice failed: {e}"))?;

        Ok(D3d11Device {
            device,
            context,
            winrt_device,
        })
    }

    /// HWND -> `GraphicsCaptureItem` via the Win32 interop factory —
    /// deliberately NOT `GraphicsCaptureItem::TryCreateFromWindowId`, which is
    /// a WinAppSDK API that additionally needs interactive user consent nova
    /// (an unpackaged .exe) cannot provide.
    fn create_capture_item(hwnd: HWND) -> Result<GraphicsCaptureItem, String> {
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|e| format!("activating IGraphicsCaptureItemInterop failed: {e}"))?;
        // SAFETY: `hwnd` is a live top-level window handle resolved by
        // `capture::resolve_window` just before this call.
        unsafe { interop.CreateForWindow(hwnd) }
            .map_err(|e| format!("IGraphicsCaptureItemInterop::CreateForWindow failed: {e}"))
    }

    /// Read `frame`'s composited surface back to CPU-side RGB pixels:
    /// `Surface()` -> QI `IDirect3DDxgiInterfaceAccess` -> `GetInterface::
    /// <ID3D11Texture2D>()` (the live GPU texture) -> copy into a STAGING
    /// texture (the live one is typically render-target-only, no CPU access)
    /// -> `Map`/read/`Unmap`. See this module's doc for the RowPitch gotcha
    /// this function's row loop exists specifically to avoid.
    fn frame_to_rgb(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        frame: &Direct3D11CaptureFrame,
    ) -> Result<image::RgbImage, String> {
        let surface = frame
            .Surface()
            .map_err(|e| format!("Direct3D11CaptureFrame::Surface failed: {e}"))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|e| format!("cast IDirect3DSurface -> IDirect3DDxgiInterfaceAccess: {e}"))?;
        // SAFETY: `access` was just obtained from a live frame surface;
        // `GetInterface::<ID3D11Texture2D>` is the documented WinRT<->DXGI
        // interop call to reach the surface's backing D3D11 texture.
        let src_tex: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|e| format!("GetInterface<ID3D11Texture2D> failed: {e}"))?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `src_tex` is the valid, live texture just obtained above.
        unsafe { src_tex.GetDesc(&mut desc) };
        let (w, h) = (desc.Width, desc.Height);
        if w == 0 || h == 0 {
            return Err(format!("WGC frame has a non-positive size ({w}x{h})"));
        }

        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.Format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: `device` is the SAME device that created `src_tex`
        // (both trace back to this module's one `create_d3d_device` call);
        // `staging_desc` describes a same-size, same-format, CPU-readable
        // staging copy — the standard "copy then map" GPU readback pattern.
        unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
            .map_err(|e| format!("CreateTexture2D(staging) failed: {e}"))?;
        let staging = staging.ok_or("CreateTexture2D(staging) returned a null texture")?;

        // SAFETY: `staging`/`src_tex` are both live, same-device, same-size,
        // same-format textures — a valid `CopyResource` pair.
        unsafe { context.CopyResource(&staging, &src_tex) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `staging` was created with `USAGE_STAGING` +
        // `CPU_ACCESS_READ` above, making it valid to `Map` for reading;
        // `context` is the immediate context paired with `device`.
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
            .map_err(|e| format!("Map(staging) failed: {e}"))?;

        // THE ROW-PITCH GOTCHA: `mapped.RowPitch` is the GPU/driver-padded
        // stride, almost always > `w * 4` — index each row by `RowPitch`, and
        // read only the first `w * 4` bytes of it. Treating the buffer as one
        // contiguous `w * 4 * h` block (ignoring `RowPitch`) is THE most
        // common WGC integration bug and would shift/corrupt every row past
        // the first on most GPUs.
        let mut rgb = Vec::with_capacity((w as usize) * (h as usize) * 3);
        // SAFETY: `mapped.pData` is valid for `mapped.RowPitch * h` bytes
        // (the successful `Map` above guarantees at least that much, padded
        // per row); this loop only ever reads the first `w * 4` bytes of each
        // `RowPitch`-strided row, staying within that bound.
        unsafe {
            let base = mapped.pData as *const u8;
            for row in 0..h as usize {
                let row_ptr = base.add(row * mapped.RowPitch as usize);
                let row_slice = std::slice::from_raw_parts(row_ptr, w as usize * 4);
                for px in row_slice.chunks_exact(4) {
                    // B8G8R8A8UIntNormalized -> RGB (same BGRA byte order as
                    // GDI's 32bpp DIB — see `read_bitmap_rgb` in this file).
                    rgb.push(px[2]);
                    rgb.push(px[1]);
                    rgb.push(px[0]);
                }
            }
        }

        // SAFETY: unmaps exactly the subresource just `Map`ped above.
        unsafe { context.Unmap(&staging, 0) };

        image::RgbImage::from_raw(w, h, rgb)
            .ok_or_else(|| "failed to build an RgbImage from the WGC frame".to_string())
    }

    /// The full WGC pipeline for one window: HWND -> item -> device -> pool +
    /// session -> ONE composited frame -> RGB pixels. Returns `Err` for the
    /// caller ([`super::capture_window`]) to fall back to `PrintWindow`/
    /// `BitBlt` on: an unsupported OS, any WinRT/D3D11 activation failure, or
    /// a 3s timeout waiting for the first frame (cold start budget — see
    /// [`FIRST_FRAME_TIMEOUT`]).
    pub(super) fn capture_window(hwnd: HWND) -> Result<image::RgbImage, String> {
        ensure_com_mta();

        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err(
                "GraphicsCaptureSession::IsSupported() = false (needs Windows 10 1903+)"
                    .to_string(),
            );
        }

        let item = create_capture_item(hwnd)?;
        let size = item
            .Size()
            .map_err(|e| format!("GraphicsCaptureItem::Size failed: {e}"))?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err(format!(
                "GraphicsCaptureItem::Size is non-positive ({}x{})",
                size.Width, size.Height
            ));
        }

        let d3d = create_d3d_device()?;

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d.winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .map_err(|e| format!("Direct3D11CaptureFramePool::CreateFreeThreaded failed: {e}"))?;

        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|e| format!("CreateCaptureSession failed: {e}"))?;
        // Deliberately NOT calling `session.SetIsBorderRequired(false)` — see
        // this module's doc ("Border / DRM") for why: it needs MSIX packaging
        // identity + a consent prompt nova can't provide, and the border is
        // purely a cosmetic overlay outside the captured pixels anyway.

        let (tx, rx) = mpsc::sync_channel::<Result<image::RgbImage, String>>(1);
        let device_for_handler = d3d.device.clone();
        let context_for_handler = d3d.context.clone();
        let token = pool
            .FrameArrived(&TypedEventHandler::<
                Direct3D11CaptureFramePool,
                windows::core::IInspectable,
            >::new(move |sender, _args| {
                let result = (|| -> Result<image::RgbImage, String> {
                    let sender = sender
                        .as_ref()
                        .ok_or_else(|| "FrameArrived: null sender".to_string())?;
                    let frame = sender
                        .TryGetNextFrame()
                        .map_err(|e| format!("TryGetNextFrame failed: {e}"))?;
                    frame_to_rgb(&device_for_handler, &context_for_handler, &frame)
                })();
                // Bounded (1) channel: if a frame somehow already delivered
                // (a second FrameArrived firing before we unsubscribe below),
                // `try_send` just drops this one — we only need the first.
                let _ = tx.try_send(result);
                Ok(())
            }))
            .map_err(|e| format!("FrameArrived subscribe failed: {e}"))?;

        session
            .StartCapture()
            .map_err(|e| format!("GraphicsCaptureSession::StartCapture failed: {e}"))?;

        let outcome = rx.recv_timeout(FIRST_FRAME_TIMEOUT);

        // Teardown, on EITHER outcome — unsubscribe first so no further
        // `FrameArrived` fires while we're closing the session/pool under it.
        let _ = pool.RemoveFrameArrived(token);
        let _ = session.Close();
        let _ = pool.Close();

        match outcome {
            Ok(Ok(img)) => Ok(img),
            Ok(Err(e)) => Err(format!("WGC frame processing failed: {e}")),
            Err(_) => Err(format!(
                "WGC timed out after {FIRST_FRAME_TIMEOUT:?} waiting for the first composited \
                 frame"
            )),
        }
    }
}
