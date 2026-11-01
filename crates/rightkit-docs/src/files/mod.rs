use std::io::Write;
use std::path::Path;
use std::sync::{LazyLock, OnceLock};

use base64::Engine;
use rightkit_fs::digest::{sha256_bytes, sha256_file_with_len};
use rightkit_fs::{AtomicWriteOptions, FsError, KeyedWriteLocks};
use serde::Serialize;

const MAX_TEXT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 150 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_BINARY_BASE64_CHARS: usize = (MAX_BINARY_BYTES as usize).div_ceil(3) * 4;
const TEXT_BINARY_SNIFF_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
struct DocumentFormatEntry {
    extension: &'static str,
    format: &'static str,
    mime: Option<&'static str>,
}

/// A malformed line in the embedded `data/document_formats.tsv`.
#[derive(Debug, PartialEq, Eq)]
enum FormatTableError {
    MissingExtension { line: usize },
    MissingFormat { line: usize },
}

impl std::fmt::Display for FormatTableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingExtension { line } => {
                write!(f, "document_formats.tsv line {line}: missing extension")
            }
            Self::MissingFormat { line } => {
                write!(f, "document_formats.tsv line {line}: missing format")
            }
        }
    }
}

fn parse_format_table(source: &'static str) -> Result<Vec<DocumentFormatEntry>, FormatTableError> {
    let mut entries = Vec::new();
    for (index, line) in source.lines().enumerate() {
        // `str::lines` already strips CRLF; blank-or-whitespace rows are skipped like the TS parser.
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let line_no = index + 1;
        let mut fields = line.split('\t');
        let extension = fields
            .next()
            .filter(|field| !field.is_empty())
            .ok_or(FormatTableError::MissingExtension { line: line_no })?;
        let format = fields
            .next()
            .filter(|field| !field.is_empty())
            .ok_or(FormatTableError::MissingFormat { line: line_no })?;
        entries.push(DocumentFormatEntry {
            extension,
            format,
            mime: fields.next(),
        });
    }
    Ok(entries)
}

fn document_formats() -> &'static [DocumentFormatEntry] {
    static FORMATS: OnceLock<Vec<DocumentFormatEntry>> = OnceLock::new();
    FORMATS
        .get_or_init(|| {
            // The table is compiled in, so a parse failure is a build defect
            // that fails on first use; the named error is the panic text.
            parse_format_table(include_str!("../../data/document_formats.tsv"))
                .unwrap_or_else(|error| panic!("embedded format table is malformed: {error}"))
        })
        .as_slice()
}

fn format_entry(extension: &str) -> Option<DocumentFormatEntry> {
    document_formats()
        .iter()
        .copied()
        .find(|entry| entry.extension == extension)
}

pub fn openable_extensions() -> &'static [&'static str] {
    static OPENABLE: OnceLock<Vec<&'static str>> = OnceLock::new();
    OPENABLE
        .get_or_init(|| {
            document_formats()
                .iter()
                .map(|entry| entry.extension)
                .collect()
        })
        .as_slice()
}

pub fn text_extensions() -> &'static [&'static str] {
    static TEXT: OnceLock<Vec<&'static str>> = OnceLock::new();
    TEXT.get_or_init(|| {
        document_formats()
            .iter()
            .filter(|entry| entry.format == "txt")
            .map(|entry| entry.extension)
            .collect()
    })
    .as_slice()
}

#[derive(Debug, Serialize)]
pub struct RawDocument {
    pub path: String,
    pub content: String,
    pub last_view: Option<String>,
    /// Per-format view metadata (JSON string) restored from the DB by read_document.
    pub view_meta: Option<String>,
    /// True only when even the detected encoding couldn't decode the bytes cleanly (genuinely
    /// garbled input) — replacement characters were substituted. A correctly-decoded legacy file
    /// (Windows-1252 etc.) is NOT lossy.
    pub lossy: bool,
    /// The encoding the content was decoded from (e.g. "UTF-8", "windows-1252"). Saving re-encodes
    /// to this encoding to preserve the file's original byte format. Surfaced in the UI.
    pub encoding: String,
    /// Modified-time (ms since epoch) at read time. 0 if the platform/FS doesn't report it.
    pub mtime_ms: i64,
    /// Byte size at read time. Together with mtime_ms this is the cheap on-disk fingerprint used to
    /// detect background-tab changes on activation without re-reading the file.
    pub size: i64,
    /// SHA-256 of text bytes at read time. Save uses it to reject stale in-place writes even when
    /// mtime and size collide. Binary viewers do not need this identity in the renderer.
    pub source_sha256: Option<String>,
    /// Opaque token for binary viewer URLs. The renderer turns this into a custom-protocol URL; it
    /// never receives the filesystem path in that URL.
    pub stream_token: Option<String>,
}

