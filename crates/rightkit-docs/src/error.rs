//! Typed errors with stable snake_case wire codes (the `{ code, message }` envelope `rightkit-pdf`
//! uses for `PdfEditError`).

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

/// Everything `read` and `write` can refuse. Every variant has a stable [`DocsError::code`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DocsError {
    /// The bytes are not a DOCX, XLSX or PDF (legacy OLE `.doc`/`.xls`, PPTX, XLSB, plain text...).
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    /// A macro-enabled package (`.docm`, `.xlsm`, `.dotm`, `.xltm`, or any package holding a VBA
    /// project). Refused, never opened, never run.
    #[error("macro-enabled documents are refused ({0})")]
    MacroEnabled(String),
    /// Password-protected: an encrypted OOXML container, a zip-level encrypted entry, or an
    /// encrypted PDF. This crate does not decrypt.
    #[error("the document is encrypted")]
    Encrypted,
    /// The input exceeds the compressed-size cap.
    #[error("input is {actual} bytes, over the {limit} byte limit")]
    TooLarge { limit: u64, actual: u64 },
    /// A zip-bomb guard fired: entry count, decompressed size, or compression ratio.
    #[error("archive limit exceeded: {0}")]
    ArchiveLimit(String),
    /// The structure is not a valid package or part, or it contradicts itself (duplicate entries,
    /// unsafe entry names, truncated archive, broken XML).
    #[error("malformed document: {0}")]
    Malformed(String),
    /// A DOCTYPE or entity declaration. XML external entities are not supported and the part is
    /// refused rather than parsed.
    #[error("forbidden XML construct in {part}: {what}")]
    ForbiddenXml { part: String, what: &'static str },
    /// The target differs from the file the edit was based on.
    #[error("the target changed since it was read")]
    SourceChanged,
    /// `create` over an existing file without `overwrite`.
    #[error("the target already exists")]
    TargetExists,
    /// An edit op on a target that does not exist.
    #[error("the target does not exist")]
    TargetMissing,
    /// A named sheet, page, range or text was not found.
    #[error("{kind} not found: {name}")]
    NotFound { kind: &'static str, name: String },
    /// The op itself is invalid (bad cell address, empty find string, too-long string...).
    #[error("invalid operation: {0}")]
    InvalidOp(String),
    /// A formula the policy refuses: external workbook link, DDE, network fetch function.
    #[error("formula refused: {0}")]
    UnsafeFormula(String),
    /// The op does not apply to this format.
    #[error("{op} does not apply to a {format} document")]
    WrongFormat {
        op: &'static str,
        format: &'static str,
    },
    /// A `rightkit-pdf` refusal; the PDF wire code is in the message.
    #[error("pdf: {0}")]
    Pdf(#[from] rightkit_pdf::model::PdfEditError),
    /// PDF text extraction failed.
    #[error("pdf text extraction failed: {0}")]
    PdfExtract(String),
    /// Reading the source or writing the target failed. Nothing was published.
    #[error("i/o: {0}")]
    Io(String),
    /// The replacement was written but a step after it failed (directory sync); treat the file as
    /// saved, durability across power loss as not established.
    #[error("saved, but a step after the replacement failed: {0}")]
    SavedWithWarning(String),
}

impl DocsError {
    /// Stable wire code.
    pub fn code(&self) -> &'static str {
        match self {
            DocsError::UnsupportedFormat(_) => "unsupported_format",
            DocsError::MacroEnabled(_) => "macro_enabled",
            DocsError::Encrypted => "encrypted",
            DocsError::TooLarge { .. } => "too_large",
            DocsError::ArchiveLimit(_) => "archive_limit",
            DocsError::Malformed(_) => "malformed",
            DocsError::ForbiddenXml { .. } => "forbidden_xml",
            DocsError::SourceChanged => "source_changed",
            DocsError::TargetExists => "target_exists",
            DocsError::TargetMissing => "target_missing",
            DocsError::NotFound { .. } => "not_found",
            DocsError::InvalidOp(_) => "invalid_op",
            DocsError::UnsafeFormula(_) => "unsafe_formula",
            DocsError::WrongFormat { .. } => "wrong_format",
            DocsError::Pdf(rightkit_pdf::model::PdfEditError::EncryptedDocument) => "encrypted",
            DocsError::Pdf(_) => "pdf_edit_failed",
            DocsError::PdfExtract(_) => "pdf_extract_failed",
            DocsError::Io(_) => "io",
            DocsError::SavedWithWarning(_) => "saved_with_warning",
        }
    }
}

impl Serialize for DocsError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("DocsError", 2)?;
        state.serialize_field("code", self.code())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
    }
}

pub type Result<T> = std::result::Result<T, DocsError>;
