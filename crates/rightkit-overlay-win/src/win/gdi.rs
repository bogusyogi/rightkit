//! GDI plumbing: the top-down 32-bit DIB section a frame is copied into, the
//! `UpdateLayeredWindow` call that publishes it with per-pixel alpha, and the GDI text
//! rasteriser behind `PaintCtx::text`. Every GDI object is released on drop on every path, and
//! every cache of GDI objects is bounded.

use std::ffi::c_void;
use std::mem::size_of;

use windows_sys::Win32::Foundation::{HWND, POINT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject, GdiFlush,
    GetDC, GetTextExtentPoint32W, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TextOutW,
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, DEFAULT_CHARSET, DIB_RGB_COLORS, HBITMAP, HDC, HFONT, HGDIOBJ, LOGFONTW,
    TRANSPARENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

use crate::geometry::MAX_SURFACE_SIDE;
use crate::text::Mask;

/// A 32-bit top-down DIB section selected into a memory DC. Pixels are premultiplied
/// `0xAARRGGBB`, the format `UpdateLayeredWindow` blends with `AC_SRC_ALPHA`.
pub(crate) struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
    /// Pixel width and height of the DIB. A window may use only the top-left part of it (see
    /// `present`); rows are always `width` pixels apart.
    pub width: usize,
    pub height: usize,
}

