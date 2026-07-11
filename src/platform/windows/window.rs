//! Window/application enumeration, activation, and launching (Windows).
//!
//! Window listing is a single `EnumWindows` pass (Windows enumerates top-level
//! windows in Z-order, frontmost first — the same ordering
//! `tools::window::frontmost_app_pid` relies on for its "first titled,
//! non-system window" heuristic, so no extra sorting is needed here),
//! **filtered to on-screen windows** (see [`enum_all_windows`]): this reproduces
//! the invariant macOS's `list_windows` gets for free from
//! `SCShareableContent`'s `on_screen_only`/`exclude_desktop`, so the neutral
//! `tools::window::list_windows` filter (`!title.is_empty()`) stays sufficient
//! on BOTH platforms. Without it, a raw `EnumWindows` sweep leaks hidden
//! IME/tray/helper windows and minimized windows (whose rect is off at
//! ~(-32000,-32000)), which would in turn let `frontmost_app_pid`/
//! `pid_for_window` mistarget and let `capture`'s largest-area `resolve_window`
//! pick a minimized window → a black/garbage grab.
//!
//! Application listing walks the two Start-Menu "Programs" folders for
//! shortcuts (no COM shell-link parsing needed for a name/path listing — P1
//! MVP, same spirit as the macOS `mdfind` scan). `open_application` prefers
//! raising an already-running, ON-SCREEN window over spawning a second
//! instance (mirroring macOS's `open -a`), falling back to `ShellExecuteW`
//! whenever no such window is found (so it never reports success for a
//! tray-only background instance it couldn't actually raise).
use crate::platform::WindowHandle;
use crate::tools::application::ApplicationInfo;
use std::ffi::c_void;
use windows::core::{Result as WinResult, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::ProcessStatus::GetModuleBaseNameW;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOWNORMAL,
};

/// One window discovered by an `EnumWindows` pass, before we decide whether the
/// caller wants the neutral [`WindowHandle`] or just the raw handle (activation).
struct RawWindow {
    hwnd: HWND,
    pid: i32,
    title: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    is_visible: bool,
}

/// `EnumWindows` callback: append every top-level window to the `Vec<HWND>`
/// behind `lparam`. Always returns `TRUE` (continue enumeration) — filtering
/// happens afterward so this stays a pure collector.
unsafe extern "system" fn collect_hwnd(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<HWND>);
    out.push(hwnd);
    BOOL(1)
}

/// Read `hwnd`'s title via `GetWindowTextW`, sized exactly (`GetWindowTextLengthW`
/// first) rather than a fixed guess buffer.
fn window_title(hwnd: HWND) -> String {
    // SAFETY: both calls take only `hwnd` (or, for the second, a slice we own
    // and size ourselves) and are safe to call from any thread for any window
    // handle, live or stale (a stale handle just yields 0).
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    buf.truncate(copied.max(0) as usize);
    String::from_utf16_lossy(&buf)
}

/// The owning process's image base name (e.g. `notepad.exe`), best-effort —
/// empty string if the process can't be opened (protected/elevated process
/// without matching privilege, or it exited between enumeration and lookup).
fn process_base_name(pid: u32) -> String {
    // SAFETY: `OpenProcess` takes a plain access-rights flag and a pid; the
    // returned handle is closed unconditionally before returning below.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return String::new();
    };
    let mut buf = [0u16; 260]; // MAX_PATH
                               // SAFETY: `buf` is a stack array we own and size correctly; `handle` is a
                               // valid, just-opened process handle.
    let len = unsafe { GetModuleBaseNameW(handle, None, &mut buf) };
    // SAFETY: closes the handle opened just above; no other reference to it.
    unsafe {
        let _ = CloseHandle(handle);
    }
    if len == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

/// Whether `hwnd` is DWM-**cloaked** — composited but deliberately hidden by
/// the shell (a suspended background UWP/Store app, a window on another virtual
/// desktop, etc.). Such a window reports `IsWindowVisible == TRUE` yet is not
/// actually on screen, so it must be excluded alongside the minimized check.
/// Best-effort: any `DwmGetWindowAttribute` failure is treated as "not cloaked"
/// (never hides a window we couldn't classify).
fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: `hwnd` is a live top-level handle from `EnumWindows`; we pass a
    // pointer to our own `u32` and its exact size, exactly as
    // `DwmGetWindowAttribute(DWMWA_CLOAKED, ...)` documents. On any error the
    // `Result` is `Err` and `cloaked` stays 0 (untouched).
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    ok.is_ok() && cloaked != 0
}

