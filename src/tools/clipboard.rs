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
    crate::platform::clipboard().write(text)
}
