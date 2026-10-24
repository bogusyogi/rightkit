//! Apple Vision text recognition — the macOS OCR backend.
//!
//! Ported (via ViewRight's `viewright-ocr`, the most mature variant) from ScrapeRight's `vision.rs`, which measured it (2026-07-18, macOS
//! 26.5, Apple silicon, 8 dense two-column pages, ground truth = the PDF text
//! layer) at 0.054-0.072 CER and 0.8-1.0 s/page — roughly 10x lower character
//! error and 8-16x faster than the PP-OCRv6 ONNX pipeline, while running on the
//! ANE that ONNX cannot reach on macOS. That evidence is why ViewRight ships
//! this and no bundled model.
//!
//! Pure FFI, no bundled binary and no subprocess: RgbImage -> CGImage ->
//! VNImageRequestHandler -> VNRecognizeTextRequest (accurate, revision 3,
//! language correction on).
//!
//! Divergence from ScrapeRight: each observation's `boundingBox` and top
//! candidate `confidence` are carried out as [`OcrLine`] instead of being
//! discarded in favour of newline-joined text. Vision's rect is normalized with
//! a **lower-left** origin; `to_top_left` flips it into ViewRight's top-left
//! convention.

use std::borrow::Cow;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::OnceLock;

use image::{imageops, RgbImage};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AllocAnyThread;
use objc2_core_foundation::{CFRetained, CGRect};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage,
};
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_vision::{
    VNErrorCode, VNErrorDomain, VNImageRequestHandler, VNRecognizeTextRequest,
    VNRecognizeTextRequestRevision3, VNRequest, VNRequestTextRecognitionLevel,
};

use crate::types::{OcrLine, OcrRect};

/// How a failed `performRequests` classifies for availability purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestErrorClass {
    /// Vision cannot run on this machine — a capability verdict callers may cache.
    Unavailable,
    /// Everything else: transient service failures, request/input complaints, and
    /// errors from foreign domains (XPC, Cocoa) whose persistence is unknown.
    /// Callers must retry rather than remember `false`.
    Transient,
}

/// Classify a `performRequests` failure by NSError domain + code.
///
/// Only a `VNErrorDomain` capability code is definitive proof the backend cannot
/// run here; every other error — including XPC/service failures that may clear on
/// their own — stays retryable so a one-off fault can never be remembered as
/// "this machine has no OCR".
pub(crate) fn classify_request_error(domain: &str, code: VNErrorCode) -> RequestErrorClass {
    // SAFETY: VNErrorDomain is a framework-provided immutable string constant.
    if unsafe { VNErrorDomain }.is_none_or(|d| domain != d.to_string()) {
        return RequestErrorClass::Transient;
    }
    match code {
        VNErrorCode::NotImplemented
        | VNErrorCode::InvalidModel
        | VNErrorCode::UnsupportedRevision
        | VNErrorCode::DataUnavailable
        | VNErrorCode::UnsupportedRequest
        | VNErrorCode::UnsupportedComputeStage
        | VNErrorCode::UnsupportedComputeDevice => RequestErrorClass::Unavailable,
        _ => RequestErrorClass::Transient,
    }
}

/// The health check's three outcomes, mirroring `probe_available`'s contract:
/// only a definitive capability failure may be remembered as "no OCR".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HealthCheck {
    /// Vision recognized the canary — proven working.
    Healthy,
    /// The canary failed with a capability-class error (unsupported revision,
    /// missing model assets, unsupported compute) — a stable "no OCR" verdict.
    Unavailable,
    /// The canary failed transiently or returned nothing — a starved Vision can
    /// recover, so the verdict must not be cached as `false`.
    Transient,
}

/// Recognize text in an RGB image via Apple Vision.
///
/// An empty result is disambiguated before it is returned as success — see
/// `vision_health`. Vision reports "I could not run" and "this image has
/// no text" identically (measured 2026-07-20 in ScrapeRight: a service-starved
/// Vision and a blank white page BOTH yield `Some(results)` with `count == 0`,
/// and `performRequests` does not error), so without the self-check a broken
/// Vision is indistinguishable from a blank page and the user is told a
/// perfectly good scan contains no text.
pub fn recognize(img: &RgbImage) -> Result<Vec<OcrLine>, String> {
    let lines = recognize_once(img)?;
    if !lines.is_empty() {
        return Ok(lines);
    }
    match vision_health() {
        HealthCheck::Healthy => Ok(lines),
        HealthCheck::Unavailable => Err(format!(
            "{} Vision returned no observations and then failed its built-in \
             self-check, so text recognition is not working on this machine \
             (this is not a blank page). Common causes: the process is \
             sandboxed away from Vision's XPC service, or the OCR model \
             assets are unavailable.",
            crate::types::MACOS_VISION_UNAVAILABLE
        )),
        HealthCheck::Transient => Err(
            "Vision returned no observations and its self-check could not confirm \
             the backend either — a transient failure, retry recognition"
                .to_string(),
        ),
    }
}

