//! OCR via Apple's Vision framework (`VNRecognizeTextRequest`).
//!
//! Runs on an encoded image (the same JPEG a `screenshot` produces) and returns
//! recognized text lines with their centers in the image's pixel space — so the
//! recognized text is clickable through the very same coordinate frame as the
//! screenshot the agent is looking at.
//!
//! Native and on-device: no model files to download, no runtime to install, and
//! Chinese + Latin + many other scripts out of the box. This is the OCR member
//! of the same Apple-framework family nova already builds on (ScreenCaptureKit,
//! CoreGraphics, Accessibility).
//!
//! This is the platform-abstraction EXEMPLAR move: the implementation below is
//! unchanged from the old `src/ocr.rs` (same objc2 FFI quirks — `initWithData`,
//! the `boundingBox` unsafe/feature gating, the normalized-box Y-flip, safe to
//! call from a `spawn_blocking` thread); only its home and its trait wiring
//! moved. See the platform-abstraction move plan for the pattern the remaining subsystems follow.
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
    VNRequestTextRecognitionLevel,
};

use crate::platform::{OcrLine, OcrMode};

/// Fast results at or above this mean confidence are returned immediately in
/// [`OcrMode::Auto`]. UI text is normally well above this value; lower scores
/// are where Vision's Accurate recognizer materially improves glyph/language
/// disambiguation.
const AUTO_CONFIDENCE_THRESHOLD: f32 = 0.75;

/// Recognize text in `image` (an encoded JPEG/PNG of `img_w` × `img_h` pixels)
/// using the given BCP-47 language hints (e.g. `["zh-Hans", "en-US"]`). Returns
/// the lines Vision found, each with its center mapped into the image's pixels.
///
/// Synchronous and self-contained (creates and drops all Objective-C objects
/// internally), so it is safe to call from a `spawn_blocking` thread.
pub fn recognize(
    image: &[u8],
    img_w: u32,
    img_h: u32,
    languages: &[&str],
) -> Result<Vec<OcrLine>, String> {
    recognize_with_mode(image, img_w, img_h, languages, OcrMode::Accurate)
}

/// Recognize with an explicit latency/quality policy.
///
/// `Auto` takes the cheap path first. It only pays for Accurate when Fast found
/// nothing or its aggregate confidence is low. A successful Fast result is
/// retained if that fallback itself fails, so the optimization cannot turn a
/// partial read into a hard failure.
pub fn recognize_with_mode(
    image: &[u8],
    img_w: u32,
    img_h: u32,
    languages: &[&str],
    mode: OcrMode,
) -> Result<Vec<OcrLine>, String> {
    match mode {
        OcrMode::Fast => recognize_once(
            image,
            img_w,
            img_h,
            languages,
            VNRequestTextRecognitionLevel::Fast,
            false,
        ),
        OcrMode::Accurate => recognize_once(
            image,
            img_w,
            img_h,
            languages,
            VNRequestTextRecognitionLevel::Accurate,
            true,
        ),
        OcrMode::Auto => {
            let fast = match recognize_once(
                image,
                img_w,
                img_h,
                languages,
                VNRequestTextRecognitionLevel::Fast,
                false,
            ) {
                Ok(lines) => lines,
                Err(fast_error) => {
                    return recognize_once(
                        image,
                        img_w,
                        img_h,
                        languages,
                        VNRequestTextRecognitionLevel::Accurate,
                        true,
                    )
                    .map_err(|accurate_error| {
                        format!(
                            "Vision Fast failed ({fast_error}); Accurate fallback failed \
                             ({accurate_error})"
                        )
                    });
                }
            };

            if !needs_accurate_fallback(&fast) {
                return Ok(fast);
            }

            match recognize_once(
                image,
                img_w,
                img_h,
                languages,
                VNRequestTextRecognitionLevel::Accurate,
                true,
            ) {
                Ok(accurate) if !accurate.is_empty() => Ok(accurate),
                Ok(_) => Ok(fast),
                Err(error) if !fast.is_empty() => {
                    tracing::warn!(
                        "Vision Accurate fallback failed after a usable Fast OCR result: {error}"
                    );
                    Ok(fast)
                }
                Err(error) => Err(format!(
                    "Vision Fast found no text and Accurate fallback failed ({error})"
                )),
            }
        }
    }
}

