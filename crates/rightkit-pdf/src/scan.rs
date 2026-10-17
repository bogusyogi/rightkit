//! Scanned-page detection and the shared page content walker (also used by `raster`).
//!
//! A scanned page paints (almost) one big image and shows no text. [`classify_page`] reads a page's
//! content under the same byte and token budget as inspection and reports what it found;
//! [`PageProfile::is_scanned`] is the "route this page to OCR" decision. No renderer is involved.

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::inspect::{
    deref_obj, load_pdf, object_number, page_content_stream_ids, resolve_inherited, resolve_rect,
    ContentBudget,
};

/// Errors of page classification and rasterization.
#[derive(Debug, thiserror::Error)]
pub enum RasterError {
    #[error("PDF is malformed, or the page does not exist")]
    Malformed,
    #[error("encrypted PDF cannot be rasterized")]
    Encrypted,
    #[error("dpi {0} is outside 1..=1200")]
    InvalidDpi(f32),
    #[error("page would be {width}x{height} pixels, over the limit of {max_pixels}")]
    TooLarge {
        width: u64,
        height: u64,
        max_pixels: u64,
    },
    #[error("unsupported image: {0}")]
    UnsupportedImage(String),
    #[error("image is too large to decode")]
    ImageTooLarge,
    #[error("image could not be decoded: {0}")]
    Decode(String),
    #[error("page content exceeds the walk limits")]
    TooMuchWork,
    #[error("PNG encoding failed: {0}")]
    Png(String),
}

/// PDF affine matrix `[a b c d e f]` (row-vector convention).
pub(crate) type Matrix6 = [f64; 6];

