//! `ObservedVersion` — an explicit optimistic-concurrency token for a
//! workspace file, plus line-ending preservation helpers.
//!
//! ENG-17 ("Atomic/version-guarded writes") requires that write/edit tools
//! reject a mutation when the on-disk file changed since it was last
//! observed by the caller, instead of silently clobbering a concurrent edit.
//! `FileService` (see `file_service.rs`) already enforces this at the
//! receipt layer (`record_read` + `write_with_receipt`) using an equivalent
//! `FileRevision` fingerprint; `ObservedVersion` generalizes that fingerprint
//! into a standalone, reusable type so any caller — not only the read-file
//! receipt flow — can capture a version and later assert it is still
//! current before writing.
//!
//! NOTE (integration): this module must be declared with `pub mod
//! observed_version;` in `engine/crates/tools/src/lib.rs` before other files
//! in this crate can reference `crate::observed_version::*`. That line is
//! outside this lane's exclusive file allowlist and is intentionally left
//! for the integration owner — see the dispatch report for this lane.
//!
//! Wanted verification (not run in this lane): `rustright cargo test
//! --manifest-path engine/Cargo.toml -p host_tools observed_version`.

use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// A lightweight fingerprint of a file's on-disk state at the moment it was
/// observed: size, modification time (nanoseconds since epoch), and a fast
/// (non-cryptographic) hash of its head and tail bytes. Two files with the
/// same size, mtime, and edge-byte hash are treated as the same version —
/// this mirrors `file_service::FileRevision` exactly so the two checks never
/// disagree about whether a file changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedVersion {
    /// `true` when the version was captured for a path that did not exist.
    /// A write against a `missing` observed version is only valid if the
    /// path still does not exist at write time (i.e. this is a fresh
    /// create, not an overwrite of something that appeared concurrently).
    pub existed: bool,
    pub size: u64,
    pub modified_ns: u128,
    /// Cheap screening hash over head/tail samples. Not sufficient alone.
    pub fast_hash: u64,
    /// EFF-005: authoritative full-content SHA-256, captured at observation
    /// time. Size + mtime + edge samples all collide for a mutation confined
    /// to the middle of a file that preserves length and restores mtime;
    /// this does not.
    pub content_digest: [u8; 32],
}

/// Why a version check failed. Kept as a small enum (rather than a bare
/// `String`, which is this crate's dominant tool-result convention) so
/// callers that need to branch on "stale vs. i/o error" can do so without
/// string-matching; `Display` still renders an actionable message for the
/// `ToolResult::error` path most call sites use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCheckError {
    /// The file could not be inspected (permissions, transient I/O, etc).
    Io(String),
    /// The file's current state no longer matches what was observed —
    /// someone else wrote to it (or created/deleted it) in the meantime.
    Stale {
        observed: ObservedVersion,
        current: ObservedVersion,
    },
}

impl std::fmt::Display for VersionCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VersionCheckError::Io(message) => write!(f, "{message}"),
            VersionCheckError::Stale { observed, current } => write!(
                f,
                "stale observed version: file changed since it was read \
                 (observed existed={}, size={}, mtime_ns={}; current existed={}, size={}, mtime_ns={}). \
                 Re-read the file and retry.",
                observed.existed,
                observed.size,
                observed.modified_ns,
                current.existed,
                current.size,
                current.modified_ns,
            ),
        }
    }
}

impl std::error::Error for VersionCheckError {}

