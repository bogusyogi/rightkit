//! RightKit PDF engine: the pure, Tauri-free half of the PDF edit pipeline, extracted from
//! ViewRight's `crates/pdf` (the most mature donor), plus ScrapeRight text extraction and Legion
//! page merge as opt-in features.
//!
//! Every function takes bytes, paths, or in-memory `lopdf::Document`s and returns typed values.
//! Path authorization (the host's file broker) stays in the app; [`commit`] provides the atomic
//! disk write once the host has authorized a target.
//!
//! | module    | responsibility                                                    |
//! |-----------|-------------------------------------------------------------------|
//! | `model`   | wire types: commands, islands, locators, typed `PdfEditError`     |
//! | `inspect` | page graph walk: text islands with quads, fonts, source locators  |
//! | `font`    | bundled + system font catalog, subsetting, ToUnicode              |
//! | `compose` | sparse-write composition of edit commands onto fresh bytes        |
//! | `save`    | candidate verification, single-use save tokens, commit resolution |
//! | `commit`  | (feature `edit`) atomic on-disk write for a resolved save token   |
//! | `merge`   | (feature `merge`) merge PDFs, select/reorder pages                |
//! | `extract` | (feature `extract`) text extraction, whole-document and per page  |

#[cfg(feature = "edit")]
pub mod commit;
#[cfg(feature = "edit")]
pub mod compose;
#[cfg(feature = "extract")]
pub mod extract;
#[cfg(feature = "fonts")]
pub mod font;
pub mod inspect;
#[cfg(feature = "merge")]
pub mod merge;
pub mod model;
#[cfg(feature = "edit")]
pub mod save;
#[cfg(test)]
mod test_fixtures;
