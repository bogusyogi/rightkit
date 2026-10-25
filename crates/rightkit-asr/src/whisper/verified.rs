//! Verified load of the whisper.cpp library set ([`WhisperCpp::load_verified`]).
//!
//! The library is checked against the native-library catalog, together with the ggml files that
//! must sit beside it, before the loader maps anything: dlopen runs a library's initialisers, so a
//! file that is not the catalog's must never reach it. [`WhisperCpp::load`] stays the unverified
//! path (tests, development builds with a locally built library).
//!
//! [`WhisperCpp::load`]: super::WhisperCpp::load
//! [`WhisperCpp::load_verified`]: super::WhisperCpp::load_verified

use crate::error::Error;
use rightkit_ffmpeg::native::{
    verified_libraries_in, NativeCatalog, NativeCatalogError, NativeLibrary, NativeRecord,
    NativeStatus, VerifiedLibrary,
};
use std::fmt;
use std::path::Path;

/// Why a verified load did not produce a model.
#[derive(Debug)]
pub enum VerifiedLoadError {
    /// A library file is not the catalog's (a different digest, an unlisted name, a symbolic link,
    /// a row that is `planned`, a missing sibling): nothing was loaded.
    Verification(NativeCatalogError),
    /// Every file matched its row; the load itself failed (the library would not open, a symbol
    /// or the pinned version was wrong, the model was missing).
    Load(Error),
}

impl fmt::Display for VerifiedLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Verification(e) => write!(f, "whisper.cpp library verification: {e}"),
            Self::Load(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for VerifiedLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Verification(e) => Some(e),
            Self::Load(e) => Some(e),
        }
    }
}

impl From<VerifiedLoadError> for Error {
    /// A load error stays itself; a verification error becomes an [`Error::Model`].
    fn from(error: VerifiedLoadError) -> Self {
        match error {
            VerifiedLoadError::Load(e) => e,
            VerifiedLoadError::Verification(e) => {
                Error::Model(format!("whisper.cpp library verification: {e}"))
            }
        }
    }
}

/// Verify the whisper.cpp library at `library` and every ggml file the catalog lists for this host
/// beside it, all or nothing. The first entry of the result is the whisper.cpp library (load its
/// canonical [`VerifiedLibrary::path`]); the ggml files follow in catalog order.
///
/// The whisper.cpp file is matched by its file name among the `whisper-cpp` rows for `os` and
/// `arch` that may serve (`approved` or `pinned`). The ggml set is every `ggml` row of this host
/// except stale ones: each must sit in the same directory and match its row, so a row that is
/// still `planned` (no digest yet) refuses the load, as does an absent file. A ggml row for
/// another OS or architecture is not part of the set.
pub(crate) fn verify_set(
    catalog: &NativeCatalog,
    os: &str,
    arch: &str,
    library: &Path,
) -> Result<Vec<VerifiedLibrary>, NativeCatalogError> {
    let unsafe_path = |reason| NativeCatalogError::UnsafePath {
        path: library.to_owned(),
        reason,
    };
    let dir = library
        .parent()
        .ok_or_else(|| unsafe_path("the library path has no directory"))?;
    let name = library
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| unsafe_path("the library path has no UTF-8 file name"))?;

    let mut ggml_names: Vec<&str> = Vec::new();
    for row in catalog.records(NativeLibrary::Ggml) {
        if row.status == NativeStatus::StaleUnverified || !row_is_for_host(row, os, arch) {
            continue;
        }
        let file =
            row.artifact_file_name
                .as_deref()
                .ok_or_else(|| NativeCatalogError::Invalid {
                    id: row.id.clone(),
                    reason: "the ggml row has no artifactFileName".to_owned(),
                })?;
        if !ggml_names.contains(&file) {
            ggml_names.push(file);
        }
    }

    let mut entries = vec![(NativeLibrary::WhisperCpp, name)];
    entries.extend(
        ggml_names
            .into_iter()
            .map(|file| (NativeLibrary::Ggml, file)),
    );
    verified_libraries_in(catalog, os, arch, dir, &entries)
}