impl Surface {
    pub(crate) fn new(width: usize, height: usize) -> Option<Self> {
        let side = MAX_SURFACE_SIDE as usize;
        if width == 0 || height == 0 || width > side || height > side {
            return None;
        }
        // SAFETY: plain GDI object creation; every created object is either stored in the
        // returned Surface (released by Drop) or released on the failing path below.
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            let dc = CreateCompatibleDC(screen);
            ReleaseDC(std::ptr::null_mut(), screen);
            if dc.is_null() {
                return None;
            }
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // Negative height: rows run top to bottom.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if bitmap.is_null() || bits.is_null() {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
                return None;
            }
            let previous = SelectObject(dc, bitmap);
            if previous.is_null() {
                // The bitmap could not be selected: release both objects rather than leak them.
                DeleteObject(bitmap);
                DeleteDC(dc);
                return None;
            }
            Some(Self {
                dc,
                bitmap,
                previous,
                bits: bits.cast::<u32>(),
                width,
                height,
            })
        }
    }

    pub(crate) fn dc(&self) -> HDC {
        self.dc
    }

    /// The pixel memory, `width * height` values, top row first. GDI drawing into the DC must be
    /// flushed before reading it.
    pub(crate) fn pixels_mut(&mut self) -> &mut [u32] {
        // SAFETY: `bits` points at width * height 32-bit pixels owned by the DIB section, which
        // lives as long as `self`; the &mut self borrow keeps access exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.width * self.height) }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: restores the DC original bitmap before deleting what we created.
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

/// Publishes the top-left `width` x `height` pixels of the surface as the window contents with
/// per-pixel alpha, moving the window to `(x, y)` and sizing it to `width` x `height` in the same
/// call. The surface may be larger than the window (grow-only backing); a request larger than the
/// surface is refused.
pub(crate) fn present(
    hwnd: HWND,
    surface: &Surface,
    x: i32,
    y: i32,
    width: usize,
    height: usize,
) -> bool {
    if width == 0 || height == 0 || width > surface.width || height > surface.height {
        return false;
    }
    let size = SIZE {
        cx: width as i32,
        cy: height as i32,
    };
    let destination = POINT { x, y };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        // The bitmap carries premultiplied alpha.
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    // SAFETY: every pointer refers to a local that outlives the synchronous call; the source DC
    // holds the DIB that was just filled.
    unsafe {
        UpdateLayeredWindow(
            hwnd,
            std::ptr::null_mut(),
            &destination,
            &size,
            surface.dc(),
            &source,
            0,
            &blend,
            ULW_ALPHA,
        ) != 0
    }
}

// ---- text ----------------------------------------------------------------------------------

/// Widest line the scratch surface will grow to.
const MAX_TEXT_WIDTH: usize = 4096;
/// Tallest line the scratch surface will grow to.
const MAX_TEXT_HEIGHT: usize = 512;
/// Longest string rendered or measured, in UTF-16 units. The scratch surface is at most
/// [`MAX_TEXT_WIDTH`] wide, so more could not be drawn anyway; the cap keeps the GDI calls (whose
/// lengths are `i32`) and the work bounded for any input.
const MAX_TEXT_UNITS: usize = 1024;
/// Fonts kept alive at once (least recently used goes first). Each is a GDI object, and the
/// process has a limit of 10,000 of them.
const FONT_CACHE_MAX: usize = 32;
/// Largest font cell height requested from GDI, in device pixels.
const MAX_FONT_PX: i32 = 1024;

#[derive(Clone, PartialEq, Eq)]
struct FontKey {
    family: String,
    px: i32,
    weight: u16,
    italic: bool,
}

/// The UTF-16 text handed to GDI: at most [`MAX_TEXT_UNITS`] units, never ending in half of a
/// surrogate pair.
fn wide_text(text: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = text.encode_utf16().take(MAX_TEXT_UNITS).collect();
    if wide
        .last()
        .is_some_and(|unit| (0xD800..0xDC00).contains(unit))
    {
        wide.pop();
    }
    wide
}

/// Renders strings with GDI (grey anti-aliasing, no ClearType) into coverage masks. Fonts are
/// created once per `(family, size, weight, italic)` and kept in a bounded least-recently-used
/// list ([`FONT_CACHE_MAX`]); the scratch surface grows to the widest string seen.
pub(crate) struct GdiText {
    scratch: Option<Surface>,
    fonts: Vec<(FontKey, HFONT)>,
    /// Every font handed to `DeleteObject` by eviction, with whether GDI accepted it (tests).
    #[cfg(test)]
    evicted: Vec<(HFONT, bool)>,
}

impl GdiText {
    pub(crate) fn new() -> Self {
        Self {
            scratch: None,
            fonts: Vec::new(),
            #[cfg(test)]
            evicted: Vec::new(),
        }
    }

    fn font(&mut self, family: &str, px: f32, weight: u16, italic: bool) -> Option<HFONT> {
        let px = if px.is_finite() {
            (px.round() as i32).clamp(1, MAX_FONT_PX)
        } else {
            1
        };
        let key = FontKey {
            family: family.to_owned(),
            px,
            weight,
            italic,
        };
        if let Some(at) = self.fonts.iter().position(|(k, _)| *k == key) {
            // Most recently used goes last.
            let entry = self.fonts.remove(at);
            let font = entry.1;
            self.fonts.push(entry);
            return Some(font);
        }
        let mut logical = LOGFONTW {
            lfHeight: -key.px,
            lfWeight: i32::from(weight),
            lfItalic: u8::from(italic),
            lfCharSet: DEFAULT_CHARSET,
            lfQuality: ANTIALIASED_QUALITY,
            ..Default::default()
        };
        // Leave the last unit zero so the face name stays NUL-terminated.
        for (slot, unit) in logical
            .lfFaceName
            .iter_mut()
            .zip(family.encode_utf16().take(31))
        {
            *slot = unit;
        }
        // SAFETY: `logical` is a fully initialised LOGFONTW that outlives the call.
        let font = unsafe { CreateFontIndirectW(&logical) };
        if font.is_null() {
            return None;
        }
        if self.fonts.len() >= FONT_CACHE_MAX {
            let (_, oldest) = self.fonts.remove(0);
            // SAFETY: the font is not selected into any DC (every selection is undone before the
            // calls below return) and its handle is dropped here.
            #[cfg(test)]
            let deleted = unsafe { DeleteObject(oldest) } != 0;
            #[cfg(test)]
            self.evicted.push((oldest, deleted));
            #[cfg(not(test))]
            unsafe {
                DeleteObject(oldest);
            }
        }
        self.fonts.push((key, font));
        Some(font)
    }

    /// How many fonts are alive (for the bound test).
    #[cfg(test)]
    pub(crate) fn font_count(&self) -> usize {
        self.fonts.len()
    }

    fn ensure_scratch(&mut self, width: usize, height: usize) -> Option<&mut Surface> {
        let big_enough = self
            .scratch
            .as_ref()
            .is_some_and(|s| s.width >= width && s.height >= height);
        if !big_enough {
            // Grow with some headroom so a changing string does not reallocate every frame.
            let (old_w, old_h) = self
                .scratch
                .as_ref()
                .map_or((256, 48), |s| (s.width, s.height));
            let new_w = width
                .max(old_w)
                .max(256)
                .next_power_of_two()
                .min(MAX_TEXT_WIDTH);
            let new_h = height
                .max(old_h)
                .max(48)
                .next_multiple_of(16)
                .min(MAX_TEXT_HEIGHT);
            self.scratch = Some(Surface::new(new_w, new_h)?);
        }
        self.scratch.as_mut()
    }

    /// Text extent in device pixels.
    fn extent(&mut self, wide: &[u16], font: HFONT) -> Option<(usize, usize)> {
        let surface = self.ensure_scratch(1, 1)?;
        let dc = surface.dc();
        let mut size = SIZE { cx: 0, cy: 0 };
        // SAFETY: `dc` is the scratch memory DC; the font is selected for the call and the
        // previous object restored; `wide` and `size` outlive it.
        let ok = unsafe {
            let previous = SelectObject(dc, font);
            let ok = GetTextExtentPoint32W(dc, wide.as_ptr(), wide.len() as i32, &mut size) != 0;
            SelectObject(dc, previous);
            ok
        };
        if !ok {
            return None;
        }
        Some((size.cx.max(0) as usize, size.cy.max(0) as usize))
    }

    pub(crate) fn measure(
        &mut self,
        text: &str,
        family: &str,
        px: f32,
        weight: u16,
        italic: bool,
    ) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        let wide = wide_text(text);
        let Some(font) = self.font(family, px, weight, italic) else {
            return (0.0, 0.0);
        };
        self.extent(&wide, font)
            .map_or((0.0, 0.0), |(w, h)| (w as f32, h as f32))
    }

    pub(crate) fn render(
        &mut self,
        text: &str,
        family: &str,
        px: f32,
        weight: u16,
        italic: bool,
    ) -> Option<Mask> {
        if text.is_empty() {
            return Some(Mask {
                width: 0,
                height: 0,
                coverage: Vec::new(),
            });
        }
        let wide = wide_text(text);
        let font = self.font(family, px, weight, italic)?;
        let (extent_w, extent_h) = self.extent(&wide, font)?;
        // One spare column: glyph overhang and the last advance round up.
        let width = (extent_w + 1).min(MAX_TEXT_WIDTH);
        let height = extent_h.min(MAX_TEXT_HEIGHT);
        if width == 0 || height == 0 {
            return None;
        }
        let surface = self.ensure_scratch(width, height)?;
        let dc = surface.dc();
        let stride = surface.width;
        surface.pixels_mut().fill(0);
        // SAFETY: as above; white-on-black text is read back after GdiFlush.
        let drawn = unsafe {
            let previous = SelectObject(dc, font);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, 0x00FF_FFFF);
            let ok = TextOutW(dc, 0, 0, wide.as_ptr(), wide.len() as i32) != 0;
            GdiFlush();
            SelectObject(dc, previous);
            ok
        };
        if !drawn {
            return None;
        }
        let pixels = surface.pixels_mut();
        let mut coverage = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                // White on black: every colour channel carries the grey coverage.
                coverage.push((pixels[row * stride + column] & 0xFF) as u8);
            }
        }
        Some(Mask {
            width,
            height,
            coverage,
        })
    }
}

