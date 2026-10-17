//! RightKit PDF engine: the pure, Tauri-free half of the PDF edit pipeline, extracted from
//! ViewRight's `crates/pdf` (the most mature donor), plus ScrapeRight text extraction and Legion
//! page merge as opt-in features.
//!
//! Every function takes bytes, paths, or in-memory `lopdf::Document`s and returns typed values.
//! Path authorization (the host's file broker) stays in the app; [`commit`] provides the atomic
//! disk write (through `rightkit-fs`) once the host has authorized a target.
//!
//! | module    | responsibility                                                    |
//! |-----------|-------------------------------------------------------------------|
//! | `model`   | wire types: commands, islands, locators, typed `PdfEditError`     |
//! | `inspect` | page graph walk: text islands with quads, fonts, source locators  |
//! | `font`    | bundled + system font catalog, subsetting, ToUnicode              |
//! | `compose` | sparse-write composition of edit commands onto fresh bytes        |
//! | `save`    | candidate verification, single-use save tokens, commit resolution |
//! | `commit`  | (feature `edit`) lock, recheck and rightkit-fs replace of a token |
//! | `merge`   | (feature `merge`) merge PDFs, select/reorder pages                |
//! | `organize`| (feature `merge`) delete, reorder, rotate, insert pages; keeps outline and labels |
//! | `extract` | (feature `extract`) text extraction, whole-document and per page  |
//! | `scan`    | scanned-page detection (no text layer, page-sized image)          |
//! | `raster`  | (feature `raster`) render scanned pages to gray/RGBA/PNG for OCR  |
//! | `redact`  | (feature `redact`) remove content under marks, fail closed        |
//! | `forms`   | (feature `forms`) AcroForm fields, set values, appearances        |
//! | `optimize`| (feature `optimize`) unused objects, measured image downsampling  |
//! | `write`   | build a new PDF: JPEG pages, ICC profiles, page sizes (`write_to` needs `edit`) |

#[cfg(feature = "edit")]
pub mod commit;
#[cfg(feature = "edit")]
pub mod compose;
#[cfg(feature = "extract")]
pub mod extract;
#[cfg(feature = "fonts")]
pub mod font;
#[cfg(feature = "forms")]
pub mod forms;
pub mod inspect;
#[cfg(feature = "merge")]
pub mod merge;
pub mod model;
#[cfg(feature = "optimize")]
pub mod optimize;
#[cfg(feature = "merge")]
pub mod organize;
#[cfg(feature = "raster")]
pub mod raster;
#[cfg(feature = "redact")]
pub mod redact;
#[cfg(feature = "edit")]
pub mod save;
pub mod scan;
#[cfg(test)]
mod test_fixtures;
pub mod write;
