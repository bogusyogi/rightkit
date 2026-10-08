//! OS-native OCR backends: Apple Vision (macOS) and Windows.Media.Ocr.

use image::RgbImage;

use crate::types::{OcrBackend, OcrLine};

#[cfg(all(target_os = "macos", feature = "native"))]
pub mod vision;
#[cfg(all(windows, feature = "native"))]
pub mod win_native;

/// The native backend for this OS, if compiled in (`native` feature) and supported.
pub const fn native_backend() -> Option<OcrBackend> {
    if !cfg!(feature = "native") {
        None
    } else if cfg!(target_os = "macos") {
        Some(OcrBackend::Vision)
    } else if cfg!(windows) {
        Some(OcrBackend::WindowsMedia)
    } else {
        None
    }
}

/// Recognize with the native backend, including macOS's blank-page health
/// self-check (an empty Vision result is disambiguated before returning).
pub fn recognize(img: &RgbImage) -> Result<Vec<OcrLine>, String> {
    #[cfg(all(target_os = "macos", feature = "native"))]
    {
        vision::recognize(img)
    }
    #[cfg(all(windows, feature = "native"))]
    {
        win_native::recognize(img)
    }
    #[cfg(not(all(any(target_os = "macos", windows), feature = "native")))]
    {
        let _ = img;
        Err(format!(
            "{} no native OCR backend on this platform",
            crate::types::OCR_UNSUPPORTED_PLATFORM
        ))
    }
}

/// Exactly one native request, no in-process canary. Used by the isolated
/// worker so a poisoned process never runs the health check itself.
pub fn recognize_once(img: &RgbImage) -> Result<Vec<OcrLine>, String> {
    #[cfg(all(target_os = "macos", feature = "native"))]
    {
        vision::recognize_once(img)
    }
    #[cfg(not(all(target_os = "macos", feature = "native")))]
    {
        recognize(img)
    }
}

/// Embedded known-text PNG for the separate-process health canary.
pub fn canary_png() -> &'static [u8] {
    include_bytes!("assets/vision_canary.png")
}
