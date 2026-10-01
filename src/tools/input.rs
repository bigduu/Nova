//! Input delivery target — the neutral enum shared between the tool layer and
//! the platform's `InputInjector` implementation.
//!
//! The actual OS-level input mechanics (CoreGraphics `CGEvent` posting on
//! macOS) moved to `crate::platform::mac::input` behind
//! `crate::platform::input()` as part of the platform-abstraction split (see
//! `crate::platform`) — this file only keeps the enum itself,
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

/// Bounded, non-sensitive metadata for caller-provided native input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputMetadata {
    pub(crate) chars: usize,
    pub(crate) bytes: usize,
}

pub(crate) fn input_metadata(text: &str) -> InputMetadata {
    InputMetadata {
        chars: text.chars().count(),
        bytes: text.len(),
    }
}

pub(crate) fn type_text_ack(text: &str) -> String {
    let metadata = input_metadata(text);
    format!(
        "typed input (chars={}, bytes={})",
        metadata.chars, metadata.bytes
    )
}

/// Remove raw and Rust-debug representations (standalone or embedded in a
/// quoted query) of a submitted value from a diagnostic. Empty values are left
/// unchanged so they cannot erase diagnostics; non-empty values are deleted
/// rather than replaced with a fixed token that could reproduce the input.
pub(crate) fn redact_diagnostic(submitted: &str, diagnostic: impl Into<String>) -> String {
    let mut diagnostic = diagnostic.into();
    if submitted.is_empty() {
        return diagnostic;
    }
    let debug = format!("{submitted:?}");
    diagnostic = diagnostic.replace(&debug, "");
    // Debug string formatting always surrounds the escaped content in quotes.
    let escaped = &debug[1..debug.len() - 1];
    if escaped != submitted {
        diagnostic = diagnostic.replace(escaped, "");
    }
    diagnostic.replace(submitted, "")
}

fn input_error_detail(error: &crate::error::NovaError) -> String {
    match error {
        crate::error::NovaError::Input(detail) => detail.clone(),
        _ => "native input operation failed".to_string(),
    }
}

pub(crate) fn type_text_with(
    input: &dyn crate::platform::InputInjector,
    text: &str,
    target: InputTarget,
) -> crate::error::Result<String> {
    let metadata = input_metadata(text);
    tracing::info!(
        chars = metadata.chars,
        bytes = metadata.bytes,
        "typing native input"
    );
    input
        .type_text(text, target)
        .map(|()| type_text_ack(text))
        .map_err(|error| {
            crate::error::NovaError::Input(format!(
                "{} (chars={}, bytes={})",
                redact_diagnostic(text, input_error_detail(&error)),
                metadata.chars,
                metadata.bytes
            ))
        })
}

impl InputTarget {
    /// Whether this is the global HID stream (which moves the real cursor).
    ///
    /// `pub(crate)` rather than private: the macOS `InputInjector`
    /// implementation (`crate::platform::mac::input`) is a different module
    /// post-move and reads this to decide whether to glide the real cursor
    /// before posting a click/scroll. Windows' `InputInjector`
    /// (`platform::windows::input`) always delivers via the global `SendInput`
    /// queue regardless of `InputTarget` (see that module's doc), so it never
    /// reads this — legitimately unused, not dead code, on a non-macOS build.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

    #[test]
    fn redact_diagnostic_removes_literal_and_debug_escaped_secret() {
        let submitted = "line\n\"quoted\\path";
        let debug = format!("{submitted:?}");
        for diagnostic in [
            format!("route=hid detail={debug} suffix=preserved"),
            format!(
                "route=hid query={:?} suffix=preserved",
                format!("field {submitted}")
            ),
        ] {
            let redacted = redact_diagnostic(submitted, diagnostic);
            assert!(!redacted.contains(submitted));
            assert!(!redacted.contains(&debug[1..debug.len() - 1]));
            assert!(redacted.contains("route=hid"));
            assert!(redacted.contains("suffix=preserved"));
        }
    }

    #[test]
    fn redact_diagnostic_removes_replacement_token_sentinels() {
        for submitted in ["[REDACTED]", "REDACTED"] {
            let diagnostic = format!("route=ax detail={submitted} suffix=preserved");
            let redacted = redact_diagnostic(submitted, diagnostic);

            assert!(!redacted.contains(submitted));
            assert!(redacted.contains("route=ax"));
            assert!(redacted.contains("suffix=preserved"));
        }
    }

    #[test]
    fn redact_diagnostic_preserves_empty_submitted_value() {
        let diagnostic = "route=ax detail=unchanged";
        assert_eq!(redact_diagnostic("", diagnostic), diagnostic);
    }
}
