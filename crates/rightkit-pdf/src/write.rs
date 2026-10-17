//! Document writer: a NEW PDF, one full-page JPEG per page, optional embedded ICC profile.
//!
//! The editing half of this crate changes existing files; this module creates one from
//! nothing, for print, slideshow and book exports that place a JPEG on every page.
//!
//! ```rust,ignore
//! let mut doc = Document::new("1.4")?;
//! let srgb = doc.add_icc_profile(srgb_profile_bytes, 3)?;
//! doc.add_jpeg_page(595.0, 842.0, jpeg_bytes, Some(srgb))?;
//! let pdf: Vec<u8> = doc.finish();
//! doc.write_to(path)?; // feature `edit`: rightkit-fs atomic replace
//! ```
//!
//! ## What is written
//!
//! * The header is `%PDF-<version>` (1.0 to 1.7) plus the binary-marker comment, then a classic
//!   cross-reference table (no object or xref streams), so a PDF 1.4 reader accepts it.
//! * Per page: `/Page` with `/MediaBox [0 0 w h]`, a content stream
//!   `q w 0 0 h 0 0 cm /Im0 Do Q`, and an image XObject with `/Filter /DCTDecode`,
//!   `/BitsPerComponent 8`. The JPEG bytes are copied unchanged (no re-encoding).
//! * Colour space: `/DeviceRGB` (3 components) or `/DeviceGray` (1), or, when an ICC profile is
//!   chosen, `[/ICCBased n 0 R]` with the profile as a stream (`/N`, `/Alternate`). A profile is
//!   written once however many pages use it.
//! * Deterministic: object numbers follow insertion order, no dates, no `/Info`, and the trailer
//!   `/ID` is the SHA-256 of the body. The same calls give the same bytes.
//!
//! ## Limits
//!
//! * JPEG: baseline, extended-sequential or progressive Huffman (SOF0, SOF1, SOF2), 8 bits per
//!   component, 1 or 3 components. 4-component (CMYK/YCCK, whose Adobe inversion needs a
//!   `/Decode` array), 12-bit, lossless and arithmetic-coded files are refused, not guessed.
//!   The ICC profile's component count must equal the JPEG's.
//! * The ICC profile is checked for the `acsp` signature at byte 36 and a header of at least 128
//!   bytes, not for validity. Streams are stored uncompressed (this module has no deflate).
//! * One image per page, full page, no rotation or crop: the image is stretched to the media
//!   box, so pass the JPEG's own aspect ratio.
//! * Vector text with an embedded subset font (Identity-H CID with ToUnicode) is not here yet;
//!   `font::subset_for_pdf` and `font::build_to_unicode_cmap` are the pieces for it.
//! * The PDF is built in memory; `lopdf` is used by the tests to read it back.

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Longest page side accepted, in points (the PDF 1.4 implementation limit, 200 inches).
pub const MAX_PAGE_SIDE_PT: f64 = 14_400.0;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WriteError {
    #[error("unsupported PDF version {0:?}: use 1.0 to 1.7")]
    InvalidVersion(String),
    #[error("page size {width} x {height} pt is not a finite size between 0 and 14400 pt")]
    InvalidPageSize { width: String, height: String },
    #[error("not a JPEG: {0}")]
    NotJpeg(&'static str),
    #[error("unsupported JPEG: {0}")]
    UnsupportedJpeg(String),
    #[error("ICC profile is not usable: {0}")]
    InvalidIccProfile(&'static str),
    #[error("ICC profile has {profile} components but the JPEG has {jpeg}")]
    IccComponentMismatch { profile: u8, jpeg: u8 },
    #[error("ICCBased colour needs PDF 1.3 or later, this document is {0}")]
    IccNeedsNewerVersion(String),
    #[error("no such page or ICC profile")]
    UnknownHandle,
    #[error("page {0} already has an image")]
    PageHasImage(usize),
}

/// Handle to an ICC profile added with [`Document::add_icc_profile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IccId(usize);

/// Handle to a page added with [`Document::add_page`] (its zero-based index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageId(usize);

impl PageId {
    pub fn index(self) -> usize {
        self.0
    }
}

/// What a JPEG header says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegInfo {
    pub width: u32,
    pub height: u32,
    /// 1 (gray) or 3 (RGB or YCbCr).
    pub components: u8,
}

impl JpegInfo {
    /// Read the frame header (SOF) of `data`, checking it is a JPEG this writer can place.
    pub fn parse(data: &[u8]) -> Result<JpegInfo, WriteError> {
        if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
            return Err(WriteError::NotJpeg("missing SOI marker"));
        }
        let mut i = 2usize;
        loop {
            // Skip to the next marker (fill bytes are 0xFF).
            while i < data.len() && data[i] != 0xFF {
                i += 1;
            }
            while i < data.len() && data[i] == 0xFF {
                i += 1;
            }
            let Some(&marker) = data.get(i) else {
                return Err(WriteError::NotJpeg("no frame header before the end"));
            };
            i += 1;
            match marker {
                0x00 | 0x01 | 0xD0..=0xD8 => continue,
                0xD9 => return Err(WriteError::NotJpeg("no frame header before EOI")),
                0xDA => return Err(WriteError::NotJpeg("scan before frame header")),
                _ => {}
            }
            let len = match data.get(i..i + 2) {
                Some(b) => u16::from_be_bytes([b[0], b[1]]) as usize,
                None => return Err(WriteError::NotJpeg("truncated segment")),
            };
            if len < 2 {
                return Err(WriteError::NotJpeg("bad segment length"));
            }
            let is_sof = (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker);
            if !is_sof {
                i += len;
                continue;
            }
            if !matches!(marker, 0xC0..=0xC2) {
                return Err(WriteError::UnsupportedJpeg(format!(
                    "SOF{} (only baseline, extended-sequential and progressive Huffman)",
                    marker - 0xC0
                )));
            }
            let seg = data
                .get(i + 2..i + len)
                .filter(|s| s.len() >= 6)
                .ok_or(WriteError::NotJpeg("truncated frame header"))?;
            let precision = seg[0];
            let height = u32::from(u16::from_be_bytes([seg[1], seg[2]]));
            let width = u32::from(u16::from_be_bytes([seg[3], seg[4]]));
            let components = seg[5];
            if precision != 8 {
                return Err(WriteError::UnsupportedJpeg(format!(
                    "{precision} bits per component (8 only)"
                )));
            }
            if width == 0 || height == 0 {
                return Err(WriteError::UnsupportedJpeg("zero width or height".into()));
            }
            if components != 1 && components != 3 {
                return Err(WriteError::UnsupportedJpeg(format!(
                    "{components} components (1 or 3 only)"
                )));
            }
            return Ok(JpegInfo {
                width,
                height,
                components,
            });
        }
    }
}

