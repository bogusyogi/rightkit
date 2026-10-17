//! Page rasterization for scanned PDFs (OCR input). Feature `raster`.
//!
//! **Scope.** This is not a general PDF renderer. A scanned page is a page-sized image, and that
//! is what is drawn: image XObjects (also inside forms) are placed through the page's CTM, its
//! crop box and its `/Rotate`, on a white background, and every other operator is ignored.
//! [`RasterPage::limitations`] says when the page held something this does not draw (visible text,
//! vector graphics, inline images), so a caller can decide whether the picture is faithful. Use
//! [`crate::scan::classify_page`] first to find the pages that need this.
//!
//! **Supported images:** 1, 2, 4, 8, 16 bit samples with `DeviceGray`, `DeviceRGB`, `DeviceCMYK`,
//! `CalGray`, `CalRGB`, `ICCBased` (by component count) and `Indexed`; `/Decode` arrays;
//! `ImageMask` stencils (painted in the current fill colour); `/SMask` soft masks; filters
//! `FlateDecode`, `LZWDecode`, `ASCII85Decode` and `DCTDecode` (JPEG, 8-bit gray or RGB).
//! **Not supported** (typed [`RasterError::UnsupportedImage`]): `CCITTFaxDecode`, `JBIG2Decode`,
//! `JPXDecode`, `RunLengthDecode`, `ASCIIHexDecode`, CMYK JPEG, `Separation`, `DeviceN`, `Lab`,
//! patterns. `/Mask` (stencil or colour key) is ignored.

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::inspect::{deref_obj, load_pdf, object_number, resolve_rotation};
use crate::scan::{
    classify_page_doc, cmyk_to_rgb, mul, page_box, page_id_of, walk_page, Event, Matrix6,
    RasterError, IDENTITY,
};

/// Most pixels one source image may have (a 600 dpi A3 page is about 70 million; this is 67).
const MAX_IMAGE_PIXELS: u64 = 1 << 26;
/// Most bytes one image stream may decode to.
const MAX_IMAGE_BYTES: usize = 256 * 1024 * 1024;
/// Most distinct image paints one page may perform.
const MAX_IMAGES_PER_PAGE: usize = 256;

/// Pixel layout of a [`RasterPage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// One byte per pixel, luma (0.299 R + 0.587 G + 0.114 B). The usual OCR input.
    Gray8,
    /// Four bytes per pixel, alpha always 255 (the page is composited on white).
    Rgba8,
}

impl PixelFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Gray8 => 1,
            PixelFormat::Rgba8 => 4,
        }
    }
}

/// Options for [`rasterize_page`].
#[derive(Clone, Copy, Debug)]
pub struct RasterOptions {
    /// Output resolution, 1..=1200. Default 300.
    pub dpi: f32,
    /// Refuse a page whose output would have more pixels than this, before allocating anything.
    /// Default 60 million (a 300 dpi A4 page is 8.7 million; RGBA output at the default maximum is
    /// 240 MB).
    pub max_pixels: u64,
    pub format: PixelFormat,
}

impl Default for RasterOptions {
    fn default() -> Self {
        RasterOptions {
            dpi: 300.0,
            max_pixels: 60_000_000,
            format: PixelFormat::Gray8,
        }
    }
}

/// Something on the page that rasterization does not draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterLimitation {
    /// Text shown in a visible render mode.
    VisibleText,
    /// Path painting or shadings.
    VectorGraphics,
    /// `BI` inline images.
    InlineImage,
}

/// One rendered page. Pixels are row-major, top row first, rows tightly packed.
#[derive(Clone, Debug)]
pub struct RasterPage {
    /// 1-based page number.
    pub page: u32,
    pub width: u32,
    pub height: u32,
    pub dpi: f32,
    pub format: PixelFormat,
    pub pixels: Vec<u8>,
    /// What the page held that is not in the picture. Empty means the picture is the whole page.
    pub limitations: Vec<RasterLimitation>,
}

impl RasterPage {
    /// True when nothing on the page was left out.
    pub fn is_faithful(&self) -> bool {
        self.limitations.is_empty()
    }

