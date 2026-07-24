//! Platform abstraction layer.
//!
//! `src/tools/*` and `src/server.rs` are the OS-agnostic business logic: they
//! decide WHAT to do (click this mark, capture that window, read the
//! clipboard). This module is the seam between that logic and HOW it actually
//! happens on a given OS — every trait below is the exact surface the tool
//! layer needs, derived from its real call sites (not an invented ideal), and
//! every signature uses ONLY platform-neutral types: plain data (`String`,
//! numbers, pids), the pure-math geometry types already shared across the
//! crate (`crate::display::view::ViewFrame`, `crate::tools::elements::UiElement`,
//! `crate::capture::screenshot::RawCapture`, ...), and `image::RgbImage` (a
//! portable crate, not an Apple framework type). NONE of `objc2`, `objc2-*`,
//! `core-graphics`, `core-foundation`, or `accessibility(-sys)` may appear in
//! a signature here — those belong exclusively inside `platform::mac`.
//!
//! # Coordinate spaces — read this before touching any method below
//!
//! Nova juggles three coordinate spaces; conflating them is THE classic way to
//! introduce a "clicks land in the wrong place" bug. Every method below
//! documents which one it uses:
//! - **Screenshot-pixel space** — pixels of the image the model was shown
//!   (post any downscale). What the MCP tool params (`x`, `y` on
//!   `left_click`, `mouse_move`, ...) arrive in; the *server* (not this
//!   layer) converts these to global-logical via `ViewFrame` before calling
//!   [`InputInjector`].
//! - **Global logical points** — macOS's own device-independent coordinate
//!   space (Retina-independent). What [`InputInjector`], [`UiTree`] element
//!   frames, and [`WindowManager`] window frames are expressed in, and what
//!   `CGEvent`/the Accessibility API natively use.
//! - **Physical/device pixels** — the raw backing-store resolution (2x on a
//!   Retina display). Only [`ScreenCapture`] implementations deal with this,
//!   when deciding how many pixels to actually capture.
//!
//! # macOS + Windows (P1 MVP)
//!
//! The `mac` submodule holds the full macOS implementation (ScreenCaptureKit,
//! CoreGraphics, Accessibility). `windows` is its P1-MVP sibling — SendInput,
//! GDI/PrintWindow capture, EnumWindows, Win32 clipboard — implementing the
//! SAME traits below, gated the same way, so the tool layer
//! (`src/tools/*`/`src/server.rs`) never has to know which OS it's running on.
//! [`UiTree`] on Windows is now a real implementation (P2: Microsoft UI
//! Automation — see `platform::windows::elements`), and [`OcrEngine`] is too
//! (P3: `Windows.Media.Ocr` — see `platform::windows::ocr`). Every capability
//! on Windows is now a real implementation. Any OS beyond these two
//! is a deliberate, immediate compile error rather than a confusing pile of
//! missing-symbol errors from platform-only crates (see also the dependency
//! gating in `Cargo.toml`, `[target.'cfg(target_os = "...")'.dependencies]`).
#[cfg(target_os = "macos")]
pub mod mac;

#[cfg(target_os = "windows")]
pub mod windows;

// Every other OS gets the headless stub backend: the crate builds, the MCP
// server starts and answers introspection (`initialize`/`tools/list`), and
// every actual desktop action returns a uniform "this build is headless"
// error. Exists for registry health checks (e.g. Glama's Docker probe) and
// Linux CI — see `platform::headless`'s module doc.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub mod headless;

// ── Shared neutral types ─────────────────────────────────────────────
//
// Data returned across the trait boundary. Kept here (rather than duplicated
// per-OS) because every field is already plain data — a future Windows impl
// returns the exact same shapes.

/// One recognized line of OCR text and where it sits in the source image.
#[derive(Debug, Clone)]
pub struct OcrLine {
    /// The recognized text (the engine's top candidate for this line).
    pub text: String,
    /// Recognition confidence in `[0, 1]`.
    pub confidence: f32,
    /// Center of the text's bounding box, in the SOURCE IMAGE's pixel space
    /// (origin top-left) — directly usable as a click coordinate against that
    /// same image.
    pub center: (f64, f64),
}

