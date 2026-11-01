//! GDI plumbing for the software-rendered notch: a top-down 32-bit DIB section that is
//! handed to `UpdateLayeredWindow` (per-pixel alpha, no activation), and a GDI text
//! rasteriser that turns a string into a grey coverage mask for `Canvas::draw_mask`.
//! Every GDI object is released on drop on every path.

use crate::canvas::{Canvas, Mask};
use crate::diag;
use crate::raii::{GdiObject, SelectScope};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, CreateFontIndirectW, DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GdiFlush, GetDC, GetTextExtentPoint32W, HBITMAP, HDC, HFONT, HGDIOBJ, LOGFONTW, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, ULW_ALPHA, UpdateLayeredWindow};
use windows::core::Error;

const MAX_SURFACE_SIDE: usize = 8192;
const FONT_FACE: &str = "Segoe UI";
/// GDI weights: the Mac's titles are SF semibold, but Segoe UI Semibold (600) reads visibly
/// lighter than SF Semibold, so the bold weight asks for FW_BOLD (700) to match it by eye.
const BOLD: i32 = 700;
const SEMIBOLD: i32 = 600;
const SEMIBOLD_FACE: &str = "Segoe UI Semibold";
/// The face of code text (every Windows has it).
const MONO_FACE: &str = "Consolas";
const REGULAR: i32 = 400;

/// A 32-bit top-down DIB section selected into a memory DC.
pub struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut c_void,
    pub width: usize,
    pub height: usize,
}

impl Surface {
    pub fn new(width: usize, height: usize) -> Option<Self> {
        if width == 0 || height == 0 || width > MAX_SURFACE_SIDE || height > MAX_SURFACE_SIDE {
            return None;
        }
        // SAFETY: plain GDI object creation; every created object is either stored in the
        // returned `Surface` (released by Drop) or released on the failing path below.
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let _ = ReleaseDC(None, screen);
            if dc.0.is_null() {
                diag::last_error("CreateCompatibleDC", "surface");
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // negative: top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(bitmap) if !bits.is_null() => bitmap,
                Ok(bitmap) => {
                    let _ = DeleteObject(bitmap.into());
                    let _ = DeleteDC(dc);
                    return None;
                }
                Err(error) => {
                    diag::win32_error("CreateDIBSection", &error, "surface");
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
            let previous = SelectObject(dc, bitmap.into());
            Some(Self {
                dc,
                bitmap,
                previous,
                bits,
                width,
                height,
            })
        }
    }

    pub fn dc(&self) -> HDC {
        self.dc
    }

    /// Pixel memory (`0xAARRGGBB`, row-major). GDI drawing must be flushed first.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        // SAFETY: `bits` points at `width * height` 32-bit pixels owned by the DIB section,
        // which lives as long as `self`; the `&mut self` borrow keeps access exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.bits.cast::<u32>(), self.width * self.height) }
    }

