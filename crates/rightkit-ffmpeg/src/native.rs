//! One approved native-library catalog for the whole Right Suite.
//!
//! Generalises the FFmpeg artifact catalog ([`crate::ArtifactCatalog`], which
//! stays untouched) to every native dependency apps may bundle or link:
//! FFmpeg, ONNX Runtime, PDFium, sqlite-vec, libgit2, GGML, whisper.cpp and
//! sherpa-onnx. Every record pins ONE version per library for the suite,
//! carries digests, SPDX licence, notices, platforms and a verification
//! method, and is checked fail-closed.
//!
//! FFmpeg records are *derived* from the legacy catalog JSON, so there is a
//! single source of truth for them.

use std::{collections::BTreeSet, fmt, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    sha256_file, Artifact, ArtifactCatalog, ArtifactStatus, MediaError, Tool,
    APPROVED_CATALOG_JSON, DECODE_VARIANT, TOOLCHAIN_RUNTIME_COMPONENTS,
};

pub const NATIVE_CATALOG_JSON: &str = include_str!("../catalog/native-libraries.json");

/// SPDX expressions the suite accepts. Anything else (notably plain GPL/AGPL)
/// is rejected until Adrian approves it explicitly.
pub const ALLOWED_LICENSES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-3-Clause",
    "MIT OR Apache-2.0",
    "LGPL-2.1-or-later",
    "GPL-2.0-only WITH GCC-exception-2.0",
];