    /// Encodes the page as PNG (8-bit gray or RGBA).
    pub fn to_png(&self) -> Result<Vec<u8>, RasterError> {
        use image::{ExtendedColorType, ImageEncoder};
        let color = match self.format {
            PixelFormat::Gray8 => ExtendedColorType::L8,
            PixelFormat::Rgba8 => ExtendedColorType::Rgba8,
        };
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&self.pixels, self.width, self.height, color)
            .map_err(|e| RasterError::Png(e.to_string()))?;
        Ok(out)
    }
}

/// Renders page `page` (1-based) of PDF `bytes`.
pub fn rasterize_page(
    bytes: &[u8],
    page: u32,
    options: &RasterOptions,
) -> Result<RasterPage, RasterError> {
    let doc = load_pdf(bytes).map_err(|_| RasterError::Malformed)?;
    rasterize_page_doc(&doc, page, options)
}

/// Renders page `page` (1-based) of an already loaded document.
pub fn rasterize_page_doc(
    doc: &Document,
    page: u32,
    options: &RasterOptions,
) -> Result<RasterPage, RasterError> {
    let dpi = options.dpi;
    if !dpi.is_finite() || !(1.0..=1200.0).contains(&dpi) {
        return Err(RasterError::InvalidDpi(dpi));
    }
    let page_id = page_id_of(doc, page)?;
    let b = page_box(doc, page_id);
    let (pw, ph) = (b[2] - b[0], b[3] - b[1]);
    if !(pw.is_finite() && ph.is_finite() && pw > 0.0 && ph > 0.0) {
        return Err(RasterError::Malformed);
    }
    let rotation = doc
        .get_object(page_id)
        .ok()
        .and_then(|o| o.as_dict().ok())
        .map(|d| resolve_rotation(doc, d))
        .filter(|r| r % 90 == 0)
        .unwrap_or(0);

    let s = f64::from(dpi) / 72.0;
    let (wf, hf) = if rotation == 90 || rotation == 270 {
        ((ph * s).ceil().max(1.0), (pw * s).ceil().max(1.0))
    } else {
        ((pw * s).ceil().max(1.0), (ph * s).ceil().max(1.0))
    };
    let (w64, h64) = (wf as u64, hf as u64);
    // Checked before any allocation; the per-side cap keeps the product from overflowing.
    if w64 > u64::from(u32::MAX) / 2
        || h64 > u64::from(u32::MAX) / 2
        || w64.saturating_mul(h64) > options.max_pixels
    {
        return Err(RasterError::TooLarge {
            width: w64,
            height: h64,
            max_pixels: options.max_pixels,
        });
    }
    let (width, height) = (w64 as u32, h64 as u32);

    // User space -> device pixels (y down), for the visible box and rotation.
    let page_matrix: Matrix6 = match rotation {
        90 => [0.0, s, s, 0.0, -b[1] * s, -b[0] * s],
        180 => [-s, 0.0, 0.0, s, b[2] * s, -b[1] * s],
        270 => [0.0, -s, -s, 0.0, b[3] * s, b[2] * s],
        _ => [s, 0.0, 0.0, -s, -b[0] * s, b[3] * s],
    };

    let channels = options.format.bytes_per_pixel();
    let len = (w64 * h64) as usize * channels;
    let mut canvas = Canvas {
        width,
        height,
        channels,
        px: vec![255u8; len],
    };

    let mut limitations: Vec<RasterLimitation> = Vec::new();
    let mut note = |l: RasterLimitation| {
        if !limitations.contains(&l) {
            limitations.push(l);
        }
    };
    let mut images = 0usize;
    walk_page(doc, page_id, IDENTITY, &mut |event| {
        match event {
            Event::Text { visible } => {
                if visible {
                    note(RasterLimitation::VisibleText);
                }
            }
            Event::Vector => note(RasterLimitation::VectorGraphics),
            Event::InlineImage => note(RasterLimitation::InlineImage),
            Event::Image {
                id,
                ctm,
                fill,
                resources,
            } => {
                images += 1;
                if images > MAX_IMAGES_PER_PAGE {
                    return Err(RasterError::TooMuchWork);
                }
                let image = decode_image(doc, id, resources, true)?;
                draw_image(&mut canvas, &image, &mul(&ctm, &page_matrix), fill);
            }
        }
        Ok(())
    })?;

    Ok(RasterPage {
        page,
        width,
        height,
        dpi,
        format: options.format,
        pixels: canvas.px,
        limitations,
    })
}

