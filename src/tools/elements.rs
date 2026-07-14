//! Thin re-export of the platform-neutral Set-of-Mark element types.
//!
//! The Accessibility (AX) machinery that used to live in this directory moved
//! to `crate::platform::mac::elements` behind the `UiTree`/`ElementHandle`
//! traits — real call sites now go through
//! `crate::platform::ui_tree()`. [`UiElement`] and [`CachedElement`] stay
//! reachable at THIS stable path rather than moving with the rest of the
//! mac-specific code: `crate::platform::mod.rs`'s `UiTree` trait references
//! `crate::tools::elements::UiElement` directly (it's already plain,
//! platform-neutral data — role/label/frame), and the server caches
//! `CachedElement`s (by mark number) between a `marks=true` screenshot and a
//! later `click_mark` without needing to know they hold a `Box<dyn
//! ElementHandle>` underneath.
#[cfg(target_os = "macos")]
pub use crate::platform::mac::elements::{CachedElement, UiElement};

/// Same re-export, from the Windows sibling (`platform::windows::elements`) —
/// field-for-field identical types (see that module's doc for why they exist
/// even though `collect_actionable` never constructs one there yet).
#[cfg(target_os = "windows")]
pub use crate::platform::windows::elements::{CachedElement, UiElement};

/// Same re-export, from the headless stub backend (every other OS) — the
/// types exist so the shared tool layer compiles; marks are always empty
/// there (see `platform::headless`'s module doc).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use crate::platform::headless::{CachedElement, UiElement};
