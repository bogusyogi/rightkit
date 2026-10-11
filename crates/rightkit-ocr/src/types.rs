//! Shared result types, error prefixes and the decode guard.
//!
//! One result shape for every backend: text + normalized top-left geometry +
//! optional confidence (ViewRight's `OcrLine`, the most mature donor).

use image::RgbImage;
use serde::{Deserialize, Serialize};

/// Prefix for a Windows backend that cannot recognize at all (no language pack).
///
/// Deprecated for keying: match [`OcrErrorKind::WindowsOcrUnavailable`] instead. The prefix
/// stays in the error `Display` text for compatibility with existing log and UI matching.
pub const WINDOWS_OCR_UNAVAILABLE: &str = "windows-ocr-unavailable:";
/// Prefix for an Apple Vision request that could not be performed.
///
/// Deprecated for keying: match [`OcrErrorKind::MacosVisionUnavailable`] instead. The prefix
/// stays in the error `Display` text for compatibility.
pub const MACOS_VISION_UNAVAILABLE: &str = "macos-vision-unavailable:";
/// Prefix for a platform with no native OCR backend at all (Linux builds).
///
/// Deprecated for keying: match [`OcrErrorKind::UnsupportedPlatform`] instead. The prefix stays
/// in the error `Display` text for compatibility.
pub const OCR_UNSUPPORTED_PLATFORM: &str = "ocr-unsupported-platform:";

/// Does this error mean the native backend is unusable on this machine (a
/// capability verdict, and the only thing that arms the ONNX fallback), as
/// opposed to a per-image failure that must stay visible?
///
/// Equivalent to `OcrErrorKind::from_message(err).is_some_and(OcrErrorKind::is_backend_unavailable)`.
/// Prefer [`OcrError::kind`](crate::OcrError::kind) or [`OcrErrorKind`] in new code.
pub fn is_backend_unavailable(err: &str) -> bool {
    OcrErrorKind::from_message(err).is_some_and(OcrErrorKind::is_backend_unavailable)
}

/// Typed classification of an OCR failure, so hosts key on a kind instead of message text.
///
/// The kind is the stable contract; `Display` text (including the legacy `...-unavailable:`
/// prefixes) is for humans and logs. The enum is `#[non_exhaustive]`: match with a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum OcrErrorKind {
    /// Apple Vision cannot perform recognition here (capability verdict; arms the ONNX fallback).
    MacosVisionUnavailable,
    /// `Windows.Media.Ocr` cannot recognize here, e.g. no language pack (capability verdict).
    WindowsOcrUnavailable,
    /// No native OCR backend is compiled for this platform (capability verdict).
    UnsupportedPlatform,
    /// Some other backend is unusable, e.g. the ONNX fallback was not compiled in.
    BackendUnavailable,
    /// The native backend is unusable and no ONNX fallback is configured.
    NoFallbackConfigured,
    /// Reserved. The crate never returns this today: a page with no observations that passes the
    /// canary check is `Ok` with empty lines, and a failing canary is a backend verdict instead.
    BlankPage,
    /// The request bytes exceed the 32 MiB worker input limit.
    RequestTooLarge,
    /// The child exceeded its deadline, either the parent's own deadline or the child's watchdog.
    WorkerTimeout,
    /// The child's output exceeded the bounded protocol limit.
    WorkerOutputExceeded,
    /// The child's stdout was not a valid worker response.
    WorkerProtocol,
    /// The child could not start, exited with failure, or its pipes or wait failed.
    WorkerFailed,
    /// Model provenance is unresolved or incomplete.
    Provenance,
    /// An ONNX model artifact is missing, unsafe, or fails its digest check.
    Artifact,
    /// Any other runtime failure, including per-image faults that must stay visible.
    Runtime,
}

impl OcrErrorKind {
    /// Classify a legacy error message by its backend prefix.
    ///
    /// Returns `None` for ordinary failures. This is the only place the crate parses prefixes; a
    /// child process reports errors as text, so the worker boundary calls it once per response.
    pub fn from_message(message: &str) -> Option<Self> {
        if message.starts_with(MACOS_VISION_UNAVAILABLE) {
            Some(Self::MacosVisionUnavailable)
        } else if message.starts_with(WINDOWS_OCR_UNAVAILABLE) {
            Some(Self::WindowsOcrUnavailable)
        } else if message.starts_with(OCR_UNSUPPORTED_PLATFORM) {
            Some(Self::UnsupportedPlatform)
        } else {
            None
        }
    }

    /// Is this a native capability verdict, i.e. the same set [`is_backend_unavailable`] accepts?
    /// `BackendUnavailable` (for example, ONNX not compiled in) is not included.
    pub fn is_backend_unavailable(self) -> bool {
        matches!(
            self,
            Self::MacosVisionUnavailable | Self::WindowsOcrUnavailable | Self::UnsupportedPlatform
        )
    }

    /// Stable snake_case name for logs and telemetry, e.g. `macos_vision_unavailable`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MacosVisionUnavailable => "macos_vision_unavailable",
            Self::WindowsOcrUnavailable => "windows_ocr_unavailable",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::BackendUnavailable => "backend_unavailable",
            Self::NoFallbackConfigured => "no_fallback_configured",
            Self::BlankPage => "blank_page",
            Self::RequestTooLarge => "request_too_large",
            Self::WorkerTimeout => "worker_timeout",
            Self::WorkerOutputExceeded => "worker_output_exceeded",
            Self::WorkerProtocol => "worker_protocol",
            Self::WorkerFailed => "worker_failed",
            Self::Provenance => "provenance",
            Self::Artifact => "artifact",
            Self::Runtime => "runtime",
        }
    }
}

/// A normalized rectangle: 0..1 of the source image, **origin top-left**.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// One recognized line with its position on the image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrLine {
    pub text: String,
    pub rect: OcrRect,
    /// 0..1 where the backend reports it (Vision, ONNX). Windows.Media.Ocr
    /// exposes none, so it is `None` there; `None` is not low confidence.
    pub confidence: Option<f32>,
}

/// Which backend produced a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OcrBackend {
    /// Apple Vision (macOS).
    Vision,
    /// Windows.Media.Ocr.
    WindowsMedia,
    /// The two-stage PP-OCR ONNX pipeline.
    Onnx,
}

impl OcrBackend {
    pub fn is_native(self) -> bool {
        !matches!(self, Self::Onnx)
    }
}

/// Largest image decoded, in pixels (enforced on the header, before any pixel
/// buffer is allocated — a decompression-bomb guard).
pub const MAX_PIXELS: u64 = 40_000_000;

/// Decode PNG/JPEG bytes to RGB with the [`MAX_PIXELS`] guard.
pub fn decode_encoded(bytes: &[u8]) -> Result<RgbImage, String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Cannot read image header: {e}"))?;
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("Cannot read image dimensions: {e}"))?;
    let pixels = u64::from(w) * u64::from(h);
    if pixels > MAX_PIXELS {
        return Err(format!(
            "Image is {w}x{h} ({pixels} pixels), over the {MAX_PIXELS}-pixel limit"
        ));
    }
    let image = image::load_from_memory(bytes).map_err(|e| format!("Cannot decode image: {e}"))?;
    Ok(image.into_rgb8())
}