impl ObservedVersion {
    /// Capture the current on-disk fingerprint of `path`. Returns an
    /// `existed: false` sentinel (rather than an error) when the path does
    /// not exist, so "I observed this file as absent" is a first-class,
    /// comparable version — this lets a caller detect "someone created this
    /// file after I decided it didn't exist" as staleness too.
    pub fn capture(path: &Path) -> Result<Self, String> {
        match std::fs::metadata(path) {
            Ok(metadata) => Self::from_metadata(path, &metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                existed: false,
                size: 0,
                modified_ns: 0,
                fast_hash: 0,
                content_digest: [0_u8; 32],
            }),
            Err(error) => Err(format!("inspect '{}': {error}", path.display())),
        }
    }

    /// Stream every byte through SHA-256. This is the authoritative identity
    /// used for staleness decisions.
    fn content_digest(path: &Path) -> Result<[u8; 32], String> {
        let mut file = std::fs::File::open(path)
            .map_err(|error| format!("open '{}': {error}", path.display()))?;
        let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("read '{}': {error}", path.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hasher.finalize_bytes())
    }

    fn from_metadata(path: &Path, metadata: &std::fs::Metadata) -> Result<Self, String> {
        let modified_ns = metadata
            .modified()
            .map_err(|error| format!("read modification time for '{}': {error}", path.display()))?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                format!(
                    "invalid modification time for '{}': {error}",
                    path.display()
                )
            })?
            .as_nanos();

        let mut file = std::fs::File::open(path)
            .map_err(|error| format!("open '{}': {error}", path.display()))?;
        let mut sample = [0_u8; 4096];
        let first = file
            .read(&mut sample)
            .map_err(|error| format!("read '{}': {error}", path.display()))?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        metadata.len().hash(&mut hasher);
        sample[..first].hash(&mut hasher);
        if metadata.len() > sample.len() as u64 {
            file.seek(SeekFrom::End(-(sample.len() as i64)))
                .map_err(|error| format!("seek '{}': {error}", path.display()))?;
            let last = file
                .read(&mut sample)
                .map_err(|error| format!("read '{}': {error}", path.display()))?;
            sample[..last].hash(&mut hasher);
        }
        Ok(Self {
            existed: true,
            size: metadata.len(),
            modified_ns,
            fast_hash: hasher.finish(),
            content_digest: Self::content_digest(path)?,
        })
    }

    /// Assert this observed version still matches the file's current state.
    /// Returns `Ok(())` when nothing changed, `Err(VersionCheckError::Stale)`
    /// when it did, and `Err(VersionCheckError::Io)` if the file could not
    /// be inspected at all.
    pub fn assert_current(&self, path: &Path) -> Result<(), VersionCheckError> {
        let current = Self::capture(path).map_err(VersionCheckError::Io)?;
        if &current != self {
            return Err(VersionCheckError::Stale {
                observed: self.clone(),
                current,
            });
        }
        Ok(())
    }

    /// Convenience predicate for call sites that only want a bool.
    pub fn is_stale(&self, path: &Path) -> Result<bool, String> {
        match self.assert_current(path) {
            Ok(()) => Ok(false),
            Err(VersionCheckError::Stale { .. }) => Ok(true),
            Err(VersionCheckError::Io(message)) => Err(message),
        }
    }
}

/// Line-ending style detected in a text file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
    /// No newline found, or a mix that doesn't clearly favor either style —
    /// callers should leave content untouched in this case.
    Mixed,
}

/// Detect the dominant line-ending style of `text` by counting `\r\n` pairs
/// against bare `\n` occurrences that are not part of a `\r\n` pair.
pub fn detect_line_ending(text: &str) -> LineEnding {
    let mut crlf = 0usize;
    let mut lf_only = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            if i > 0 && bytes[i - 1] == b'\r' {
                crlf += 1;
            } else {
                lf_only += 1;
            }
        }
        i += 1;
    }
    match (crlf, lf_only) {
        (0, 0) => LineEnding::Mixed,
        (c, l) if c > 0 && l == 0 => LineEnding::Crlf,
        (c, l) if l > 0 && c == 0 => LineEnding::Lf,
        _ => LineEnding::Mixed,
    }
}

/// Rewrite `new_content`'s line endings to match `original_content`'s
/// dominant style, when that style is unambiguous and `new_content` itself
/// is not already mixed. This is the ENG-17 "preserve line endings" guard
/// for full-file overwrites (`write_file`): a caller supplying LF-only
/// replacement text for a CRLF file should not silently flip the whole
/// file's line-ending convention.
pub fn preserve_line_endings(original_content: &str, new_content: &str) -> String {
    let original_style = detect_line_ending(original_content);
    let new_style = detect_line_ending(new_content);
    match (original_style, new_style) {
        (LineEnding::Crlf, LineEnding::Lf) => new_content.replace('\n', "\r\n"),
        (LineEnding::Lf, LineEnding::Crlf) => new_content.replace("\r\n", "\n"),
        _ => new_content.to_string(),
    }
}
