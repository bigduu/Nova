//! System clipboard read/write (macOS).
//!
//! Uses pbpaste/pbcopy — unchanged in substance from the old
//! `tools::clipboard`; only its home and its trait wiring moved.
use crate::error::Result;
use std::process::Command;

/// Read the current clipboard contents as text.
pub fn read_clipboard() -> Result<String> {
    let output = Command::new("pbpaste")
        .output()
        .map_err(|e| crate::error::NovaError::Clipboard(format!("pbpaste failed: {e}")))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Write text to the system clipboard.
pub fn write_clipboard(text: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| crate::error::NovaError::Clipboard(format!("pbcopy failed: {e}")))?;

    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| crate::error::NovaError::Clipboard(format!("write failed: {e}")))?;
    }

    let status = child
        .wait()
        .map_err(|e| crate::error::NovaError::Clipboard(format!("wait failed: {e}")))?;

    if !status.success() {
        return Err(crate::error::NovaError::Clipboard(
            "pbcopy exited with error".into(),
        ));
    }
    Ok(())
}

/// The macOS [`crate::platform::Clipboard`]: `pbpaste`/`pbcopy`, via
/// [`read_clipboard`]/[`write_clipboard`].
pub struct MacClipboard;

impl crate::platform::Clipboard for MacClipboard {
    fn read(&self) -> Result<String> {
        read_clipboard()
    }

    fn write(&self, text: &str) -> Result<()> {
        write_clipboard(text)
    }
}