    pub fn copy_from(&mut self, canvas: &Canvas) {
        if canvas.width == self.width && canvas.height == self.height {
            self.pixels_mut().copy_from_slice(&canvas.pixels);
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: restores the DC's original bitmap before deleting what we created.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Publishes `canvas` as the window's contents with per-pixel alpha. `position` moves the
/// window to a new top-left in the same call; the window size becomes the canvas size.
pub fn present(hwnd: HWND, canvas: &Canvas, position: Option<(i32, i32)>) -> Result<(), Error> {
    PRESENT_SURFACES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let key = hwnd.0 as isize;
        // A window gone (or a first sight of this one) is the moment to free the surfaces of
        // windows that no longer exist; a size change replaces the window's surface below.
        if !cache.iter().any(|(kept, _)| *kept == key) {
            // SAFETY: IsWindow only reads the handle's validity.
            cache.retain(|(kept, _)| {
                unsafe { IsWindow(Some(HWND(*kept as *mut c_void))) }.as_bool()
            });
        }
        let index = match cache.iter().position(|(kept, _)| *kept == key) {
            Some(index) => index,
            None => {
                let created = Surface::new(canvas.width, canvas.height)
                    .ok_or_else(|| Error::from(windows::Win32::Foundation::E_FAIL))?;
                cache.push((key, created));
                cache.len() - 1
            }
        };
        if cache[index].1.width != canvas.width || cache[index].1.height != canvas.height {
            // The old DIB is deselected and deleted by Drop when it is replaced.
            cache[index].1 = Surface::new(canvas.width, canvas.height)
                .ok_or_else(|| Error::from(windows::Win32::Foundation::E_FAIL))?;
        }
        present_with(hwnd, canvas, position, &mut cache[index].1)
    })
}

thread_local! {
    /// One surface per window that was presented to, kept between frames (a DC and a DIB per
    /// frame was most of the cost of a moving notch). Single-threaded: `Surface` is not `Send`.
    static PRESENT_SURFACES: RefCell<Vec<(isize, Surface)>> = const { RefCell::new(Vec::new()) };
}

fn present_with(
    hwnd: HWND,
    canvas: &Canvas,
    position: Option<(i32, i32)>,
    surface: &mut Surface,
) -> Result<(), Error> {
    surface.copy_from(canvas);
    let size = SIZE {
        cx: canvas.width as i32,
        cy: canvas.height as i32,
    };
    let source = POINT { x: 0, y: 0 };
    let destination = position.map(|(x, y)| POINT { x, y });
    let destination_ptr: Option<*const POINT> = destination.as_ref().map(|p| p as *const POINT);
    let blend = BLENDFUNCTION {
        BlendOp: 0, // AC_SRC_OVER
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: 1, // AC_SRC_ALPHA: the bitmap carries premultiplied alpha
    };
    // SAFETY: all pointers refer to locals that outlive the synchronous call; the source DC
    // holds the DIB just filled.
    unsafe {
        UpdateLayeredWindow(
            hwnd,
            None,
            destination_ptr,
            Some(&size),
            Some(surface.dc()),
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
    }
}

/// Widths kept by `measure` before the cache starts over (a card's frames ask for the same
/// strings again; a scroll through many cards must not grow it without end).
const MEASURED_CAP: usize = 16_384;

/// Measured widths of one font face, by text.
type Widths = HashMap<String, i32>;

/// Renders strings with GDI (grey anti-aliasing, no ClearType) into coverage masks, and is the
/// notch's one text layout owner: widths (`measure`) and the bounded fitting and wrapping in
/// `text_layout` (issue 558).
pub struct TextPainter {
    scratch: Surface,
    fonts: Vec<(i32, u8, GdiObject<HFONT>)>,
    /// `measure` results by (pixel size, weight), then text.
    measured: HashMap<(i32, u8), Widths>,
    measured_len: usize,
}

impl TextPainter {
    pub fn new() -> Option<Self> {
        Some(Self {
            // Wide enough for one line of the widest window at 200% scale (the Ask AI view is
            // 1336 logical px): a line wider than this is clipped, though the wrap measured it whole.
            scratch: Surface::new(4096, 96)?,
            fonts: Vec::new(),
            measured: HashMap::new(),
            measured_len: 0,
        })
    }

    /// `weight`: 0 regular, 1 bold, 2 semibold (the Mac's medium and semibold titles).
    fn font(&mut self, size_px: i32, weight: u8) -> Option<HFONT> {
        if let Some((_, _, font)) = self
            .fonts
            .iter()
            .find(|(size, kept, _)| *size == size_px && *kept == weight)
        {
            return Some(font.get());
        }
        let face = match weight {
            2 => SEMIBOLD_FACE,
            3 => MONO_FACE,
            _ => FONT_FACE,
        };
        let mut logical = LOGFONTW {
            lfHeight: -size_px.max(1),
            lfWeight: match weight {
                0 | 3 => REGULAR,
                1 => BOLD,
                _ => SEMIBOLD,
            },
            lfCharSet: DEFAULT_CHARSET,
            lfQuality: ANTIALIASED_QUALITY,
            ..Default::default()
        };
        for (slot, unit) in logical.lfFaceName.iter_mut().zip(face.encode_utf16()) {
            *slot = unit;
        }
        // SAFETY: `logical` is a fully initialised LOGFONTW (face name NUL-terminated by the
        // zeroed default) that outlives the call.
        let font = GdiObject::new(
            unsafe { CreateFontIndirectW(&logical) },
            "CreateFontIndirectW",
        )?;
        let handle = font.get();
        self.fonts.push((size_px, weight, font));
        Some(handle)
    }

    /// Coverage mask for `text` at `size_px` (pixel height of the font cell).
    pub fn render(&mut self, text: &str, size_px: i32, bold: bool) -> Option<Mask> {
        self.render_weight(text, size_px, u8::from(bold))
    }

    /// `render` in Segoe UI Semibold, between regular and bold (a launcher row's title).
    pub fn render_semibold(&mut self, text: &str, size_px: i32) -> Option<Mask> {
        self.render_weight(text, size_px, 2)
    }

    /// `render` in Consolas, for code (the clipboard strip's code cards and previews).
    pub fn render_mono(&mut self, text: &str, size_px: i32) -> Option<Mask> {
        self.render_weight(text, size_px, 3)
    }

    /// Width in pixels of `render`'s mask for `text` (bold or regular), without drawing it:
    /// GDI's extent only, remembered per string.
    pub fn measure(&mut self, text: &str, size_px: i32, bold: bool) -> i32 {
        self.measure_weight(text, size_px, u8::from(bold))
    }

    /// `measure` in Consolas (`render_mono`).
    pub fn measure_mono(&mut self, text: &str, size_px: i32) -> i32 {
        self.measure_weight(text, size_px, 3)
    }

    pub(crate) fn measure_weight(&mut self, text: &str, size_px: i32, weight: u8) -> i32 {
        if text.is_empty() {
            return 0;
        }
        if let Some(width) = self
            .measured
            .get(&(size_px, weight))
            .and_then(|kept| kept.get(text))
        {
            return *width;
        }
        let width = self
            .extent(text, size_px, weight)
            .map_or(
                0,
                |(width, height)| if height == 0 { 0 } else { width as i32 },
            );
        if self.measured_len >= MEASURED_CAP {
            self.measured.clear();
            self.measured_len = 0;
        }
        self.measured
            .entry((size_px, weight))
            .or_default()
            .insert(text.to_string(), width);
        self.measured_len += 1;
        width
    }

    /// The mask's width and height for `text`: GDI's extent, one pixel wider (antialiased
    /// edges), clipped to the scratch surface.
    fn extent(&mut self, text: &str, size_px: i32, weight: u8) -> Option<(usize, usize)> {
        let font = self.font(size_px, weight)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let dc = self.scratch.dc();
        let _selected = SelectScope::select(dc, font.into(), "SelectObject")?;
        let mut extent = SIZE::default();
        // SAFETY: `dc` is the scratch memory DC with the font selected; `wide` and `extent`
        // outlive the call.
        unsafe {
            if !GetTextExtentPoint32W(dc, &wide, &mut extent).as_bool() {
                diag::last_error("GetTextExtentPoint32W", "text");
                return None;
            }
        }
        Some((
            (extent.cx.max(0) as usize + 1).min(self.scratch.width),
            (extent.cy.max(0) as usize).min(self.scratch.height),
        ))
    }

    fn render_weight(&mut self, text: &str, size_px: i32, weight: u8) -> Option<Mask> {
        if text.is_empty() {
            return Some(Mask {
                width: 0,
                height: 0,
                coverage: Vec::new(),
            });
        }
        let (width, height) = self.extent(text, size_px, weight)?;
        if width == 0 || height == 0 {
            return None;
        }
        let font = self.font(size_px, weight)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let dc = self.scratch.dc();
        let _selected = SelectScope::select(dc, font.into(), "SelectObject")?;
        // Only the box read back below is cleared: the whole 4096×96 scratch was most of a
        // short string's cost.
        let stride = self.scratch.width;
        for row in self.scratch.pixels_mut().chunks_mut(stride).take(height) {
            row[..width].fill(0);
        }
        // SAFETY: `dc` is the scratch memory DC with the font selected; the white-on-black
        // text is read back after GdiFlush.
        unsafe {
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00FF_FFFF));
            if !TextOutW(dc, 0, 0, &wide).as_bool() {
                diag::last_error("TextOutW", "text");
                return None;
            }
            let _ = GdiFlush();
        }
        let pixels = self.scratch.pixels_mut();
        let mut coverage = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
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