/// One on-screen window's metadata, frontmost-first.
///
/// Deliberately richer than the MCP-facing `crate::types::WindowInfo` (which
/// drops `pid`/`id`): callers like `tools::window::pid_for_window` and the
/// Set-of-Mark AX walk need the owning pid and a stable per-window identifier
/// (macOS: `CGWindowID`) to disambiguate two same-sized sibling windows before
/// they ever produce a `WindowInfo` for the model.
#[derive(Debug, Clone)]
pub struct WindowHandle {
    /// Opaque, OS-stable window identifier (macOS: `CGWindowID`). `0` if the
    /// implementation can't resolve one for this window.
    pub id: u64,
    /// Owning process id.
    pub pid: i32,
    pub title: String,
    pub app_name: String,
    /// Frame in global logical points.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub is_visible: bool,
}

/// Which portion of an accessibility tree a semantic read should return.
///
/// `Interactive` preserves the existing Set-of-Mark/read_ui contract,
/// `Content` returns human-readable semantic content even when it is not
/// actionable, and `All` combines both in one deterministic snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiReadMode {
    Interactive,
    Content,
    All,
}

impl UiReadMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Content => "content",
            Self::All => "all",
        }
    }

    pub fn includes_interactive(self) -> bool {
        matches!(self, Self::Interactive | Self::All)
    }

    pub fn includes_content(self) -> bool {
        matches!(self, Self::Content | Self::All)
    }
}

/// A semantic node's optional bounds in global logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl UiBounds {
    pub fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn as_tuple(self) -> (f64, f64, f64, f64) {
        (self.x, self.y, self.width, self.height)
    }
}

/// Cross-platform semantic state. `None` means the backend/control does not
/// expose that state; it must never be rendered as a misleading `false`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiNodeStates {
    pub enabled: Option<bool>,
    pub focused: Option<bool>,
    pub selected: Option<bool>,
    pub checked: Option<bool>,
    pub expanded: Option<bool>,
}

/// A node value whose secure/redacted state is impossible to confuse with
/// ordinary text. Platform implementations must decide this before reading or
/// returning a password value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiNodeValue {
    Absent,
    Text(String),
    Redacted,
}

impl UiNodeValue {
    pub fn as_filter_text(&self) -> &str {
        match self {
            Self::Absent => "",
            Self::Text(value) => value,
            Self::Redacted => "[REDACTED]",
        }
    }

    pub fn is_redacted(&self) -> bool {
        matches!(self, Self::Redacted)
    }
}

/// One platform-neutral accessibility/UIA node.
///
/// The optional live handle is carried separately in [`CollectedUiNode`] so
/// readable content nodes can share this same DTO without pretending they are
/// actionable.
#[derive(Debug, Clone, PartialEq)]
pub struct UiNode {
    pub role: String,
    pub name: String,
    pub description: String,
    pub value: UiNodeValue,
    pub actions: Vec<String>,
    pub states: UiNodeStates,
    pub bounds: Option<UiBounds>,
    pub depth: usize,
    pub actionable: bool,
}

/// One collected semantic node plus its optional live action handle.
#[derive(Debug, Clone)]
pub struct CollectedUiNode {
    pub node: UiNode,
    pub handle: Option<Box<dyn ElementHandle>>,
}

/// Exact app/window selected for a semantic read without taking a screenshot.
#[derive(Debug, Clone, PartialEq)]
pub struct UiTarget {
    pub pid: i32,
    pub app_name: String,
    pub window_title: String,
    /// Opaque native window id (CGWindowID/HWND) when available.
    pub window_id: Option<u64>,
    pub bounds: Option<UiBounds>,
}