/// Perform exactly one Vision request. The app's isolated worker uses this narrow entrypoint so
/// an empty page does not trigger the canary in the same poisoned process; health classification
/// belongs to a separate fresh worker invocation.
pub fn recognize_once(img: &RgbImage) -> Result<Vec<OcrLine>, String> {
    recognize_lines(img)
}

/// Is Vision actually able to recognize text on this machine?
///
/// Asks Vision something whose answer is already known: a small embedded PNG of
/// known text. A healthy Vision returns at least one observation for it; a
/// Vision that cannot run returns none, which is exactly the ambiguity this
/// resolves.
///
/// Only a proven-healthy verdict is cached for the process lifetime — a failed
/// check is never remembered, because a starved or hiccupping Vision can
/// recover on the very next call. The cache is consulted only when a real
/// result came back empty, so a genuinely blank page pays the check once and is
/// still correctly reported as empty.
///
/// A canary that cannot be decoded is [`HealthCheck::Healthy`] (assume working):
/// failing to read our own embedded asset is a build defect and must not
/// masquerade as "the user's Vision is broken".
fn vision_health() -> HealthCheck {
    static HEALTHY: OnceLock<()> = OnceLock::new();
    cached_health(&HEALTHY, recognize_lines)
}

/// Cache wrapper around [`check_health`]: only `Healthy` is written to `slot`,
/// so a transiently failed check re-probes on the next call instead of
/// poisoning the process lifetime.
pub(crate) fn cached_health(
    slot: &'static OnceLock<()>,
    recognize: fn(&RgbImage) -> Result<Vec<OcrLine>, String>,
) -> HealthCheck {
    if slot.get().is_some() {
        return HealthCheck::Healthy;
    }
    match check_health(recognize) {
        HealthCheck::Healthy => {
            let _ = slot.set(());
            HealthCheck::Healthy
        }
        other => other,
    }
}

/// Run the canary through `recognize` (injected for tests) and classify the
/// outcome. An `Ok` with observations proves health; an `Err` classified as
/// backend-unavailable is a definitive capability verdict; anything else —
/// empty results or transient errors — is recoverable.
pub(crate) fn check_health(
    recognize: fn(&RgbImage) -> Result<Vec<OcrLine>, String>,
) -> HealthCheck {
    match decode_canary() {
        Some(canary) => match recognize(&canary) {
            Ok(lines) if !lines.is_empty() => HealthCheck::Healthy,
            Err(e) if crate::types::is_backend_unavailable(&e) => HealthCheck::Unavailable,
            _ => HealthCheck::Transient,
        },
        None => HealthCheck::Healthy,
    }
}

/// Decode the embedded canary PNG, or `None` if the asset is unusable.
fn decode_canary() -> Option<RgbImage> {
    let bytes = canary_png();
    image::load_from_memory(bytes).ok().map(|i| i.into_rgb8())
}

/// Flip Vision's normalized lower-left rect into a normalized top-left rect.
///
/// Vision's origin is the bottom-left of the image and y grows upward, so the
/// top edge of a box is `1 - (origin.y + height)`. Split out and `pub(crate)`
/// so the geometry can be unit-tested without a live Vision call.
pub(crate) fn to_top_left(b: CGRect) -> OcrRect {
    // Vision may return slightly overscanned boxes (observed x=-0.000606). Clip the
    // bounds together so normalized overlays remain inside image space without shifting
    // their visible right/bottom edges.
    let x = b.origin.x.clamp(0.0, 1.0);
    let y = (1.0 - (b.origin.y + b.size.height)).clamp(0.0, 1.0);
    let right = (b.origin.x + b.size.width).clamp(0.0, 1.0);
    let bottom = (1.0 - b.origin.y).clamp(0.0, 1.0);
    OcrRect {
        x: x as f32,
        y: y as f32,
        width: (right - x).max(0.0) as f32,
        height: (bottom - y).max(0.0) as f32,
    }
}

