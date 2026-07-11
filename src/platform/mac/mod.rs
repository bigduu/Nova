//! macOS implementations of the `crate::platform` capability traits.
//!
//! Every `objc2`/`core-graphics`/`core-foundation`/`accessibility(-sys)` use
//! in the crate belongs under this module (enforced by the dependency gating
//! in `Cargo.toml` — those crates aren't even fetched off macOS). Each
//! submodule below implements exactly one `crate::platform` trait for one
//! subsystem.

pub mod capture;
pub mod clipboard;
pub mod elements;
pub mod geometry;
pub mod input;
pub mod ocr;
pub mod window;