/// Hard budgets passed into a blocking AX/UIA walk. The platform implementation
/// checks the deadline itself: an outer async timeout cannot cancel an already
/// running `spawn_blocking` task.
#[derive(Debug, Clone, Copy)]
pub struct UiSnapshotOptions {
    pub mode: UiReadMode,
    pub max_nodes: usize,
    pub max_chars: usize,
    pub deadline: std::time::Instant,
}

/// Why a successful snapshot has partial rather than complete coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiPartialReason {
    NodeLimit,
    CharacterLimit,
    Deadline,
    ProviderPartial,
}

impl UiPartialReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NodeLimit => "node_limit",
            Self::CharacterLimit => "character_limit",
            Self::Deadline => "deadline",
            Self::ProviderPartial => "provider_partial",
        }
    }
}

/// Coverage of a successful semantic walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiReadCoverage {
    Complete,
    Partial,
    Empty,
}

impl UiReadCoverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Empty => "empty_view",
        }
    }
}

/// Successful accessibility/UIA snapshot before the server assigns ephemeral
/// snapshot/node ids and click marks.
#[derive(Debug, Clone)]
pub struct UiSnapshot {
    pub target: UiTarget,
    pub nodes: Vec<CollectedUiNode>,
    pub coverage: UiReadCoverage,
    pub truncated: bool,
    pub partial_reason: Option<UiPartialReason>,
}

/// Typed reason a semantic read could not produce a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiReadErrorKind {
    PermissionDenied,
    TargetNotFound,
    NoSemanticTree,
    TimedOut,
    UnsupportedPlatform,
    BackendFailure,
}

impl UiReadErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::TargetNotFound => "target_not_found",
            Self::NoSemanticTree => "no_semantic_tree",
            Self::TimedOut => "timed_out",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::BackendFailure => "backend_failure",
        }
    }
}

/// A typed semantic-read failure with a human diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiReadError {
    pub kind: UiReadErrorKind,
    pub message: String,
}

