//! One approved native-library catalog for the whole Right Suite.
//!
//! Generalises the FFmpeg artifact catalog ([`crate::ArtifactCatalog`], which
//! stays untouched) to every native dependency apps may bundle or link:
//! FFmpeg, ONNX Runtime, PDFium, sqlite-vec, libgit2, GGML, whisper.cpp and
//! sherpa-onnx. Every record pins ONE version per library and OS for the suite,
//! carries digests, SPDX licence, notices, platforms and a verification
//! method, and is checked fail-closed.
//!
//! FFmpeg records are *derived* from the legacy catalog JSON, so there is a
//! single source of truth for them.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt, fs, io,
    path::{Component, Path, PathBuf},
};

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

impl NativeStatus {
    /// `true` for the statuses that may serve a file (stage it or load it): `Approved` and
    /// `Pinned`. `Planned` has no digest and `StaleUnverified` is history, so neither serves.
    pub fn serves(self) -> bool {
        matches!(self, Self::Approved | Self::Pinned)
    }
}

/// How far a row's bytes were proven on a real host.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostProof {
    /// The bytes were never loaded or run on a host of the row's OS: a session without such a
    /// host measured the digests. Only legal on a row that is not `Approved`. Staging and loading
    /// still serve a pinned row (that is how host proof gets produced), and the marker travels
    /// into receipts and reports ([`VerifiedLibrary::host_proof`]) so nobody mistakes the row
    /// for proven.
    NeverRun,
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
    /// Path inside the archive of the shipped file (for example `bin/x64-win/DirectML.dll`).
    /// Needed where the archive holds several files with the same base name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
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
    /// Content digest of each slice of a `universal` (fat Mach-O) row, keyed `aarch64` and
    /// `x86_64`. Used instead of `content_sha256`, which a universal row cannot carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256_by_arch: Option<BTreeMap<String, String>>,
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
    /// Ids of the rows that must be staged and shipped beside this row's file (the Windows
    /// ONNX Runtime row lists its DirectML row).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_proof: Option<HostProof>,
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
    /// The path handed to [`verified_library`] is not a plain absolute regular file.
    UnsafePath {
        path: PathBuf,
        reason: &'static str,
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
                    "{library:?} pins more than one suite version for one OS: {versions:?}"
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
            Self::UnsafePath { path, reason } => {
                write!(f, "{}: unsafe library path: {reason}", path.display())
            }
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

/// An archive member path: forward slashes only, no drive letter, and no empty, `.` or `..`
/// component (so no leading or trailing `/`), which keeps it relative, a file name and inside the
/// extraction root.
fn is_archive_member(member: &str) -> bool {
    !member.contains('\\')
        && !matches!(member.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
        && member
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
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
        if let Some(by_arch) = &self.content_sha256_by_arch {
            if self.arch != "universal" {
                return Err(invalid(
                    &self.id,
                    "contentSha256ByArch needs arch universal",
                ));
            }
            if self.content_sha256.is_some() {
                return Err(invalid(
                    &self.id,
                    "contentSha256ByArch and contentSha256 exclude each other",
                ));
            }
            // Both slices are pinned: a universal row that left one unlisted would load that
            // slice unchecked.
            if !by_arch.keys().map(String::as_str).eq(["aarch64", "x86_64"]) {
                return Err(invalid(
                    &self.id,
                    "contentSha256ByArch needs exactly aarch64 and x86_64",
                ));
            }
            if !by_arch.values().all(|d| is_digest(d)) {
                return Err(invalid(&self.id, "digest is not SHA-256 hex"));
            }
        }
        if let Some(member) = self.archive.as_ref().and_then(|a| a.member.as_deref()) {
            if !is_archive_member(member) {
                return Err(invalid(
                    &self.id,
                    "archive.member must be a relative path of /-separated names without '..'",
                ));
            }
        }
        if self.companions.iter().any(|c| c.trim().is_empty()) {
            return Err(invalid(&self.id, "empty companion id"));
        }
        if self.companions.contains(&self.id) {
            return Err(invalid(&self.id, "a row cannot be its own companion"));
        }
        if self.host_proof.is_some() && self.status == NativeStatus::Approved {
            return Err(invalid(
                &self.id,
                "hostProof never-run is only legal on a row that is not approved",
            ));
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

    fn matches_arch(&self, arch: &str) -> bool {
        self.arch == "any"
            || self.arch.eq_ignore_ascii_case(arch)
            || (self.arch == "universal"
                && (arch.eq_ignore_ascii_case("aarch64") || arch.eq_ignore_ascii_case("x86_64")))
    }

    fn matches_platform(&self, os: &str, arch: &str) -> bool {
        let os_ok = self.os == "any" || self.os.eq_ignore_ascii_case(os);
        os_ok && self.matches_arch(arch)
    }

    /// `true` when some host architecture is served by both rows, so a file of one can sit
    /// beside a file of the other.
    fn shares_arch_with(&self, other: &Self) -> bool {
        self.arch.eq_ignore_ascii_case(&other.arch)
            || ["aarch64", "x86_64"]
                .iter()
                .any(|arch| self.matches_arch(arch) && other.matches_arch(arch))
    }

    /// Verify a real file against this record's shipped-file digest: each slice's content digest
    /// for a universal row with `content_sha256_by_arch`, else `content_sha256` when the row has
    /// one, else the whole-file `sha256`.
    pub fn verify_file(&self, path: &Path) -> Result<(), NativeCatalogError> {
        if let Some(by_arch) = &self.content_sha256_by_arch {
            return self.verify_slices(path, by_arch);
        }
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

    /// Every listed slice must match the file's digest for that architecture; a slice the file
    /// does not have is a mismatch. A row that is not universal, or lists no slice, cannot be
    /// verified this way.
    fn verify_slices(
        &self,
        path: &Path,
        by_arch: &BTreeMap<String, String>,
    ) -> Result<(), NativeCatalogError> {
        if self.arch != "universal" || by_arch.is_empty() {
            return Err(NativeCatalogError::NotVerifiable {
                id: self.id.clone(),
            });
        }
        let identity = crate::content_identity_file(path)?;
        for (arch, expected) in by_arch {
            match identity.digest_for(arch) {
                Some(actual) if actual.eq_ignore_ascii_case(expected) => {}
                other => {
                    return Err(NativeCatalogError::DigestMismatch {
                        id: self.id.clone(),
                        expected: expected.clone(),
                        actual: other.map_or_else(|| format!("no {arch} slice"), str::to_owned),
                    })
                }
            }
        }
        Ok(())
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
        for r in &self.libraries {
            for companion in &r.companions {
                let Some(other) = self.libraries.iter().find(|c| c.id == *companion) else {
                    return Err(invalid(
                        &r.id,
                        format!("companion {companion} is not in the catalog"),
                    ));
                };
                if other.os != r.os {
                    return Err(invalid(
                        &r.id,
                        format!("companion {companion} is for {}, not {}", other.os, r.os),
                    ));
                }
                if !r.shares_arch_with(other) {
                    return Err(invalid(
                        &r.id,
                        format!("companion {companion} serves no architecture this row serves"),
                    ));
                }
                if other.status == NativeStatus::StaleUnverified {
                    return Err(invalid(
                        &r.id,
                        format!("companion {companion} is stale-unverified"),
                    ));
                }
            }
        }
        // One version per library and OS across the suite (stale records excluded). The rule is
        // per OS, not suite-wide, because ONNX Runtime diverges by OS (macOS 1.30.0, Windows
        // 1.24.4) until RightKit CI builds its own universal2 ORT: bogusyogi/rightkit#889.
        let pins: BTreeSet<(NativeLibrary, &str)> = self
            .libraries
            .iter()
            .filter(|r| r.status != NativeStatus::StaleUnverified)
            .map(|r| (r.library, r.os.as_str()))
            .collect();
        for (library, os) in pins {
            let versions: BTreeSet<String> = self
                .libraries
                .iter()
                .filter(|r| {
                    r.library == library && r.os == os && r.status != NativeStatus::StaleUnverified
                })
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

    /// The suite version of the first non-stale record for `library` (stale records ignored).
    /// ONNX Runtime pins a different version per OS (bogusyogi/rightkit#889); use
    /// [`Self::suite_version_for`] when the OS matters.
    pub fn suite_version(&self, library: NativeLibrary) -> Option<&str> {
        self.records(library)
            .find(|r| r.status != NativeStatus::StaleUnverified)
            .map(|r| r.version.as_str())
    }

    /// The suite version for `library` on `os` (stale records ignored).
    pub fn suite_version_for(&self, library: NativeLibrary, os: &str) -> Option<&str> {
        self.records(library)
            .find(|r| r.status != NativeStatus::StaleUnverified && r.os.eq_ignore_ascii_case(os))
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
        let hits = prefer_exact_arch(
            self.records(library)
                .filter(|r| r.status == NativeStatus::Approved && r.matches_platform(os, arch))
                .collect(),
            arch,
        );
        exactly_one(hits, library, os, arch)
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
        let hits = prefer_exact_arch(
            self.records(library)
                .filter(|r| {
                    r.status == NativeStatus::Approved
                        && r.matches_platform(os, arch)
                        && r.artifact_file_name.as_deref() == Some(file_name)
                })
                .collect(),
            arch,
        );
        exactly_one(hits, library, os, arch)
    }

    /// [`NativeCatalog::approved_file`] over every row that may serve ([`NativeStatus::serves`]):
    /// `Approved` and `Pinned`. A pinned row has digests but no host proof yet; it is what staging
    /// and loading use to produce that proof. `Planned` and `StaleUnverified` rows never match.
    /// An exact-architecture row beats a `universal` or `any` one.
    pub fn serving_file(
        &self,
        library: NativeLibrary,
        os: &str,
        arch: &str,
        file_name: &str,
    ) -> Result<&NativeRecord, NativeCatalogError> {
        self.validate()?;
        let hits = prefer_exact_arch(
            self.records(library)
                .filter(|r| {
                    r.status.serves()
                        && r.matches_platform(os, arch)
                        && r.artifact_file_name.as_deref() == Some(file_name)
                })
                .collect(),
            arch,
        );
        exactly_one(hits, library, os, arch)
    }
}

fn exactly_one<'a>(
    hits: Vec<&'a NativeRecord>,
    library: NativeLibrary,
    os: &str,
    arch: &str,
) -> Result<&'a NativeRecord, NativeCatalogError> {
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => Err(NativeCatalogError::NoApproved {
            library,
            os: os.to_owned(),
            arch: arch.to_owned(),
        }),
        _ => Err(NativeCatalogError::Ambiguous {
            library,
            os: os.to_owned(),
            arch: arch.to_owned(),
        }),
    }
}

/// When several approved records match a platform, an exact-architecture record wins over a
/// `universal` or `any` one (a universal2 row and a thin row for the same arch are not ambiguous).
/// Two records of the same exact architecture stay ambiguous.
fn prefer_exact_arch<'a>(hits: Vec<&'a NativeRecord>, arch: &str) -> Vec<&'a NativeRecord> {
    if hits.len() < 2 {
        return hits;
    }
    let exact: Vec<&'a NativeRecord> = hits
        .iter()
        .copied()
        .filter(|r| r.arch.eq_ignore_ascii_case(arch))
        .collect();
    if exact.is_empty() {
        hits
    } else {
        exact
    }
}

/// A native library file that matched its catalog row when it was checked: the token
/// [`verified_library`] returns, with what a Doctor or a receipt needs to report it (for example
/// "pinned, never run": [`Self::status`] and [`Self::host_proof`]).
///
/// It cannot be built outside this module, so holding one means a check ran. It proves the
/// file's content at that moment only: see [`verified_library`] for the window before the load.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedLibrary {
    id: String,
    library: NativeLibrary,
    version: String,
    path: PathBuf,
    status: NativeStatus,
    host_proof: Option<HostProof>,
}