/// Whether `hwnd` is genuinely ON SCREEN — visible, not minimized, not cloaked.
/// `IsWindowVisible` alone is insufficient: it stays `TRUE` for a minimized
/// window (hence the `IsIconic` check) and for a DWM-cloaked one (hence
/// [`is_cloaked`]). This is the predicate that reproduces macOS
/// `SCShareableContent`'s on-screen invariant — see the module doc.
fn is_on_screen(hwnd: HWND) -> bool {
    // SAFETY: both are argless-per-handle Win32 queries on a live top-level
    // handle from `EnumWindows`.
    unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() && !is_cloaked(hwnd) }
}

/// One `EnumWindows` pass, decorated with pid/title/rect, WITHOUT the on-screen
/// filter — includes minimized and hidden windows. `is_visible` is the raw
/// `IsWindowVisible` result per window. Only `open_application`'s "raise an
/// existing instance" path uses this directly (it must see a MINIMIZED window
/// in order to restore it); every other caller goes through the filtered
/// [`enum_all_windows`].
fn enum_windows_raw() -> Result<Vec<RawWindow>, String> {
    // Ensure DPI awareness even if reached without main() (direct call / e2e
    // test): GetWindowRect below returns scaled coordinates for an unaware
    // process on a non-100% display, which would then misplace window captures
    // and coordinate clicks. Idempotent + cheap after the first call.
    super::ensure_dpi_awareness();
    let mut hwnds: Vec<HWND> = Vec::new();
    // SAFETY: `collect_hwnd` only pushes onto the `Vec` behind `lparam`, which
    // outlives the call (it's `hwnds` below, borrowed for the duration of
    // `EnumWindows`); no aliasing beyond that single mutable borrow.
    unsafe {
        EnumWindows(
            Some(collect_hwnd),
            LPARAM(&mut hwnds as *mut Vec<HWND> as isize),
        )
        .map_err(|e| format!("EnumWindows failed: {e}"))?;
    }

    let mut out = Vec::with_capacity(hwnds.len());
    for hwnd in hwnds {
        let mut pid: u32 = 0;
        // SAFETY: `hwnd` came straight from `EnumWindows`; `pid` is a local we own.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        let mut rect = RECT::default();
        // SAFETY: `rect` is a local we own; a stale hwnd just yields an error,
        // which we treat as a zero rect rather than propagating.
        let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
        // SAFETY: argless-per-handle Win32 query.
        let is_visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        out.push(RawWindow {
            hwnd,
            pid: pid as i32,
            title: window_title(hwnd),
            x: rect.left as f64,
            y: rect.top as f64,
            width: (rect.right - rect.left) as f64,
            height: (rect.bottom - rect.top) as f64,
            is_visible,
        });
    }
    Ok(out)
}

/// On-screen windows only ([`is_on_screen`] applied to [`enum_windows_raw`]),
/// so the output matches the invariant macOS's `SCShareableContent`-backed
/// `list_windows` provides — hidden/minimized/cloaked windows never reach
/// `tools::window`. This is what list/targeting/capture all consume (baking the
/// filter in HERE, not leaving it to callers, keeps the neutral
/// `tools::window::list_windows` filter — `!title.is_empty()` — sufficient on
/// both platforms). Still includes UNTITLED on-screen windows, which that
/// neutral filter then drops, same as on macOS.
fn enum_all_windows() -> Result<Vec<RawWindow>, String> {
    Ok(enum_windows_raw()?
        .into_iter()
        .filter(|w| is_on_screen(w.hwnd))
        .collect())
}