impl UiReadError {
    pub fn new(kind: UiReadErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for UiReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for UiReadError {}

// ── Capability traits ────────────────────────────────────────────────

/// Screen/window pixel capture.
///
/// Returns [`crate::capture::screenshot::RawCapture`] — pixels plus the
/// [`crate::display::view::ViewFrame`] mapping them back to global-logical
/// points, and (for a window capture) its owning pid. That type is already
/// OS-neutral (`image::RgbImage` + plain math), so overlay drawing,
/// Set-of-Mark annotation, and JPEG encoding all stay in shared code
/// (`crate::capture::screenshot::finish_capture`) — only the raw pixel grab
/// below is behind this trait.
pub trait ScreenCapture: Send + Sync {
    /// Capture the whole main display.
    fn capture_display(&self) -> Result<crate::capture::screenshot::RawCapture, String>;

    /// Capture a single on-screen window whose title or owning-app name
    /// contains `query` (case-insensitive substring).
    fn capture_window(&self, query: &str)
        -> Result<crate::capture::screenshot::RawCapture, String>;

    /// Capture exactly the rectangle `(x, y, w, h)`, given in GLOBAL LOGICAL
    /// points — e.g. a `zoom_region` re-capture of part of the previous view.
    fn capture_region(
        &self,
        rect: (f64, f64, f64, f64),
    ) -> Result<crate::capture::screenshot::RawCapture, String>;
}

/// Synthetic mouse/keyboard input.
///
/// All coordinates are GLOBAL LOGICAL points (the server converts from
/// screenshot-pixel space via `ViewFrame` before calling these). Delivery
/// target (`crate::tools::input::InputTarget`) is already a plain `Global |
/// Pid(i32)` enum — neutral as-is, so it is reused here rather than
/// reinvented; only its two macOS delivery mechanisms (`CGEventPost` to the
/// HID stream vs `CGEventPostToPid`) live behind the trait.
pub trait InputInjector: Send + Sync {
    /// Move the real (global) cursor to `(x, y)`.
    fn mouse_move(&self, x: f64, y: f64) -> crate::error::Result<()>;
    /// Current cursor position.
    fn cursor_position(&self) -> crate::error::Result<(f64, f64)>;
    fn left_click_at(
        &self,
        x: f64,
        y: f64,
        target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()>;
    fn right_click_at(
        &self,
        x: f64,
        y: f64,
        target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()>;
    fn double_click_at(
        &self,
        x: f64,
        y: f64,
        target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()>;
    /// Scroll vertically by `lines` at `(x, y)` (positive = up).
    fn scroll_at(
        &self,
        x: f64,
        y: f64,
        lines: i32,
        target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()>;
    /// A key combination such as `"cmd+c"` or `"shift+tab"`.
    fn key_combo(
        &self,
        combo: &str,
        target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()>;
    /// Type literal Unicode text (CJK/emoji included) into the focused element.
    fn type_text(
        &self,
        text: &str,
        target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()>;
}

/// Application + window enumeration and launching.
pub trait WindowManager: Send + Sync {
    /// On-screen windows, frontmost first. Frames are global logical points.
    fn list_windows(&self) -> Result<Vec<WindowHandle>, String>;
    /// Installed applications (name, bundle path, bundle id).
    fn list_applications(
        &self,
    ) -> crate::error::Result<Vec<crate::tools::application::ApplicationInfo>>;
    /// Launch or focus an application by name.
    fn open_application(&self, name: &str) -> crate::error::Result<()>;
}

/// A live handle to one discovered UI element — the object-safe stand-in for
/// what `AxHandle` wraps (`AXUIElement`) on macOS. Opaque outside a
/// [`UiTree`] implementation; the server caches these between a
/// `marks=true` screenshot and a later `click_mark` purely through this
/// trait, so it never needs to know it's holding an Accessibility handle.
///
/// `Clone` isn't object-safe, hence `clone_box`; a blanket
/// `impl Clone for Box<dyn ElementHandle>` forwards to it so callers can
/// still write ordinary `.clone()`.
pub trait ElementHandle: std::fmt::Debug + Send {
    /// Configure provider-side action timeouts before any live validation or
    /// action RPC. Implementations that have no provider state may keep the
    /// default no-op.
    fn prepare_for_action(&self, _deadline: std::time::Instant) -> Result<(), String> {
        Ok(())
    }
    /// Perform this element's click-like action. Returns the action name
    /// performed (e.g. `"AXPress"`), or an error if nothing in the element's
    /// subtree/ancestry exposes one.
    fn click(&self) -> Result<&'static str, String>;
    /// Whether the handle still points at a live, laid-out element (false
    /// after e.g. a web navigation destroys and rebuilds the tree).
    fn is_alive(&self) -> bool;
    /// This handle's current center in global logical points, if still laid
    /// out — used to re-validate a cached mark against where it now sits.
    fn current_center(&self) -> Option<(f64, f64)>;
    /// If this handle sits inside web content owned by a scriptable browser
    /// (`pid`), click it through the page's OWN JavaScript engine instead of
    /// the Accessibility action (`AXPress` is a silent no-op on most web
    /// content). Returns `None` when this isn't web content in a scriptable
    /// browser (caller should fall back to [`ElementHandle::click`] and then
    /// a coordinate click); `Some(Err)` when the JS path was attempted but
    /// failed (also fall back).
    fn try_web_click(
        &self,
        pid: i32,
        label: &str,
        deadline: std::time::Instant,
    ) -> Option<Result<String, String>>;
    fn clone_box(&self) -> Box<dyn ElementHandle>;
}

impl Clone for Box<dyn ElementHandle> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Set-of-Mark element discovery + query-driven Accessibility actions.
///
/// Degrades gracefully (empty results / `Err` with a human message), never
/// panics: the whole point of marks is to work even when the target app
/// exposes a thin or absent accessibility tree.
pub trait UiTree: Send + Sync {
    /// Resolve an app/window using Accessibility/UIA only. This path must not
    /// invoke screen capture (or ScreenCaptureKit metadata enumeration on
    /// macOS), so `ax_read` works when Screen Recording is denied.
    fn resolve_target(
        &self,
        query: Option<&str>,
        preferred_pid: Option<i32>,
        deadline: std::time::Instant,
    ) -> Result<UiTarget, UiReadError>;

    /// Read a bounded semantic snapshot for `target`.
    fn read_snapshot(
        &self,
        target: &UiTarget,
        options: UiSnapshotOptions,
    ) -> Result<UiSnapshot, UiReadError>;

    /// Discover actionable elements for `pid`, clipped to `clip` (a
    /// global-logical rect) when given. Empty when Accessibility permission
    /// is missing or the app exposes no tree — callers degrade gracefully.
    fn collect_actionable(
        &self,
        pid: i32,
        max: usize,
        clip: Option<(f64, f64, f64, f64)>,
    ) -> Vec<(crate::tools::elements::UiElement, Box<dyn ElementHandle>)>;
    /// Press the first element of `pid` whose role/label contains `query`
    /// (case-insensitive), via whichever click-like action it supports.
    fn ax_click(
        &self,
        pid: i32,
        query: &str,
        deadline: std::time::Instant,
    ) -> Result<String, String>;
    /// Set the matched element's value directly (e.g. fill a field without
    /// focusing/typing).
    fn ax_set_value(
        &self,
        pid: i32,
        query: &str,
        value: &str,
        deadline: std::time::Instant,
    ) -> Result<String, String>;
    /// Move keyboard focus to the matched element.
    fn ax_focus(
        &self,
        pid: i32,
        query: &str,
        deadline: std::time::Instant,
    ) -> Result<String, String>;
    /// Bring `pid`'s app to the front (best-effort) before a coordinate-click
    /// fallback, so the click lands on content rather than merely focusing
    /// the window.
    fn raise_app(&self, pid: i32);
    /// Dump `pid`'s accessibility tree as indented debug text (roles,
    /// labels, actions, frames) — diagnostic only.
    fn dump_tree(&self, pid: i32, max_nodes: usize) -> String;
    /// Keep `pid`'s accessibility tree warm between captures (Chromium/
    /// Electron reap their semantic tree once nothing polls it).
    fn keep_warm(&self, pid: i32);
    /// Stop keeping any app's tree warm (called on a full-display capture,
    /// which has no single target app).
    fn clear_warm(&self);
}

/// System clipboard read/write.
///
/// `tools/clipboard.rs` shells out to `pbpaste`/`pbcopy` today rather than
/// using a cross-platform crate (e.g. `arboard`) — so, per the brief, this
/// stays a first-class capability rather than being skipped.
pub trait Clipboard: Send + Sync {
    fn read(&self) -> crate::error::Result<String>;
    fn write(&self, text: &str) -> crate::error::Result<()>;
}

/// Recognize text in an encoded image (the same JPEG a `screenshot` produces).
pub trait OcrEngine: Send + Sync {
    /// `image` is encoded JPEG/PNG bytes of `img_w` x `img_h` pixels; `languages`
    /// is BCP-47 hints in priority order (e.g. `["zh-Hans", "en-US"]`). Returns
    /// lines with centers in the SAME pixel space as `image` — directly usable
    /// as a click coordinate against the screenshot that produced it.
    fn recognize(
        &self,
        image: &[u8],
        img_w: u32,
        img_h: u32,
        languages: &[&str],
    ) -> Result<Vec<OcrLine>, String>;
}

// ── Facade ────────────────────────────────────────────────────────────
//
// Per-capability accessor functions rather than one `Platform` struct
// bundling every impl. Chosen for TWO reasons specific to this being a
// foundation multiple agents build on in parallel:
//   1. The tools' own usage is already per-capability — `server.rs` never
//      needs "the platform", it needs "the OCR engine" or "the input
//      injector" at one call site each. A single struct would force every
//      field to exist (and every impl to be constructed) before ANYTHING
//      compiles; these functions let each capability land independently.
//   2. Merge friendliness: each parallel agent adds exactly one new
//      `pub fn <capability>() -> &'static dyn <Trait>` function, appended
//      at the end of this file, plus one `pub mod <name>;` line appended in
//      `platform/mac/mod.rs`. Both are pure line-additions at a stable
//      position, which is the cheapest possible shape for independent
//      branches to land without textual conflicts.
//      A shared `Platform { ocr: .., capture: .., .. }` struct would instead
//      make every agent edit the SAME struct literal and the SAME
//      constructor — guaranteed conflicts every time two land close
//      together.
//
// Only OCR has a real implementation so far (the exemplar move); the other
// capabilities' accessor functions are added by the agent that moves that
// subsystem.

#[cfg(target_os = "macos")]
static MAC_OCR: mac::ocr::MacOcrEngine = mac::ocr::MacOcrEngine;

/// The OCR engine (Apple Vision on macOS).
#[cfg(target_os = "macos")]
pub fn ocr() -> &'static dyn OcrEngine {
    &MAC_OCR
}

#[cfg(target_os = "macos")]
static MAC_SCREEN_CAPTURE: mac::capture::MacScreenCapture = mac::capture::MacScreenCapture;

/// Screen/window pixel capture (the shared per-user capture daemon on macOS —
/// see `mac::capture::broker`).
#[cfg(target_os = "macos")]
pub fn screen_capture() -> &'static dyn ScreenCapture {
    &MAC_SCREEN_CAPTURE
}

#[cfg(target_os = "macos")]
static MAC_WINDOW_MANAGER: mac::window::MacWindowManager = mac::window::MacWindowManager;

/// Window/application enumeration and launching (shared capture daemon +
/// `mdfind`/`open` on macOS).
#[cfg(target_os = "macos")]
pub fn window_manager() -> &'static dyn WindowManager {
    &MAC_WINDOW_MANAGER
}

#[cfg(target_os = "macos")]
static MAC_CLIPBOARD: mac::clipboard::MacClipboard = mac::clipboard::MacClipboard;

/// The system clipboard (`pbpaste`/`pbcopy` on macOS).
#[cfg(target_os = "macos")]
pub fn clipboard() -> &'static dyn Clipboard {
    &MAC_CLIPBOARD
}

#[cfg(target_os = "macos")]
static MAC_INPUT: mac::input::MacInputInjector = mac::input::MacInputInjector;

/// The input injector (CoreGraphics `CGEvent` posting on macOS).
#[cfg(target_os = "macos")]
pub fn input() -> &'static dyn InputInjector {
    &MAC_INPUT
}

#[cfg(target_os = "macos")]
static MAC_UI_TREE: mac::elements::MacUiTree = mac::elements::MacUiTree;

/// Set-of-Mark element discovery + AX actions (the macOS Accessibility API).
#[cfg(target_os = "macos")]
pub fn ui_tree() -> &'static dyn UiTree {
    &MAC_UI_TREE
}

// ── Windows accessors (P1 MVP) ──────────────────────────────────────
//
// Mirrors the macOS accessors above exactly — same rationale (per-capability
// functions, not one bundling struct). Every capability below is now a real
// implementation: `ui_tree()` via UI Automation (P2) and `ocr()` via
// `Windows.Media.Ocr` (P3); the rest are Win32.

#[cfg(target_os = "windows")]
static WIN_OCR: windows::ocr::WinOcrEngine = windows::ocr::WinOcrEngine;

/// The OCR engine (`Windows.Media.Ocr` on Windows — see `platform::windows::ocr`).
#[cfg(target_os = "windows")]
pub fn ocr() -> &'static dyn OcrEngine {
    &WIN_OCR
}

#[cfg(target_os = "windows")]
static WIN_SCREEN_CAPTURE: windows::capture::WinScreenCapture = windows::capture::WinScreenCapture;

/// Screen/window pixel capture (GDI `BitBlt`, `PrintWindow` for a single window
/// on Windows — see `platform::windows::capture`).
#[cfg(target_os = "windows")]
pub fn screen_capture() -> &'static dyn ScreenCapture {
    &WIN_SCREEN_CAPTURE
}

