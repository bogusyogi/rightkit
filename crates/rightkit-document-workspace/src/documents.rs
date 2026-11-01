//! Document types and the backend seam. The host supplies format classification and the actual
//! document reads and writes (encoding detection, atomic writes) through [`DocumentBackend`].

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct RawDocument {
    pub path: String,
    pub content: String,
    pub last_view: Option<String>,
    /// Per-format view metadata (JSON string) restored from the database by `read_document`.
    pub view_meta: Option<String>,
    /// True only when even the detected encoding could not decode the bytes cleanly (genuinely
    /// garbled input): replacement characters were substituted.
    pub lossy: bool,
    /// The encoding the content was decoded from. Saving re-encodes to this encoding.
    pub encoding: String,
    /// Modified time (ms since epoch) at read time. 0 if the platform does not report it.
    pub mtime_ms: i64,
    /// Byte size at read time. With `mtime_ms` this is the cheap on-disk fingerprint.
    pub size: i64,
    /// SHA-256 of text bytes at read time. Save uses it to reject stale in-place writes.
    pub source_sha256: Option<String>,
    /// Opaque token for binary viewer URLs; the renderer never receives the path in that URL.
    pub stream_token: Option<String>,
}

/// Cheap on-disk fingerprint (mtime ms + size) used to detect that a file changed while its tab
/// was inactive. Reads only metadata, never the file body.
#[derive(Debug, Serialize)]
pub struct FileFingerprint {
    pub mtime_ms: i64,
    pub size: i64,
}

#[derive(Debug, Serialize)]
pub struct WriteDocumentResult {
    pub encoding: String,
    pub source_sha256: String,
}

/// Document format classification and file IO as the host implements them. Errors are plain
/// strings; the workspace classifies them into [`crate::WorkspaceError`] variants.
pub trait DocumentBackend {
    /// The format id of a path (`"image"`, `"pdf"`, `"md"`, ..., or `"unknown"`).
    fn format_of(&self, path: &str) -> &'static str;
    /// Read and decode a document.
    fn read(&self, path: &str) -> Result<RawDocument, String>;
    /// Encode and atomically write text, rejecting stale in-place saves.
    fn write(
        &self,
        path: &str,
        content: &str,
        encoding: &str,
        expected_source_sha256: Option<&str>,
    ) -> Result<WriteDocumentResult, String>;
    /// Write base64 content as raw bytes.
    fn write_bytes(&self, path: &str, content_base64: &str) -> Result<(), String>;
    /// Write raw bytes.
    fn write_raw_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), String>;
    /// Copy a document to a new location.
    fn copy_document(&self, source: &str, target: &str) -> Result<(), String>;
    /// Metadata-only fingerprint.
    fn fingerprint(&self, path: &str) -> Result<FileFingerprint, String>;
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A backend that classifies by extension only; reads and writes are not exercised.
    pub struct StubDocs;

    impl DocumentBackend for StubDocs {
        fn format_of(&self, path: &str) -> &'static str {
            match std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
            {
                Some("md") => "md",
                Some("png") => "image",
                _ => "unknown",
            }
        }
        fn read(&self, _path: &str) -> Result<RawDocument, String> {
            Err("not supported".into())
        }
        fn write(
            &self,
            _path: &str,
            _content: &str,
            _encoding: &str,
            _expected_source_sha256: Option<&str>,
        ) -> Result<WriteDocumentResult, String> {
            Err("not supported".into())
        }
        fn write_bytes(&self, _path: &str, _content_base64: &str) -> Result<(), String> {
            Err("not supported".into())
        }
        fn write_raw_bytes(&self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
            Err("not supported".into())
        }
        fn copy_document(&self, _source: &str, _target: &str) -> Result<(), String> {
            Err("not supported".into())
        }
        fn fingerprint(&self, _path: &str) -> Result<FileFingerprint, String> {
            Err("not supported".into())
        }
    }
}
