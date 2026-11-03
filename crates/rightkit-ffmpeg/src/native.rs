//! One approved native-library catalog for the whole Right Suite.
//!
//! Generalises the FFmpeg artifact catalog ([`crate::ArtifactCatalog`], which
//! stays untouched) to every native dependency apps may bundle or link:
//! FFmpeg, ONNX Runtime, PDFium, sqlite-vec, libgit2, GGML and whisper.cpp.
//! Every record pins ONE version per library and OS for the suite,
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

pub use crate::archive::ArchiveRef;

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
        // `verify_content` answers `NotVerifiable` for a universal row, so such a row could never
        // verify at load: the by-arch map is the universal form of the content digest.
        if self.arch == "universal" && self.content_sha256.is_some() {
            return Err(invalid(
                &self.id,
                "a universal row cannot carry contentSha256 (it can never verify); use contentSha256ByArch",
            ));
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

    /// `true` when this row's `os` and `arch` serve a host: `os` is `any` or equals the host's
    /// (ASCII case-insensitive), and `arch` is `any`, equals the host's, or is `universal` for
    /// either Apple architecture (`aarch64`, `x86_64`). The status is not looked at: a `planned`
    /// or `stale-unverified` row still answers `true`, so a caller that must refuse them checks
    /// [`NativeRecord::status`] itself (see [`NativeStatus::serves`]). `os` and `arch` use the
    /// names of `std::env::consts::OS` and `ARCH`.
    pub fn serves_host(&self, os: &str, arch: &str) -> bool {
        self.matches_platform(os, arch)
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
                // Staging (packages/release/stage-sidecar.mjs) refuses nested companions; the
                // catalog refuses them too so the two cannot disagree.
                if !other.companions.is_empty() {
                    return Err(invalid(
                        &r.id,
                        format!("companion {companion} lists companions of its own (nested companions are refused)"),
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
/// Windows host proof pending: the canonical path is `std::fs::canonicalize`'s, which is a
/// verbatim path (`\\?\C:\dir\file.dll`) on Windows. This crate has not loaded such a path with
/// `LoadLibraryW`, nor put its directory on `PATH` or in a loader search list, on a Windows host,
/// and does not strip the prefix (a verbatim path is also what makes long or unusual names
/// work, so stripping is a decision to make with a Windows run, not by guess). A caller that
/// loads [`VerifiedLibrary::path`] on Windows must prove that load on a Windows host first.
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