/// Cheap on-disk fingerprint (mtime ms + size) used to detect that a file changed while its tab was
/// inactive. Reads only metadata, never the file body.
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

pub const WRITE_CONFLICT: &str = "write conflict: file changed on disk before save";

/// In-process save locks keyed by destination file (case-folding and aliases of the same file share
/// one lock), from `rightkit-fs`. A save waits up to the owner's default 30 s for a busy destination.
static WRITE_LOCKS: LazyLock<KeyedWriteLocks> = LazyLock::new(KeyedWriteLocks::new);

fn source_sha256_path(path: &Path) -> Result<String, String> {
    let too_large = || "text document is too large to save safely".to_string();
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.len() > MAX_TEXT_BYTES {
        return Err(too_large());
    }
    let (digest, total) = sha256_file_with_len(path).map_err(|e| e.to_string())?;
    if total > MAX_TEXT_BYTES {
        return Err(too_large());
    }
    Ok(digest)
}

/// Convert a file's metadata into the (mtime_ms, size) pair. A missing/unsupported mtime maps to 0
/// ("can't tell") rather than failing — callers treat 0 as "no fingerprint", never as "unchanged".
fn fingerprint_of(meta: &std::fs::Metadata) -> (i64, i64) {
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    (mtime_ms, meta.len() as i64)
}

/// Stat a path for its current fingerprint. Used on tab activation to compare against the value
/// captured when the document was last read or saved.
pub fn fingerprint(path: &str) -> Result<FileFingerprint, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let (mtime_ms, size) = fingerprint_of(&meta);
    Ok(FileFingerprint { mtime_ms, size })
}

pub fn format_of(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    extension
        .as_deref()
        .and_then(format_entry)
        .map(|entry| entry.format)
        .unwrap_or_else(|| {
            if extension.is_none() {
                "txt"
            } else {
                "unknown"
            }
        })
}

fn image_mime(path: &str) -> Option<&'static str> {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
        .and_then(format_entry)
        .and_then(|entry| (entry.format == "image").then_some(entry.mime).flatten())
}

pub fn document_mime(path: &str) -> Option<&'static str> {
    match format_of(path) {
        "pdf" => Some("application/pdf"),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "image" => image_mime(path),
        _ => None,
    }
}

pub fn is_openable_format(path: &str) -> bool {
    !matches!(format_of(path), "unknown")
}

/// Decode raw file bytes into text, returning (content, encoding_name, lossy). UTF-8 is the fast
/// path (the common case for our formats). Otherwise chardetng detects the legacy encoding and
/// encoding_rs decodes it faithfully — so Windows-1252/Latin-1 text reads correctly instead of
/// being mangled into U+FFFD replacement characters by a lossy UTF-8 decode. `lossy` is true only
/// if even the detected encoding hit undecodable bytes.
fn decode_text(bytes: &[u8]) -> (String, String, bool) {
    if let Some((little_endian, bom_len)) = utf16_byte_order(bytes) {
        let body = &bytes[bom_len..];
        let (pairs, remainder) = body.as_chunks::<2>();
        let units = pairs
            .iter()
            .map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect::<Vec<_>>();
        let had_errors = !remainder.is_empty() || String::from_utf16(&units).is_err();
        return (
            String::from_utf16_lossy(&units),
            match (little_endian, bom_len > 0) {
                (true, true) => "UTF-16LE BOM",
                (true, false) => "UTF-16LE",
                (false, true) => "UTF-16BE BOM",
                (false, false) => "UTF-16BE",
            }
            .to_string(),
            had_errors,
        );
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_string(), "UTF-8".to_string(), false);
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    detector.feed(bytes, true);
    let encoding = detector.guess(None, chardetng::Utf8Detection::Allow);
    let (content, used, had_errors) = encoding.decode(bytes);
    (content.into_owned(), used.name().to_string(), had_errors)
}

