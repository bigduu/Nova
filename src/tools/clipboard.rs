/// Clipboard tools — read/write system clipboard.
///
/// The actual `pbpaste`/`pbcopy` shelling now lives behind
/// `crate::platform::Clipboard` (`src/platform/mac/clipboard.rs`); kept here
/// as a thin, stable wrapper so existing tool/test call sites don't need to
/// change.
use crate::error::Result;

/// Read the current clipboard contents as text.
pub fn read_clipboard() -> Result<String> {
    crate::platform::clipboard().read()
}

/// Write text to the system clipboard.
pub fn write_clipboard(text: &str) -> Result<()> {
    write_clipboard_with(crate::platform::clipboard(), text)
}

fn clipboard_error_detail(error: &crate::error::NovaError) -> String {
    match error {
        crate::error::NovaError::Clipboard(detail) => detail.clone(),
        _ => "clipboard write failed".to_string(),
    }
}

pub(crate) fn write_clipboard_with(
    clipboard: &dyn crate::platform::Clipboard,
    text: &str,
) -> Result<()> {
    let metadata = crate::tools::input::input_metadata(text);
    tracing::info!(
        chars = metadata.chars,
        bytes = metadata.bytes,
        "writing clipboard input"
    );
    clipboard.write(text).map_err(|error| {
        crate::error::NovaError::Clipboard(format!(
            "{} (chars={}, bytes={})",
            crate::tools::input::redact_diagnostic(text, clipboard_error_detail(&error)),
            metadata.chars,
            metadata.bytes
        ))
    })
}