/// Convenience: [`rasterize_page`] then [`RasterPage::to_png`].
pub fn rasterize_page_png(
    bytes: &[u8],
    page: u32,
    options: &RasterOptions,
) -> Result<Vec<u8>, RasterError> {
    rasterize_page(bytes, page, options)?.to_png()
}

/// Renders every page that [`crate::scan::PageProfile::is_scanned`], returning them in order.
pub fn rasterize_scanned_pages(
    bytes: &[u8],
    options: &RasterOptions,
) -> Result<Vec<RasterPage>, RasterError> {
    let doc = load_pdf(bytes).map_err(|_| RasterError::Malformed)?;
    if doc.is_encrypted() {
        return Err(RasterError::Encrypted);
    }
    let numbers: Vec<u32> = doc.get_pages().keys().copied().collect();
    let mut out = Vec::new();
    for n in numbers {
        if classify_page_doc(&doc, n).is_ok_and(|p| p.is_scanned()) {
            out.push(rasterize_page_doc(&doc, n, options)?);
        }
    }
    Ok(out)
}

// -------------------------------------------------------------------------------------------
// Canvas and image placement
// -------------------------------------------------------------------------------------------

struct Canvas {
    width: u32,
    height: u32,
    channels: usize,
    px: Vec<u8>,
}

