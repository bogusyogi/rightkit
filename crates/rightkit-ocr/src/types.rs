//! Shared result types, error prefixes and the decode guard.
//!
//! One result shape for every backend: text + normalized top-left geometry +
//! optional confidence (ViewRight's `OcrLine`, the most mature donor).

use image::RgbImage;
use serde::{Deserialize, Serialize};

/// Prefix for a Windows backend that cannot recognize at all (no language pack).
pub const WINDOWS_OCR_UNAVAILABLE: &str = "windows-ocr-unavailable:";
/// Prefix for an Apple Vision request that could not be performed.
pub const MACOS_VISION_UNAVAILABLE: &str = "macos-vision-unavailable:";
/// Prefix for a platform with no native OCR backend at all (Linux builds).
pub const OCR_UNSUPPORTED_PLATFORM: &str = "ocr-unsupported-platform:";

/// Does this error mean the native backend is unusable on this machine (a
/// capability verdict, and the only thing that arms the ONNX fallback), as
/// opposed to a per-image failure that must stay visible?
pub fn is_backend_unavailable(err: &str) -> bool {
    err.starts_with(WINDOWS_OCR_UNAVAILABLE)
        || err.starts_with(MACOS_VISION_UNAVAILABLE)
        || err.starts_with(OCR_UNSUPPORTED_PLATFORM)
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
