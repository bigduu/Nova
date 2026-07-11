//! System clipboard read/write (Windows).
//!
//! Uses the raw Win32 clipboard API (`CF_UNICODETEXT`) directly rather than a
//! cross-platform crate (e.g. `arboard`) — the open/lock/copy/close sequence
//! is short and well-understood, and this keeps the Windows dependency
//! surface to just the `windows` crate (see Cargo.toml's
//! `[target.'cfg(target_os = "windows")'.dependencies]` doc comment).
use crate::error::{NovaError, Result};
use windows::Win32::Foundation::{GlobalFree, HANDLE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Read the current clipboard contents as text. Returns an empty string (not
/// an error) when the clipboard holds no `CF_UNICODETEXT` data — mirrors
/// `pbpaste` on an empty/non-text clipboard, which also just prints nothing.
pub fn read_clipboard() -> Result<String> {
    // SAFETY: `None` (no new clipboard owner window) is documented as valid —
    // this process only reads, it never needs to own the clipboard for that.
    unsafe { OpenClipboard(None) }
        .map_err(|e| NovaError::Clipboard(format!("OpenClipboard failed: {e}")))?;
    let result = read_locked();
    // SAFETY: closes exactly the clipboard just opened above, unconditionally
    // (even if `read_locked` returned an error), so a failure never leaves the
    // clipboard wedged open for the rest of the system.
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

fn read_locked() -> Result<String> {
    // SAFETY: `GetClipboardData` returns a handle still OWNED by the
    // clipboard — we read through it but never free it ourselves.
    let handle = match unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) } {
        Ok(h) => h,
        Err(_) => return Ok(String::new()), // no CF_UNICODETEXT on the clipboard
    };
    // SAFETY: `handle` is the HANDLE `GetClipboardData` just returned for a
    // CF_UNICODETEXT entry, which is always backed by an HGLOBAL; `GlobalLock`
    // returns a pointer valid until the matching `GlobalUnlock` below.
    let ptr = unsafe { GlobalLock(hglobal_from(handle)) };
    if ptr.is_null() {
        return Ok(String::new());
    }
    // The data is a NUL-terminated UTF-16 string; scan for the terminator
    // rather than trusting `GlobalSize` (which rounds up to the allocator's
    // granularity, not the exact string length).
    let text = unsafe {
        let base = ptr as *const u16;
        let mut len = 0usize;
        while *base.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(base, len))
    };
    // SAFETY: unlocks exactly the handle just locked above.
    unsafe {
        let _ = GlobalUnlock(hglobal_from(handle));
    }
    Ok(text)
}

/// Write `text` to the system clipboard as `CF_UNICODETEXT`.
pub fn write_clipboard(text: &str) -> Result<()> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0); // NUL terminator CF_UNICODETEXT readers expect

    // SAFETY: see `read_clipboard` — `None` is a documented valid owner arg.
    unsafe { OpenClipboard(None) }
        .map_err(|e| NovaError::Clipboard(format!("OpenClipboard failed: {e}")))?;
    let result = write_locked(&utf16);
    // SAFETY: closes exactly the clipboard opened above, unconditionally.
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

fn write_locked(utf16: &[u16]) -> Result<()> {
    // SAFETY: `EmptyClipboard` requires the clipboard already be open by this
    // thread (true here — the caller just opened it) and takes ownership of
    // the clipboard for the subsequent `SetClipboardData`.
    unsafe { EmptyClipboard() }
        .map_err(|e| NovaError::Clipboard(format!("EmptyClipboard failed: {e}")))?;

    let bytes = std::mem::size_of_val(utf16);
    // SAFETY: allocates a moveable global block sized exactly for `utf16`.
    // Per `SetClipboardData`'s contract, a SUCCESSFUL call transfers ownership
    // of this block to the clipboard/system — we must not free or otherwise
    // touch it again after that call succeeds (we don't, below).
    let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
        .map_err(|e| NovaError::Clipboard(format!("GlobalAlloc failed: {e}")))?;

    // Until SetClipboardData SUCCEEDS, `hmem` is still OURS — every early
    // return below must GlobalFree it or it leaks (a realistic trigger is a
    // clipboard-manager holding the clipboard, making GlobalLock/
    // SetClipboardData fail). Ownership transfers to the clipboard ONLY on a
    // successful SetClipboardData; we free on every failing path before then.
    // SAFETY: `hmem` was just allocated above with exactly `bytes` capacity;
    // the returned pointer is valid until the matching `GlobalUnlock` below.
    let ptr = unsafe { GlobalLock(hmem) };
    if ptr.is_null() {
        // SAFETY: `hmem` is still owned by us (SetClipboardData not yet called)
        // and is not locked (GlobalLock returned null), so freeing it is sound.
        unsafe {
            let _ = GlobalFree(hmem);
        }
        return Err(NovaError::Clipboard(
            "GlobalLock returned null right after a successful GlobalAlloc".to_string(),
        ));
    }
    // SAFETY: `ptr` has room for exactly `utf16.len()` u16s (that's what we
    // just allocated); `utf16` is a distinct, non-overlapping source buffer.
    unsafe {
        std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr as *mut u16, utf16.len());
        let _ = GlobalUnlock(hmem);
    }

    // SAFETY: `hmem` is a live HGLOBAL we just filled; on success the
    // clipboard now owns it (see the GlobalAlloc comment above).
    if let Err(e) = unsafe { SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(hmem.0)) } {
        // SetClipboardData failed, so ownership did NOT transfer — free our block.
        // SAFETY: `hmem` is still ours and is unlocked (GlobalUnlock above).
        unsafe {
            let _ = GlobalFree(hmem);
        }
        return Err(NovaError::Clipboard(format!(
            "SetClipboardData failed: {e}"
        )));
    }
    Ok(())
}

/// `GetClipboardData`/`SetClipboardData` traffic in the generic `HANDLE` type,
/// but every other clipboard memory API (`GlobalLock`/`GlobalUnlock`) wants
/// the more specific `HGLOBAL` — both are `#[repr(transparent)]` wrappers
/// around the same `*mut c_void`, so this is a same-representation reinterpret,
/// not a real conversion.
fn hglobal_from(h: windows::Win32::Foundation::HANDLE) -> windows::Win32::Foundation::HGLOBAL {
    windows::Win32::Foundation::HGLOBAL(h.0)
}

/// The Windows [`crate::platform::Clipboard`]: Win32 `CF_UNICODETEXT`, via
/// [`read_clipboard`]/[`write_clipboard`].
pub struct WinClipboard;

impl crate::platform::Clipboard for WinClipboard {
    fn read(&self) -> Result<String> {
        read_clipboard()
    }

    fn write(&self, text: &str) -> Result<()> {
        write_clipboard(text)
    }
}