impl Canvas {
    fn blend(&mut self, x: u32, y: u32, rgb: [f32; 3], a: f32) {
        let at = (y as usize * self.width as usize + x as usize) * self.channels;
        if self.channels == 1 {
            let luma = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
            let d = f32::from(self.px[at]);
            self.px[at] = (d + (luma - d) * a).round().clamp(0.0, 255.0) as u8;
        } else {
            for (px, v) in self.px[at..at + 3].iter_mut().zip(rgb) {
                let d = f32::from(*px);
                *px = (d + (v - d) * a).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn draw_image(canvas: &mut Canvas, image: &Decoded, t: &Matrix6, fill: [u8; 3]) {
    let det = t[0] * t[3] - t[1] * t[2];
    if !det.is_finite() || det.abs() < 1e-12 || t.iter().any(|v| !v.is_finite()) {
        return;
    }
    // Device-space bounding box of the unit square.
    let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in corners {
        let dx = x * t[0] + y * t[2] + t[4];
        let dy = x * t[1] + y * t[3] + t[5];
        min_x = min_x.min(dx);
        max_x = max_x.max(dx);
        min_y = min_y.min(dy);
        max_y = max_y.max(dy);
    }
    let x_start = min_x.floor().max(0.0) as u32;
    let y_start = min_y.floor().max(0.0) as u32;
    let x_end = (max_x.ceil().max(0.0) as u64).min(u64::from(canvas.width)) as u32;
    let y_end = (max_y.ceil().max(0.0) as u64).min(u64::from(canvas.height)) as u32;
    if x_start >= x_end || y_start >= y_end {
        return;
    }

    // Source pixels per device pixel along each image axis -> supersampling grid (1..=4).
    let dev_x = (t[0] * t[0] + t[1] * t[1]).sqrt().max(1e-9);
    let dev_y = (t[2] * t[2] + t[3] * t[3]).sqrt().max(1e-9);
    let ratio = (f64::from(image.width) / dev_x).max(f64::from(image.height) / dev_y);
    let n = (ratio.ceil().max(1.0) as u32).min(4);
    let inv_n2 = 1.0 / f64::from(n * n);

    for py in y_start..y_end {
        for px in x_start..x_end {
            let mut sum = [0f32; 3];
            let mut sum_a = 0f32;
            for sy in 0..n {
                for sx in 0..n {
                    let dx = f64::from(px) + (f64::from(sx) + 0.5) / f64::from(n) - t[4];
                    let dy = f64::from(py) + (f64::from(sy) + 0.5) / f64::from(n) - t[5];
                    let x = (t[3] * dx - t[2] * dy) / det;
                    let y = (t[0] * dy - t[1] * dx) / det;
                    if !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y) {
                        continue;
                    }
                    let (color, mut a) = image.sample(x, 1.0 - y, fill);
                    if let Some((sw, sh, alpha)) = &image.alpha {
                        let i = ((x * f64::from(*sw)) as usize).min(*sw as usize - 1);
                        let j = (((1.0 - y) * f64::from(*sh)) as usize).min(*sh as usize - 1);
                        a *= f32::from(alpha[j * *sw as usize + i]) / 255.0;
                    }
                    for (acc, v) in sum.iter_mut().zip(color) {
                        *acc += v * a;
                    }
                    sum_a += a;
                }
            }
            if sum_a > 0.0 {
                let rgb = [sum[0] / sum_a, sum[1] / sum_a, sum[2] / sum_a];
                canvas.blend(px, py, rgb, (f64::from(sum_a) * inv_n2) as f32);
            }
        }
    }
}

// -------------------------------------------------------------------------------------------
// Image decoding
// -------------------------------------------------------------------------------------------

enum Plane {
    Gray(Vec<u8>),
    Rgb(Vec<u8>),
    /// Stencil: 255 where the fill colour is painted.
    Mask(Vec<u8>),
}

struct Decoded {
    width: u32,
    height: u32,
    plane: Plane,
    /// Soft mask: width, height, 8-bit alpha.
    alpha: Option<(u32, u32, Vec<u8>)>,
}

impl Decoded {
    /// Colour and alpha at unit-square position `(x, v)` with `v` measured from the image top.
    fn sample(&self, x: f64, v: f64, fill: [u8; 3]) -> ([f32; 3], f32) {
        let i = ((x * f64::from(self.width)) as usize).min(self.width as usize - 1);
        let j = ((v * f64::from(self.height)) as usize).min(self.height as usize - 1);
        let at = j * self.width as usize + i;
        match &self.plane {
            Plane::Gray(p) => {
                let g = f32::from(p[at]);
                ([g, g, g], 1.0)
            }
            Plane::Rgb(p) => (
                [
                    f32::from(p[at * 3]),
                    f32::from(p[at * 3 + 1]),
                    f32::from(p[at * 3 + 2]),
                ],
                1.0,
            ),
            Plane::Mask(p) => (
                [f32::from(fill[0]), f32::from(fill[1]), f32::from(fill[2])],
                f32::from(p[at]) / 255.0,
            ),
        }
    }
}

#[derive(Clone)]
enum Cs {
    Gray,
    Rgb,
    Cmyk,
    Indexed {
        base: Box<Cs>,
        hival: usize,
        table: Vec<u8>,
    },
}

impl Cs {
    fn ncomp(&self) -> usize {
        match self {
            Cs::Gray | Cs::Indexed { .. } => 1,
            Cs::Rgb => 3,
            Cs::Cmyk => 4,
        }
    }

    /// `comps` are 0..=1 values.
    fn to_rgb(&self, comps: &[f64]) -> [u8; 3] {
        use crate::scan::unit_to_u8 as u8_of;
        match self {
            Cs::Gray => {
                let v = u8_of(comps[0]);
                [v, v, v]
            }
            Cs::Rgb => [u8_of(comps[0]), u8_of(comps[1]), u8_of(comps[2])],
            Cs::Cmyk => cmyk_to_rgb(comps[0], comps[1], comps[2], comps[3]),
            Cs::Indexed { .. } => [0, 0, 0],
        }
    }
}

fn dict_int(doc: &Document, dict: &Dictionary, keys: &[&[u8]]) -> Option<i64> {
    keys.iter().find_map(|k| {
        dict.get(k)
            .ok()
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_i64().ok())
    })
}

fn name_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn unsupported(what: impl Into<String>) -> RasterError {
    RasterError::UnsupportedImage(what.into())
}

fn parse_cs(
    doc: &Document,
    obj: &Object,
    resources: Option<&Dictionary>,
    depth: usize,
) -> Result<Cs, RasterError> {
    if depth > 4 {
        return Err(unsupported("colour space nesting"));
    }
    let obj = deref_obj(doc, obj).ok_or_else(|| unsupported("colour space"))?;
    match obj {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Ok(Cs::Gray),
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Ok(Cs::Rgb),
            b"DeviceCMYK" | b"CMYK" => Ok(Cs::Cmyk),
            other => {
                let named = resources
                    .and_then(|r| r.get(b"ColorSpace").ok())
                    .and_then(|o| deref_obj(doc, o))
                    .and_then(|o| o.as_dict().ok())
                    .and_then(|d| d.get(other).ok());
                match named {
                    Some(o) => parse_cs(doc, o, resources, depth + 1),
                    None => Err(unsupported(format!("colour space {}", name_of(other)))),
                }
            }
        },
        Object::Array(items) => {
            let head = items
                .first()
                .and_then(|o| deref_obj(doc, o))
                .and_then(|o| o.as_name().ok())
                .ok_or_else(|| unsupported("colour space array"))?;
            match head {
                b"CalGray" => Ok(Cs::Gray),
                b"CalRGB" => Ok(Cs::Rgb),
                b"ICCBased" => {
                    let n = items
                        .get(1)
                        .and_then(|o| deref_obj(doc, o))
                        .and_then(|o| o.as_stream().ok())
                        .and_then(|s| dict_int(doc, &s.dict, &[b"N"]))
                        .ok_or_else(|| unsupported("ICC profile"))?;
                    match n {
                        1 => Ok(Cs::Gray),
                        3 => Ok(Cs::Rgb),
                        4 => Ok(Cs::Cmyk),
                        _ => Err(unsupported("ICC component count")),
                    }
                }
                b"Indexed" | b"I" => {
                    if items.len() < 4 {
                        return Err(unsupported("Indexed colour space"));
                    }
                    let base = parse_cs(doc, &items[1], resources, depth + 1)?;
                    if matches!(base, Cs::Indexed { .. }) {
                        return Err(unsupported("nested Indexed colour space"));
                    }
                    let hival = deref_obj(doc, &items[2])
                        .and_then(|o| o.as_i64().ok())
                        .filter(|v| (0..=255).contains(v))
                        .ok_or_else(|| unsupported("Indexed hival"))?
                        as usize;
                    let table = match deref_obj(doc, &items[3]) {
                        Some(Object::String(bytes, _)) => bytes.clone(),
                        Some(Object::Stream(s)) => s
                            .decompressed_content_with_limit(1 << 20)
                            .map_err(|_| unsupported("Indexed lookup table"))?,
                        _ => return Err(unsupported("Indexed lookup table")),
                    };
                    Ok(Cs::Indexed {
                        base: Box::new(base),
                        hival,
                        table,
                    })
                }
                other => Err(unsupported(format!("colour space {}", name_of(other)))),
            }
        }
        _ => Err(unsupported("colour space")),
    }
}

fn sample_at(row: &[u8], i: usize, bpc: u8) -> u32 {
    match bpc {
        8 => u32::from(row[i]),
        16 => (u32::from(row[2 * i]) << 8) | u32::from(row[2 * i + 1]),
        _ => {
            let bit = i * usize::from(bpc);
            let shift = 8 - usize::from(bpc) - (bit % 8);
            (u32::from(row[bit / 8]) >> shift) & ((1u32 << bpc) - 1)
        }
    }
}

fn decode_image(
    doc: &Document,
    id: ObjectId,
    resources: Option<&Dictionary>,
    allow_smask: bool,
) -> Result<Decoded, RasterError> {
    let stream = doc
        .get_object(id)
        .ok()
        .and_then(|o| o.as_stream().ok())
        .ok_or(RasterError::Malformed)?;
    let dict = &stream.dict;
    let width = dict_int(doc, dict, &[b"Width", b"W"]).ok_or(RasterError::Malformed)?;
    let height = dict_int(doc, dict, &[b"Height", b"H"]).ok_or(RasterError::Malformed)?;
    if width <= 0 || height <= 0 {
        return Err(RasterError::Malformed);
    }
    let (width, height) = (width as u64, height as u64);
    if width.saturating_mul(height) > MAX_IMAGE_PIXELS {
        return Err(RasterError::ImageTooLarge);
    }
    let (w, h) = (width as u32, height as u32);

    let is_mask = matches!(
        dict.get(b"ImageMask").ok().and_then(|o| deref_obj(doc, o)),
        Some(Object::Boolean(true))
    );
    let decode_array: Option<Vec<f64>> = dict
        .get(b"Decode")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_array().ok())
        .map(|a| a.iter().filter_map(object_number).collect());

