//! OS-native OCR backends: Apple Vision (macOS) and Windows.Media.Ocr.

use image::RgbImage;

use crate::types::{OcrBackend, OcrLine};

#[cfg(target_os = "macos")]
pub mod vision;
#[cfg(windows)]
pub mod win_native;

/// The native backend for this OS, if any.
pub const fn native_backend() -> Option<OcrBackend> {
    if cfg!(target_os = "macos") {
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
    #[cfg(target_os = "macos")]
    {
        vision::recognize(img)
    }
    #[cfg(windows)]
    {
        win_native::recognize(img)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
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
    #[cfg(target_os = "macos")]
    {
        vision::recognize_once(img)
    }
    #[cfg(not(target_os = "macos"))]
    {
        recognize(img)
    }
}

/// Embedded known-text PNG for the separate-process health canary.
pub fn canary_png() -> &'static [u8] {
    include_bytes!("assets/vision_canary.png")
}
