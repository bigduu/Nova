//! Input delivery target — the neutral enum shared between the tool layer and
//! the platform's `InputInjector` implementation.
//!
//! The actual OS-level input mechanics (CoreGraphics `CGEvent` posting on
//! macOS) moved to `crate::platform::mac::input` behind
//! `crate::platform::input()` as part of the platform-abstraction split (see
//! PARALLEL_PLAN.md at the repo root) — this file only keeps the enum itself,
//! since `src/server.rs` and `src/tools/batch.rs` need to name a delivery
//! target without depending on anything platform-specific.

/// Where an input event is delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputTarget {
    /// Global HID event stream: routed to the frontmost app; the real cursor
    /// moves. Works for any app but requires foreground and takes over the
    /// user's mouse/keyboard.
    Global,
    /// Delivered directly to a specific process via `CGEventPostToPid`. The
    /// global cursor is NOT moved and the app usually need not be frontmost —
    /// i.e. as close to background input as macOS allows. Apps that handle their
    /// own events (some Electron/custom-rendered apps) may ignore these.
    Pid(i32),
}

impl InputTarget {
    /// Whether this is the global HID stream (which moves the real cursor).
    ///
    /// `pub(crate)` rather than private: the macOS `InputInjector`
    /// implementation (`crate::platform::mac::input`) is a different module
    /// post-move and reads this to decide whether to glide the real cursor
    /// before posting a click/scroll.
    pub(crate) fn is_global(self) -> bool {
        matches!(self, InputTarget::Global)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_target_is_global_only_for_global() {
        assert!(InputTarget::Global.is_global());
        assert!(!InputTarget::Pid(123).is_global());
    }
}