    // Filters: only these are decoded; anything else is a typed refusal.
    let filters: Vec<&[u8]> = stream.filters().unwrap_or_default();
    for (i, f) in filters.iter().enumerate() {
        let ok = match *f {
            b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode" => true,
            b"DCTDecode" => i + 1 == filters.len(),
            _ => false,
        };
        if !ok {
            return Err(unsupported(format!("filter {}", name_of(f))));
        }
    }

    let plane = if filters.last() == Some(&&b"DCTDecode"[..]) {
        let data = if filters.len() == 1 {
            stream.content.clone()
        } else {
            let mut prefix = stream.clone();
            let names = filters[..filters.len() - 1]
                .iter()
                .map(|n| Object::Name(n.to_vec()))
                .collect();
            prefix.dict.set("Filter", Object::Array(names));
            prefix.dict.remove(b"DecodeParms");
            prefix
                .decompressed_content_with_limit(MAX_IMAGE_BYTES)
                .map_err(|e| RasterError::Decode(e.to_string()))?
        };
        decode_jpeg(&data, w, h)?
    } else {
        let raw = stream
            .decompressed_content_with_limit(MAX_IMAGE_BYTES)
            .map_err(|e| RasterError::Decode(e.to_string()))?;
        decode_raw(
            doc,
            dict,
            resources,
            &raw,
            w,
            h,
            is_mask,
            decode_array.as_deref(),
        )?
    };

