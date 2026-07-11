//! OCR via `Windows.Media.Ocr` (WinRT) — the Windows analog of macOS's Apple
//! Vision path (`platform::mac::ocr`).
//!
//! # Flow
//!
//! 1. Wrap the encoded JPEG/PNG bytes (the same bytes a `screenshot` produces)
//!    in a WinRT [`InMemoryRandomAccessStream`] via a [`DataWriter`], then
//!    decode with [`BitmapDecoder`] into a [`SoftwareBitmap`] and convert it to
//!    `Bgra8` — the pixel format `OcrEngine::RecognizeAsync` requires.
//! 2. Pick an [`OcrEngine`](WinRtOcrEngine) for the caller's BCP-47 language
//!    hints, in priority order, via `TryCreateFromLanguage`; fall back to
//!    `TryCreateFromUserProfileLanguages` if none of the hints have an
//!    installed OCR pack. See [`engine_for_languages`]'s doc for exactly how a
//!    missing pack surfaces (NOT a panic-able null).
//! 3. `RecognizeAsync` the bitmap, then union each line's words'
//!    `BoundingRect`s into that line's box and take its center.
//!
//! # Coordinate space — no Y-flip, unlike macOS
//!
//! Unlike Apple Vision (`VNRecognizedTextObservation.boundingBox`, which is
//! NORMALIZED `[0, 1]` with a BOTTOM-left origin — see `platform::mac::ocr`'s
//! doc for the flip that requires), `OcrWord::BoundingRect` is already
//! reported in the decoded bitmap's own DEVICE-PIXEL, TOP-left-origin space —
//! and that bitmap is decoded directly from `image` (`img_w` × `img_h`), so a
//! recognized rect's center is already exactly the
//! [`crate::platform::OcrLine::center`] the trait promises: no normalization,
//! no flip, no use of the `img_w`/`img_h` parameters at all (they exist only
//! because Vision's normalized boxes need them on macOS).
//!
//! # Threading
//!
//! WinRT async calls need this thread joined to the process's Multi-Threaded
//! Apartment, exactly like the UI Automation calls in
//! `platform::windows::elements` (see `elements::automation`'s module doc for
//! the full rationale) — [`recognize`] below reuses that SAME
//! `ensure_com_mta` (widened from `pub(super)` to `pub(crate)` for this)
//! rather than duplicating the join. `.get()` blocking on an
//! `IAsyncOperation<T>` from an MTA thread (no message pump) is the
//! documented, supported way to wait on a WinRT async call synchronously — we
//! deliberately do NOT pull in an async executor for this, since `recognize`
//! is always invoked from a blocking context: the `ocr` MCP tool runs it on a
//! `tokio::spawn_blocking` thread (see `server.rs`'s `ocr` tool handler), and
//! the one-shot `--ocr-probe` CLI diagnostic (see `main.rs`) calls it directly
//! on a process that exits right after — neither drives an async runtime that
//! the blocking `.get()` could stall.
//!
//! # Confidence
//!
//! `Windows.Media.Ocr` exposes no per-line/per-word confidence score (unlike
//! Vision's `topCandidates(_:).confidence`) — [`recognize`] reports a constant
//! `1.0` for every line. This is NOT comparable to macOS's real
//! Vision-reported confidence; a caller that branches on `OcrLine::confidence`
//! should not assume the two OSes' values carry the same meaning.
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, SoftwareBitmap,
};
use windows::Media::Ocr::OcrEngine as WinRtOcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

use crate::platform::windows::elements::automation::ensure_com_mta;
use crate::platform::OcrLine;

pub struct WinOcrEngine;

