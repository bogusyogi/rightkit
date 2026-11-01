#![allow(clippy::result_large_err)]

//! Guarded-save API for ENG-109 ("version-guarded in-app file editing").
//!
//! This is the etag-style save contract a client (the iOS in-app file
//! editor, notably) holds onto for the lifetime of an open file: capture an
//! [`ObservedVersion`] when the file is opened, carry it through to save,
//! and refuse the write — returning the current fingerprint instead of
//! clobbering it — when the file changed underneath the editor. This
//! protects both directions: a user's in-app edit cannot silently stomp an
//! agent's concurrent edit, and vice versa.
//!
//! This module deliberately does not duplicate `ObservedVersion`'s
//! fingerprint logic (see `observed_version.rs`, added in Dispatch A) or
//! `FileService`'s receipt-ledger write path (`file_service.rs`, the
//! ENG-17 mechanism for tool-originated edits). It is a third, narrower
//! entry point for callers — such as a mobile client's save action — that
//! hold an `ObservedVersion` directly rather than a session-scoped read
//! receipt, and that want a single call which captures-checks-writes
//! atomically under one path lock.
//!
//! NOTE (integration): this module must be declared with `pub mod
//! file_version_guard;` in `engine/crates/tools/src/lib.rs` before other
//! files in this crate can reference `crate::coding_tools::file_version_guard::*`. That
//! line is outside this lane's exclusive file allowlist and is
//! intentionally left for the integration owner — see the dispatch report
//! for this lane.
//!
//! Wanted verification (not run in this lane): `rustright cargo test
//! --manifest-path engine/Cargo.toml -p host_tools file_version_guard`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::coding_tools::file_service::FileService;
use crate::coding_tools::observed_version::{ObservedVersion, VersionCheckError};

/// Compatibility facade for callers that carry an `ObservedVersion`. The
/// actual mutation lock/revision authority is always the injected
/// [`FileService`]; this type deliberately owns no lock table of its own.
pub struct FileVersionGuard {
    file_service: Arc<FileService>,
}

impl Default for FileVersionGuard {
    fn default() -> Self {
        Self {
            file_service: FileService::new(),
        }
    }
}

/// Outcome of a rejected guarded save. Carries the fingerprint the caller
/// observed when it opened the file and the fingerprint found on disk at
/// save time, so a client can render a clear conflict state (e.g. "reload"
/// vs. "overwrite anyway") instead of a bare error string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleWriteRejection {
    pub path: PathBuf,
    pub observed: ObservedVersion,
    pub current: ObservedVersion,
}

impl std::fmt::Display for StaleWriteRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stale save rejected for '{}': file changed since it was opened",
            self.path.display()
        )
    }
}

impl std::error::Error for StaleWriteRejection {}

/// Why a guarded save failed.
#[derive(Debug)]
pub enum GuardedWriteError {
    /// The observed version no longer matches what is on disk.
    Stale(StaleWriteRejection),
    /// The version check or the write itself hit an I/O error.
    Io(String),
}