pub(crate) const IDENTITY: Matrix6 = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a` applied first, then `b`.
pub(crate) fn mul(a: &Matrix6, b: &Matrix6) -> Matrix6 {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

const MAX_FORM_DEPTH: usize = 8;
const MAX_XOBJECT_INVOCATIONS: usize = 4096;
const MAX_Q_DEPTH: usize = 1024;

/// What the walker found, in paint order.
pub(crate) enum Event<'a> {
    /// A show-text operator with a non-empty string. `visible` is false for render mode 3 / 7
    /// (the invisible OCR text layer of an already-recognised scan).
    Text { visible: bool },
    /// A path painting operator or a shading.
    Vector,
    /// `BI` inline image (not decoded).
    InlineImage,
    /// An image XObject painted with `ctm` (user space of the page, before the page matrix).
    // The fields are consumed by the raster walker only; without the `raster` feature the
    // classifier reads `ctm` alone, so rustc reports the rest as dead there (false positive).
    #[cfg_attr(not(feature = "raster"), allow(dead_code))]
    Image {
        id: ObjectId,
        ctm: Matrix6,
        fill: [u8; 3],
        resources: Option<&'a Dictionary>,
    },
}

#[derive(Clone)]
struct GState {
    ctm: Matrix6,
    fill: [u8; 3],
    tr: i64,
}

struct Walker<'a, 'f> {
    doc: &'a Document,
    budget: ContentBudget,
    invocations: usize,
    sink: &'f mut dyn FnMut(Event<'a>) -> Result<(), RasterError>,
}

/// Walks page `page_id`'s content (and the forms it paints), reporting events to `sink`.
pub(crate) fn walk_page<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    base: Matrix6,
    sink: &mut dyn FnMut(Event<'a>) -> Result<(), RasterError>,
) -> Result<(), RasterError> {
    let page_dict = doc
        .get_object(page_id)
        .ok()
        .and_then(|o| o.as_dict().ok())
        .ok_or(RasterError::Malformed)?;
    if page_dict.get(b"Contents").is_err() {
        return Ok(());
    }
    let resources = resolve_inherited(doc, page_dict, b"Resources")
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok());
    let ids = page_content_stream_ids(doc, page_id).map_err(|_| RasterError::Malformed)?;
    let mut walker = Walker {
        doc,
        budget: ContentBudget::new(),
        invocations: 0,
        sink,
    };
    // One logical stream: graphics state carries from entry to entry.
    let mut gs = GState {
        ctm: base,
        fill: [0, 0, 0],
        tr: 0,
    };
    let mut stack: Vec<GState> = Vec::new();
    for id in ids {
        let content = walker
            .budget
            .content_to_read(doc, id)
            .map_err(|_| RasterError::Malformed)?;
        walker.run(&content.operations, resources, &mut gs, &mut stack, 0)?;
    }
    Ok(())
}

impl<'a, 'f> Walker<'a, 'f> {
    fn run(
        &mut self,
        ops: &[lopdf::content::Operation],
        resources: Option<&'a Dictionary>,
        gs: &mut GState,
        stack: &mut Vec<GState>,
        depth: usize,
    ) -> Result<(), RasterError> {
        let nums = |operands: &[Object]| -> Vec<f64> {
            operands.iter().filter_map(object_number).collect()
        };
        for op in ops {
            match op.operator.as_str() {
                "q" => {
                    if stack.len() >= MAX_Q_DEPTH {
                        return Err(RasterError::TooMuchWork);
                    }
                    stack.push(gs.clone());
                }
                "Q" => {
                    if let Some(prev) = stack.pop() {
                        *gs = prev;
                    }
                }
                "cm" => {
                    let n = nums(&op.operands);
                    if n.len() == 6 && n.iter().all(|v| v.is_finite()) {
                        gs.ctm = mul(&[n[0], n[1], n[2], n[3], n[4], n[5]], &gs.ctm);
                    }
                }
                "g" => {
                    let n = nums(&op.operands);
                    if n.len() == 1 {
                        let v = unit_to_u8(n[0]);
                        gs.fill = [v, v, v];
                    }
                }
                "rg" => {
                    let n = nums(&op.operands);
                    if n.len() == 3 {
                        gs.fill = [unit_to_u8(n[0]), unit_to_u8(n[1]), unit_to_u8(n[2])];
                    }
                }
                "k" => {
                    let n = nums(&op.operands);
                    if n.len() == 4 {
                        gs.fill = cmyk_to_rgb(n[0], n[1], n[2], n[3]);
                    }
                }
                "Tr" => {
                    if let Some(v) = op.operands.first().and_then(|o| o.as_i64().ok()) {
                        gs.tr = v;
                    }
                }
                "Tj" | "'" => {
                    if op.operands.first().is_some_and(non_empty_string) {
                        self.text(gs)?;
                    }
                }
                "\"" => {
                    if op.operands.get(2).is_some_and(non_empty_string) {
                        self.text(gs)?;
                    }
                }
                "TJ" => {
                    let any = op
                        .operands
                        .first()
                        .and_then(|o| o.as_array().ok())
                        .is_some_and(|a| a.iter().any(non_empty_string));
                    if any {
                        self.text(gs)?;
                    }
                }
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "sh" => {
                    (self.sink)(Event::Vector)?;
                }
                "BI" => (self.sink)(Event::InlineImage)?,
                "Do" => {
                    self.invocations += 1;
                    if self.invocations > MAX_XOBJECT_INVOCATIONS {
                        return Err(RasterError::TooMuchWork);
                    }
                    self.do_xobject(op, resources, gs, depth)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn text(&mut self, gs: &GState) -> Result<(), RasterError> {
        let visible = gs.tr != 3 && gs.tr != 7;
        (self.sink)(Event::Text { visible })
    }

    fn do_xobject(
        &mut self,
        op: &lopdf::content::Operation,
        resources: Option<&'a Dictionary>,
        gs: &GState,
        depth: usize,
    ) -> Result<(), RasterError> {
        let doc = self.doc;
        let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
            return Ok(());
        };
        let Some(entry) = resources
            .and_then(|r| r.get(b"XObject").ok())
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_dict().ok())
            .and_then(|d| d.get(name).ok())
        else {
            return Ok(());
        };
        let Some(id) = (match entry {
            Object::Reference(id) => Some(*id),
            _ => None,
        }) else {
            return Ok(());
        };
        let Some(stream) = doc.get_object(id).ok().and_then(|o| o.as_stream().ok()) else {
            return Ok(());
        };
        let subtype = stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok())
            .unwrap_or(&b""[..]);
        match subtype {
            b"Image" => (self.sink)(Event::Image {
                id,
                ctm: gs.ctm,
                fill: gs.fill,
                resources,
            }),
            b"Form" => {
                if depth >= MAX_FORM_DEPTH {
                    return Err(RasterError::TooMuchWork);
                }
                let matrix = stream
                    .dict
                    .get(b"Matrix")
                    .ok()
                    .and_then(|o| deref_obj(doc, o))
                    .and_then(|o| o.as_array().ok())
                    .map(|a| a.iter().filter_map(object_number).collect::<Vec<_>>())
                    .filter(|n| n.len() == 6 && n.iter().all(|v| v.is_finite()))
                    .map(|n| [n[0], n[1], n[2], n[3], n[4], n[5]])
                    .unwrap_or(IDENTITY);
                let form_resources = stream
                    .dict
                    .get(b"Resources")
                    .ok()
                    .and_then(|o| deref_obj(doc, o))
                    .and_then(|o| o.as_dict().ok())
                    .or(resources);
                let content = self
                    .budget
                    .content_to_read(doc, id)
                    .map_err(|_| RasterError::Malformed)?;
                let mut inner = gs.clone();
                inner.ctm = mul(&matrix, &gs.ctm);
                let mut inner_stack = Vec::new();
                self.run(
                    &content.operations,
                    form_resources,
                    &mut inner,
                    &mut inner_stack,
                    depth + 1,
                )
            }
            _ => Ok(()),
        }
    }
}

fn non_empty_string(o: &Object) -> bool {
    matches!(o, Object::String(b, _) if !b.is_empty())
}

pub(crate) fn unit_to_u8(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub(crate) fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> [u8; 3] {
    let k = k.clamp(0.0, 1.0);
    let f = |v: f64| unit_to_u8((1.0 - v.clamp(0.0, 1.0)) * (1.0 - k));
    [f(c), f(m), f(y)]
}

/// What one page paints.
#[derive(Clone, Debug, PartialEq)]
pub struct PageProfile {
    /// 1-based page number.
    pub page: u32,
    /// The page shows at least one non-empty text string, visible or not (an already recognised
    /// scan carries an invisible text layer and counts as having text).
    pub has_text: bool,
    /// Text is shown in a visible render mode.
    pub has_visible_text: bool,
    /// Image XObjects painted (forms followed), inline images counted separately.
    pub image_count: u32,
    pub inline_image_count: u32,
    /// Path painting or shading operators were seen.
    pub has_vector_graphics: bool,
    /// Largest painted image area as a fraction of the visible page area, capped at 1.0.
    pub largest_image_coverage: f64,
}

impl PageProfile {
    /// Minimum [`PageProfile::largest_image_coverage`] for a page to count as a scan.
    pub const SCAN_COVERAGE: f64 = 0.5;

    /// True when the page has no text layer and an image covers at least half of it: the page
    /// needs OCR.
    pub fn is_scanned(&self) -> bool {
        !self.has_text && self.largest_image_coverage >= Self::SCAN_COVERAGE
    }
}

/// Visible page box (CropBox, else MediaBox, else Letter) width and height in points.
pub(crate) fn page_box(doc: &Document, page_id: ObjectId) -> [f64; 4] {
    let dict = doc.get_object(page_id).ok().and_then(|o| o.as_dict().ok());
    let Some(dict) = dict else {
        return [0.0, 0.0, 612.0, 792.0];
    };
    let m = resolve_rect(doc, dict, b"MediaBox").unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let r = resolve_rect(doc, dict, b"CropBox").unwrap_or(m);
    [
        r[0].min(r[2]),
        r[1].min(r[3]),
        r[0].max(r[2]),
        r[1].max(r[3]),
    ]
}

pub(crate) fn page_id_of(doc: &Document, page: u32) -> Result<ObjectId, RasterError> {
    if doc.is_encrypted() {
        return Err(RasterError::Encrypted);
    }
    doc.get_pages()
        .get(&page)
        .copied()
        .ok_or(RasterError::Malformed)
}

/// Profiles page `page` (1-based) of an already loaded document.
pub fn classify_page_doc(doc: &Document, page: u32) -> Result<PageProfile, RasterError> {
    let page_id = page_id_of(doc, page)?;
    let b = page_box(doc, page_id);
    let area = ((b[2] - b[0]) * (b[3] - b[1])).abs().max(1e-9);
    let mut profile = PageProfile {
        page,
        has_text: false,
        has_visible_text: false,
        image_count: 0,
        inline_image_count: 0,
        has_vector_graphics: false,
        largest_image_coverage: 0.0,
    };
    walk_page(doc, page_id, IDENTITY, &mut |event| {
        match event {
            Event::Text { visible } => {
                profile.has_text = true;
                profile.has_visible_text |= visible;
            }
            Event::Vector => profile.has_vector_graphics = true,
            Event::InlineImage => profile.inline_image_count += 1,
            Event::Image { ctm, .. } => {
                profile.image_count += 1;
                let det = (ctm[0] * ctm[3] - ctm[1] * ctm[2]).abs();
                if det.is_finite() {
                    profile.largest_image_coverage =
                        profile.largest_image_coverage.max((det / area).min(1.0));
                }
            }
        }
        Ok(())
    })?;
    Ok(profile)
}

/// Profiles page `page` (1-based) of PDF `bytes`.
pub fn classify_page(bytes: &[u8], page: u32) -> Result<PageProfile, RasterError> {
    let doc = load_pdf(bytes).map_err(|_| RasterError::Malformed)?;
    classify_page_doc(&doc, page)
}

/// 1-based numbers of the pages that [`PageProfile::is_scanned`]. A page whose content cannot be
/// read is skipped.
pub fn scanned_pages(bytes: &[u8]) -> Result<Vec<u32>, RasterError> {
    let doc = load_pdf(bytes).map_err(|_| RasterError::Malformed)?;
    if doc.is_encrypted() {
        return Err(RasterError::Encrypted);
    }
    let numbers: Vec<u32> = doc.get_pages().keys().copied().collect();
    Ok(numbers
        .into_iter()
        .filter(|n| {
            classify_page_doc(&doc, *n)
                .map(|p| p.is_scanned())
                .unwrap_or(false)
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_fixtures::{one_page_pdf, PageFixture};
    use lopdf::{dictionary, Stream};

    /// One 72x72 pt page painting one image XObject over the whole page.
    pub(crate) fn scan_doc(
        content: &[u8],
        image: Stream,
        extras: Vec<(&'static str, Object)>,
    ) -> Document {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(image);
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), 72.into(), 72.into()],
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image_id } },
        };
        for (k, v) in extras {
            page.set(k, v);
        }
        let page_id = doc.add_object(page);
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc
    }

    pub(crate) fn gray_2x2() -> Stream {
        Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 2,
                "Height" => 2,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0, 255, 255, 0],
        )
    }

    #[test]
    fn an_image_only_page_is_scanned() {
        let doc = scan_doc(b"q 72 0 0 72 0 0 cm /Im0 Do Q", gray_2x2(), vec![]);
        let p = classify_page_doc(&doc, 1).unwrap();
        assert_eq!(p.image_count, 1);
        assert!(!p.has_text);
        assert!(p.is_scanned());
    }

    #[test]
    fn a_small_image_does_not_make_a_scan() {
        let doc = scan_doc(b"q 10 0 0 10 0 0 cm /Im0 Do Q", gray_2x2(), vec![]);
        assert!(!classify_page_doc(&doc, 1).unwrap().is_scanned());
    }

    #[test]
    fn a_text_page_is_not_scanned() {
        let doc = one_page_pdf(PageFixture::for_content(
            b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET",
        ));
        let p = classify_page_doc(&doc, 1).unwrap();
        assert!(p.has_text && p.has_visible_text);
        assert!(!p.is_scanned());
    }

    #[test]
    fn invisible_text_over_an_image_counts_as_a_text_layer() {
        let doc = scan_doc(
            b"q 72 0 0 72 0 0 cm /Im0 Do Q BT 3 Tr 0 0 Td (hi) Tj ET",
            gray_2x2(),
            vec![],
        );
        let p = classify_page_doc(&doc, 1).unwrap();
        assert!(p.has_text && !p.has_visible_text);
        assert!(!p.is_scanned());
    }

    #[test]
    fn a_blank_page_is_not_scanned() {
        let doc = one_page_pdf(PageFixture::for_content(b""));
        let p = classify_page_doc(&doc, 1).unwrap();
        assert_eq!(p.image_count, 0);
        assert!(!p.is_scanned());
    }

    #[test]
    fn scanned_pages_reads_bytes() {
        let mut doc = scan_doc(b"q 72 0 0 72 0 0 cm /Im0 Do Q", gray_2x2(), vec![]);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        assert_eq!(scanned_pages(&bytes).unwrap(), vec![1]);
        assert!(classify_page(&bytes, 2).is_err());
    }
}