/// Find a [`WinRtOcrEngine`] able to recognize one of `languages` (BCP-47,
/// priority order), falling back to the user's profile languages.
///
/// `OcrEngine::TryCreateFromLanguage` does NOT hand back a null
/// [`WinRtOcrEngine`] you could accidentally `.unwrap()` into a crash: WinRT
/// signals "no OCR pack installed for this language" by leaving the method's
/// out-param null while still returning `S_OK`, and `windows-rs`'s
/// `Type::from_abi` (the glue every generated method funnels its out-param
/// through) already null-checks that pointer and turns it into
/// `Err(windows::core::Error::empty())` for us — so every call below is a
/// plain, safe `Result`; a missing pack is just another `Err` arm, never a
/// null to dereference.
fn engine_for_languages(languages: &[&str]) -> Result<WinRtOcrEngine, String> {
    for lang in languages {
        // A malformed BCP-47 tag fails `CreateLanguage` itself — skip it and
        // try the next hint rather than erroring the whole call out.
        let Ok(language) = Language::CreateLanguage(&HSTRING::from(*lang)) else {
            continue;
        };
        if let Ok(engine) = WinRtOcrEngine::TryCreateFromLanguage(&language) {
            return Ok(engine);
        }
    }
    WinRtOcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| {
        format!(
            "no Windows OCR language pack is installed for any of {languages:?} (and none of \
             the user's profile languages have one either) — install one via Settings > Time & \
             Language > Language & region > Add a language > (pick a language) > Options > Add \
             (\"Optical character recognition\"); this build's list of installed packs can be \
             checked with `nova --ocr-langs`"
        )
    })
}

/// Decode `image` (encoded JPEG/PNG bytes) into a [`SoftwareBitmap`] in the
/// `Bgra8` pixel format `OcrEngine::RecognizeAsync` requires.
fn decode_to_bgra8(image: &[u8]) -> Result<SoftwareBitmap, String> {
    let stream = InMemoryRandomAccessStream::new()
        .map_err(|e| format!("InMemoryRandomAccessStream::new failed: {e}"))?;
    // The writer is created FROM the stream's own `IOutputStream` view, so
    // writing through it lands directly in the stream's shared buffer — no
    // separate copy/attach step needed before decoding.
    let writer = DataWriter::CreateDataWriter(&stream)
        .map_err(|e| format!("DataWriter::CreateDataWriter failed: {e}"))?;
    writer
        .WriteBytes(image)
        .map_err(|e| format!("DataWriter::WriteBytes failed: {e}"))?;
    writer
        .StoreAsync()
        .and_then(|op| op.get())
        .map_err(|e| format!("DataWriter::StoreAsync failed: {e}"))?;
    writer
        .FlushAsync()
        .and_then(|op| op.get())
        .map_err(|e| format!("DataWriter::FlushAsync failed: {e}"))?;
    // Rewind before decoding — the writer left the stream's position at EOF.
    stream
        .Seek(0)
        .map_err(|e| format!("stream Seek(0) failed: {e}"))?;

    let decoder = BitmapDecoder::CreateAsync(&stream)
        .and_then(|op| op.get())
        .map_err(|e| format!("BitmapDecoder::CreateAsync failed: {e}"))?;
    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .and_then(|op| op.get())
        .map_err(|e| format!("BitmapDecoder::GetSoftwareBitmapAsync failed: {e}"))?;
    // Reconvert unconditionally rather than branching on the decoded format —
    // a JPEG screenshot typically decodes straight to Bgra8 already, so this
    // is then a cheap no-op copy; a PNG with an alpha channel is the case that
    // actually needs it.
    //
    // `ConvertWithAlpha(_, _, Premultiplied)`, NOT the 2-arg `Convert`: MS
    // guidance/samples force PREMULTIPLIED alpha before `RecognizeAsync`, and a
    // Straight-alpha Bgra8 bitmap makes `RecognizeAsync` throw "value does not
    // fall within the expected range". Today's inputs are always alpha-less
    // JPEG screenshots (so the 2-arg `Convert`'s default would be fine), but
    // the alpha branch this comment reasons about — a PNG with a real alpha
    // channel, which the trait contract's "JPEG/PNG bytes" explicitly allows —
    // must actually be correct, so pin Premultiplied unconditionally.
    // (In windows-rs the 3-arg WinRT overload is projected as the distinct
    // name `ConvertWithAlpha`, not `Convert` with a third argument.)
    SoftwareBitmap::ConvertWithAlpha(
        &bitmap,
        BitmapPixelFormat::Bgra8,
        BitmapAlphaMode::Premultiplied,
    )
    .map_err(|e| format!("SoftwareBitmap::ConvertWithAlpha(Bgra8, Premultiplied) failed: {e}"))
}

