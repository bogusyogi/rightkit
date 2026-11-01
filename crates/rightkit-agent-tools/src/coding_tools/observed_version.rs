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
//! in this crate can reference `crate::coding_tools::observed_version::*`. That line is
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
        Ok(hasher.finalize_bytes().into())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn capture_reports_absent_file_as_not_existed() {
        let dir = tempdir().unwrap();
        let version = ObservedVersion::capture(&dir.path().join("ghost.txt")).unwrap();
        assert!(!version.existed);
    }

    #[test]
    fn assert_current_ok_when_unchanged() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let version = ObservedVersion::capture(&file).unwrap();
        assert!(version.assert_current(&file).is_ok());
    }

    #[test]
    fn assert_current_detects_content_change() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let version = ObservedVersion::capture(&file).unwrap();
        // Force a distinguishable mtime + content change.
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&file, "goodbye, world, this is different content").unwrap();
        let err = version.assert_current(&file).unwrap_err();
        assert!(matches!(err, VersionCheckError::Stale { .. }));
        assert!(version.is_stale(&file).unwrap());
    }

    #[test]
    fn assert_current_detects_concurrent_creation_of_previously_absent_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("new.txt");
        let version = ObservedVersion::capture(&file).unwrap();
        assert!(!version.existed);
        std::fs::write(&file, "surprise").unwrap();
        let err = version.assert_current(&file).unwrap_err();
        assert!(matches!(err, VersionCheckError::Stale { .. }));
    }

    #[test]
    fn preserve_line_endings_converts_lf_replacement_into_crlf_file() {
        let original = "a\r\nb\r\n";
        let replacement = "a\nB\nC\n";
        let preserved = preserve_line_endings(original, replacement);
        assert_eq!(preserved, "a\r\nB\r\nC\r\n");
    }

    #[test]
    fn preserve_line_endings_converts_crlf_replacement_into_lf_file() {
        let original = "a\nb\n";
        let replacement = "a\r\nB\r\n";
        let preserved = preserve_line_endings(original, replacement);
        assert_eq!(preserved, "a\nB\n");
    }

    #[test]
    fn preserve_line_endings_leaves_mixed_or_matching_content_untouched() {
        assert_eq!(preserve_line_endings("a\nb\n", "x\ny\n"), "x\ny\n");
        assert_eq!(preserve_line_endings("", "x\r\ny\n"), "x\r\ny\n");
    }
}

#[cfg(test)]
mod eff005_adversarial_preimage_tests {
    use super::*;

    /// EFF-005 adversarial case: the exact attack the previous identity could
    /// not see. A large file is observed; only bytes in the UNSAMPLED middle
    /// are mutated; length is preserved byte-for-byte; and the modification
    /// time is restored to its original value. Size, mtime and the head/tail
    /// `fast_hash` are therefore all unchanged, and a sampling-only identity
    /// would report "unchanged" and let a stale write clobber newer bytes.
    #[test]
    fn middle_of_file_mutation_with_restored_mtime_still_conflicts() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("large.bin");

        // Comfortably larger than 2 * 4096 so a middle region exists that the
        // head/tail sampling never reads.
        let size = 64 * 1024;
        let mut original = vec![b'A'; size];
        // Distinctive head/tail so we can prove they are untouched.
        original[..16].copy_from_slice(b"HEAD_MARKER_1234");
        let tail = size - 16;
        original[tail..].copy_from_slice(b"TAIL_MARKER_5678");
        std::fs::write(&path, &original).unwrap();

        let observed = ObservedVersion::capture(&path).unwrap();
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        // Mutate ONLY the middle, preserving length exactly.
        let mut mutated = original.clone();
        let middle = size / 2;
        mutated[middle..middle + 8].copy_from_slice(b"TAMPERED");
        assert_eq!(mutated.len(), original.len(), "length must be preserved");
        assert_eq!(&mutated[..16], &original[..16], "head must be untouched");
        assert_eq!(
            &mutated[tail..],
            &original[tail..],
            "tail must be untouched"
        );
        std::fs::write(&path, &mutated).unwrap();

        // Restore the modification time, as an adversary (or a tool that
        // preserves timestamps) would.
        let restored = std::fs::File::options().write(true).open(&path).unwrap();
        restored.set_modified(original_mtime).unwrap();
        drop(restored);

        let current = ObservedVersion::capture(&path).unwrap();

        // Demonstrate that every cheap signal genuinely collides - otherwise
        // this test would be proving something easier than the real attack.
        assert_eq!(current.size, observed.size, "size collides");
        assert_eq!(
            current.modified_ns, observed.modified_ns,
            "mtime was restored and collides"
        );
        assert_eq!(
            current.fast_hash, observed.fast_hash,
            "head/tail sampling collides - this is why sampling alone is unsafe"
        );

        // The authoritative identity must NOT collide.
        assert_ne!(
            current.content_digest, observed.content_digest,
            "full-content digest must detect a middle-of-file mutation"
        );
        assert_ne!(
            current, observed,
            "a stale observed version must not compare equal to mutated content"
        );
    }

    /// The control: an untouched file must still compare equal, or the
    /// strengthened identity would make every legitimate write conflict.
    #[test]
    fn unmodified_file_still_compares_equal() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stable.bin");
        std::fs::write(&path, vec![b'Z'; 32 * 1024]).unwrap();
        let first = ObservedVersion::capture(&path).unwrap();
        let second = ObservedVersion::capture(&path).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.content_digest, second.content_digest);
    }

    /// Same-length content change is detected even when mtime is restored.
    #[test]
    fn same_length_rewrite_with_restored_mtime_conflicts() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("swap.txt");
        std::fs::write(&path, b"aaaaaaaa").unwrap();
        let observed = ObservedVersion::capture(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, b"bbbbbbbb").unwrap();
        let handle = std::fs::File::options().write(true).open(&path).unwrap();
        handle.set_modified(mtime).unwrap();
        drop(handle);
        let current = ObservedVersion::capture(&path).unwrap();
        assert_eq!(current.size, observed.size);
        assert_ne!(current.content_digest, observed.content_digest);
    }
}
