//! Windows.Media.Ocr text recognition — the Windows OCR backend.
//!
//! Ported from ScrapeRight's `win_native.rs`, which measured it (2026-07-18,
//! Win11 26200, no NPU) at 95.7% mean word-F1 vs PP-OCRv6's 82.9% on real
//! production images, and 37-240 ms/image on CPU vs 0.6-12 s/image for PP-OCRv6
//! on warm DirectML. CPU-only and in-process.
//!
//! Pure WinRT, no bundled binary and no subprocess: RgbImage -> BGRA8 bytes ->
//! SoftwareBitmap -> OcrEngine::RecognizeAsync.
//!
//! Two divergences from ScrapeRight:
//!
//!   - **Fallback lives above this module.** A machine with no OCR language pack
//!     gets the `windows-ocr-unavailable:` error; the router (`crate::Ocr`)
//!     decides whether to fall back to the ONNX path.
//!   - **Geometry.** WinRT's `OcrLine` exposes no rect and no confidence — only
//!     `OcrWord` carries `BoundingRect` — so a line's rect is the union of its
//!     words' rects and its confidence is `None`. Word rects are top-left pixel
//!     rectangles in the recognized bitmap's space; normalizing by that bitmap's
//!     dimensions also undoes the oversize downscale below, since a normalized
//!     rect is scale-invariant.

use std::cell::RefCell;

use image::{imageops, RgbImage};
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine as WinOcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;

use crate::types::{OcrLine, OcrRect};

thread_local! {
    /// One engine per thread: creation is a WinRT factory call (cheap but not
    /// free), and OCR runs on a pooled blocking thread that may serve many pages.
    static ENGINE: RefCell<Option<WinOcrEngine>> = const { RefCell::new(None) };
}

/// Recognize all text in `img`, in engine order.
///
/// `Err` means the backend is unavailable (no OCR language pack) or a WinRT call
/// failed. `Ok(vec![])` = no text found.
pub fn recognize(img: &RgbImage) -> Result<Vec<OcrLine>, String> {
    ENGINE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(create_engine()?);
        }
        recognize_with(slot.as_ref().expect("engine cached above"), img)
    })
}

/// User-profile languages first, then en-US. Both missing → the actionable
/// `windows-ocr-unavailable:` error. This is a permanent configuration state
/// (Windows N/KN, stripped LTSC images, some non-English installs), not a
/// transient failure, so no retry helps and the UI must say so plainly.
fn create_engine() -> Result<WinOcrEngine, String> {
    if let Ok(engine) = WinOcrEngine::TryCreateFromUserProfileLanguages() {
        return Ok(engine);
    }
    Language::CreateLanguage(&HSTRING::from("en-US"))
        .ok()
        .and_then(|lang| WinOcrEngine::TryCreateFromLanguage(&lang).ok())
        .ok_or_else(|| {
            format!(
                "{} no OCR language pack installed \
                 (probed user-profile languages and en-US)",
                crate::types::WINDOWS_OCR_UNAVAILABLE
            )
        })
}

/// Union of word rects, normalized by the recognized bitmap's dimensions.
///
/// `None` when a line has no words with usable rects, which is the one case
/// where a line cannot be placed on the page.
pub(crate) fn union_normalized(
    words: impl IntoIterator<Item = (f32, f32, f32, f32)>,
    w: f32,
    h: f32,
) -> Option<OcrRect> {
    let mut acc: Option<(f32, f32, f32, f32)> = None; // left, top, right, bottom
    for (x, y, ww, wh) in words {
        let (l, t, r, b) = (x, y, x + ww, y + wh);
        acc = Some(match acc {
            None => (l, t, r, b),
            Some((al, at, ar, ab)) => (al.min(l), at.min(t), ar.max(r), ab.max(b)),
        });
    }
    let (l, t, r, b) = acc?;
    Some(OcrRect {
        x: l / w,
        y: t / h,
        width: (r - l) / w,
        height: (b - t) / h,
    })
}

fn recognize_with(engine: &WinOcrEngine, img: &RgbImage) -> Result<Vec<OcrLine>, String> {
    // OcrEngine rejects images beyond MaxImageDimension (10 000 px as of Win11);
    // downscale-only guard. Rects are normalized against the SCALED dimensions
    // below, so this needs no separate un-scaling step.
    let max = WinOcrEngine::MaxImageDimension().map_err(|e| format!("MaxImageDimension: {e}"))?;
    let scaled;
    let img = if img.width() > max || img.height() > max {
        let scale = max as f32 / img.width().max(img.height()) as f32;
        let nw = ((img.width() as f32 * scale) as u32).clamp(1, max);
        let nh = ((img.height() as f32 * scale) as u32).clamp(1, max);
        scaled = imageops::resize(img, nw, nh, imageops::FilterType::Lanczos3);
        &scaled
    } else {
        img
    };

    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    let mut bgra = Vec::with_capacity(w as usize * h as usize * 4);
    for p in img.pixels() {
        bgra.extend_from_slice(&[p[2], p[1], p[0], 255]);
    }

    let buffer =
        CryptographicBuffer::CreateFromByteArray(&bgra).map_err(|e| format!("ibuffer: {e}"))?;
    let bitmap =
        SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32)
            .map_err(|e| format!("software bitmap: {e}"))?;

    // Blocking join() is correct here: the Tauri command runs OCR on a
    // spawn_blocking thread (MTA), never on the UI thread. windows 0.62 exposes
    // join through windows-future; the former get() helper is gone.
    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| format!("RecognizeAsync: {e}"))?
        .join()
        .map_err(|e| format!("RecognizeAsync wait: {e}"))?;

    let (fw, fh) = (w as f32, h as f32);
    let lines = result.Lines().map_err(|e| format!("lines: {e}"))?;
    let mut out = Vec::new();
    for line in &lines {
        let text = line
            .Text()
            .map_err(|e| format!("line text: {e}"))?
            .to_string();

        let words = line.Words().map_err(|e| format!("words: {e}"))?;
        let rects = words.into_iter().filter_map(|word| {
            word.BoundingRect()
                .ok()
                .map(|r| (r.X, r.Y, r.Width, r.Height))
        });

        // A line with no placeable word rect cannot be positioned in the overlay,
        // so it is dropped rather than stacked at the origin where it would
        // capture clicks meant for real text.
        if let Some(rect) = union_normalized(rects, fw, fh) {
            out.push(OcrLine {
                text,
                rect,
                confidence: None, // WinRT exposes none on OcrLine or OcrWord.
            });
        }
    }
    Ok(out)
}