impl Drop for GdiText {
    fn drop(&mut self) {
        for (_, font) in self.fonts.drain(..) {
            // SAFETY: the fonts were created by CreateFontIndirectW and are not selected anywhere:
            // every selection above is undone before returning.
            unsafe {
                DeleteObject(font);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_bounded_and_never_splits_a_surrogate_pair() {
        let long = "a".repeat(10_000);
        assert_eq!(wide_text(&long).len(), MAX_TEXT_UNITS);
        assert_eq!(wide_text("hi"), vec![u16::from(b'h'), u16::from(b'i')]);
        // A pair straddling the cap loses the whole pair, not half of it.
        let straddle = format!("{}\u{1F600}", "a".repeat(MAX_TEXT_UNITS - 1));
        let wide = wide_text(&straddle);
        assert_eq!(wide.len(), MAX_TEXT_UNITS - 1);
        assert!(wide.iter().all(|&u| u == u16::from(b'a')));
        assert!(wide_text("").is_empty());
    }

    #[test]
    fn the_font_list_is_bounded_and_keeps_recent_fonts() {
        let mut text = GdiText::new();
        // Far more distinct sizes than the list holds.
        for px in 8..(8 + 3 * FONT_CACHE_MAX as i32) {
            assert!(text.font("Segoe UI", px as f32, 400, false).is_some());
            assert!(text.font_count() <= FONT_CACHE_MAX);
        }
        assert_eq!(text.font_count(), FONT_CACHE_MAX);
        // A hit does not add an entry; a request that is not finite or enormous is held to range.
        let before = text.font_count();
        let recent = 8 + 3 * FONT_CACHE_MAX as i32 - 1;
        assert!(text.font("Segoe UI", recent as f32, 400, false).is_some());
        assert_eq!(text.font_count(), before);
        assert!(text.font("Segoe UI", f32::NAN, 400, false).is_some());
        assert!(text.font("Segoe UI", 1.0e9, 400, false).is_some());
        assert!(text.font_count() <= FONT_CACHE_MAX);
    }

    #[test]
    fn huge_strings_render_and_measure_without_overflow() {
        let mut text = GdiText::new();
        let long = "W".repeat(1_000_000);
        let (w, h) = text.measure(&long, "Segoe UI", 16.0, 400, false);
        assert!(w.is_finite() && h.is_finite() && h > 0.0, "{w} x {h}");
        let mask = text.render(&long, "Segoe UI", 16.0, 400, false);
        if let Some(mask) = mask {
            assert!(mask.width <= MAX_TEXT_WIDTH && mask.height <= MAX_TEXT_HEIGHT);
        }
    }

    #[test]
    fn a_surface_publishes_only_what_fits_and_owns_its_pixels() {
        let mut surface = Surface::new(40, 20).expect("a small DIB");
        assert_eq!((surface.width, surface.height), (40, 20));
        assert_eq!(surface.pixels_mut().len(), 40 * 20);
        // Degenerate and oversized requests are refused up front.
        assert!(Surface::new(0, 10).is_none());
        assert!(Surface::new(10, 0).is_none());
        assert!(Surface::new(MAX_SURFACE_SIDE as usize + 1, 10).is_none());
        assert!(!present(std::ptr::null_mut(), &surface, 0, 0, 41, 20));
        assert!(!present(std::ptr::null_mut(), &surface, 0, 0, 40, 0));
    }
}

#[cfg(test)]
mod font_cache_tests {
    use super::*;
    #[test]
    fn lru_eviction_deletes_only_the_cold_font() {
        let mut text = GdiText::new();
        let first = text.font("Segoe UI", 10.0, 400, false).unwrap();
        let cold = text.font("Segoe UI", 11.0, 400, false).unwrap();
        for px in 12..(10 + FONT_CACHE_MAX) {
            text.font("Segoe UI", px as f32, 400, false).unwrap();
        }
        assert_eq!(text.fonts.len(), FONT_CACHE_MAX);
        assert_eq!(text.font("Segoe UI", 10.0, 400, false), Some(first));
        text.font("Segoe UI", 50.0, 400, false).unwrap();
        assert_eq!(text.fonts.len(), FONT_CACHE_MAX);
        // GDI recycles freed handle values, so a stale handle's type is not a reliable probe;
        // assert on what the cache itself did: exactly the cold font was deleted, and GDI took it.
        assert_eq!(text.evicted, vec![(cold, true)]);
        assert!(text.fonts.iter().any(|(_, font)| *font == first));
        assert!(text.fonts.iter().all(|(key, _)| key.px != 11));
    }
}
