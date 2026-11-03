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
    verified_libraries_in, NativeCatalog, NativeCatalogError, NativeLibrary, NativeStatus,
    VerifiedLibrary,
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
        if row.status == NativeStatus::StaleUnverified || !row.serves_host(os, arch) {
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