/// The raw recognition call: every observation's top candidate, in Vision's own
/// order. Split out from [`recognize`] so the health check can use it without
/// recursing back into the health check.
fn recognize_lines(img: &RgbImage) -> Result<Vec<OcrLine>, String> {
    // Vision's accurate recognizer can spend tens of seconds and then return
    // CRImageReaderError for full 300-dpi pages. A normalized rect is invariant
    // under this downscale, so keep accurate recognition while bounding its
    // input to a document-sized raster. This is macOS-only; Windows owns its
    // own platform dimension guard in win_native.rs.
    let prepared = prepare_for_vision(img);
    let cg = cg_image_from_rgb(&prepared)?;

    unsafe {
        // Empty options dictionary; the image is already device-RGB with no EXIF
        // orientation to honour.
        let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
        let handler = VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            &cg,
            &options,
        );

        let request = VNRecognizeTextRequest::init(VNRecognizeTextRequest::alloc());
        // Preserve accurate recognition; native latency does not justify silently reducing quality.
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);
        // Pin revision 3 so an OS update cannot silently change the engine under a
        // shipped app.
        request.setRevision(VNRecognizeTextRequestRevision3);
        // Language DETECTION, not an explicit recognitionLanguages list. Vision's
        // default is English-only, under which a CJK page returns ZERO observations
        // — silent total failure, not garbage. An explicit list is priority-ordered
        // and applies uniformly to every image, so a CJK-first list makes Vision
        // emit full-width punctuation for ASCII text (`(` -> `（`). Auto-detection
        // picks per image and is the only setting that gets both right
        // (measured 2026-07-18, ScrapeRight vision.rs).
        request.setAutomaticallyDetectsLanguage(true);

        // Upcast VNRecognizeTextRequest -> VNRequest (safe class-hierarchy cast).
        let requests =
            NSArray::from_retained_slice(&[Retained::cast_unchecked::<VNRequest>(request.clone())]);
        // Vision has no engine-construction step to fail at. A failed
        // performRequests classifies by its NSError: only a VNErrorDomain
        // capability code earns the unavailable prefix (a cacheable "no OCR"
        // verdict); everything else — XPC hiccups, timeouts, internal errors —
        // stays a retryable error so a transient failure is never remembered
        // as "this machine cannot recognize text".
        handler.performRequests_error(&requests).map_err(|e| {
            match classify_request_error(&e.domain().to_string(), VNErrorCode(e.code())) {
                RequestErrorClass::Unavailable => format!(
                    "{} Vision performRequests failed: {e}",
                    crate::types::MACOS_VISION_UNAVAILABLE
                ),
                RequestErrorClass::Transient => {
                    format!("Vision performRequests failed: {e}")
                }
            }
        })?;

        let Some(results) = request.results() else {
            return Ok(Vec::new());
        };

        let mut lines: Vec<OcrLine> = Vec::with_capacity(results.len());
        for obs in results.iter() {
            // One top candidate per observation, matching the benchmark harness.
            let candidates = obs.topCandidates(1);
            if let Some(text) = candidates.iter().next() {
                lines.push(OcrLine {
                    text: text.string().to_string(),
                    rect: to_top_left(obs.boundingBox()),
                    confidence: Some(text.confidence()),
                });
            }
        }
        Ok(lines)
    }
}

/// Keep Vision accurate without handing its image reader an oversized page.
///
/// ViewRight receives normalized rectangles, so resizing here does not require
/// a caller-side coordinate transform. The 1800px bound preserves the native
/// 150-dpi A4 raster (~1753px high) while making 300-dpi A4 pages safe.
pub(crate) const MAX_VISION_DIMENSION: u32 = 1_800;

pub(crate) fn prepare_for_vision(img: &RgbImage) -> Cow<'_, RgbImage> {
    let longest = img.width().max(img.height());
    if longest <= MAX_VISION_DIMENSION {
        return Cow::Borrowed(img);
    }

    let scale = f64::from(MAX_VISION_DIMENSION) / f64::from(longest);
    let width = (f64::from(img.width()) * scale).round().max(1.0) as u32;
    let height = (f64::from(img.height()) * scale).round().max(1.0) as u32;
    Cow::Owned(imageops::resize(
        img,
        width,
        height,
        imageops::FilterType::Lanczos3,
    ))
}

/// Build a device-RGB `CGImage` from an 8-bit RGB image.
///
/// The pixel buffer is copied into a `Box<[u8]>` handed to Core Graphics with a
/// release callback that frees it, so the CGImage owns its bytes for its whole
/// lifetime — Vision may read them after this function returns.
fn cg_image_from_rgb(img: &RgbImage) -> Result<CFRetained<CGImage>, String> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }

    // Own the bytes for the CGImage's lifetime.
    let bytes: Box<[u8]> = img.as_raw().clone().into_boxed_slice();
    let len = bytes.len();
    let ptr = Box::into_raw(bytes) as *mut c_void;

    // Frees the boxed pixel buffer once Core Graphics is done with the image.
    unsafe extern "C-unwind" fn release(_info: *mut c_void, data: NonNull<c_void>, size: usize) {
        let slice = std::ptr::slice_from_raw_parts_mut(data.as_ptr() as *mut u8, size);
        drop(Box::from_raw(slice));
    }

    unsafe {
        let provider = CGDataProvider::with_data(std::ptr::null_mut(), ptr, len, Some(release))
            .ok_or_else(|| {
                // with_data adopts the buffer only on success; on failure reclaim it
                // here so the Box is not leaked.
                let slice = std::ptr::slice_from_raw_parts_mut(ptr as *mut u8, len);
                drop(Box::from_raw(slice));
                "CGDataProvider creation failed".to_string()
            })?;

        let space = CGColorSpace::new_device_rgb().ok_or("device RGB color space failed")?;

        // 3 bytes/pixel, no alpha — CGImageAlphaInfo::None (0). Default byte order.
        let bitmap_info = CGBitmapInfo(0);

        let cg = CGImage::new(
            w,
            h,
            8,     // bits per component
            24,    // bits per pixel (RGB, no alpha)
            w * 3, // bytes per row
            Some(&space),
            bitmap_info,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
        .ok_or("CGImageCreate failed")?;

        Ok(cg)
    }
}

/// Embedded known-text PNG used for the Vision health canary.
pub fn canary_png() -> &'static [u8] {
    include_bytes!("assets/vision_canary.png")
}