/// The licence of Microsoft's DirectML redistributable: the "Microsoft Software License Terms"
/// text shipped in the `Microsoft.AI.DirectML` package. It is not an SPDX licence and not open
/// source, so it is not in [`ALLOWED_LICENSES`]. Approved by Adrian 2026-10-11 for the
/// [`NativeLibrary::Directml`] records only, on Windows only: no other library and no other OS
/// may carry it.
pub const DIRECTML_LICENSE: &str = "LicenseRef-Microsoft-DirectML";

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeLibrary {
    Ffmpeg,
    Onnxruntime,
    Pdfium,
    SqliteVec,
    Libgit2,
    Ggml,
    WhisperCpp,
    SherpaOnnx,
    /// `DirectML.dll`, which ONNX Runtime's DirectML execution provider loads on Windows.
    Directml,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeStatus {
    /// Digest verified against a real artifact or lock file; shippable.
    Approved,
    /// Version and upstream digest pinned from a donor, not yet verified here.
    Pinned,
    /// Version chosen for the suite; no digest yet. Never resolvable.
    Planned,
    StaleUnverified,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Linkage {
    DynamicLibrary,
    Executable,
    /// Built from a crates.io source crate; verified by its lock checksum.
    StaticCrate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationMethod {
    FileSha256,
    ArchiveSha256,
    CargoChecksum,
    None,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveRef {
    pub url: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CargoRef {
    #[serde(rename = "crate")]
    pub crate_name: String,
    pub version: String,
    pub checksum: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Verification {
    pub method: VerificationMethod,
    /// Where the digest was observed (artifact, lock file, donor pin).
    pub evidence: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeRecord {
    pub id: String,
    pub library: NativeLibrary,
    pub version: String,
    /// `windows`, `macos`, `linux`, or `any`.
    pub os: String,
    /// `aarch64`, `x86_64`, `universal` (both Apple slices) or `any`.
    pub arch: String,
    pub status: NativeStatus,
    pub linkage: Linkage,
    #[serde(default)]
    pub artifact_file_name: Option<String>,
    /// SHA-256 of the retained runtime file. For a re-signed Mach-O or PE it is provenance;
    /// verification matches `content_sha256` when the record has one.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Signature-independent content digest ([`crate::content_digest`]) of the runtime file.
    /// Rows without one are verified by whole-file `sha256`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    #[serde(default)]
    pub archive: Option<ArchiveRef>,
    #[serde(default)]
    pub cargo: Option<CargoRef>,
    pub source_url: String,
    pub license_spdx: String,
    #[serde(default)]
    pub notice_files: Vec<String>,
    #[serde(default)]
    pub required_build_flags: Vec<String>,
    pub verification: Verification,
}

#[non_exhaustive]
#[derive(Debug)]
pub enum NativeCatalogError {
    Json(serde_json::Error),
    Io(io::Error),
    Media(MediaError),
    UnsupportedSchema(u32),
    Invalid {
        id: String,
        reason: String,
    },
    MixedVersions {
        library: NativeLibrary,
        versions: Vec<String>,
    },
    NoApproved {
        library: NativeLibrary,
        os: String,
        arch: String,
    },
    Ambiguous {
        library: NativeLibrary,
        os: String,
        arch: String,
    },
    NotVerifiable {
        id: String,
    },
    DigestMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    CargoLockMismatch {
        id: String,
        reason: String,
    },
}

impl fmt::Display for NativeCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "native catalog JSON: {e}"),
            Self::Io(e) => write!(f, "native catalog I/O: {e}"),
            Self::Media(e) => write!(f, "{e}"),
            Self::UnsupportedSchema(v) => write!(f, "unsupported native catalog schema {v}"),
            Self::Invalid { id, reason } => write!(f, "native record {id}: {reason}"),
            Self::MixedVersions { library, versions } => {
                write!(
                    f,
                    "{library:?} pins more than one suite version: {versions:?}"
                )
            }
            Self::NoApproved { library, os, arch } => {
                write!(f, "no approved {library:?} artifact for {os}/{arch}")
            }
            Self::Ambiguous { library, os, arch } => {
                write!(f, "multiple approved {library:?} artifacts for {os}/{arch}")
            }
            Self::NotVerifiable { id } => write!(f, "{id} has no digest to verify against"),
            Self::DigestMismatch {
                id,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "{id}: digest mismatch (expected {expected}, got {actual})"
                )
            }
            Self::CargoLockMismatch { id, reason } => write!(f, "{id}: lock mismatch: {reason}"),
        }
    }
}

impl std::error::Error for NativeCatalogError {}

impl From<io::Error> for NativeCatalogError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<MediaError> for NativeCatalogError {
    fn from(e: MediaError) -> Self {
        Self::Media(e)
    }
}

fn invalid(id: &str, reason: impl Into<String>) -> NativeCatalogError {
    NativeCatalogError::Invalid {
        id: id.to_owned(),
        reason: reason.into(),
    }
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl NativeRecord {
    pub fn validate(&self) -> Result<(), NativeCatalogError> {
        for (field, v) in [
            ("id", &self.id),
            ("version", &self.version),
            ("os", &self.os),
            ("arch", &self.arch),
            ("sourceUrl", &self.source_url),
        ] {
            if v.trim().is_empty() {
                return Err(invalid(&self.id, format!("missing {field}")));
            }
        }
        let ffmpeg_with_runtime =
            self.library == NativeLibrary::Ffmpeg && is_ffmpeg_license(&self.license_spdx);
        let directml = self.library == NativeLibrary::Directml;
        if directml != (self.license_spdx == DIRECTML_LICENSE) {
            return Err(invalid(
                &self.id,
                "the DirectML licence and the DirectML library go together only",
            ));
        }
        if directml && self.os != "windows" {
            return Err(invalid(&self.id, "DirectML is approved on Windows only"));
        }
        if !ffmpeg_with_runtime
            && !directml
            && !ALLOWED_LICENSES.contains(&self.license_spdx.as_str())
        {
            return Err(invalid(
                &self.id,
                format!("unapproved licence {}", self.license_spdx),
            ));
        }
        if !self.source_url.starts_with("https://") {
            return Err(invalid(&self.id, "sourceUrl must be https"));
        }
        for d in self
            .sha256
            .iter()
            .chain(self.content_sha256.iter())
            .chain(self.archive.iter().map(|a| &a.sha256))
        {
            if !is_digest(d) {
                return Err(invalid(&self.id, "digest is not SHA-256 hex"));
            }
        }
        if let Some(c) = &self.cargo {
            if !is_digest(&c.checksum) {
                return Err(invalid(&self.id, "cargo checksum is not SHA-256 hex"));
            }
        }
        if self.library == NativeLibrary::Ffmpeg {
            if !is_ffmpeg_license(&self.license_spdx) {
                return Err(invalid(&self.id, "FFmpeg must be LGPL-2.1-or-later"));
            }
            let flags: Vec<String> = self
                .required_build_flags
                .iter()
                .map(|f| f.to_ascii_lowercase())
                .collect();
            for must in ["--disable-gpl", "--disable-nonfree"] {
                if !flags.iter().any(|f| f == must) {
                    return Err(invalid(&self.id, format!("FFmpeg missing {must}")));
                }
            }
            if flags
                .iter()
                .any(|f| f == "--enable-gpl" || f == "--enable-nonfree")
            {
                return Err(invalid(&self.id, "FFmpeg forbidden build flag"));
            }
        }
        match self.status {
            NativeStatus::Approved | NativeStatus::Pinned => {
                if self.notice_files.is_empty() && self.linkage != Linkage::StaticCrate {
                    return Err(invalid(
                        &self.id,
                        "shipped binary needs licence notice files",
                    ));
                }
                match (self.linkage, self.verification.method) {
                    (Linkage::StaticCrate, VerificationMethod::CargoChecksum) => {
                        if self.cargo.is_none() {
                            return Err(invalid(&self.id, "static crate needs cargo ref"));
                        }
                    }
                    (Linkage::StaticCrate, _) => {
                        return Err(invalid(&self.id, "static crate verifies by cargo-checksum"))
                    }
                    (_, VerificationMethod::FileSha256) => {
                        if self.sha256.is_none() || self.artifact_file_name.is_none() {
                            return Err(invalid(
                                &self.id,
                                "file-sha256 needs sha256 and artifactFileName",
                            ));
                        }
                    }
                    (_, VerificationMethod::ArchiveSha256) => {
                        if self.archive.is_none() {
                            return Err(invalid(&self.id, "archive-sha256 needs archive"));
                        }
                    }
                    (_, _) => {
                        return Err(invalid(
                            &self.id,
                            "binary needs a digest verification method",
                        ))
                    }
                }
                if self.status == NativeStatus::Approved
                    && self.linkage != Linkage::StaticCrate
                    && self.sha256.is_none()
                {
                    return Err(invalid(&self.id, "approved binary needs a file sha256"));
                }
            }
            NativeStatus::Planned | NativeStatus::StaleUnverified => {}
        }
        Ok(())
    }

    fn matches_platform(&self, os: &str, arch: &str) -> bool {
        let os_ok = self.os == "any" || self.os.eq_ignore_ascii_case(os);
        let arch_ok = self.arch == "any"
            || self.arch.eq_ignore_ascii_case(arch)
            || (self.arch == "universal"
                && (arch.eq_ignore_ascii_case("aarch64") || arch.eq_ignore_ascii_case("x86_64")));
        os_ok && arch_ok
    }

    /// Verify a real file against this record's shipped-file digest.
    pub fn verify_file(&self, path: &Path) -> Result<(), NativeCatalogError> {
        if let Some(expected) = self.content_sha256.as_deref() {
            return self.verify_content(path, expected);
        }
        let expected = self
            .sha256
            .as_deref()
            .ok_or_else(|| NativeCatalogError::NotVerifiable {
                id: self.id.clone(),
            })?;
        let actual = sha256_file(path)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(NativeCatalogError::DigestMismatch {
                id: self.id.clone(),
                expected: expected.to_owned(),
                actual,
            });
        }
        Ok(())
    }

    /// Match the file's signature-independent content digest for this record's architecture.
    fn verify_content(&self, path: &Path, expected: &str) -> Result<(), NativeCatalogError> {
        let identity = crate::content_identity_file(path)?;
        let actual = match self.arch.as_str() {
            "universal" => {
                return Err(NativeCatalogError::NotVerifiable {
                    id: self.id.clone(),
                })
            }
            "any" => identity.slices.first().map(|slice| slice.sha256.as_str()),
            arch => match identity.format {
                crate::ContentFormat::Plain => {
                    identity.slices.first().map(|slice| slice.sha256.as_str())
                }
                _ => identity.digest_for(arch),
            },
        };
        match actual {
            Some(actual) if actual.eq_ignore_ascii_case(expected) => Ok(()),
            other => Err(NativeCatalogError::DigestMismatch {
                id: self.id.clone(),
                expected: expected.to_owned(),
                actual: other.unwrap_or("no slice for this architecture").to_owned(),
            }),
        }
    }

    /// Verify a downloaded/staged archive against the upstream archive digest.
    pub fn verify_archive(&self, path: &Path) -> Result<(), NativeCatalogError> {
        let expected = self
            .archive
            .as_ref()
            .ok_or_else(|| NativeCatalogError::NotVerifiable {
                id: self.id.clone(),
            })?
            .sha256
            .clone();
        let actual = sha256_file(path)?;
        if !actual.eq_ignore_ascii_case(&expected) {
            return Err(NativeCatalogError::DigestMismatch {
                id: self.id.clone(),
                expected,
                actual,
            });
        }
        Ok(())
    }

    /// Verify that a `Cargo.lock` pins exactly this crate version and checksum.
    pub fn verify_cargo_lock(&self, lock_text: &str) -> Result<(), NativeCatalogError> {
        let cargo = self
            .cargo
            .as_ref()
            .ok_or_else(|| NativeCatalogError::NotVerifiable {
                id: self.id.clone(),
            })?;
        let mismatch = |reason: String| NativeCatalogError::CargoLockMismatch {
            id: self.id.clone(),
            reason,
        };
        for block in lock_text.split("[[package]]") {
            let field = |key: &str| {
                block.lines().find_map(|l| {
                    l.strip_prefix(key)?
                        .strip_prefix(" = \"")?
                        .strip_suffix('"')
                })
            };
            if field("name") == Some(cargo.crate_name.as_str())
                && field("version") == Some(cargo.version.as_str())
            {
                return match field("checksum") {
                    Some(c) if c.eq_ignore_ascii_case(&cargo.checksum) => Ok(()),
                    Some(c) => Err(mismatch(format!("checksum {c} != {}", cargo.checksum))),
                    None => Err(mismatch("package has no checksum".into())),
                };
            }
        }
        Err(mismatch(format!(
            "{} {} not in lock",
            cargo.crate_name, cargo.version
        )))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeCatalog {
    pub schema_version: u32,
    pub libraries: Vec<NativeRecord>,
}

impl NativeCatalog {
    /// The suite catalog: `native-libraries.json` plus the FFmpeg artifact
    /// catalog's decode-variant `ffmpeg` records converted in place. `ffprobe` and
    /// feature variants (for example `export-ass`) stay in the FFmpeg catalog,
    /// which selects them by tool and variant; here one FFmpeg record per platform
    /// keeps [`NativeCatalog::approved`] unambiguous.
    pub fn embedded() -> Result<Self, NativeCatalogError> {
        let mut catalog = Self::parse_native_only(NATIVE_CATALOG_JSON)?;
        let ffmpeg = ArtifactCatalog::parse(APPROVED_CATALOG_JSON)?;
        catalog.libraries.extend(
            ffmpeg
                .artifacts
                .iter()
                .filter(|a| a.tool == Tool::Ffmpeg && a.variant == DECODE_VARIANT)
                .map(ffmpeg_record),
        );
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn parse_native_only(json: &str) -> Result<Self, NativeCatalogError> {
        let catalog: Self = serde_json::from_str(json).map_err(NativeCatalogError::Json)?;
        if catalog.schema_version != 1 {
            return Err(NativeCatalogError::UnsupportedSchema(
                catalog.schema_version,
            ));
        }
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), NativeCatalogError> {
        let mut ids = BTreeSet::new();
        for r in &self.libraries {
            r.validate()?;
            if !ids.insert(r.id.as_str()) {
                return Err(invalid(&r.id, "duplicate id"));
            }
        }
        // One version per library across the suite (stale records excluded).
        let libs: BTreeSet<_> = self.libraries.iter().map(|r| r.library).collect();
        for library in libs {
            let versions: BTreeSet<String> = self
                .libraries
                .iter()
                .filter(|r| r.library == library && r.status != NativeStatus::StaleUnverified)
                .map(|r| r.version.clone())
                .collect();
            if versions.len() > 1 {
                return Err(NativeCatalogError::MixedVersions {
                    library,
                    versions: versions.into_iter().collect(),
                });
            }
        }
        Ok(())
    }

    pub fn records(&self, library: NativeLibrary) -> impl Iterator<Item = &NativeRecord> {
        self.libraries.iter().filter(move |r| r.library == library)
    }

    /// The single suite version for `library` (stale records ignored).
    pub fn suite_version(&self, library: NativeLibrary) -> Option<&str> {
        self.records(library)
            .find(|r| r.status != NativeStatus::StaleUnverified)
            .map(|r| r.version.as_str())
    }

    /// Exactly one approved record for the platform, like the FFmpeg rule.
    pub fn approved(
        &self,
        library: NativeLibrary,
        os: &str,
        arch: &str,
    ) -> Result<&NativeRecord, NativeCatalogError> {
        self.validate()?;
        let mut hits = self
            .records(library)
            .filter(|r| r.status == NativeStatus::Approved && r.matches_platform(os, arch));
        let first = hits.next().ok_or_else(|| NativeCatalogError::NoApproved {
            library,
            os: os.to_owned(),
            arch: arch.to_owned(),
        })?;
        if hits.next().is_some() {
            return Err(NativeCatalogError::Ambiguous {
                library,
                os: os.to_owned(),
                arch: arch.to_owned(),
            });
        }
        Ok(first)
    }

    /// The single approved record of a multi-file library (for example the ggml dylibs)
    /// for a platform and runtime file name. [`NativeCatalog::approved`] stays ambiguous
    /// for such a library on purpose.
    pub fn approved_file(
        &self,
        library: NativeLibrary,
        os: &str,
        arch: &str,
        file_name: &str,
    ) -> Result<&NativeRecord, NativeCatalogError> {
        self.validate()?;
        let mut hits = self.records(library).filter(|r| {
            r.status == NativeStatus::Approved
                && r.matches_platform(os, arch)
                && r.artifact_file_name.as_deref() == Some(file_name)
        });
        let first = hits.next().ok_or_else(|| NativeCatalogError::NoApproved {
            library,
            os: os.to_owned(),
            arch: arch.to_owned(),
        })?;
        if hits.next().is_some() {
            return Err(NativeCatalogError::Ambiguous {
                library,
                os: os.to_owned(),
                arch: arch.to_owned(),
            });
        }
        Ok(first)
    }
}

/// FFmpeg's own licence, optionally followed by the licences of the toolchain runtime a static
/// MinGW-w64 link carries into the binary, and by nothing else.
fn is_ffmpeg_license(expression: &str) -> bool {
    let mut terms = expression.split(" AND ").map(str::trim);
    terms.next() == Some("LGPL-2.1-or-later")
        && terms.all(|term| {
            TOOLCHAIN_RUNTIME_COMPONENTS
                .iter()
                .any(|(_, license)| license.split(" AND ").any(|allowed| allowed == term))
        })
}

fn ffmpeg_record(a: &Artifact) -> NativeRecord {
    NativeRecord {
        id: a.id.clone(),
        library: NativeLibrary::Ffmpeg,
        version: a.version.clone(),
        os: a.os.clone(),
        arch: a.arch.clone(),
        status: match a.status {
            ArtifactStatus::Approved => NativeStatus::Approved,
            ArtifactStatus::Pending => NativeStatus::Planned,
            ArtifactStatus::StaleUnverified => NativeStatus::StaleUnverified,
        },
        linkage: Linkage::Executable,
        artifact_file_name: Some(
            if a.os == "windows" {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            }
            .to_owned(),
        ),
        sha256: a.sha256.clone(),
        content_sha256: a.content_sha256.clone(),
        archive: None,
        cargo: None,
        source_url: a.attribution.source_url.clone(),
        license_spdx: a.attribution.license_spdx.clone(),
        notice_files: a.attribution.notice_files.clone(),
        required_build_flags: a.required_configure_flags.clone(),
        verification: Verification {
            method: VerificationMethod::FileSha256,
            evidence: format!("legacy FFmpeg catalog; source sha256 {}", a.source_sha256),
        },
    }
}