/// The platform rule of `NativeRecord::matches_platform` (`any`, an exact match, or `universal`
/// for either Apple architecture), which is private to the catalog crate.
fn row_is_for_host(row: &NativeRecord, os: &str, arch: &str) -> bool {
    let os_ok = row.os == "any" || row.os.eq_ignore_ascii_case(os);
    let arch_ok = row.arch == "any"
        || row.arch.eq_ignore_ascii_case(arch)
        || (row.arch == "universal"
            && (arch.eq_ignore_ascii_case("aarch64") || arch.eq_ignore_ascii_case("x86_64")));
    os_ok && arch_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whisper::{Whisper, WhisperCpp, WhisperModel, WhisperOptions};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const WHISPER: (&str, &[u8]) = ("libwhisper.1.9.3.dylib", b"fixture whisper bytes");
    const GGML: [(&str, &[u8]); 3] = [
        ("libggml.0.20.2.dylib", b"fixture ggml bytes"),
        ("libggml-base.0.20.2.dylib", b"fixture ggml-base bytes"),
        ("libggml-cpu.0.20.2.dylib", b"fixture ggml-cpu bytes"),
    ];

    /// The crate has no hashing dependency of its own: the catalog crate hashes a scratch file.
    fn sha256_hex(bytes: &[u8]) -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let scratch = std::env::temp_dir().join(format!(
            "rightkit-asr-digest-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&scratch, bytes).unwrap();
        let digest = rightkit_ffmpeg::sha256_file(&scratch).unwrap();
        let _ = std::fs::remove_file(&scratch);
        digest
    }

    fn row(
        id: &str,
        library: &str,
        os: &str,
        status: &str,
        file: &str,
        bytes: &[u8],
    ) -> serde_json::Value {
        json!({
            "id": id, "library": library, "version": "1.0.0", "os": os,
            "arch": if os == "macos" { "aarch64" } else { "x86_64" },
            "status": status, "linkage": "dynamic-library", "artifactFileName": file,
            "sha256": sha256_hex(bytes),
            "sourceUrl": "https://example.test/source", "licenseSpdx": "MIT",
            "noticeFiles": ["third-party/NOTICE.txt"],
            "verification": { "method": "file-sha256", "evidence": "fixture" }
        })
    }

    /// A macOS arm64 catalog: the whisper row with `whisper_status`, the three ggml rows with
    /// `ggml_status`, and a Windows ggml row that is not part of the macOS set.
    fn catalog(whisper_status: &str, ggml_status: &str) -> NativeCatalog {
        let mut rows = vec![row(
            "whisper-fixture-macos",
            "whisper-cpp",
            "macos",
            whisper_status,
            WHISPER.0,
            WHISPER.1,
        )];
        for (file, bytes) in GGML {
            rows.push(row(
                &format!("ggml-fixture-{file}"),
                "ggml",
                "macos",
                ggml_status,
                file,
                bytes,
            ));
        }
        rows.push(row(
            "ggml-fixture-windows",
            "ggml",
            "windows",
            "planned",
            "ggml-vulkan.dll",
            b"windows only",
        ));
        NativeCatalog::parse_native_only(
            &json!({ "schemaVersion": 1, "libraries": rows }).to_string(),
        )
        .unwrap()
    }

    struct Fixture(PathBuf);

    impl Fixture {
        /// A directory holding the whisper library and the three ggml files, byte for byte as
        /// the catalog describes them.
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "rightkit-asr-verified-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            for (file, bytes) in std::iter::once(WHISPER).chain(GGML) {
                std::fs::write(root.join(file), bytes).unwrap();
            }
            Self(root)
        }

        fn library(&self) -> PathBuf {
            self.0.join(WHISPER.0)
        }

        /// An existing model file (any bytes: the verified path never reads it before dlopen).
        fn model(&self) -> PathBuf {
            let model = self.0.join("model.bin");
            std::fs::write(&model, b"not a ggml model").unwrap();
            model
        }

        fn load(
            &self,
            catalog: &NativeCatalog,
            model: &Path,
        ) -> Result<WhisperCpp, VerifiedLoadError> {
            WhisperCpp::load_verified_in(
                catalog,
                "macos",
                "aarch64",
                &self.library(),
                model,
                1,
                None,
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mismatch_id(error: VerifiedLoadError) -> String {
        match error {
            VerifiedLoadError::Verification(NativeCatalogError::DigestMismatch { id, .. }) => id,
            other => panic!("expected a digest mismatch, got {other:?}"),
        }
    }

    #[test]
    fn the_whole_set_verifies_with_the_whisper_library_first() {
        let fixture = Fixture::new();
        let set = verify_set(
            &catalog("approved", "approved"),
            "macos",
            "aarch64",
            &fixture.library(),
        )
        .unwrap();
        let ids: Vec<&str> = set.iter().map(|library| library.id()).collect();
        assert_eq!(
            ids,
            [
                "whisper-fixture-macos",
                "ggml-fixture-libggml.0.20.2.dylib",
                "ggml-fixture-libggml-base.0.20.2.dylib",
                "ggml-fixture-libggml-cpu.0.20.2.dylib",
            ]
        );
        assert_eq!(set[0].path(), fixture.library());
        // The Windows ggml row is not this host's: its file is not required.
        assert!(!fixture.0.join("ggml-vulkan.dll").exists());
    }

    #[test]
    fn a_tampered_whisper_library_is_refused_before_the_model_or_dlopen() {
        let fixture = Fixture::new();
        std::fs::write(fixture.library(), b"tampered whisper").unwrap();
        // No model file either: the verification error comes first, not "model not found".
        let error = fixture
            .load(
                &catalog("approved", "approved"),
                &fixture.0.join("absent.bin"),
            )
            .err()
            .expect("tampered library");
        assert_eq!(mismatch_id(error), "whisper-fixture-macos");
    }

    #[test]
    fn a_tampered_ggml_sibling_is_refused() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join(GGML[1].0), b"tampered ggml-base").unwrap();
        let model = fixture.model();
        let error = fixture
            .load(&catalog("approved", "approved"), &model)
            .err()
            .expect("tampered sibling");
        assert_eq!(mismatch_id(error), "ggml-fixture-libggml-base.0.20.2.dylib");
    }

    #[test]
    fn an_absent_ggml_sibling_is_refused() {
        let fixture = Fixture::new();
        std::fs::remove_file(fixture.0.join(GGML[2].0)).unwrap();
        let model = fixture.model();
        let error = fixture
            .load(&catalog("approved", "approved"), &model)
            .err()
            .expect("absent sibling");
        assert!(matches!(
            error,
            VerifiedLoadError::Verification(NativeCatalogError::Io(_))
        ));
    }

    #[test]
    fn a_planned_row_is_refused_for_the_library_and_for_a_sibling() {
        let fixture = Fixture::new();
        let model = fixture.model();
        for (whisper, ggml) in [("planned", "approved"), ("approved", "planned")] {
            let error = fixture
                .load(&catalog(whisper, ggml), &model)
                .err()
                .expect("planned row");
            assert!(
                matches!(
                    error,
                    VerifiedLoadError::Verification(NativeCatalogError::NoApproved { .. })
                ),
                "{whisper}/{ggml}: {error:?}"
            );
        }
    }

    #[test]
    fn a_relative_library_path_is_refused() {
        let error = verify_set(
            &catalog("approved", "approved"),
            "macos",
            "aarch64",
            Path::new(WHISPER.0),
        )
        .unwrap_err();
        assert!(matches!(error, NativeCatalogError::UnsafePath { .. }));
    }

    #[test]
    fn the_verified_path_reaches_dlopen_and_fails_there_for_fixture_bytes() {
        let fixture = Fixture::new();
        let model = fixture.model();
        // Pinned rows serve too: that is how a library gets its first host run.
        let error = fixture
            .load(&catalog("pinned", "pinned"), &model)
            .err()
            .expect("fixture bytes are not a library");
        let VerifiedLoadError::Load(Error::Model(message)) = error else {
            panic!("expected the open-library error, got {error:?}");
        };
        assert!(
            message.starts_with(&format!("load {}", fixture.library().display())),
            "{message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_canonical_path_is_the_one_that_is_loaded() {
        let fixture = Fixture::new();
        let model = fixture.model();
        let alias = fixture.0.join("alias");
        std::os::unix::fs::symlink(&fixture.0, &alias).unwrap();
        let error = WhisperCpp::load_verified_in(
            &catalog("approved", "approved"),
            "macos",
            "aarch64",
            &alias.join(WHISPER.0),
            &model,
            1,
            None,
        )
        .err()
        .expect("fixture bytes are not a library");
        let VerifiedLoadError::Load(Error::Model(message)) = error else {
            panic!("expected the open-library error, got {error:?}");
        };
        assert!(
            message.starts_with(&format!("load {}", fixture.library().display())),
            "{message}"
        );
    }

    #[test]
    fn the_public_whisper_entry_verifies_before_it_loads() {
        let fixture = Fixture::new();
        let model = fixture.model();
        // The embedded catalog has no row with these fixture bytes on any host.
        let error = Whisper::load_verified(
            WhisperModel::Ggml {
                library: fixture.library(),
                model,
            },
            WhisperOptions::default(),
        )
        .err()
        .expect("not the catalog's library");
        assert!(
            matches!(error, VerifiedLoadError::Verification(_)),
            "{error:?}"
        );
    }

    #[test]
    fn a_verification_error_converts_to_a_model_error_and_a_load_error_stays_itself() {
        let verification =
            VerifiedLoadError::Verification(NativeCatalogError::NotVerifiable { id: "x".into() });
        assert!(matches!(Error::from(verification), Error::Model(m) if m.contains("verification")));
        let load = VerifiedLoadError::Load(Error::Config("c".into()));
        assert!(matches!(Error::from(load), Error::Config(m) if m == "c"));
    }
}
