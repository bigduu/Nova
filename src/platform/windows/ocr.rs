//! OCR — Windows STUB (P1).
//!
//! `Windows.Media.Ocr` (WinRT) is the real analog of macOS's Apple Vision path
//! (`platform::mac::ocr`) and is tracked as a later phase (P3) — it needs the
//! WinRT projection crate family, not just `windows` Win32 bindings, and
//! language-pack availability checks that are out of scope for the P1 MVP.
//! [`WinOcrEngine`] satisfies `crate::platform::OcrEngine` so the crate links
//! and the `ocr` MCP tool returns a clean, actionable error instead of the
//! handler panicking on an unimplemented capability.
use crate::platform::OcrLine;

pub struct WinOcrEngine;

impl crate::platform::OcrEngine for WinOcrEngine {
    fn recognize(
        &self,
        _image: &[u8],
        _img_w: u32,
        _img_h: u32,
        _languages: &[&str],
    ) -> Result<Vec<OcrLine>, String> {
        Err(
            "ocr is not yet implemented on Windows (Windows.Media.Ocr integration is tracked for \
             a later phase); take a screenshot and read it visually instead"
                .to_string(),
        )
    }
}