struct Icc {
    data: Vec<u8>,
    components: u8,
}

struct Placed {
    info: JpegInfo,
    data: Vec<u8>,
    icc: Option<usize>,
}

struct Page {
    width: f64,
    height: f64,
    image: Option<Placed>,
}

/// A PDF being built. See the module documentation.
pub struct Document {
    minor: u8,
    version: String,
    icc: Vec<Icc>,
    pages: Vec<Page>,
}

impl Document {
    /// Start a document. `version` is `"1.0"` to `"1.7"` (`"1.4"` for the widest readers).
    pub fn new(version: &str) -> Result<Document, WriteError> {
        let minor = match version.as_bytes() {
            [b'1', b'.', d @ b'0'..=b'7'] => d - b'0',
            _ => return Err(WriteError::InvalidVersion(version.to_string())),
        };
        Ok(Document {
            minor,
            version: version.to_string(),
            icc: Vec::new(),
            pages: Vec::new(),
        })
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Register an ICC profile (`components` 1 or 3) to colour-tag JPEGs. Needs PDF 1.3+.
    pub fn add_icc_profile(&mut self, profile: Vec<u8>, components: u8) -> Result<IccId, WriteError> {
        if self.minor < 3 {
            return Err(WriteError::IccNeedsNewerVersion(self.version.clone()));
        }
        if components != 1 && components != 3 {
            return Err(WriteError::InvalidIccProfile("1 or 3 components only"));
        }
        if profile.len() < 128 || &profile[36..40] != b"acsp" {
            return Err(WriteError::InvalidIccProfile(
                "no 128-byte header with the acsp signature",
            ));
        }
        self.icc.push(Icc {
            data: profile,
            components,
        });
        Ok(IccId(self.icc.len() - 1))
    }

    /// Append an empty page of `width_pt` x `height_pt` (1/72 inch).
    pub fn add_page(&mut self, width_pt: f64, height_pt: f64) -> Result<PageId, WriteError> {
        let ok = |v: f64| v.is_finite() && v > 0.0 && v <= MAX_PAGE_SIDE_PT;
        if !ok(width_pt) || !ok(height_pt) {
            return Err(WriteError::InvalidPageSize {
                width: width_pt.to_string(),
                height: height_pt.to_string(),
            });
        }
        self.pages.push(Page {
            width: width_pt,
            height: height_pt,
            image: None,
        });
        Ok(PageId(self.pages.len() - 1))
    }

    /// Draw `jpeg` over the whole of `page` (`q w 0 0 h 0 0 cm /Im0 Do Q`). `icc` tags it with a
    /// profile whose component count equals the JPEG's; `None` uses DeviceRGB or DeviceGray.
    pub fn place_jpeg(
        &mut self,
        page: PageId,
        jpeg: Vec<u8>,
        icc: Option<IccId>,
    ) -> Result<JpegInfo, WriteError> {
        let info = JpegInfo::parse(&jpeg)?;
        if let Some(IccId(k)) = icc {
            let profile = self.icc.get(k).ok_or(WriteError::UnknownHandle)?;
            if profile.components != info.components {
                return Err(WriteError::IccComponentMismatch {
                    profile: profile.components,
                    jpeg: info.components,
                });
            }
        }
        let slot = self.pages.get_mut(page.0).ok_or(WriteError::UnknownHandle)?;
        if slot.image.is_some() {
            return Err(WriteError::PageHasImage(page.0));
        }
        slot.image = Some(Placed {
            info,
            data: jpeg,
            icc: icc.map(|i| i.0),
        });
        Ok(info)
    }

    /// [`Self::add_page`] then [`Self::place_jpeg`]. On an error no page is added.
    pub fn add_jpeg_page(
        &mut self,
        width_pt: f64,
        height_pt: f64,
        jpeg: Vec<u8>,
        icc: Option<IccId>,
    ) -> Result<PageId, WriteError> {
        let before = self.pages.len();
        let page = self.add_page(width_pt, height_pt)?;
        if let Err(error) = self.place_jpeg(page, jpeg, icc) {
            self.pages.truncate(before);
            return Err(error);
        }
        Ok(page)
    }

    /// The finished file. A document with no pages has an empty page tree.
    pub fn finish(&self) -> Vec<u8> {
        // Object numbers: 1 catalog, 2 page tree, ICC streams, then per page its page object,
        // content stream and (when it has an image) image XObject.
        let icc_base = 3usize;
        let mut next = icc_base + self.icc.len();
        let mut ids = Vec::with_capacity(self.pages.len());
        for page in &self.pages {
            let page_id = next;
            let content_id = next + 1;
            let image_id = page.image.as_ref().map(|_| next + 2);
            next += if image_id.is_some() { 3 } else { 2 };
            ids.push((page_id, content_id, image_id));
        }
        let total = next; // objects 1..next-1, plus the free entry 0

        let mut objects: Vec<(usize, Vec<u8>)> = Vec::with_capacity(total);
        objects.push((1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()));
        let kids = ids
            .iter()
            .map(|(p, _, _)| format!("{p} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        objects.push((
            2,
            format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", self.pages.len()).into_bytes(),
        ));
        for (k, profile) in self.icc.iter().enumerate() {
            let alt = if profile.components == 1 {
                "/DeviceGray"
            } else {
                "/DeviceRGB"
            };
            objects.push((
                icc_base + k,
                stream_object(
                    &format!("/N {} /Alternate {alt}", profile.components),
                    &profile.data,
                ),
            ));
        }
        for (page, &(page_id, content_id, image_id)) in self.pages.iter().zip(&ids) {
            let (w, h) = (num(page.width), num(page.height));
            let resources = match (&page.image, image_id) {
                (Some(placed), Some(image_id)) => {
                    let procset = if placed.info.components == 1 {
                        "/ImageB"
                    } else {
                        "/ImageC"
                    };
                    format!(
                        "<< /XObject << /Im0 {image_id} 0 R >> /ProcSet [/PDF {procset}] >>"
                    )
                }
                _ => "<< /ProcSet [/PDF] >>".to_string(),
            };
            objects.push((
                page_id,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources {resources} /Contents {content_id} 0 R >>"
                )
                .into_bytes(),
            ));
            let content = match image_id {
                Some(_) => format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q\n"),
                None => String::new(),
            };
            objects.push((content_id, stream_object("", content.as_bytes())));
            if let (Some(placed), Some(image_id)) = (&page.image, image_id) {
                let colour = match placed.icc {
                    Some(k) => format!("[/ICCBased {} 0 R]", icc_base + k),
                    None if placed.info.components == 1 => "/DeviceGray".to_string(),
                    None => "/DeviceRGB".to_string(),
                };
                objects.push((
                    image_id,
                    stream_object(
                        &format!(
                            "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {colour} /BitsPerComponent 8 /Filter /DCTDecode",
                            placed.info.width, placed.info.height
                        ),
                        &placed.data,
                    ),
                ));
            }
        }

        let mut out = Vec::new();
        out.extend_from_slice(format!("%PDF-{}\n", self.version).as_bytes());
        out.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
        let mut offsets = vec![0usize; total];
        for (id, body) in &objects {
            offsets[*id] = out.len();
            out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let id_hex = hex(&Sha256::digest(&out)[..16]);
        let xref_at = out.len();
        out.extend_from_slice(format!("xref\n0 {total}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets[1..] {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {total} /Root 1 0 R /ID [<{id_hex}> <{id_hex}>] >>\nstartxref\n{xref_at}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    /// Write [`Self::finish`] to `path` through [`crate::commit::write_atomic`] (temp sibling,
    /// flush, rename, directory sync): the one documented way to put a new file on disk. Path
    /// authorization is the caller's job.
    #[cfg(feature = "edit")]
    pub fn write_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        crate::commit::write_atomic(path, &self.finish())
    }
}

fn stream_object(dict_entries: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict_entries} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A point value with at most three decimals, trailing zeros trimmed (`595`, `841.89`).
fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal baseline JPEG header: SOI, SOF0 (8-bit, `h` x `w`, `n` components), EOI.
    fn jpeg(w: u16, h: u16, n: u8) -> Vec<u8> {
        let mut d = vec![0xFF, 0xD8, 0xFF, 0xC0];
        let len = 8 + 3 * u16::from(n);
        d.extend_from_slice(&len.to_be_bytes());
        d.push(8);
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&w.to_be_bytes());
        d.push(n);
        for c in 0..n {
            d.extend_from_slice(&[c + 1, 0x11, 0]);
        }
        d.extend_from_slice(&[0xFF, 0xD9]);
        d
    }

    fn profile() -> Vec<u8> {
        let mut p = vec![0u8; 200];
        p[36..40].copy_from_slice(b"acsp");
        p
    }

    #[test]
    fn jpeg_header_is_read() {
        assert_eq!(
            JpegInfo::parse(&jpeg(640, 480, 3)).unwrap(),
            JpegInfo {
                width: 640,
                height: 480,
                components: 3
            }
        );
        assert_eq!(JpegInfo::parse(&jpeg(2, 3, 1)).unwrap().components, 1);
    }

    #[test]
    fn jpeg_refusals() {
        assert_eq!(
            JpegInfo::parse(b"GIF89a....").unwrap_err(),
            WriteError::NotJpeg("missing SOI marker")
        );
        assert!(matches!(
            JpegInfo::parse(&jpeg(8, 8, 4)),
            Err(WriteError::UnsupportedJpeg(_))
        ));
        let mut twelve = jpeg(8, 8, 3);
        twelve[6] = 12;
        assert!(matches!(
            JpegInfo::parse(&twelve),
            Err(WriteError::UnsupportedJpeg(_))
        ));
        let mut arithmetic = jpeg(8, 8, 3);
        arithmetic[3] = 0xC9;
        assert!(matches!(
            JpegInfo::parse(&arithmetic),
            Err(WriteError::UnsupportedJpeg(_))
        ));
        assert!(matches!(
            JpegInfo::parse(&[0xFF, 0xD8, 0xFF, 0xD9]),
            Err(WriteError::NotJpeg(_))
        ));
    }

    #[test]
    fn version_and_page_size_are_checked() {
        assert!(Document::new("1.4").is_ok());
        assert!(Document::new("2.0").is_err());
        assert!(Document::new("1.8").is_err());
        let mut doc = Document::new("1.4").unwrap();
        assert!(doc.add_page(0.0, 10.0).is_err());
        assert!(doc.add_page(f64::NAN, 10.0).is_err());
        assert!(doc.add_page(10.0, 20_000.0).is_err());
        assert_eq!(doc.page_count(), 0);
    }

    #[test]
    fn icc_rules() {
        let mut old = Document::new("1.2").unwrap();
        assert!(matches!(
            old.add_icc_profile(profile(), 3),
            Err(WriteError::IccNeedsNewerVersion(_))
        ));
        let mut doc = Document::new("1.4").unwrap();
        assert!(doc.add_icc_profile(vec![0; 10], 3).is_err());
        assert!(doc.add_icc_profile(profile(), 4).is_err());
        let rgb = doc.add_icc_profile(profile(), 3).unwrap();
        let error = doc
            .add_jpeg_page(100.0, 100.0, jpeg(4, 4, 1), Some(rgb))
            .unwrap_err();
        assert_eq!(
            error,
            WriteError::IccComponentMismatch {
                profile: 3,
                jpeg: 1
            }
        );
        assert_eq!(doc.page_count(), 0, "a refused page is not added");
    }

    #[test]
    fn one_image_per_page() {
        let mut doc = Document::new("1.4").unwrap();
        let page = doc.add_page(100.0, 100.0).unwrap();
        doc.place_jpeg(page, jpeg(4, 4, 3), None).unwrap();
        assert_eq!(
            doc.place_jpeg(page, jpeg(4, 4, 3), None).unwrap_err(),
            WriteError::PageHasImage(0)
        );
    }

    #[test]
    fn number_formatting() {
        assert_eq!(num(595.0), "595");
        assert_eq!(num(841.89), "841.89");
        assert_eq!(num(0.1 + 0.2), "0.3");
        assert_eq!(num(612.0004), "612");
    }

    #[test]
    fn output_has_header_pages_icc_and_content_stream() {
        let mut doc = Document::new("1.4").unwrap();
        let srgb = doc.add_icc_profile(profile(), 3).unwrap();
        doc.add_jpeg_page(595.0, 842.0, jpeg(10, 20, 3), Some(srgb))
            .unwrap();
        doc.add_jpeg_page(300.5, 200.0, jpeg(30, 20, 3), Some(srgb))
            .unwrap();
        doc.add_jpeg_page(100.0, 100.0, jpeg(5, 5, 1), None).unwrap();
        let bytes = doc.finish();
        assert!(bytes.starts_with(b"%PDF-1.4\n"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("q 595 0 0 842 0 0 cm /Im0 Do Q"));
        assert!(text.contains("q 300.5 0 0 200 0 0 cm /Im0 Do Q"));
        assert!(text.contains("/Filter /DCTDecode"));
        assert!(text.contains("[/ICCBased 3 0 R]"));
        assert!(text.contains("/ColorSpace /DeviceGray"));
        assert_eq!(text.matches("/N 3 /Alternate /DeviceRGB").count(), 1);

        let loaded = lopdf::Document::load_mem(&bytes).expect("lopdf reads the writer's output");
        assert_eq!(loaded.get_pages().len(), 3);
        let first = *loaded.get_pages().get(&1).unwrap();
        let page = loaded.get_dictionary(first).unwrap();
        let media = page.get(b"MediaBox").unwrap().as_array().unwrap();
        assert_eq!(media[2].as_i64().unwrap(), 595);
        let content = loaded.get_page_content(first).unwrap();
        assert_eq!(content, b"q 595 0 0 842 0 0 cm /Im0 Do Q\n");
        assert!(text.contains("/XObject << /Im0 "));
    }

    #[test]
    fn output_is_deterministic_and_empty_document_loads() {
        let build = || {
            let mut doc = Document::new("1.4").unwrap();
            let srgb = doc.add_icc_profile(profile(), 3).unwrap();
            doc.add_jpeg_page(595.0, 842.0, jpeg(10, 20, 3), Some(srgb))
                .unwrap();
            doc.finish()
        };
        assert_eq!(build(), build());
        let empty = Document::new("1.4").unwrap().finish();
        let loaded = lopdf::Document::load_mem(&empty).expect("empty page tree loads");
        assert_eq!(loaded.get_pages().len(), 0);
    }

    #[cfg(feature = "edit")]
    #[test]
    fn write_to_goes_through_the_atomic_writer() {
        let dir = std::env::temp_dir().join(format!("rightkit-pdf-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.pdf");
        let mut doc = Document::new("1.4").unwrap();
        doc.add_jpeg_page(100.0, 100.0, jpeg(4, 4, 3), None).unwrap();
        doc.write_to(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), doc.finish());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
