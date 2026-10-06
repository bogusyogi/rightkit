//! Text extraction (ScrapeRight donor: `pdf-extract`, no PDFium dependency).
//!
//! Scanned (image-only) pages yield empty text; callers route those to OCR.

use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("pdf extract failed: {0}")]
    Failed(String),
    #[error("pdf extractor panicked on malformed input")]
    Panicked,
}

fn guarded<T>(f: impl FnOnce() -> Result<T, pdf_extract::OutputError>) -> Result<T, ExtractError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(ExtractError::Failed(e.to_string())),
        Err(_) => Err(ExtractError::Panicked),
    }
}

/// Whole-document text, trimmed.
pub fn extract_text(bytes: &[u8]) -> Result<String, ExtractError> {
    guarded(|| pdf_extract::extract_text_from_mem(bytes)).map(|t| t.trim().to_string())
}

/// Text per page in page order (index 0 is page 1).
pub fn extract_text_by_pages(bytes: &[u8]) -> Result<Vec<String>, ExtractError> {
    guarded(|| pdf_extract::extract_text_from_mem_by_pages(bytes))
        .map(|pages| pages.into_iter().map(|t| t.trim().to_string()).collect())
}