#[cfg(target_os = "windows")]
static WIN_WINDOW_MANAGER: windows::window::WinWindowManager = windows::window::WinWindowManager;

/// Window/application enumeration and launching (`EnumWindows` + `ShellExecuteW`
/// on Windows).
#[cfg(target_os = "windows")]
pub fn window_manager() -> &'static dyn WindowManager {
    &WIN_WINDOW_MANAGER
}

#[cfg(target_os = "windows")]
static WIN_CLIPBOARD: windows::clipboard::WinClipboard = windows::clipboard::WinClipboard;

/// The system clipboard (Win32 `OpenClipboard`/`CF_UNICODETEXT` on Windows).
#[cfg(target_os = "windows")]
pub fn clipboard() -> &'static dyn Clipboard {
    &WIN_CLIPBOARD
}

#[cfg(target_os = "windows")]
static WIN_INPUT: windows::input::WinInputInjector = windows::input::WinInputInjector;

/// The input injector (`SendInput` on Windows).
#[cfg(target_os = "windows")]
pub fn input() -> &'static dyn InputInjector {
    &WIN_INPUT
}

#[cfg(target_os = "windows")]
static WIN_UI_TREE: windows::elements::WinUiTree = windows::elements::WinUiTree;

/// Set-of-Mark element discovery + query-driven actions (Microsoft UI
/// Automation on Windows — see `platform::windows::elements`).
#[cfg(target_os = "windows")]
pub fn ui_tree() -> &'static dyn UiTree {
    &WIN_UI_TREE
}