    let mut alpha = None;
    if allow_smask && !is_mask {
        if let Ok(Object::Reference(sm)) = dict.get(b"SMask") {
            if let Ok(Decoded {
                width: sw,
                height: sh,
                plane: Plane::Gray(a),
                ..
            }) = decode_image(doc, *sm, resources, false)
            {
                alpha = Some((sw, sh, a));
            }
        }
    }
    Ok(Decoded {
        width: w,
        height: h,
        plane,
        alpha,
    })
}

// `Limits` is `#[non_exhaustive]`, so fields are set after `default()`.
#[allow(clippy::field_reassign_with_default)]
fn decode_jpeg(data: &[u8], w: u32, h: u32) -> Result<Plane, RasterError> {
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(data), image::ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(w);
    limits.max_image_height = Some(h);
    limits.max_alloc = Some(MAX_IMAGE_BYTES as u64);
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| RasterError::Decode(e.to_string()))?;
    if img.width() != w || img.height() != h {
        return Err(RasterError::Decode(
            "JPEG size differs from the image dictionary".into(),
        ));
    }
    Ok(match img.color() {
        image::ColorType::L8
        | image::ColorType::L16
        | image::ColorType::La8
        | image::ColorType::La16 => Plane::Gray(img.into_luma8().into_raw()),
        _ => Plane::Rgb(img.into_rgb8().into_raw()),
    })
}