/// Recognize text in `image` (`img_w` × `img_h` pixels) using `languages`
/// (BCP-47 priority order) — see the module doc for the full flow and the
/// coordinate-space/threading/confidence notes.
pub fn recognize(image: &[u8], languages: &[&str]) -> Result<Vec<OcrLine>, String> {
    // Join the process's Multi-Threaded Apartment before any WinRT call on
    // this thread — see the module doc; reuses the exact same join
    // `platform::windows::elements`'s UI Automation calls use.
    ensure_com_mta();

    let engine = engine_for_languages(languages)?;
    let bitmap = decode_to_bgra8(image)?;

    let result = engine
        .RecognizeAsync(&bitmap)
        .and_then(|op| op.get())
        .map_err(|e| format!("OcrEngine::RecognizeAsync failed: {e}"))?;

    let ocr_lines = result
        .Lines()
        .map_err(|e| format!("OcrResult::Lines failed: {e}"))?;
    let mut lines = Vec::new();
    for line in ocr_lines {
        let text = line.Text().map(|h| h.to_string_lossy()).unwrap_or_default();
        if text.trim().is_empty() {
            continue;
        }
        let words = match line.Words() {
            Ok(words) => words,
            Err(e) => {
                // Drop the line rather than fabricate a (0,0) center, but leave
                // a trail so a "fewer lines than expected" symptom is traceable.
                tracing::debug!("dropping recognized line {text:?}: Words() failed: {e}");
                continue;
            }
        };
        // Union every word's bounding rect into one line-level box — Windows
        // exposes no single per-line rect (only per-word), unlike Vision's
        // single `boundingBox` per observation.
        let mut union: Option<(f32, f32, f32, f32)> = None;
        for word in words {
            let Ok(r) = word.BoundingRect() else { continue };
            let (left, top, right, bottom) = (r.X, r.Y, r.X + r.Width, r.Y + r.Height);
            union = Some(match union {
                None => (left, top, right, bottom),
                Some((ul, ut, ur, ub)) => {
                    (ul.min(left), ut.min(top), ur.max(right), ub.max(bottom))
                }
            });
        }
        let Some((left, top, right, bottom)) = union else {
            // Every word's BoundingRect failed (or the line had no words) — no
            // box to place a clickable center at. Same drop-with-a-trail policy.
            tracing::debug!("dropping recognized line {text:?}: no usable word bounding rect");
            continue;
        };
        lines.push(OcrLine {
            text,
            // No confidence score is available from Windows.Media.Ocr — see
            // the module doc's "Confidence" section.
            confidence: 1.0,
            center: (((left + right) / 2.0) as f64, ((top + bottom) / 2.0) as f64),
        });
    }
    Ok(lines)
}

/// The BCP-47 language tags this machine has an installed OCR pack for
/// (`OcrEngine::AvailableRecognizerLanguages`) — used by the `--ocr-langs`
/// diagnostic (see `main.rs`) to check pack availability before assuming a
/// recognition failure is a code bug rather than a missing pack.
pub fn available_languages() -> Result<Vec<String>, String> {
    let langs = WinRtOcrEngine::AvailableRecognizerLanguages()
        .map_err(|e| format!("OcrEngine::AvailableRecognizerLanguages failed: {e}"))?;
    let mut tags = Vec::new();
    for lang in langs {
        if let Ok(tag) = lang.LanguageTag() {
            tags.push(tag.to_string_lossy());
        }
    }
    Ok(tags)
}

impl crate::platform::OcrEngine for WinOcrEngine {
    fn recognize(
        &self,
        image: &[u8],
        _img_w: u32,
        _img_h: u32,
        languages: &[&str],
    ) -> Result<Vec<OcrLine>, String> {
        // `img_w`/`img_h` are unused: OCR rects come back in the decoded
        // bitmap's own pixel space, which IS `img_w` x `img_h` by
        // construction (same encoded image) — see the module doc's
        // "Coordinate space" section for why no math against them is needed
        // here, unlike Vision's normalized `[0, 1]` boxes on macOS.
        recognize(image, languages)
    }
}
