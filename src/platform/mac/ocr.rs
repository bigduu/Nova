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
//! moved. See PARALLEL_PLAN.md for the pattern the remaining subsystems follow.
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
    VNRequestTextRecognitionLevel,
};

use crate::platform::OcrLine;

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
    autoreleasepool(|_| {
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);
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
}