impl VerifiedLibrary {
    /// The catalog row id the file matched.
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn library(&self) -> NativeLibrary {
        self.library
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// The canonical path that was verified: load this one, not the path that was passed in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `Approved` or `Pinned`; no other status verifies.
    pub fn status(&self) -> NativeStatus {
        self.status
    }

    /// `Some(HostProof::NeverRun)` for a row whose bytes were never run on a host of its OS.
    pub fn host_proof(&self) -> Option<HostProof> {
        self.host_proof
    }
}

/// Verify the native library file at `path` against the embedded catalog for this host's OS and
/// architecture, immediately before the caller loads it.
///
/// The row is chosen by `library`, this host and the file name (it must equal the row's
/// `artifactFileName`) among the rows that may serve ([`NativeStatus::serves`]: `Approved` and
/// `Pinned`); an exact-architecture row beats a `universal` or `any` one. No row is
/// [`NativeCatalogError::NoApproved`], several are [`NativeCatalogError::Ambiguous`], and a
/// `Planned` or `StaleUnverified` row never serves. The file is then checked with
/// [`NativeRecord::verify_file`]: its content digest when the row has one (each slice of a
/// universal Mach-O for `contentSha256ByArch`), else its whole-file `sha256`.
///
/// `path` must be absolute and free of `..`, and the file must exist, must not be a symbolic
/// link and must be a regular file ([`NativeCatalogError::UnsafePath`]; a missing file is an
/// [`NativeCatalogError::Io`] `NotFound`). Symbolic links in the directories above the file are
/// resolved: [`VerifiedLibrary::path`] is the canonical path, and that is the file that was
/// hashed.
///
/// The digest is checked now, not while the loader maps the file: a local writer that replaces
/// the file between this call and the load is outside the check, the same window
/// `rightkit_onnx::configure_runtime_verified` documents. Keep the file in a directory only the
/// app can write, and call this right before the load.
pub fn verified_library(
    library: NativeLibrary,
    path: &Path,
) -> Result<VerifiedLibrary, NativeCatalogError> {
    verified_library_in(
        &NativeCatalog::embedded()?,
        env::consts::OS,
        env::consts::ARCH,
        library,
        path,
    )
}

/// [`verified_library`] against an explicit catalog and platform (`os` is `macos`, `windows` or
/// `linux`; `arch` is `aarch64` or `x86_64`), for tests and for tools that stage another
/// platform's files.
pub fn verified_library_in(
    catalog: &NativeCatalog,
    os: &str,
    arch: &str,
    library: NativeLibrary,
    path: &Path,
) -> Result<VerifiedLibrary, NativeCatalogError> {
    let file_name = plain_regular_file(path)?;
    let canonical = fs::canonicalize(path)?;
    let record = catalog.serving_file(library, os, arch, file_name)?;
    record.verify_file(&canonical)?;
    Ok(VerifiedLibrary {
        id: record.id.clone(),
        library,
        version: record.version.clone(),
        path: canonical,
        status: record.status,
        host_proof: record.host_proof,
    })
}

/// [`verified_library`] for a set that is loaded together, such as whisper.cpp and the ggml files
/// that must sit beside it: each `(library, file name)` entry names a file directly inside `dir`
/// (a bare file name, no separators). All or nothing: the first entry that fails returns its
/// error and no entry's result is returned. The results are in the order of `entries`. List the
/// [`NativeRecord::companions`] of a row as entries too; this does not add them.
pub fn verified_libraries(
    dir: &Path,
    entries: &[(NativeLibrary, &str)],
) -> Result<Vec<VerifiedLibrary>, NativeCatalogError> {
    verified_libraries_in(
        &NativeCatalog::embedded()?,
        env::consts::OS,
        env::consts::ARCH,
        dir,
        entries,
    )
}

/// [`verified_libraries`] against an explicit catalog and platform; see [`verified_library_in`].
pub fn verified_libraries_in(
    catalog: &NativeCatalog,
    os: &str,
    arch: &str,
    dir: &Path,
    entries: &[(NativeLibrary, &str)],
) -> Result<Vec<VerifiedLibrary>, NativeCatalogError> {
    entries
        .iter()
        .map(|(library, file_name)| {
            let path = dir.join(file_name);
            let mut parts = Path::new(file_name).components();
            match (parts.next(), parts.next()) {
                (Some(Component::Normal(_)), None) => {}
                _ => {
                    return Err(NativeCatalogError::UnsafePath {
                        path,
                        reason: "the entry is not a bare file name",
                    })
                }
            }
            verified_library_in(catalog, os, arch, *library, &path)
        })
        .collect()
}

/// The file name of `path` once it is known to be an absolute, `..`-free path to an existing
/// regular file that is not itself a symbolic link.
fn plain_regular_file(path: &Path) -> Result<&str, NativeCatalogError> {
    let refuse = |reason| NativeCatalogError::UnsafePath {
        path: path.to_owned(),
        reason,
    };
    if !path.is_absolute() {
        return Err(refuse("the path is not absolute"));
    }
    if path.components().any(|part| part == Component::ParentDir) {
        return Err(refuse("the path contains '..'"));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| refuse("the path has no UTF-8 file name"))?;
    let kind = fs::symlink_metadata(path)?.file_type();
    if kind.is_symlink() {
        return Err(refuse("the library is a symbolic link"));
    }
    if !kind.is_file() {
        return Err(refuse("the library is not a regular file"));
    }
    Ok(name)
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
        content_sha256_by_arch: None,
        archive: None,
        cargo: None,
        source_url: a.attribution.source_url.clone(),
        license_spdx: a.attribution.license_spdx.clone(),
        notice_files: a.attribution.notice_files.clone(),
        required_build_flags: a.required_configure_flags.clone(),
        companions: Vec::new(),
        host_proof: None,
        verification: Verification {
            method: VerificationMethod::FileSha256,
            evidence: format!("legacy FFmpeg catalog; source sha256 {}", a.source_sha256),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_digest::{
        content_identity,
        tests::{fat, macho},
        CPU_TYPE_ARM64, CPU_TYPE_X86_64,
    };
    use serde_json::{json, Value};

    const BYTES: &[u8] = b"fixture library bytes";
    const PAYLOAD: &[u8] = b"symtab-and-strtab-bytes!";

    fn sha(bytes: &[u8]) -> String {
        rightkit_fs::digest::sha256_bytes(bytes)
    }

    /// A shipped-library row: `fields` adds or replaces keys, `null` removes one.
    fn row(id: &str, file: &str, status: &str, fields: Value) -> Value {
        let mut value = json!({
            "id": id,
            "library": "whisper-cpp",
            "version": "1.0.0",
            "os": "macos",
            "arch": "aarch64",
            "status": status,
            "linkage": "dynamic-library",
            "artifactFileName": file,
            "sha256": sha(BYTES),
            "sourceUrl": "https://example.com/whisper",
            "licenseSpdx": "MIT",
            "noticeFiles": ["third-party/whisper.cpp-MIT.txt"],
            "verification": { "method": "file-sha256", "evidence": "fixture" }
        });
        for (key, v) in fields.as_object().unwrap() {
            if v.is_null() {
                value.as_object_mut().unwrap().remove(key);
            } else {
                value[key] = v.clone();
            }
        }
        value
    }

    fn parse(rows: Vec<Value>) -> Result<NativeCatalog, NativeCatalogError> {
        let catalog = NativeCatalog::parse_native_only(
            &json!({ "schemaVersion": 1, "libraries": rows }).to_string(),
        )?;
        catalog.validate()?;
        Ok(catalog)
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn check(catalog: &NativeCatalog, path: &Path) -> Result<VerifiedLibrary, NativeCatalogError> {
        verified_library_in(catalog, "macos", "aarch64", NativeLibrary::WhisperCpp, path)
    }

    #[test]
    fn an_approved_row_verifies_and_reports_what_it_matched() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "libwhisper.dylib", BYTES);
        let catalog = parse(vec![row(
            "whisper-1",
            "libwhisper.dylib",
            "approved",
            json!({}),
        )])
        .unwrap();
        let verified = check(&catalog, &path).unwrap();
        assert_eq!(verified.id(), "whisper-1");
        assert_eq!(verified.library(), NativeLibrary::WhisperCpp);
        assert_eq!(verified.version(), "1.0.0");
        assert_eq!(verified.path(), fs::canonicalize(&path).unwrap());
        assert_eq!(verified.status(), NativeStatus::Approved);
        assert_eq!(verified.host_proof(), None);
    }

    #[test]
    fn a_tampered_file_is_a_digest_mismatch_checked_at_call_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "libwhisper.dylib", BYTES);
        let catalog = parse(vec![row(
            "whisper-1",
            "libwhisper.dylib",
            "approved",
            json!({}),
        )])
        .unwrap();
        check(&catalog, &path).unwrap();
        // The same path, the same catalog: the file changed after the first check.
        write(dir.path(), "libwhisper.dylib", b"fixture library bytez");
        assert!(matches!(
            check(&catalog, &path),
            Err(NativeCatalogError::DigestMismatch { ref id, .. }) if id == "whisper-1"
        ));
    }

    #[test]
    fn a_pinned_row_verifies_and_carries_its_host_proof() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "whisper.dll", BYTES);
        let catalog = parse(vec![row(
            "whisper-win",
            "whisper.dll",
            "pinned",
            json!({ "os": "windows", "hostProof": "never-run" }),
        )])
        .unwrap();
        let verified = verified_library_in(
            &catalog,
            "windows",
            "aarch64",
            NativeLibrary::WhisperCpp,
            &path,
        )
        .unwrap();
        assert_eq!(verified.status(), NativeStatus::Pinned);
        assert_eq!(verified.host_proof(), Some(HostProof::NeverRun));
    }

    #[test]
    fn planned_and_stale_rows_never_serve_even_with_a_matching_digest() {
        let dir = tempfile::tempdir().unwrap();
        let planned = write(dir.path(), "planned.dylib", BYTES);
        let stale = write(dir.path(), "stale.dylib", BYTES);
        let catalog = parse(vec![
            row("whisper-planned", "planned.dylib", "planned", json!({})),
            row(
                "whisper-stale",
                "stale.dylib",
                "stale-unverified",
                json!({}),
            ),
        ])
        .unwrap();
        for path in [planned, stale] {
            assert!(matches!(
                check(&catalog, &path),
                Err(NativeCatalogError::NoApproved { .. })
            ));
        }
    }

    #[test]
    fn a_file_name_the_catalog_does_not_list_is_not_approved() {
        let dir = tempfile::tempdir().unwrap();
        let other = write(dir.path(), "other.dylib", BYTES);
        let catalog = parse(vec![row(
            "whisper-1",
            "libwhisper.dylib",
            "approved",
            json!({}),
        )])
        .unwrap();
        assert!(matches!(
            check(&catalog, &other),
            Err(NativeCatalogError::NoApproved { .. })
        ));
        // Another library, another OS or another architecture does not match either.
        let listed = write(dir.path(), "libwhisper.dylib", BYTES);
        let wrong_library =
            verified_library_in(&catalog, "macos", "aarch64", NativeLibrary::Ggml, &listed);
        assert!(matches!(
            wrong_library,
            Err(NativeCatalogError::NoApproved { .. })
        ));
        let wrong_os = verified_library_in(
            &catalog,
            "windows",
            "aarch64",
            NativeLibrary::WhisperCpp,
            &listed,
        );
        assert!(matches!(
            wrong_os,
            Err(NativeCatalogError::NoApproved { .. })
        ));
        let wrong_arch = verified_library_in(
            &catalog,
            "macos",
            "x86_64",
            NativeLibrary::WhisperCpp,
            &listed,
        );
        assert!(matches!(
            wrong_arch,
            Err(NativeCatalogError::NoApproved { .. })
        ));
    }

    #[test]
    fn two_rows_for_one_file_are_ambiguous_unless_one_is_the_exact_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "libwhisper.dylib", BYTES);
        let twice = parse(vec![
            row("a", "libwhisper.dylib", "approved", json!({})),
            row("b", "libwhisper.dylib", "pinned", json!({})),
        ])
        .unwrap();
        assert!(matches!(
            check(&twice, &path),
            Err(NativeCatalogError::Ambiguous { .. })
        ));
        let by_arch = |digest: &str| json!({ "aarch64": digest, "x86_64": digest });
        let exact = parse(vec![
            row(
                "universal",
                "libwhisper.dylib",
                "approved",
                json!({ "arch": "universal", "contentSha256ByArch": by_arch(&sha(b"x")) }),
            ),
            row("exact", "libwhisper.dylib", "pinned", json!({})),
        ])
        .unwrap();
        assert_eq!(check(&exact, &path).unwrap().id(), "exact");
    }

    #[test]
    fn a_relative_a_dotdot_a_directory_and_a_missing_path_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "libwhisper.dylib", BYTES);
        let catalog = parse(vec![row(
            "whisper-1",
            "libwhisper.dylib",
            "approved",
            json!({}),
        )])
        .unwrap();

        let relative = check(&catalog, Path::new("libwhisper.dylib"));
        assert!(matches!(
            relative,
            Err(NativeCatalogError::UnsafePath { reason, .. }) if reason.contains("not absolute")
        ));

        fs::create_dir(dir.path().join("sub")).unwrap();
        let dotdot = dir.path().join("sub").join("..").join("libwhisper.dylib");
        assert!(dotdot.is_file(), "the path resolves to the verified file");
        assert!(matches!(
            check(&catalog, &dotdot),
            Err(NativeCatalogError::UnsafePath { reason, .. }) if reason.contains("'..'")
        ));

        let directory = dir.path().join("dir");
        fs::create_dir(&directory).unwrap();
        let named_like_a_library = directory.join("libwhisper.dylib");
        fs::create_dir(&named_like_a_library).unwrap();
        assert!(matches!(
            check(&catalog, &named_like_a_library),
            Err(NativeCatalogError::UnsafePath { reason, .. }) if reason.contains("regular file")
        ));

        fs::remove_file(&path).unwrap();
        assert!(matches!(
            check(&catalog, &path),
            Err(NativeCatalogError::Io(ref e)) if e.kind() == io::ErrorKind::NotFound
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused_but_a_symlinked_directory_is_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let target = write(&real, "libwhisper.dylib", BYTES);
        let catalog = parse(vec![
            row("whisper-1", "libwhisper.dylib", "approved", json!({})),
            row("whisper-link", "link.dylib", "approved", json!({})),
        ])
        .unwrap();

        let link = dir.path().join("link.dylib");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // The target has the right digest and the link has a row: only the link is refused.
        assert!(matches!(
            check(&catalog, &link),
            Err(NativeCatalogError::UnsafePath { reason, .. }) if reason.contains("symbolic link")
        ));

        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let verified = check(&catalog, &alias.join("libwhisper.dylib")).unwrap();
        assert_eq!(verified.path(), fs::canonicalize(&target).unwrap());
    }

    /// A universal fixture row: the digests are those of the two thin slices.
    fn universal_row(arm: &[u8], x86: &[u8], status: &str) -> Value {
        let slice = |bytes: &[u8]| content_identity(bytes).unwrap().slices[0].sha256.clone();
        row(
            "ort-universal",
            "libort.dylib",
            status,
            json!({
                "library": "onnxruntime",
                "arch": "universal",
                "sha256": sha(b"the upstream bytes, before any re-signing"),
                "contentSha256ByArch": { "aarch64": slice(arm), "x86_64": slice(x86) }
            }),
        )
    }

    fn verify_ort(
        catalog: &NativeCatalog,
        arch: &str,
        path: &Path,
    ) -> Result<VerifiedLibrary, NativeCatalogError> {
        verified_library_in(catalog, "macos", arch, NativeLibrary::Onnxruntime, path)
    }

    #[test]
    fn a_universal_row_verifies_every_slice_whatever_the_signature() {
        let arm = macho(CPU_TYPE_ARM64, PAYLOAD, None);
        let x86 = macho(CPU_TYPE_X86_64, b"other-linkedit", None);
        let catalog = parse(vec![universal_row(&arm, &x86, "approved")]).unwrap();
        let dir = tempfile::tempdir().unwrap();

        // The unsigned build, and a copy re-signed slice by slice (an app bundle's copy).
        let unsigned = write(
            dir.path(),
            "libort.dylib",
            &fat(&[(CPU_TYPE_ARM64, &arm), (CPU_TYPE_X86_64, &x86)], false),
        );
        let resigned_arm = macho(CPU_TYPE_ARM64, PAYLOAD, Some(&[0x11; 300]));
        let resigned_x86 = macho(CPU_TYPE_X86_64, b"other-linkedit", Some(&[0x22; 900]));
        let resigned_dir = dir.path().join("bundle");
        fs::create_dir(&resigned_dir).unwrap();
        let resigned = write(
            &resigned_dir,
            "libort.dylib",
            &fat(
                &[
                    (CPU_TYPE_ARM64, &resigned_arm),
                    (CPU_TYPE_X86_64, &resigned_x86),
                ],
                true,
            ),
        );
        for path in [&unsigned, &resigned] {
            for arch in ["aarch64", "x86_64"] {
                let verified = verify_ort(&catalog, arch, path).unwrap();
                assert_eq!(verified.id(), "ort-universal");
            }
        }
    }

    #[test]
    fn a_universal_row_refuses_a_wrong_or_missing_slice_even_on_the_other_host() {
        let arm = macho(CPU_TYPE_ARM64, PAYLOAD, None);
        let x86 = macho(CPU_TYPE_X86_64, b"other-linkedit", None);
        let catalog = parse(vec![universal_row(&arm, &x86, "approved")]).unwrap();
        let dir = tempfile::tempdir().unwrap();

        // Only the x86_64 slice differs; an arm64 host still refuses the file.
        let swapped = macho(CPU_TYPE_X86_64, b"swapped-linkedit", None);
        let wrong = write(
            dir.path(),
            "libort.dylib",
            &fat(
                &[(CPU_TYPE_ARM64, &arm), (CPU_TYPE_X86_64, &swapped)],
                false,
            ),
        );
        for arch in ["aarch64", "x86_64"] {
            assert!(matches!(
                verify_ort(&catalog, arch, &wrong),
                Err(NativeCatalogError::DigestMismatch { ref id, .. }) if id == "ort-universal"
            ));
        }

        // A thin arm64 file and a plain file lack the x86_64 slice: a mismatch, not a pass.
        for bytes in [arm.clone(), b"not a mach-o".to_vec()] {
            let thin = write(dir.path(), "libort.dylib", &bytes);
            match verify_ort(&catalog, "aarch64", &thin) {
                Err(NativeCatalogError::DigestMismatch { actual, .. }) => {
                    assert!(actual.starts_with("no "), "{actual}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_pinned_universal_row_serves_like_an_approved_one() {
        let arm = macho(CPU_TYPE_ARM64, PAYLOAD, None);
        let x86 = macho(CPU_TYPE_X86_64, b"other-linkedit", None);
        let catalog = parse(vec![universal_row(&arm, &x86, "pinned")]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "libort.dylib",
            &fat(&[(CPU_TYPE_ARM64, &arm), (CPU_TYPE_X86_64, &x86)], false),
        );
        assert_eq!(
            verify_ort(&catalog, "x86_64", &path).unwrap().status(),
            NativeStatus::Pinned
        );
    }

    #[test]
    fn a_row_without_a_digest_for_its_file_cannot_verify() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "libwhisper.dylib", BYTES);
        let mut record: NativeRecord = serde_json::from_value(row(
            "whisper-1",
            "libwhisper.dylib",
            "approved",
            json!({ "sha256": null }),
        ))
        .unwrap();
        assert!(matches!(
            record.verify_file(&path),
            Err(NativeCatalogError::NotVerifiable { .. })
        ));
        // A by-arch map that lists nothing, or sits on a thin row, is not a digest either.
        record.arch = "universal".into();
        record.content_sha256_by_arch = Some(BTreeMap::new());
        assert!(matches!(
            record.verify_file(&path),
            Err(NativeCatalogError::NotVerifiable { .. })
        ));
        record.arch = "aarch64".into();
        record.content_sha256_by_arch = Some(BTreeMap::from([("aarch64".to_owned(), sha(BYTES))]));
        assert!(matches!(
            record.verify_file(&path),
            Err(NativeCatalogError::NotVerifiable { .. })
        ));
    }

    #[test]
    fn a_set_verifies_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ggml = b"ggml bytes";
        write(dir.path(), "libwhisper.dylib", BYTES);
        write(dir.path(), "libggml.dylib", ggml);
        let catalog = parse(vec![
            row("whisper-1", "libwhisper.dylib", "approved", json!({})),
            row(
                "ggml-1",
                "libggml.dylib",
                "pinned",
                json!({ "library": "ggml", "sha256": sha(ggml) }),
            ),
        ])
        .unwrap();
        let entries = [
            (NativeLibrary::WhisperCpp, "libwhisper.dylib"),
            (NativeLibrary::Ggml, "libggml.dylib"),
        ];
        let set = |entries: &[(NativeLibrary, &str)]| {
            verified_libraries_in(&catalog, "macos", "aarch64", dir.path(), entries)
        };
        let verified = set(&entries).unwrap();
        let ids: Vec<&str> = verified.iter().map(VerifiedLibrary::id).collect();
        assert_eq!(ids, ["whisper-1", "ggml-1"]);

        // One tampered file fails the whole set, whichever side it is on.
        write(dir.path(), "libggml.dylib", b"ggml bytez");
        assert!(matches!(
            set(&entries),
            Err(NativeCatalogError::DigestMismatch { ref id, .. }) if id == "ggml-1"
        ));
        assert!(matches!(
            set(&[entries[1], entries[0]]),
            Err(NativeCatalogError::DigestMismatch { ref id, .. }) if id == "ggml-1"
        ));
        // A missing file fails it too, and so does an entry that is not a bare file name.
        fs::remove_file(dir.path().join("libggml.dylib")).unwrap();
        assert!(matches!(set(&entries), Err(NativeCatalogError::Io(_))));
        for bad in [
            "sub/libwhisper.dylib",
            "../libwhisper.dylib",
            "/libwhisper.dylib",
            "",
        ] {
            assert!(
                matches!(
                    set(&[(NativeLibrary::WhisperCpp, bad)]),
                    Err(NativeCatalogError::UnsafePath { reason, .. }) if reason.contains("bare file name")
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_embedded_catalog_wrappers_refuse_a_file_the_catalog_does_not_pin() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "libwhisper.1.9.3.dylib", b"not whisper.cpp");
        // This host's row (if any) pins other bytes; either way the file never verifies.
        let single = verified_library(NativeLibrary::WhisperCpp, &path);
        assert!(
            matches!(
                single,
                Err(NativeCatalogError::DigestMismatch { .. }
                    | NativeCatalogError::NoApproved { .. })
            ),
            "{single:?}"
        );
        let set = verified_libraries(
            dir.path(),
            &[(NativeLibrary::WhisperCpp, "libwhisper.1.9.3.dylib")],
        );
        assert!(set.is_err());
    }

    fn record(fields: Value) -> NativeRecord {
        serde_json::from_value(row("rec", "lib.dylib", "pinned", fields)).unwrap()
    }

    fn refused(fields: Value) -> String {
        match record(fields).validate() {
            Err(NativeCatalogError::Invalid { reason, .. }) => reason,
            other => panic!("expected a refusal: {other:?}"),
        }
    }

    #[test]
    fn host_proof_never_run_is_legal_only_on_rows_that_are_not_approved() {
        for status in ["pinned", "planned", "stale-unverified"] {
            let mut r = record(json!({ "hostProof": "never-run", "status": status }));
            if status == "planned" {
                r.sha256 = None;
            }
            r.validate().unwrap();
        }
        assert!(
            refused(json!({ "hostProof": "never-run", "status": "approved" }))
                .contains("not approved")
        );
        // The value is a closed set.
        let unknown = serde_json::from_value::<NativeRecord>(row(
            "rec",
            "lib.dylib",
            "pinned",
            json!({ "hostProof": "proven" }),
        ));
        assert!(unknown.is_err());
    }

    #[test]
    fn content_sha256_by_arch_is_for_universal_rows_with_both_slices_and_no_other_digest() {
        let digest = sha(b"slice");
        let both = json!({ "aarch64": digest, "x86_64": digest });
        let universal = |extra: Value| {
            let mut fields = json!({ "arch": "universal", "contentSha256ByArch": both });
            for (k, v) in extra.as_object().unwrap() {
                fields[k] = v.clone();
            }
            fields
        };
        record(universal(json!({}))).validate().unwrap();

        assert!(refused(json!({ "contentSha256ByArch": both })).contains("arch universal"));
        assert!(refused(universal(json!({ "arch": "x86_64" }))).contains("arch universal"));
        assert!(
            refused(universal(json!({ "contentSha256": digest }))).contains("exclude each other")
        );
        for keys in [
            json!({ "aarch64": digest }),
            json!({ "x86_64": digest }),
            json!({}),
            json!({ "aarch64": digest, "x86_64": digest, "arm64e": digest }),
            json!({ "aarch64": digest, "i386": digest }),
        ] {
            assert!(
                refused(universal(json!({ "contentSha256ByArch": keys })))
                    .contains("exactly aarch64 and x86_64"),
                "{keys}"
            );
        }
        let not_hex = json!({ "aarch64": digest, "x86_64": "not a digest" });
        assert!(
            refused(universal(json!({ "contentSha256ByArch": not_hex }))).contains("SHA-256 hex")
        );
    }

    #[test]
    fn an_archive_member_is_a_relative_slash_path_without_dotdot() {
        let archive = |member: &str| json!({ "archive": { "url": "https://example.com/a.zip", "sha256": sha(b"a"), "member": member } });
        for good in [
            "bin/pdfium.dll",
            "lib/libpdfium.dylib",
            "runtimes/win-x64/native/onnxruntime.dll",
            "lib.dll",
        ] {
            record(archive(good)).validate().unwrap();
        }
        for bad in [
            "",
            "/bin/pdfium.dll",
            "bin/../pdfium.dll",
            "..",
            "bin\\pdfium.dll",
            "C:/pdfium.dll",
            "bin//pdfium.dll",
            "./pdfium.dll",
            "bin/",
            "bin/./pdfium.dll",
        ] {
            assert!(refused(archive(bad)).contains("archive.member"), "{bad:?}");
        }
        // No member is fine and is what every older row has.
        record(json!({ "archive": { "url": "https://example.com/a.zip", "sha256": sha(b"a") } }))
            .validate()
            .unwrap();
    }

    #[test]
    fn companions_must_exist_share_the_os_and_an_architecture_and_not_be_stale() {
        let windows = |id: &str, arch: &str, status: &str, extra: Value| {
            let mut fields = json!({ "os": "windows", "arch": arch });
            for (k, v) in extra.as_object().unwrap() {
                fields[k] = v.clone();
            }
            row(id, &format!("{id}.dll"), status, fields)
        };
        let with = |companions: Value, other: Value| {
            parse(vec![
                windows(
                    "main",
                    "x86_64",
                    "pinned",
                    json!({ "companions": companions }),
                ),
                other,
            ])
        };
        let invalid_reason = |result: Result<NativeCatalog, NativeCatalogError>| match result {
            Err(NativeCatalogError::Invalid { reason, .. }) => reason,
            other => panic!("expected a refusal: {other:?}"),
        };

        with(
            json!(["side"]),
            windows("side", "x86_64", "pinned", json!({})),
        )
        .unwrap();
        // A companion in another status that still ships is fine; only stale history is not.
        with(
            json!(["side"]),
            windows("side", "x86_64", "planned", json!({})),
        )
        .unwrap();
        // `any` and `universal` rows share an architecture with a thin one.
        with(json!(["side"]), windows("side", "any", "pinned", json!({}))).unwrap();

        assert!(invalid_reason(with(
            json!(["nope"]),
            windows("side", "x86_64", "pinned", json!({}))
        ))
        .contains("not in the catalog"));
        assert!(invalid_reason(with(
            json!(["main"]),
            windows("side", "x86_64", "pinned", json!({}))
        ))
        .contains("its own companion"));
        let other_os = row("side", "side.dylib", "pinned", json!({}));
        assert!(
            invalid_reason(with(json!(["side"]), other_os)).contains("is for macos, not windows")
        );
        assert!(invalid_reason(with(
            json!(["side"]),
            windows("side", "aarch64", "pinned", json!({}))
        ))
        .contains("serves no architecture"));
        assert!(invalid_reason(with(
            json!(["side"]),
            windows("side", "x86_64", "stale-unverified", json!({}))
        ))
        .contains("stale-unverified"));
        assert!(refused(json!({ "companions": [""] })).contains("empty companion"));
    }

    #[test]
    fn the_new_fields_round_trip_and_stay_out_of_older_rows() {
        let digest = sha(b"slice");
        let with_all = row(
            "ort",
            "libort.dylib",
            "pinned",
            json!({
                "arch": "universal",
                "contentSha256ByArch": { "aarch64": digest, "x86_64": digest },
                "companions": [],
                "hostProof": "never-run",
                "archive": { "url": "https://example.com/a.zip", "sha256": sha(b"a"), "member": "lib/libort.dylib" }
            }),
        );
        let parsed: NativeRecord = serde_json::from_value(with_all).unwrap();
        assert_eq!(parsed.host_proof, Some(HostProof::NeverRun));
        assert_eq!(
            parsed.archive.as_ref().unwrap().member.as_deref(),
            Some("lib/libort.dylib")
        );
        let again: NativeRecord =
            serde_json::from_value(serde_json::to_value(&parsed).unwrap()).unwrap();
        assert_eq!(again, parsed);

        let plain = serde_json::to_value(record(json!({}))).unwrap();
        for key in ["contentSha256ByArch", "companions", "hostProof"] {
            assert!(
                plain.get(key).is_none(),
                "{key} serialised for a row without it"
            );
        }
        // The nested type still rejects keys it does not know.
        let unknown = serde_json::from_value::<ArchiveRef>(
            json!({ "url": "https://example.com/a.zip", "sha256": sha(b"a"), "members": "x" }),
        );
        assert!(unknown.is_err());
    }
}