fn utf16_byte_order(bytes: &[u8]) -> Option<(bool, usize)> {
    if bytes.starts_with(&[0xff, 0xfe]) {
        return Some((true, 2));
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        return Some((false, 2));
    }
    let sample = &bytes[..bytes.len().min(TEXT_BINARY_SNIFF_BYTES)];
    let pairs = sample.len() / 2;
    if pairs == 0 {
        return None;
    }
    let even_nuls = sample.iter().step_by(2).filter(|&&byte| byte == 0).count();
    let odd_nuls = sample
        .iter()
        .skip(1)
        .step_by(2)
        .filter(|&&byte| byte == 0)
        .count();
    if odd_nuls > even_nuls.saturating_mul(3) && utf16_sample_is_text(sample, true) {
        return Some((true, 0));
    }
    if even_nuls > odd_nuls.saturating_mul(3) && utf16_sample_is_text(sample, false) {
        return Some((false, 0));
    }
    None
}

fn utf16_sample_is_text(sample: &[u8], little_endian: bool) -> bool {
    let (pairs, _) = sample.as_chunks::<2>();
    let units = pairs
        .iter()
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect::<Vec<_>>();
    let Ok(text) = String::from_utf16(&units) else {
        return false;
    };
    let total = text.chars().count();
    if total == 0 || text.chars().any(|character| character == '\0') {
        return false;
    }
    let disallowed_controls = text
        .chars()
        .filter(|character| {
            character.is_control() && !matches!(*character, '\t' | '\n' | '\r' | '\u{000c}')
        })
        .count();
    disallowed_controls * 4 <= total
}

/// Reject binary payloads that merely carry a text-like extension before allocating a giant editor
/// document. Preserve UTF-16 text (with or without a BOM); its alternating NUL bytes are encoding,
/// not binary content.
fn looks_like_binary_text(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(TEXT_BINARY_SNIFF_BYTES)];
    if sample.is_empty() || utf16_byte_order(sample).is_some() {
        return false;
    }
    let nul_count = sample.iter().filter(|&&byte| byte == 0).count();
    if nul_count * 100 >= sample.len() {
        return true;
    }
    let controls = sample
        .iter()
        .filter(|&&byte| byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r' | 0x0c))
        .count();
    controls * 3 > sample.len()
}

/// Encode text to bytes for a target encoding label, returning (bytes, encoding_written). UTF-8 (and
/// new/untitled files) write through unchanged. For a legacy encoding we re-encode to preserve the
/// file's original byte format — UNLESS the edited content now contains characters that encoding
/// can't represent: rather than silently inserting numeric character references / '?', we upgrade
/// the file to UTF-8 and report that back so the document state reflects the new encoding.
fn encode_text(content: &str, encoding_label: &str) -> Result<(Vec<u8>, String), String> {
    let label = encoding_label.trim();
    if label.is_empty() || label.eq_ignore_ascii_case("UTF-8") {
        return Ok((content.as_bytes().to_vec(), "UTF-8".to_string()));
    }
    let utf16 = [
        ("UTF-16LE", true, false),
        ("UTF-16LE BOM", true, true),
        ("UTF-16BE", false, false),
        ("UTF-16BE BOM", false, true),
    ]
    .into_iter()
    .find(|(name, _, _)| label.eq_ignore_ascii_case(name));
    if let Some((name, little_endian, write_bom)) = utf16 {
        let mut bytes = Vec::with_capacity(content.len() * 2 + usize::from(write_bom) * 2);
        if write_bom {
            bytes.extend_from_slice(if little_endian {
                &[0xff, 0xfe]
            } else {
                &[0xfe, 0xff]
            });
        }
        for unit in content.encode_utf16() {
            let encoded = if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            };
            bytes.extend_from_slice(&encoded);
        }
        return Ok((bytes, name.to_string()));
    }
    let Some(encoding) = encoding_rs::Encoding::for_label(label.as_bytes()) else {
        return Err(format!("unknown text encoding: {label}"));
    };
    let (bytes, _used, had_unmappable) = encoding.encode(content);
    if had_unmappable {
        return Ok((content.as_bytes().to_vec(), "UTF-8".to_string()));
    }
    Ok((bytes.into_owned(), encoding.name().to_string()))
}

