//! Headless stub backend — every OS that is neither macOS nor Windows.
//!
//! Nova's actual desktop control needs a desktop OS backend (`platform::mac`,
//! `platform::windows`). This module exists so the crate still BUILDS and the
//! MCP server still STARTS everywhere else — most importantly Linux
//! containers used by registry health checks (e.g. Glama's Docker-based
//! "does it start and answer introspection?" probe) and CI. The server comes
//! up, answers `initialize` and `tools/list` with the full tool catalog, and
//! every actual tool invocation returns a clear, uniform error instead of
//! pretending a desktop exists.
//!
//! Mirrors the per-capability layout of the real backends: one zero-sized
//! type per trait in `platform::mod`, wired up by the
//! `#[cfg(not(any(target_os = "macos", target_os = "windows")))]` accessor
//! functions there.

use super::{ElementHandle, OcrLine, WindowHandle};

/// An actionable UI element with its frame in global logical points.
/// Field-for-field identical to the macOS/Windows definitions — re-exported
/// through `crate::tools::elements` like theirs, so the shared tool layer
/// compiles unchanged.
#[derive(Debug, Clone)]
pub struct UiElement {
    pub role: String,
    pub label: String,
    /// The control's current value for text-like roles, else empty. Mirrors the
    /// mac/Windows field so the platform-neutral `read_ui` renderer compiles on
    /// every target. Always empty here (this backend discovers no elements).
    pub value: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl UiElement {
    /// Center of the element in global logical points.
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// A marked actionable element kept for index-based clicking. Field-for-field
/// identical to the macOS/Windows definitions. Never constructed here (marks
/// are always empty on a headless build), but the server's mark cache is
/// typed against it.
#[derive(Debug, Clone)]
pub struct CachedElement {
    pub number: u32,
    pub handle: Box<dyn ElementHandle>,
    pub center: (f64, f64),
    pub role: String,
    pub label: String,
    pub pid: i32,
}

/// The single explanation every stubbed capability returns, so a model (or a
/// registry probe reading logs) sees the same story everywhere.
fn unsupported(what: &str) -> String {
    format!(
        "{what} is unavailable in this build: nova controls the macOS or Windows desktop, and \
         this binary was built for an OS with neither backend (headless/registry build). Run \
         nova on macOS or Windows for real desktop control."
    )
}

// ── ScreenCapture ────────────────────────────────────────────────────

pub struct HeadlessScreenCapture;

impl super::ScreenCapture for HeadlessScreenCapture {
    fn capture_display(&self) -> Result<crate::capture::screenshot::RawCapture, String> {
        Err(unsupported("screen capture"))
    }

    fn capture_window(
        &self,
        _query: &str,
    ) -> Result<crate::capture::screenshot::RawCapture, String> {
        Err(unsupported("window capture"))
    }

    fn capture_region(
        &self,
        _rect: (f64, f64, f64, f64),
    ) -> Result<crate::capture::screenshot::RawCapture, String> {
        Err(unsupported("region capture"))
    }
}

// ── InputInjector ────────────────────────────────────────────────────

pub struct HeadlessInputInjector;

impl HeadlessInputInjector {
    fn err<T>(&self, what: &str) -> crate::error::Result<T> {
        Err(crate::error::NovaError::Input(unsupported(what)))
    }
}

impl super::InputInjector for HeadlessInputInjector {
    fn mouse_move(&self, _x: f64, _y: f64) -> crate::error::Result<()> {
        self.err("mouse_move")
    }
    fn cursor_position(&self) -> crate::error::Result<(f64, f64)> {
        self.err("cursor_position")
    }
    fn left_click_at(
        &self,
        _x: f64,
        _y: f64,
        _target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()> {
        self.err("left_click")
    }
    fn right_click_at(
        &self,
        _x: f64,
        _y: f64,
        _target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()> {
        self.err("right_click")
    }
    fn double_click_at(
        &self,
        _x: f64,
        _y: f64,
        _target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()> {
        self.err("double_click")
    }
    fn scroll_at(
        &self,
        _x: f64,
        _y: f64,
        _lines: i32,
        _target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()> {
        self.err("scroll")
    }
    fn key_combo(
        &self,
        _combo: &str,
        _target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()> {
        self.err("key_combo")
    }
    fn type_text(
        &self,
        _text: &str,
        _target: crate::tools::input::InputTarget,
    ) -> crate::error::Result<()> {
        self.err("type_text")
    }
}

// ── WindowManager ────────────────────────────────────────────────────

pub struct HeadlessWindowManager;

impl super::WindowManager for HeadlessWindowManager {
    fn list_windows(&self) -> Result<Vec<WindowHandle>, String> {
        Err(unsupported("window enumeration"))
    }
    fn list_applications(
        &self,
    ) -> crate::error::Result<Vec<crate::tools::application::ApplicationInfo>> {
        Err(crate::error::NovaError::Application(unsupported(
            "application enumeration",
        )))
    }
    fn open_application(&self, _name: &str) -> crate::error::Result<()> {
        Err(crate::error::NovaError::Application(unsupported(
            "open_application",
        )))
    }
}

// ── UiTree ───────────────────────────────────────────────────────────

pub struct HeadlessUiTree;

impl super::UiTree for HeadlessUiTree {
    fn collect_actionable(
        &self,
        _pid: i32,
        _max: usize,
        _clip: Option<(f64, f64, f64, f64)>,
    ) -> Vec<(crate::tools::elements::UiElement, Box<dyn ElementHandle>)> {
        // Marks degrade gracefully by contract: no tree → no marks, not an error.
        Vec::new()
    }
    fn ax_click(&self, _pid: i32, _query: &str) -> Result<String, String> {
        Err(unsupported("accessibility click"))
    }
    fn ax_set_value(&self, _pid: i32, _query: &str, _value: &str) -> Result<String, String> {
        Err(unsupported("accessibility set_value"))
    }
    fn ax_focus(&self, _pid: i32, _query: &str) -> Result<String, String> {
        Err(unsupported("accessibility focus"))
    }
    fn raise_app(&self, _pid: i32) {}
    fn dump_tree(&self, _pid: i32, _max_nodes: usize) -> String {
        unsupported("accessibility tree dump")
    }
    fn keep_warm(&self, _pid: i32) {}
    fn clear_warm(&self) {}
}

// ── Clipboard ────────────────────────────────────────────────────────

pub struct HeadlessClipboard;

impl super::Clipboard for HeadlessClipboard {
    fn read(&self) -> crate::error::Result<String> {
        Err(crate::error::NovaError::Clipboard(unsupported(
            "clipboard read",
        )))
    }
    fn write(&self, _text: &str) -> crate::error::Result<()> {
        Err(crate::error::NovaError::Clipboard(unsupported(
            "clipboard write",
        )))
    }
}

// ── OcrEngine ────────────────────────────────────────────────────────

pub struct HeadlessOcrEngine;

impl super::OcrEngine for HeadlessOcrEngine {
    fn recognize(
        &self,
        _image: &[u8],
        _img_w: u32,
        _img_h: u32,
        _languages: &[&str],
    ) -> Result<Vec<OcrLine>, String> {
        Err(unsupported("OCR"))
    }
}
