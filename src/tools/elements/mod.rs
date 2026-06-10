//! Set-of-Mark element discovery via the macOS Accessibility (AX) API.
//!
//! Walks an application's accessibility tree (and hit-tests the visible window)
//! to collect actionable UI elements — buttons, links, fields, list rows — with
//! their on-screen frames. The server draws numbered boxes on these and hands the
//! model a list with each element's center, so it can pick a target by its
//! labeled mark instead of estimating raw pixel coordinates — the most reliable
//! way to ground clicks.
//!
//! Requires the host process to have Accessibility permission; degrades to an
//! empty list otherwise (never errors).
//!
//! Module layout (low → high level):
//! - [`attrs`]   — raw AX attribute/action reads + geometry primitives (the FFI)
//! - [`model`]   — `UiElement`, the live `AxHandle`, `CachedElement`, targeting
//! - [`walk`]    — the bounded native-chrome tree walk
//! - [`hittest`] — the web-content hit-test pass
//! - [`warmth`]  — enabling + keeping Chromium's full tree warm ([`TreeWarmer`])
//! - [`discover`]— top-level `collect_actionable` combining walk + warm hit-test
//! - [`actions`] — query-driven `ax_click` / `ax_set_value` / `ax_focus`
//! - [`debug`]   — `--dump-ax` / `--hit-dump` / `--ax-warm` developer diagnostics

mod attrs;
mod debug;
mod discover;
mod geometry;
mod hittest;
mod model;
mod walk;
mod warmth;

pub mod actions;

// Public surface (kept stable so `crate::tools::elements::X` paths don't move).
pub use actions::{ax_click, ax_focus, ax_set_value};
pub use debug::{ax_warm_probe, dump_tree, hit_dump};
pub use discover::{actionable_elements, collect_actionable};
pub use model::{raise_app, AxHandle, CachedElement, UiElement};
pub use warmth::{warmer, TreeWarmer};