/// The HWND of `pid`'s on-screen window whose global frame matches `rect`
/// (the marks capture's clip) within a few px — the Windows analog of macOS's
/// `tools::window::window_id_for_rect`, used by `platform::windows::elements`
/// to anchor Set-of-Mark discovery on the EXACT captured window (not a
/// same-sized sibling), and returning the real `HWND` directly rather than
/// routing through the neutral `WindowHandle.id: u64` (avoids a lossy u64→u32
/// round-trip for no benefit — this caller is already Windows-only). `None`
/// if no on-screen window of `pid` is within tolerance.
pub(crate) fn hwnd_for_rect(pid: i32, rect: (f64, f64, f64, f64)) -> Option<HWND> {
    const TOL: f64 = 4.0;
    let (rx, ry, rw, rh) = rect;
    enum_all_windows()
        .ok()?
        .into_iter()
        .filter(|w| w.pid == pid)
        .map(|w| {
            let d =
                (w.x - rx).abs() + (w.y - ry).abs() + (w.width - rw).abs() + (w.height - rh).abs();
            (d, w.hwnd)
        })
        .filter(|(d, _)| *d <= TOL)
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, hwnd)| hwnd)
}

/// The first (frontmost, Z-order) on-screen `HWND` owned by `pid` — used when
/// `collect_actionable` is called with no clip rect to anchor on (no single
/// captured window), so discovery falls back to whatever window of `pid` is
/// currently frontmost.
pub(crate) fn first_hwnd_for_pid(pid: i32) -> Option<HWND> {
    enum_all_windows()
        .ok()?
        .into_iter()
        .find(|w| w.pid == pid)
        .map(|w| w.hwnd)
}

/// On-screen windows, frontmost first (Z-order, as `EnumWindows` yields them).
pub fn list_windows() -> Result<Vec<WindowHandle>, String> {
    Ok(enum_all_windows()?
        .into_iter()
        .map(|w| WindowHandle {
            // The HWND value itself, opaque outside this module — plays the
            // same "stable per-window id" role `CGWindowID` plays on macOS.
            id: w.hwnd.0 as usize as u64,
            pid: w.pid,
            title: w.title,
            app_name: process_base_name(w.pid as u32),
            x: w.x,
            y: w.y,
            width: w.width,
            height: w.height,
            is_visible: w.is_visible,
        })
        .collect())
}

/// List installed applications by walking the two Start-Menu "Programs"
/// folders (per-machine + per-user) for shortcuts. A P1 MVP: this lists
/// LAUNCHABLE names/paths (mirroring the macOS `mdfind` scan's contract)
/// without resolving `.lnk` targets via COM `IShellLink` — the shortcut path
/// itself is enough for a listing, and `open_application` below doesn't need
/// the resolved target either (it re-launches by name via `ShellExecuteW`,
/// which itself follows `.lnk`/App-Paths resolution).
pub fn list_applications() -> crate::error::Result<Vec<ApplicationInfo>> {
    let mut apps = Vec::new();
    for dir in start_menu_dirs() {
        collect_shortcuts(&dir, &mut apps);
    }
    apps.sort_by_key(|a: &ApplicationInfo| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.name == b.name && a.path == b.path);
    Ok(apps)
}