// ── Headless accessors (every other OS) ─────────────────────────────
//
// The stub backend for OSes with no desktop implementation — see
// `platform::headless`'s module doc. Same per-capability shape as the real
// accessors above; every impl is a zero-sized type whose methods return a
// uniform "headless build" error (marks degrade to empty, per UiTree's
// graceful-degradation contract).

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
static HEADLESS_OCR: headless::HeadlessOcrEngine = headless::HeadlessOcrEngine;

/// The OCR engine — headless stub (no backend on this OS).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn ocr() -> &'static dyn OcrEngine {
    &HEADLESS_OCR
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
static HEADLESS_SCREEN_CAPTURE: headless::HeadlessScreenCapture = headless::HeadlessScreenCapture;

/// Screen/window pixel capture — headless stub (no backend on this OS).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn screen_capture() -> &'static dyn ScreenCapture {
    &HEADLESS_SCREEN_CAPTURE
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
static HEADLESS_WINDOW_MANAGER: headless::HeadlessWindowManager = headless::HeadlessWindowManager;

/// Window/application enumeration — headless stub (no backend on this OS).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn window_manager() -> &'static dyn WindowManager {
    &HEADLESS_WINDOW_MANAGER
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
static HEADLESS_CLIPBOARD: headless::HeadlessClipboard = headless::HeadlessClipboard;

/// The system clipboard — headless stub (no backend on this OS).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn clipboard() -> &'static dyn Clipboard {
    &HEADLESS_CLIPBOARD
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
static HEADLESS_INPUT: headless::HeadlessInputInjector = headless::HeadlessInputInjector;

/// The input injector — headless stub (no backend on this OS).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn input() -> &'static dyn InputInjector {
    &HEADLESS_INPUT
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
static HEADLESS_UI_TREE: headless::HeadlessUiTree = headless::HeadlessUiTree;

/// Set-of-Mark element discovery — headless stub (no tree, marks come back
/// empty).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn ui_tree() -> &'static dyn UiTree {
    &HEADLESS_UI_TREE
}