fn recognize_once(
    image: &[u8],
    img_w: u32,
    img_h: u32,
    languages: &[&str],
    level: VNRequestTextRecognitionLevel,
    language_correction: bool,
) -> Result<Vec<OcrLine>, String> {
    autoreleasepool(|_| {
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(level);
        request.setUsesLanguageCorrection(language_correction);
        if !languages.is_empty() {
            let langs: Vec<Retained<NSString>> =
                languages.iter().map(|l| NSString::from_str(l)).collect();
            request.setRecognitionLanguages(&NSArray::from_retained_slice(&langs));
        }

        // A request handler over the encoded image bytes — Vision decodes the
        // image itself (JPEG/PNG), so no CGImage construction is needed.
        let data = NSData::with_bytes(image);
        let options = NSDictionary::<VNImageOption, AnyObject>::new();
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &data,
            &options,
        );

        let req_ref: &VNRequest = &request;
        let requests = NSArray::from_slice(&[req_ref]);
        handler
            .performRequests_error(&requests)
            .map_err(|e| format!("Vision performRequests failed: {e:?}"))?;

        let mut lines = Vec::new();
        if let Some(results) = request.results() {
            for obs in results.to_vec() {
                let candidates = obs.topCandidates(1);
                let Some(top) = candidates.to_vec().into_iter().next() else {
                    continue;
                };
                let text = top.string().to_string();
                if text.trim().is_empty() {
                    continue;
                }
                // Vision bounding boxes are normalized [0,1] with a BOTTOM-left
                // origin; convert the center to TOP-left image pixels.
                let bbox = unsafe { obs.boundingBox() };
                let cx = (bbox.origin.x + bbox.size.width / 2.0) * img_w as f64;
                let cy = (1.0 - (bbox.origin.y + bbox.size.height / 2.0)) * img_h as f64;
                lines.push(OcrLine {
                    text,
                    confidence: top.confidence(),
                    center: (cx, cy),
                });
            }
        }
        Ok(lines)
    })
}

fn needs_accurate_fallback(lines: &[OcrLine]) -> bool {
    if lines.is_empty() {
        return true;
    }
    let sum = lines
        .iter()
        .map(|line| line.confidence)
        .try_fold(0.0f32, |sum, confidence| {
            confidence.is_finite().then_some(sum + confidence)
        });
    match sum {
        Some(sum) => sum / (lines.len() as f32) < AUTO_CONFIDENCE_THRESHOLD,
        None => true,
    }
}

/// The macOS [`crate::platform::OcrEngine`]: Apple Vision, via [`recognize`].
pub struct MacOcrEngine;

impl crate::platform::OcrEngine for MacOcrEngine {
    fn recognize(
        &self,
        image: &[u8],
        img_w: u32,
        img_h: u32,
        languages: &[&str],
    ) -> Result<Vec<OcrLine>, String> {
        recognize(image, img_w, img_h, languages)
    }

    fn recognize_with_mode(
        &self,
        image: &[u8],
        img_w: u32,
        img_h: u32,
        languages: &[&str],
        mode: OcrMode,
    ) -> Result<Vec<OcrLine>, String> {
        crate::platform::mac::ocr::recognize_with_mode(image, img_w, img_h, languages, mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(confidence: f32) -> OcrLine {
        OcrLine {
            text: "text".to_string(),
            confidence,
            center: (1.0, 1.0),
        }
    }

    #[test]
    fn auto_falls_back_for_empty_or_low_confidence_results() {
        assert!(needs_accurate_fallback(&[]));
        assert!(needs_accurate_fallback(&[line(0.5), line(0.7)]));
        assert!(needs_accurate_fallback(&[line(f32::NAN)]));
    }

    #[test]
    fn auto_keeps_confident_fast_results() {
        assert!(!needs_accurate_fallback(&[line(0.8), line(0.9)]));
        assert!(!needs_accurate_fallback(&[line(AUTO_CONFIDENCE_THRESHOLD)]));
    }
}