fn start_menu_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(pd) = std::env::var("ProgramData") {
        dirs.push(std::path::PathBuf::from(pd).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    if let Ok(ad) = std::env::var("APPDATA") {
        dirs.push(std::path::PathBuf::from(ad).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    dirs
}

/// Recursively collect `.lnk` shortcuts under `dir` into `out`.
fn collect_shortcuts(dir: &std::path::Path, out: &mut Vec<ApplicationInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_shortcuts(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
        {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(ApplicationInfo {
                    name: stem.to_string(),
                    path: path.display().to_string(),
                    bundle_id: None,
                });
            }
        }
    }
}

/// A window this process can actually bring to the user's foreground: a
/// titled, non-cloaked, `IsWindowVisible` window — INCLUDING a minimized one,
/// which we restore. Excludes hidden helper/tray windows (`IsWindowVisible ==
/// FALSE`) and cloaked (background-UWP/other-desktop) windows, so a match here
/// is genuinely something the user would see raised — the distinction that
/// stops `open_application` reporting success for a tray-only instance.
fn is_raisable(hwnd: HWND, title: &str) -> bool {
    // SAFETY: argless-per-handle Win32 query on a live top-level handle.
    !title.is_empty() && unsafe { IsWindowVisible(hwnd).as_bool() } && !is_cloaked(hwnd)
}

/// Bring a raisable window owned by `pid` to the foreground, restoring it first
/// if minimized. Returns whether such a window was FOUND and a raise attempted
/// (`false` = `pid` has no user-visible/restorable window — e.g. a tray-only
/// background instance). The raise itself is still best-effort even when this
/// returns `true`: `SetForegroundWindow` can be refused by Windows' focus-
/// stealing prevention when the calling process isn't itself foreground/
/// recently input-active — in that case the window may only flash in the
/// taskbar, which is a Windows OS policy, not a bug here.
pub fn raise_pid(pid: i32) -> bool {
    // Raw (unfiltered) sweep on purpose: a minimized existing instance is a
    // valid raise target (we restore it), but `enum_all_windows` would have
    // filtered it out as "not on screen".
    let Ok(windows) = enum_windows_raw() else {
        return false;
    };
    let Some(w) = windows
        .into_iter()
        .find(|w| w.pid == pid && is_raisable(w.hwnd, &w.title))
    else {
        return false;
    };
    // SAFETY: `w.hwnd` came from a just-completed `EnumWindows` pass.
    unsafe {
        if IsIconic(w.hwnd).as_bool() {
            let _ = ShowWindow(w.hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(w.hwnd);
    }
    true
}

/// Raise an already-running instance whose owning process's base name matches
/// `name` (case-insensitive, `.exe` suffix optional on either side), restoring
/// it if minimized. Returns whether one was found and raised — `false` means
/// no user-visible/restorable window exists for that name (so `open_application`
/// must fall through to actually launching it, never reporting a phantom
/// success).
fn raise_existing_instance(name: &str) -> bool {
    let want = name.trim_end_matches(".exe").to_lowercase();
    let Ok(windows) = enum_windows_raw() else {
        return false;
    };
    windows
        .into_iter()
        .filter(|w| is_raisable(w.hwnd, &w.title))
        .find(|w| {
            process_base_name(w.pid as u32)
                .trim_end_matches(".exe")
                .eq_ignore_ascii_case(&want)
        })
        .map(|w| raise_pid(w.pid))
        .unwrap_or(false)
}

/// `ShellExecuteW(open, target)` — resolves a bare executable name via the
/// "App Paths" registry (the same mechanism the Run dialog uses, so
/// `"notepad"`/`"notepad.exe"`/a full path/a `.lnk` path all work), a document
/// path via its default handler, or a URL via the registered protocol handler.
fn launch(target: &str) -> WinResult<()> {
    let verb = HSTRING::from("open");
    let file = HSTRING::from(target);
    // SAFETY: both string params are owned `HSTRING`s kept alive for the
    // duration of this call; `hwnd`/`parameters`/`directory` are null, which
    // ShellExecuteW documents as valid (use the current directory, no args,
    // no owner window).
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Per the Win32 docs, ShellExecuteW returns a value > 32 on success; the
    // HINSTANCE-shaped return otherwise carries one of the SE_ERR_* codes.
    if (result.0 as isize) > 32 {
        Ok(())
    } else {
        Err(windows::core::Error::from_win32())
    }
}

/// Launch or focus an application by name — see the module doc for the
/// "raise an existing instance first" rationale. Only reports success from the
/// raise path when a user-visible/restorable window was actually raised;
/// otherwise it falls through to launching (so a tray-only background instance
/// never yields a phantom "opened" with nothing on screen).
pub fn open_application(name: &str) -> crate::error::Result<()> {
    if raise_existing_instance(name) {
        return Ok(());
    }
    launch(name)
        .or_else(|_| launch(&format!("{name}.exe")))
        .map_err(|e| {
            crate::error::NovaError::Application(format!(
                "failed to open {name:?} (tried as-is and with .exe appended): {e}"
            ))
        })
}

/// The Windows [`crate::platform::WindowManager`]: `EnumWindows`-backed
/// listing plus the Start-Menu scan / `ShellExecuteW` launching above.
pub struct WinWindowManager;

impl crate::platform::WindowManager for WinWindowManager {
    fn list_windows(&self) -> Result<Vec<WindowHandle>, String> {
        list_windows()
    }

    fn list_applications(&self) -> crate::error::Result<Vec<ApplicationInfo>> {
        list_applications()
    }

    fn open_application(&self, name: &str) -> crate::error::Result<()> {
        open_application(name)
    }
}
