//! macOS implementations of the `crate::platform` capability traits.
//!
//! Every `objc2`/`core-graphics`/`core-foundation`/`accessibility(-sys)` use
//! in the crate belongs under this module (enforced by the dependency gating
//! in `Cargo.toml` — those crates aren't even fetched off macOS). Each
//! submodule below implements exactly one `crate::platform` trait for one
//! subsystem; see PARALLEL_PLAN.md at the repo root for the in-flight moves.
//!
//! Module declarations here are APPEND-ONLY as each subsystem lands (capture,
//! input, elements, window, ...) — add your new `pub mod` line at the end of
//! this list rather than reordering, so parallel branches touching this file
//! don't collide on more than the one new line each adds.

pub mod ocr;
pub mod elements;