#[allow(clippy::too_many_arguments)]
fn decode_raw(
    doc: &Document,
    dict: &Dictionary,
    resources: Option<&Dictionary>,
    raw: &[u8],
    w: u32,
    h: u32,
    is_mask: bool,
    decode: Option<&[f64]>,
) -> Result<Plane, RasterError> {
    let (cs, bpc) = if is_mask {
        (Cs::Gray, 1u8)
    } else {
        let cs_obj = dict
            .get(b"ColorSpace")
            .or_else(|_| dict.get(b"CS"))
            .map_err(|_| unsupported("image without a colour space"))?;
        let bpc = dict_int(doc, dict, &[b"BitsPerComponent", b"BPC"]).unwrap_or(8);
        if !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
            return Err(unsupported(format!("{bpc} bits per component")));
        }
        (parse_cs(doc, cs_obj, resources, 0)?, bpc as u8)
    };
    let ncomp = cs.ncomp();
    let (w, h) = (w as usize, h as usize);
    let row_bytes = (w * ncomp * usize::from(bpc)).div_ceil(8);
    if raw.len() < row_bytes * h {
        return Err(RasterError::Decode(
            "image data is shorter than its size".into(),
        ));
    }
    let max_v = f64::from((1u32 << bpc) - 1);
    let dec = |c: usize, s: u32| -> f64 {
        match decode {
            Some(d) if d.len() >= 2 * ncomp => {
                d[2 * c] + f64::from(s) * (d[2 * c + 1] - d[2 * c]) / max_v
            }
            _ => f64::from(s) / max_v,
        }
    };

    if is_mask {
        let inverted = decode.is_some_and(|d| d.first().copied() == Some(1.0));
        let mut out = vec![0u8; w * h];
        for y in 0..h {
            let row = &raw[y * row_bytes..(y + 1) * row_bytes];
            for x in 0..w {
                let painted = (sample_at(row, x, 1) == 0) != inverted;
                out[y * w + x] = if painted { 255 } else { 0 };
            }
        }
        return Ok(Plane::Mask(out));
    }

    if matches!(cs, Cs::Gray) {
        let mut out = vec![0u8; w * h];
        for y in 0..h {
            let row = &raw[y * row_bytes..(y + 1) * row_bytes];
            for x in 0..w {
                out[y * w + x] = crate::scan::unit_to_u8(dec(0, sample_at(row, x, bpc)));
            }
        }
        return Ok(Plane::Gray(out));
    }

    let mut out = vec![0u8; w * h * 3];
    let mut comps = [0f64; 4];
    for y in 0..h {
        let row = &raw[y * row_bytes..(y + 1) * row_bytes];
        for x in 0..w {
            let rgb = match &cs {
                Cs::Indexed { base, hival, table } => {
                    let idx = (sample_at(row, x, bpc) as usize).min(*hival);
                    let n = base.ncomp();
                    let entry = table.get(idx * n..idx * n + n);
                    match entry {
                        Some(e) => {
                            for (c, v) in e.iter().enumerate() {
                                comps[c] = f64::from(*v) / 255.0;
                            }
                            base.to_rgb(&comps)
                        }
                        None => [0, 0, 0],
                    }
                }
                _ => {
                    for c in 0..ncomp {
                        comps[c] = dec(c, sample_at(row, x * ncomp + c, bpc));
                    }
                    cs.to_rgb(&comps)
                }
            };
            out[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&rgb);
        }
    }
    Ok(Plane::Rgb(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::{gray_2x2, scan_doc};
    use lopdf::{dictionary, Stream};

    fn opts(dpi: f32, format: PixelFormat) -> RasterOptions {
        RasterOptions {
            dpi,
            max_pixels: 1_000_000,
            format,
        }
    }

    const FULL: &[u8] = b"q 72 0 0 72 0 0 cm /Im0 Do Q";

    #[test]
    fn the_image_lands_upright_at_72_dpi() {
        let doc = scan_doc(FULL, gray_2x2(), vec![]);
        let r = rasterize_page_doc(&doc, 1, &opts(72.0, PixelFormat::Gray8)).unwrap();
        assert_eq!((r.width, r.height), (72, 72));
        assert!(r.is_faithful());
        let at = |x: usize, y: usize| r.pixels[y * 72 + x];
        // Image rows: [0 255] over [255 0], top row first.
        assert_eq!(at(0, 0), 0);
        assert_eq!(at(71, 0), 255);
        assert_eq!(at(0, 71), 255);
        assert_eq!(at(71, 71), 0);
    }

    #[test]
    fn dpi_sets_the_output_size() {
        let doc = scan_doc(FULL, gray_2x2(), vec![]);
        let r = rasterize_page_doc(&doc, 1, &opts(144.0, PixelFormat::Gray8)).unwrap();
        assert_eq!((r.width, r.height), (144, 144));
        assert_eq!(r.pixels.len(), 144 * 144);
    }

    #[test]
    fn rotate_90_turns_the_page_clockwise() {
        let doc = scan_doc(FULL, gray_2x2(), vec![("Rotate", Object::Integer(90))]);
        let r = rasterize_page_doc(&doc, 1, &opts(72.0, PixelFormat::Gray8)).unwrap();
        let at = |x: usize, y: usize| r.pixels[y * 72 + x];
        assert_eq!(at(0, 0), 255);
        assert_eq!(at(71, 0), 0);
        assert_eq!(at(0, 71), 0);
        assert_eq!(at(71, 71), 255);
    }

    #[test]
    fn rgba_output_is_opaque_and_four_bytes_a_pixel() {
        let doc = scan_doc(FULL, gray_2x2(), vec![]);
        let r = rasterize_page_doc(&doc, 1, &opts(72.0, PixelFormat::Rgba8)).unwrap();
        assert_eq!(r.pixels.len(), 72 * 72 * 4);
        assert_eq!(&r.pixels[0..4], &[0, 0, 0, 255]);
        assert_eq!(&r.pixels[71 * 4..71 * 4 + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn a_page_over_the_pixel_limit_is_refused_before_allocation() {
        let doc = scan_doc(FULL, gray_2x2(), vec![]);
        let o = RasterOptions {
            dpi: 300.0,
            max_pixels: 1000,
            format: PixelFormat::Gray8,
        };
        assert!(matches!(
            rasterize_page_doc(&doc, 1, &o),
            Err(RasterError::TooLarge { .. })
        ));
    }

    #[test]
    fn a_bad_dpi_or_page_is_refused() {
        let doc = scan_doc(FULL, gray_2x2(), vec![]);
        assert!(matches!(
            rasterize_page_doc(&doc, 1, &opts(0.0, PixelFormat::Gray8)),
            Err(RasterError::InvalidDpi(_))
        ));
        assert!(matches!(
            rasterize_page_doc(&doc, 1, &opts(f32::NAN, PixelFormat::Gray8)),
            Err(RasterError::InvalidDpi(_))
        ));
        assert!(matches!(
            rasterize_page_doc(&doc, 2, &opts(72.0, PixelFormat::Gray8)),
            Err(RasterError::Malformed)
        ));
    }

    #[test]
    fn visible_text_is_reported_as_a_limitation() {
        let doc = scan_doc(
            b"q 72 0 0 72 0 0 cm /Im0 Do Q BT 0 0 Td (x) Tj ET",
            gray_2x2(),
            vec![],
        );
        let r = rasterize_page_doc(&doc, 1, &opts(72.0, PixelFormat::Gray8)).unwrap();
        assert_eq!(r.limitations, vec![RasterLimitation::VisibleText]);
    }

    #[test]
    fn an_unsupported_filter_is_a_typed_error() {
        let image = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 8, "Height" => 8,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 1,
                "Filter" => "CCITTFaxDecode",
            },
            vec![0; 8],
        );
        let doc = scan_doc(FULL, image, vec![]);
        assert!(matches!(
            rasterize_page_doc(&doc, 1, &opts(72.0, PixelFormat::Gray8)),
            Err(RasterError::UnsupportedImage(_))
        ));
    }

    #[test]
    fn a_one_bit_image_and_a_decode_array_invert() {
        // 8x1, bits 10101010 -> pixels white,black,... ; Decode [1 0] swaps them.
        let image = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 8, "Height" => 1,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 1,
                "Decode" => vec![1.into(), 0.into()],
            },
            vec![0b1010_1010],
        );
        let doc = scan_doc(FULL, image, vec![]);
        let r = rasterize_page_doc(&doc, 1, &opts(72.0, PixelFormat::Gray8)).unwrap();
        // First source pixel is bit 1; inverted by Decode it is black. Image column 0 covers x 0..9.
        assert_eq!(r.pixels[0], 0);
        assert_eq!(r.pixels[9], 255);
    }

    #[test]
    fn a_jpeg_image_decodes() {
        use image::{ExtendedColorType, ImageEncoder};
        let gray = vec![128u8; 8 * 8];
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100)
            .write_image(&gray, 8, 8, ExtendedColorType::L8)
            .unwrap();
        let image = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 8, "Height" => 8,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            },
            jpeg,
        );
        let doc = scan_doc(FULL, image, vec![]);
        let r = rasterize_page_doc(&doc, 1, &opts(72.0, PixelFormat::Gray8)).unwrap();
        for p in [r.pixels[0], r.pixels[36 * 72 + 36], r.pixels[71 * 72 + 71]] {
            assert!((i16::from(p) - 128).abs() <= 3, "got {p}");
        }
    }

    #[test]
    fn png_output_has_the_png_signature_and_round_trips() {
        let mut doc = scan_doc(FULL, gray_2x2(), vec![]);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let png = rasterize_page_png(&bytes, 1, &opts(72.0, PixelFormat::Gray8)).unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let back = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .into_luma8();
        assert_eq!((back.width(), back.height()), (72, 72));
        assert_eq!(back.get_pixel(0, 0).0[0], 0);
    }

    #[test]
    fn rasterize_scanned_pages_skips_text_pages() {
        let mut doc = scan_doc(FULL, gray_2x2(), vec![]);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let pages = rasterize_scanned_pages(&bytes, &opts(72.0, PixelFormat::Gray8)).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].page, 1);
    }
}