/// Write text back to a path (txt editing), re-encoding to `encoding` to preserve the file's
/// original byte format. Returns the encoding actually written (UTF-8 if the content couldn't be
/// represented in the requested legacy encoding). Used by the editable TXT pane.
pub fn write(
    path: &str,
    content: &str,
    encoding: &str,
    expected_source_sha256: Option<&str>,
) -> Result<WriteDocumentResult, String> {
    let (bytes, written) = encode_text(content, encoding)?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err("text document is too large to save safely".into());
    }
    // Atomic temp, fsync, rename, same as binary saves: a crash mid-write must never leave a
    // half-written or truncated source file.
    write_binary(path, &bytes, expected_source_sha256)?;
    Ok(WriteDocumentResult {
        encoding: written,
        source_sha256: sha256_bytes(&bytes),
    })
}

/// Write base64-encoded bytes back to a path (docx editing — binary round-trip).
///
/// DECISION (cap, not stream): DOCX save crosses IPC as base64 (≈33% overhead), bounded by
/// MAX_BINARY_BYTES (150 MB raw). That's well above any realistic everyday Word document, so we cap
/// and reject rather than build a streaming/chunked save path. The base64 length is checked before
/// decoding so an oversized payload is rejected without allocating the decoded buffer. If real-world
/// DOCX sizes ever approach the cap, revisit with a chunked/streamed writer.
pub fn write_bytes(path: &str, content_base64: &str) -> Result<(), String> {
    if content_base64.len() > MAX_BINARY_BASE64_CHARS {
        return Err("binary document is too large to save through this build".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_base64)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BINARY_BYTES {
        return Err("binary document is too large to save through this build".into());
    }
    write_binary(path, &bytes, None)
}

/// Write already-decoded binary bytes back to a path with the same MAX_BINARY_BYTES cap and the
/// same atomic temp-file + fsync + rename semantics as [`write_bytes`]. Used by
/// `commit_pdf_save`, which already holds raw candidate bytes in-process — routing them through
/// the base64 entry point would add a full-document encode+decode round-trip for no benefit.
/// `MAX_BINARY_BASE64_CHARS` is irrelevant here: it only bounds the encoded IPC payload.
pub fn write_raw_bytes(path: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 > MAX_BINARY_BYTES {
        return Err("binary document is too large to save through this build".into());
    }
    write_binary(path, bytes, None)
}

fn write_binary(
    path: &str,
    bytes: &[u8],
    expected_source_sha256: Option<&str>,
) -> Result<(), String> {
    write_atomic(path, expected_source_sha256, |file| file.write_all(bytes))
}

/// Preserve the original binary without decoding it or allocating a document-sized IPC buffer.
pub fn copy_document(source: &str, target: &str) -> Result<(), String> {
    if source == target {
        return Ok(());
    }
    let mut source_file = std::fs::File::open(source).map_err(|e| e.to_string())?;
    let metadata = source_file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("source document is not a regular file".into());
    }
    write_atomic(target, None, |target_file| {
        let copied = std::io::copy(&mut source_file, target_file)?;
        if copied != metadata.len() || source_file.metadata()?.modified()? != metadata.modified()? {
            return Err(std::io::Error::other(
                "source document changed while copying",
            ));
        }
        Ok(())
    })
}

