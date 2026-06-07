/// Clipboard tools — read/write system clipboard.
///
/// Uses pbpaste/pbcopy on macOS.
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