impl std::fmt::Display for GuardedWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuardedWriteError::Stale(rejection) => write!(f, "{rejection}"),
            GuardedWriteError::Io(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for GuardedWriteError {}

impl From<VersionCheckError> for GuardedWriteErrorContext {
    fn from(error: VersionCheckError) -> Self {
        match error {
            VersionCheckError::Io(message) => GuardedWriteErrorContext::Io(message),
            VersionCheckError::Stale { observed, current } => {
                GuardedWriteErrorContext::Stale { observed, current }
            }
        }
    }
}

/// Internal shape used only to convert `VersionCheckError` (which has no
/// `path` field) into `GuardedWriteError` (which does) without repeating
/// the match at every call site.
enum GuardedWriteErrorContext {
    Io(String),
    Stale {
        observed: ObservedVersion,
        current: ObservedVersion,
    },
}

impl FileVersionGuard {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn from_file_service(file_service: Arc<FileService>) -> Arc<Self> {
        Arc::new(Self { file_service })
    }

    /// Attempt a guarded save: verify `observed` still matches the file's
    /// current on-disk state, and only if so, atomically write `bytes` to
    /// `path`. Serialized per-path so two concurrent guarded saves cannot
    /// both observe "still current" and both proceed.
    ///
    /// Returns `Err(GuardedWriteError::Stale)` carrying both the version the
    /// caller observed and the version found on disk when they disagree —
    /// the caller (e.g. `FileViewerFeature` on the Swift side) should
    /// surface this as a conflict, not overwrite silently and not merely
    /// log an error.
    #[allow(clippy::result_large_err)]
    pub fn guarded_write(
        &self,
        path: &Path,
        observed: &ObservedVersion,
        bytes: &[u8],
    ) -> Result<ObservedVersion, GuardedWriteError> {
        self.file_service
            .write_with_observed_version(path, observed, bytes)
            .map_err(|error| match GuardedWriteErrorContext::from(error) {
                GuardedWriteErrorContext::Io(message) => GuardedWriteError::Io(message),
                GuardedWriteErrorContext::Stale { observed, current } => {
                    GuardedWriteError::Stale(StaleWriteRejection {
                        path: path.to_path_buf(),
                        observed,
                        current,
                    })
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn guarded_write_succeeds_when_unchanged_since_observation() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let observed = ObservedVersion::capture(&file).unwrap();

        let guard = FileVersionGuard::new();
        let new_version = guard
            .guarded_write(&file, &observed, b"hello, edited")
            .expect("guarded write should succeed against an unchanged file");

        assert_eq!(std::fs::read(&file).unwrap(), b"hello, edited");
        assert!(new_version.assert_current(&file).is_ok());
    }

    #[test]
    fn guarded_write_rejects_stale_observation() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let observed = ObservedVersion::capture(&file).unwrap();

        // Someone else (an agent edit) writes to the file after it was
        // opened but before the guarded save runs.
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&file, "concurrent agent edit").unwrap();

        let guard = FileVersionGuard::new();
        let error = guard
            .guarded_write(&file, &observed, b"user edit that would clobber it")
            .expect_err("guarded write must refuse a stale observation");

        match error {
            GuardedWriteError::Stale(rejection) => {
                assert_eq!(rejection.path, file);
                assert_eq!(rejection.observed, observed);
            }
            other => panic!("expected Stale, got {other:?}"),
        }
        // The concurrent edit must survive untouched.
        assert_eq!(std::fs::read(&file).unwrap(), b"concurrent agent edit");
    }

    #[test]
    fn guarded_write_allows_fresh_create_when_observed_as_absent() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("new.txt");
        let observed = ObservedVersion::capture(&file).unwrap();
        assert!(!observed.existed);

        let guard = FileVersionGuard::new();
        guard
            .guarded_write(&file, &observed, b"brand new content")
            .expect("guarded write should allow creating a file observed as absent");

        assert_eq!(std::fs::read(&file).unwrap(), b"brand new content");
    }

    #[test]
    fn guarded_write_rejects_concurrent_creation_of_previously_absent_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("new.txt");
        let observed = ObservedVersion::capture(&file).unwrap();
        assert!(!observed.existed);

        // Another writer created the file after it was observed as absent.
        std::fs::write(&file, "surprise").unwrap();

        let guard = FileVersionGuard::new();
        let error = guard
            .guarded_write(&file, &observed, b"clobber attempt")
            .expect_err("guarded write must refuse to overwrite a surprise concurrent create");

        assert!(matches!(error, GuardedWriteError::Stale(_)));
        assert_eq!(std::fs::read(&file).unwrap(), b"surprise");
    }

    #[test]
    fn file_version_guard_and_file_service_share_one_mutation_authority() {
        use std::sync::Barrier;

        let dir = tempdir().unwrap();
        let file = dir.path().join("shared.txt");
        std::fs::write(&file, "v1").unwrap();
        let observed = ObservedVersion::capture(&file).unwrap();
        let service = FileService::new();
        let guard = FileVersionGuard::from_file_service(service.clone());
        let barrier = Arc::new(Barrier::new(2));

        let direct_service = service.clone();
        let direct_file = file.clone();
        let direct_observed = observed.clone();
        let direct_barrier = barrier.clone();
        let direct = std::thread::spawn(move || {
            direct_barrier.wait();
            direct_service.write_with_observed_version(&direct_file, &direct_observed, b"direct")
        });

        let guard_file = file.clone();
        let guard_observed = observed.clone();
        let guard_barrier = barrier.clone();
        let facade = std::thread::spawn(move || {
            guard_barrier.wait();
            guard.guarded_write(&guard_file, &guard_observed, b"facade")
        });

        let direct = direct.join().unwrap();
        let facade = facade.join().unwrap();
        assert_eq!(
            usize::from(direct.is_ok()) + usize::from(facade.is_ok()),
            1,
            "two ordinary surfaces sharing FileService must serialize and stale-fence one writer"
        );
        let landed = std::fs::read_to_string(&file).unwrap();
        assert!(landed == "direct" || landed == "facade");
    }

    #[test]
    fn concurrent_saves_against_the_same_path_serialize() {
        use std::sync::Barrier;

        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let observed = ObservedVersion::capture(&file).unwrap();

        let guard = FileVersionGuard::new();
        let barrier = Arc::new(Barrier::new(2));

        let guard_a = Arc::clone(&guard);
        let observed_a = observed.clone();
        let file_a = file.clone();
        let barrier_a = Arc::clone(&barrier);
        let handle_a = std::thread::spawn(move || {
            barrier_a.wait();
            guard_a.guarded_write(&file_a, &observed_a, b"writer A")
        });

        let guard_b = Arc::clone(&guard);
        let observed_b = observed.clone();
        let file_b = file.clone();
        let barrier_b = Arc::clone(&barrier);
        let handle_b = std::thread::spawn(move || {
            barrier_b.wait();
            guard_b.guarded_write(&file_b, &observed_b, b"writer B")
        });

        let result_a = handle_a.join().unwrap();
        let result_b = handle_b.join().unwrap();

        // Exactly one of the two racing guarded saves (both starting from
        // the same observed version) must succeed; the other must observe
        // the first one's write and be rejected as stale. Neither may
        // silently clobber the other.
        let successes = [&result_a, &result_b]
            .iter()
            .filter(|result| result.is_ok())
            .count();
        assert_eq!(successes, 1, "exactly one racing guarded write should win");
    }
}