/// Atomic temp-file write through `rightkit_fs`, under its per-destination write lock. `durable: false` adds no directory sync, so a
/// save never gains a post-publish error. The conflict recheck runs in `commit_checked`, after
/// the crate has synced the temp file and before the replace. Errors keep their old text: the
/// bare OS error, or [`WRITE_CONFLICT`].
fn write_atomic(
    path: &str,
    expected_source_sha256: Option<&str>,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> Result<(), String> {
    let path = Path::new(path);
    if path.file_name().and_then(|name| name.to_str()).is_none() {
        return Err("path has no file name".to_string());
    }
    let saved = WRITE_LOCKS.with_destination(path, |held| -> Result<(), String> {
        if let Some(expected) = expected_source_sha256 {
            let current = source_sha256_path(path).map_err(|_| WRITE_CONFLICT.to_string())?;
            if current != expected {
                return Err(WRITE_CONFLICT.to_string());
            }
        }
        let options = AtomicWriteOptions {
            durable: false,
            ..AtomicWriteOptions::default()
        };
        // Dropping `writer` before `commit_checked` removes the temp file, so early returns are clean.
        let mut writer = held.atomic_writer(options).map_err(atomic_error)?;
        write(writer.file_mut()).map_err(|error| error.to_string())?;

        // The check runs after the complete candidate is written and before anything is published, so
        // the identity comparison is immediately before the atomic replacement. An external writer
        // during encoding must leave its bytes untouched rather than being overwritten by this save.
        // `commit_checked` file-syncs the temp before it runs the check, even with `durable: false`
        // (rightkit-fs atomic.rs, `commit_checked`), so the check reads what is on stable storage and
        // what `Ok` publishes. A failed check removes the temp file.
        writer
            .commit_checked(|_temp| {
                if let Some(expected) = expected_source_sha256 {
                    let current = source_sha256_path(path)
                        .map_err(|_| std::io::Error::other(WRITE_CONFLICT))?;
                    if current != expected {
                        return Err(std::io::Error::other(WRITE_CONFLICT));
                    }
                }
                Ok(())
            })
            .map_err(atomic_error)
    });
    // An error of the lock itself (no usable parent, busy for longer than the wait) reads like any
    // other failed atomic write.
    saved.map_err(atomic_error)?
}

/// The text a failed atomic write reports: the OS error alone, as the local writer did, without
/// the temp path that `FsError::Io` adds to its display text.
fn atomic_error(error: FsError) -> String {
    match error {
        FsError::Io { source, .. } => source.to_string(),
        other => other.to_string(),
    }
}

/// Read a document off disk. PDF/DOCX return metadata only; the webview streams bytes through a
/// revocable custom-protocol token minted by the command layer. Text formats come back as UTF-8
/// content, and images also return a data URL for canvas-safe editing/export.
pub fn read(path: &str) -> Result<RawDocument, String> {
    let format = format_of(path);
    if format == "unknown" {
        return Err("unsupported file type; this format is not openable".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let (mtime_ms, size) = fingerprint_of(&meta);
    if format == "pdf" || format == "docx" {
        if format == "docx" && meta.len() > MAX_BINARY_BYTES {
            return Err("Word document is too large to open through this build".into());
        }
        // Binary formats stream to the webview via the host's streaming protocol (range requests) — no full read
        // into RAM, no base64 over IPC. metadata() above confirms the file exists and is reachable.
        Ok(RawDocument {
            path: path.to_string(),
            content: String::new(),
            last_view: None,
            view_meta: None,
            lossy: false,
            encoding: "UTF-8".to_string(),
            mtime_ms,
            size,
            source_sha256: None,
            stream_token: None,
        })
    } else if format == "image" {
        if meta.len() > MAX_IMAGE_BYTES {
            return Err("image is too large to open through this build".into());
        }
        image_mime(path).ok_or_else(|| "unsupported image format".to_string())?;
        Ok(RawDocument {
            path: path.to_string(),
            // The command layer mints a revocable stream token for every image.
            // Do not duplicate up to 50 MB as base64 over IPC on a normal open; the viewer
            // fetches bytes from that stream only when it needs to display or export them.
            content: String::new(),
            last_view: None,
            view_meta: None,
            lossy: false,
            encoding: "binary".to_string(),
            mtime_ms,
            size,
            source_sha256: None,
            stream_token: None,
        })
    } else {
        if meta.len() > MAX_TEXT_BYTES {
            return Err("text document is too large to open safely".into());
        }
        // Decode UTF-8 fast, else detect + faithfully decode the legacy encoding. The original
        // encoding is preserved on save (see `write`), so a Windows-1252 file round-trips instead
        // of being silently rewritten as UTF-8.
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if looks_like_binary_text(&bytes) {
            return Err("file contains binary data rather than editable text".into());
        }
        let (content, encoding, lossy) = decode_text(&bytes);
        Ok(RawDocument {
            path: path.to_string(),
            content,
            last_view: None,
            view_meta: None,
            lossy,
            encoding,
            mtime_ms,
            size,
            source_sha256: Some(sha256_bytes(&bytes)),
            stream_token: None,
        })
    }
}

#[cfg(test)]
#[path = "documents_tests.rs"]
mod documents_tests;
