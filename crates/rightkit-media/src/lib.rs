use std::{
    ffi::OsString,
    fmt,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use rightkit_process::OwnedCommand;
use serde::{Deserialize, Serialize};

// Runner / probe / progress / cancel / loudness modules (media-runner lane). Kept in
// separate files so the catalog lane's edits to this file stay independent.
pub mod bundled;
pub mod command;
pub mod content_digest;
pub mod decode;
pub mod dev_resolve;
pub mod loudnorm;
#[cfg(feature = "native")]
pub mod native;
pub mod probe;
pub mod quickprobe;
pub mod runner;
pub mod staging;
pub mod unverified;
pub mod verify_cache;

pub use bundled::{
    verify_bundled_ffmpeg, verify_bundled_ffmpeg_in, verify_bundled_ffprobe,
    verify_bundled_ffprobe_in,
};
pub use command::{
    transcode_audio_command, AudioTranscode, DevFfmpeg, FfmpegArg, FfmpegBinary, FfmpegCommand,
};
pub use content_digest::{
    content_identity, content_identity_file, ContentFormat, ContentIdentity, SliceDigest,
};
/// Re-exports of the `rightkit-process` types that appear in this crate's public signatures
/// ([`runner::spawn_streaming`] returns an [`OwnedChild`]; its wait methods return
/// [`WaitOutcome`] and [`ReapOutcome`]), so consumers need no direct dependency to name them.
pub use rightkit_process::{OwnedChild, ReapOutcome, TerminationOptions, WaitOutcome};
pub use runner::{
    run_capture, run_checked, run_with_progress, spawn_streaming, spawn_streaming_in, Captured,
    ProgressEvent, ProgressParser, RunError, RunOptions, RunReport, StdioMode, StdioPolicy,
};
pub use staging::{
    external_bin_path, stage_external_bin, tauri_target_triple, StagedBinary, VerifiedArtifact,
};
pub use unverified::{
    resolve_ffmpeg_or_unverified, resolve_unverified, AnyFfmpeg, BuildMode, Capabilities,
    ConfigLicense, UnverifiedRequest, UnverifiedSource, UnverifiedTool,
};
pub use verify_cache::{digest_computations, verify_cache_clear};

pub const APPROVED_CATALOG_JSON: &str = include_str!("../catalog/artifacts.json");

/// The decode-only build every app may ship (native codecs, no external libraries).
pub const DECODE_VARIANT: &str = "decode";
/// Decode build plus libass (subtitle burn-in), zlib (PNG) and
/// VideoToolbox/AudioToolbox, still `--disable-autodetect`. Audio export uses FFmpeg's
/// built-in AAC encoder; there is no MP3 encoder in any catalog variant.
pub const EXPORT_ASS_VARIANT: &str = "export-ass";
/// [`EXPORT_ASS_VARIANT`] plus libzimg (zimg, WTFPL): the `zscale` filter for HDR and
/// log-to-Rec.709 conversion next to `tonemap`. A separate variant so the `export-ass` rows,
/// digests, and licence set keep their meaning.
pub const EXPORT_ASS_ZIMG_VARIANT: &str = "export-ass-zimg";
/// Decode build plus libdav1d (AV1 decode, BSD-2-Clause), still `--disable-autodetect`.
/// Kept apart from [`DECODE_VARIANT`], which links no third-party component.
pub const DECODE_AV1_VARIANT: &str = "decode-av1";
/// Decode build plus VideoToolbox/AudioToolbox (system frameworks, no third-party code).
pub const DECODE_APPLE_VARIANT: &str = "decode-apple";

/// What a variant may link. `components` lists the only third-party components it may carry
/// (by name), apart from [`TOOLCHAIN_RUNTIME_COMPONENTS`], which any variant may list;
/// `required_flags` are configure flags every row of the variant must require.
/// A variant not listed here has no extra restriction beyond the catalog-wide rules.
struct VariantPolicy {
    components: &'static [&'static str],
    must_have_components: bool,
    required_flags: &'static [&'static str],
}

fn variant_policy(variant: &str) -> Option<VariantPolicy> {
    match variant {
        DECODE_VARIANT => Some(VariantPolicy {
            components: &[],
            must_have_components: false,
            required_flags: &[],
        }),
        DECODE_AV1_VARIANT => Some(VariantPolicy {
            components: &["dav1d"],
            must_have_components: true,
            required_flags: &["--enable-libdav1d"],
        }),
        DECODE_APPLE_VARIANT => Some(VariantPolicy {
            components: &[],
            must_have_components: false,
            required_flags: &["--enable-videotoolbox", "--enable-audiotoolbox"],
        }),
        _ => None,
    }
}

/// Catalog schemas this crate reads. Schema 2 added `tool`, `variant`, `pending`,
/// `attribution.components`, the `pending` status, and an optional `sha256`.
pub const SUPPORTED_CATALOG_SCHEMAS: &[u32] = &[1, 2];

/// Configure flags every catalog entry must carry: LGPL-only, and no host-library
/// autodetection (which could silently link GPL or system libraries).
pub const MANDATORY_CONFIGURE_FLAGS: &[&str] =
    &["--disable-gpl", "--disable-nonfree", "--disable-autodetect"];

/// Configure flags that make a build GPL or nonfree.
pub const FORBIDDEN_CONFIGURE_FLAGS: &[&str] = &[
    "--enable-gpl",
    "--enable-nonfree",
    "--enable-libx264",
    "--enable-libx265",
    "--enable-libxvid",
    "--enable-libfdk-aac",
];

/// SPDX identifiers a bundled third-party component may carry. Anything else (notably
/// GPL or AGPL) is rejected until it is approved explicitly.
pub const ALLOWED_COMPONENT_LICENSES: &[&str] = &[
    "LGPL-2.0-or-later",
    "LGPL-2.1-or-later",
    "ISC",
    "FTL",
    "MIT",
    "MIT-Modern-Variant",
    "Zlib",
    "BSD-2-Clause",
    "BSD-3-Clause",
    // zimg (libzimg, the `zscale` filter): permissive, no copyleft, no notice duty; approved
    // as a component licence 2026-10-11 (see third-party/BUILD-NOTES-ffmpeg-export-ass-zimg-macos.txt).
    "WTFPL",
    // Toolchain runtime a static MinGW-w64 link carries (see `TOOLCHAIN_RUNTIME_COMPONENTS`).
    // The mingw-w64 runtime licence is a permissive notice licence with no SPDX identifier.
    "LicenseRef-MinGW-w64-runtime",
    // libgcc and libstdc++: the GCC Runtime Library Exception lets a binary GCC built link them
    // without the GPL reaching the binary. Every other GPL identifier stays refused.
    "GPL-3.0-or-later WITH GCC-exception-3.1",
];

/// Parts of the compiler toolchain's runtime that a static MinGW-w64 link carries into a
/// Windows binary, each with the only licence it may be recorded under. Any variant may list
/// them, `decode` included; no other component may carry the two toolchain-only licences.
pub const TOOLCHAIN_RUNTIME_COMPONENTS: &[(&str, &str)] = &[
    ("mingw-w64-runtime", "LicenseRef-MinGW-w64-runtime"),
    ("winpthreads", "MIT AND BSD-3-Clause"),
    ("gcc-runtime", "GPL-3.0-or-later WITH GCC-exception-3.1"),
];

/// Licences only a toolchain runtime component may carry.
const TOOLCHAIN_ONLY_LICENSES: &[&str] = &[
    "LicenseRef-MinGW-w64-runtime",
    "GPL-3.0-or-later WITH GCC-exception-3.1",
];

fn toolchain_runtime_license(component: &str) -> Option<&'static str> {
    TOOLCHAIN_RUNTIME_COMPONENTS
        .iter()
        .find(|(name, _)| *name == component)
        .map(|(_, license)| *license)
}

/// FFmpeg's own licence; every entry's SPDX expression starts with it.
pub const FFMPEG_LICENSE_SPDX: &str = "LGPL-2.1-or-later";

/// Which FFmpeg program a catalog entry describes.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tool {
    #[default]
    Ffmpeg,
    Ffprobe,
}

impl Tool {
    /// Program name without extension (`ffmpeg`, `ffprobe`).
    pub fn stem(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
        }
    }

    /// Executable file name on `os` (`ffmpeg.exe` on Windows).
    pub fn file_name(self, os: &str) -> String {
        if os.eq_ignore_ascii_case("windows") {
            format!("{}.exe", self.stem())
        } else {
            self.stem().to_owned()
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.stem())
    }
}

fn default_variant() -> String {
    DECODE_VARIANT.to_owned()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactCatalog {
    pub schema_version: u32,
    pub artifacts: Vec<Artifact>,
}

impl ArtifactCatalog {
    pub fn parse(json: &str) -> Result<Self, MediaError> {
        let catalog: Self = serde_json::from_str(json).map_err(MediaError::CatalogJson)?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// The catalog compiled into this crate.
    pub fn embedded() -> Result<Self, MediaError> {
        Self::parse(APPROVED_CATALOG_JSON)
    }

    pub fn validate(&self) -> Result<(), MediaError> {
        if !SUPPORTED_CATALOG_SCHEMAS.contains(&self.schema_version) {
            return Err(MediaError::UnsupportedCatalogSchema(self.schema_version));
        }
        let mut ids = std::collections::BTreeSet::new();
        for artifact in &self.artifacts {
            artifact.validate()?;
            if !ids.insert(artifact.id.as_str()) {
                return Err(MediaError::DuplicateArtifactId(artifact.id.clone()));
            }
        }
        Ok(())
    }

    /// The single approved decode-variant `ffmpeg` for a platform.
    pub fn approved(&self, os: &str, arch: &str) -> Result<&Artifact, MediaError> {
        self.approved_for(Tool::Ffmpeg, DECODE_VARIANT, os, arch)
    }

    /// The single approved entry for a tool, variant, and platform.
    pub fn approved_for(
        &self,
        tool: Tool,
        variant: &str,
        os: &str,
        arch: &str,
    ) -> Result<&Artifact, MediaError> {
        self.validate()?;
        let mut matches = self.artifacts.iter().filter(|artifact| {
            artifact.status == ArtifactStatus::Approved && artifact.matches(tool, variant, os, arch)
        });
        let no_match = || {
            let pending = self.artifacts.iter().find(|artifact| {
                artifact.status == ArtifactStatus::Pending
                    && artifact.matches(tool, variant, os, arch)
            });
            match pending {
                Some(artifact) => MediaError::ArtifactPending {
                    id: artifact.id.clone(),
                },
                None => MediaError::NoApprovedArtifact {
                    os: format!("{os} ({tool} {variant})"),
                    arch: arch.to_owned(),
                },
            }
        };
        let artifact = matches.next().ok_or_else(no_match)?;
        if matches.next().is_some() {
            return Err(MediaError::AmbiguousArtifact {
                os: format!("{os} ({tool} {variant})"),
                arch: arch.to_owned(),
            });
        }
        Ok(artifact)
    }

    /// Entries whose digests have not been produced yet, with the commands that will.
    pub fn pending(&self) -> impl Iterator<Item = &Artifact> {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.status == ArtifactStatus::Pending)
    }

    pub fn by_id(&self, id: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|artifact| artifact.id == id)
    }
}

/// Exactly how a pending entry's binary and digest get produced.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingBuild {
    /// Why the digest is not recorded yet.
    pub reason: String,
    /// Recipe script, relative to the crate root.
    pub recipe: String,
    /// Commands, in order, that produce the binary and its evidence file.
    pub commands: Vec<String>,
    /// What must be recorded in this entry to approve it.
    pub promote: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    #[serde(default)]
    pub tool: Tool,
    #[serde(default = "default_variant")]
    pub variant: String,
    pub os: String,
    pub arch: String,
    pub version: String,
    pub status: ArtifactStatus,
    /// SHA-256 of the retained, unsigned bytes this row was built from. Provenance only:
    /// shipped copies are re-signed and never match it. `None` only while the entry is pending.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Signature-independent content digest (see [`content_digest`]): the identity the
    /// verification matches on. Required for an approved entry; absent while pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    /// Size in bytes of the retained bytes `sha256` was taken from. Provenance only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    pub source_sha256: String,
    pub attribution: Attribution,
    pub required_configure_flags: Vec<String>,
    #[serde(default)]
    pub pending: Option<PendingBuild>,
}

impl Artifact {
    fn matches(&self, tool: Tool, variant: &str, os: &str, arch: &str) -> bool {
        self.tool == tool
            && self.variant == variant
            && self.os.eq_ignore_ascii_case(os)
            && self.arch.eq_ignore_ascii_case(arch)
    }

    /// The approved content digest (the identity verification matches on); errors for
    /// pending or stale entries and for an approved entry that has none.
    pub fn approved_content_sha256(&self) -> Result<&str, MediaError> {
        match (self.status, self.content_sha256.as_deref()) {
            (ArtifactStatus::Approved, Some(digest)) => Ok(digest),
            (ArtifactStatus::Pending, _) => Err(MediaError::ArtifactPending {
                id: self.id.clone(),
            }),
            _ => Err(MediaError::ArtifactNotApproved {
                id: self.id.clone(),
            }),
        }
    }

    /// The provenance digest of the retained unsigned bytes; errors for pending or stale
    /// entries. Verification does not use it: see [`Artifact::approved_content_sha256`].
    pub fn approved_sha256(&self) -> Result<&str, MediaError> {
        match (self.status, self.sha256.as_deref()) {
            (ArtifactStatus::Approved, Some(digest)) => Ok(digest),
            (ArtifactStatus::Pending, _) => Err(MediaError::ArtifactPending {
                id: self.id.clone(),
            }),
            _ => Err(MediaError::ArtifactNotApproved {
                id: self.id.clone(),
            }),
        }
    }

    pub fn validate(&self) -> Result<(), MediaError> {
        for (field, value) in [
            ("id", self.id.as_str()),
            ("variant", self.variant.as_str()),
            ("os", self.os.as_str()),
            ("arch", self.arch.as_str()),
            ("version", self.version.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(MediaError::MissingMetadata(field));
            }
        }
        match (self.status, &self.content_sha256) {
            (ArtifactStatus::Pending, Some(_)) => {
                return Err(MediaError::PendingWithDigest(self.id.clone()))
            }
            (ArtifactStatus::Approved, None) => {
                return Err(MediaError::MissingMetadata("contentSha256"))
            }
            (_, Some(digest)) => validate_digest("contentSha256", digest)?,
            _ => {}
        }
        match (self.status, &self.sha256, &self.pending) {
            (ArtifactStatus::Pending, None, Some(pending)) => {
                if pending.commands.is_empty()
                    || pending.recipe.trim().is_empty()
                    || pending.promote.trim().is_empty()
                {
                    return Err(MediaError::MissingMetadata("pending build commands"));
                }
            }
            (ArtifactStatus::Pending, Some(_), _) => {
                return Err(MediaError::PendingWithDigest(self.id.clone()))
            }
            (ArtifactStatus::Pending, None, None) => {
                return Err(MediaError::MissingMetadata("pending build commands"))
            }
            (_, Some(digest), None) => validate_digest("sha256", digest)?,
            (_, None, _) => return Err(MediaError::MissingMetadata("sha256")),
            (_, Some(_), Some(_)) => return Err(MediaError::PendingWithDigest(self.id.clone())),
        }
        validate_digest("sourceSha256", &self.source_sha256)?;
        self.attribution.validate(self.status)?;
        if self.required_configure_flags.is_empty() {
            return Err(MediaError::MissingMetadata("requiredConfigureFlags"));
        }
        let required = self
            .required_configure_flags
            .iter()
            .map(|flag| flag.to_ascii_lowercase())
            .collect::<Vec<_>>();
        for flag in MANDATORY_CONFIGURE_FLAGS {
            if !required.iter().any(|candidate| candidate == flag) {
                return Err(MediaError::MissingConfigureFlag((*flag).to_owned()));
            }
        }
        for component in &self.attribution.components {
            if let Some(flag) = &component.enable_flag {
                if !required
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(flag))
                {
                    return Err(MediaError::MissingConfigureFlag(flag.clone()));
                }
            }
        }
        if let Some(policy) = variant_policy(&self.variant) {
            if let Some(component) = self.attribution.components.iter().find(|component| {
                !policy.components.contains(&component.name.as_str())
                    && toolchain_runtime_license(&component.name).is_none()
            }) {
                return Err(MediaError::ComponentNotAllowed {
                    variant: self.variant.clone(),
                    component: component.name.clone(),
                });
            }
            if policy.must_have_components
                && !policy.components.iter().all(|name| {
                    self.attribution
                        .components
                        .iter()
                        .any(|component| component.name == *name)
                })
            {
                return Err(MediaError::MissingAttribution(
                    "component the variant exists to carry",
                ));
            }
            for flag in policy.required_flags {
                if !required.iter().any(|candidate| candidate == flag) {
                    return Err(MediaError::MissingConfigureFlag((*flag).to_owned()));
                }
            }
        }
        reject_forbidden_flags(&required.join(" "))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactStatus {
    Approved,
    /// Recipe and spec recorded; binary digest not produced yet. Never resolvable.
    Pending,
    StaleUnverified,
}

/// A third-party library statically linked into the FFmpeg binary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Component {
    pub name: String,
    pub version: String,
    pub license_spdx: String,
    pub source_url: String,
    /// Source tarball digest; `None` until observed (pending entries only), or when the
    /// build used a git checkout and `source_commit` pins it instead.
    #[serde(default)]
    pub source_sha256: Option<String>,
    /// Full git commit (40 hex) of the checkout the build used, when no tarball digest exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    /// FFmpeg configure flag that enables it, when it has one.
    #[serde(default)]
    pub enable_flag: Option<String>,
    /// Notice file shipped with the binary, relative to the crate root.
    pub notice_file: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Attribution {
    /// SPDX expression for the whole binary: FFmpeg's licence `AND` every component's.
    pub license_spdx: String,
    pub source_url: String,
    pub build_provenance: String,
    pub notice_files: Vec<String>,
    #[serde(default)]
    pub components: Vec<Component>,
}

impl Attribution {
    pub fn validate(&self, status: ArtifactStatus) -> Result<(), MediaError> {
        let terms: Vec<&str> = self.license_spdx.split(" AND ").map(str::trim).collect();
        if terms.first() != Some(&FFMPEG_LICENSE_SPDX) {
            return Err(MediaError::UnapprovedLicense(self.license_spdx.clone()));
        }
        if let Some(term) = terms
            .iter()
            .find(|term| !ALLOWED_COMPONENT_LICENSES.contains(term))
        {
            return Err(MediaError::UnapprovedLicense((*term).to_owned()));
        }
        if !self.source_url.starts_with("https://ffmpeg.org/releases/") {
            return Err(MediaError::MissingAttribution("official FFmpeg source URL"));
        }
        if self.build_provenance.trim().is_empty() {
            return Err(MediaError::MissingAttribution("build provenance"));
        }
        if !self
            .notice_files
            .iter()
            .any(|path| path.to_ascii_lowercase().contains("lgpl"))
        {
            return Err(MediaError::MissingAttribution("complete LGPL notice"));
        }
        if !self
            .notice_files
            .iter()
            .any(|path| path == &self.build_provenance)
        {
            return Err(MediaError::MissingAttribution("build provenance notice"));
        }
        for component in &self.components {
            // A toolchain runtime component carries exactly its own licence, and no other
            // component may carry a toolchain-only licence.
            let misplaced = match toolchain_runtime_license(&component.name) {
                Some(license) => component.license_spdx != license,
                None => component
                    .license_spdx
                    .split(" AND ")
                    .any(|term| TOOLCHAIN_ONLY_LICENSES.contains(&term.trim())),
            };
            if misplaced {
                return Err(MediaError::UnapprovedLicense(
                    component.license_spdx.clone(),
                ));
            }
            // A component licence may be a conjunction: every term must be allowed and named
            // in the entry's expression.
            for term in component.license_spdx.split(" AND ").map(str::trim) {
                if !ALLOWED_COMPONENT_LICENSES.contains(&term) {
                    return Err(MediaError::UnapprovedLicense(term.to_owned()));
                }
                if !terms.contains(&term) {
                    return Err(MediaError::MissingAttribution(
                        "component licence absent from the SPDX expression",
                    ));
                }
            }
            if !component.source_url.starts_with("https://") {
                return Err(MediaError::MissingAttribution("component https source URL"));
            }
            match (&component.source_sha256, &component.source_commit, status) {
                (Some(digest), None, _) => validate_digest("components.sourceSha256", digest)?,
                (None, Some(commit), _) => {
                    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(MediaError::InvalidDigest {
                            field: "components.sourceCommit",
                        });
                    }
                }
                (Some(_), Some(_), _) => {
                    return Err(MediaError::MissingAttribution(
                        "one component source pin (digest or commit), not both",
                    ))
                }
                (None, None, ArtifactStatus::Pending) => {}
                (None, None, _) => {
                    return Err(MediaError::MissingMetadata("components.sourceSha256"))
                }
            }
            if !self.notice_files.contains(&component.notice_file) {
                return Err(MediaError::MissingAttribution("component notice file"));
            }
        }
        Ok(())
    }
}

#[non_exhaustive]
#[derive(Debug)]
pub enum MediaError {
    Io(io::Error),
    CatalogJson(serde_json::Error),
    UnsupportedCatalogSchema(u32),
    MissingMetadata(&'static str),
    InvalidDigest {
        field: &'static str,
    },
    UnapprovedLicense(String),
    MissingAttribution(&'static str),
    MissingConfigureFlag(String),
    ForbiddenConfigureFlag(String),
    NoApprovedArtifact {
        os: String,
        arch: String,
    },
    AmbiguousArtifact {
        os: String,
        arch: String,
    },
    ArtifactNotFound,
    DevelopmentPathDisabled,
    DigestMismatch {
        path: PathBuf,
    },
    BuildProbeFailed,
    BuildProbeTimedOut,
    CancelledBeforeSpawn,
    UnexpectedVersion {
        expected: String,
    },
    MissingConfiguration,
    InvalidLifecyclePolicy,
    DuplicateArtifactId(String),
    ArtifactPending {
        id: String,
    },
    ArtifactNotApproved {
        id: String,
    },
    PendingWithDigest(String),
    WrongTool {
        expected: Tool,
        found: Tool,
    },
    UnverifiedRefusedInRelease,
    OverrideMissing {
        path: PathBuf,
    },
    UnverifiedNotFound,
    StagingFailed(String),
    /// A Mach-O or PE whose structure does not add up; never treated as a plain file.
    MalformedBinary(String),
    ComponentNotAllowed {
        variant: String,
        component: String,
    },
}

impl MediaError {
    /// `true` exactly when the error means "no approved catalog row matches this binary's
    /// content": [`MediaError::DigestMismatch`] (the file's content digest is not the row's)
    /// and [`MediaError::NoApprovedArtifact`] (no approved row for the platform).
    ///
    /// Only these two permit a development fallback, and only outside release builds.
    /// Everything else is `false` on purpose: a missing file ([`MediaError::ArtifactNotFound`],
    /// [`MediaError::OverrideMissing`]), a pending or ambiguous row, a malformed binary, a
    /// failed probe and every I/O or catalog error describe a different problem that a
    /// fallback would hide. The match has no wildcard, so a new variant must be classified.
    pub fn is_not_in_catalog(&self) -> bool {
        match self {
            Self::DigestMismatch { .. } | Self::NoApprovedArtifact { .. } => true,
            Self::Io(_)
            | Self::CatalogJson(_)
            | Self::UnsupportedCatalogSchema(_)
            | Self::MissingMetadata(_)
            | Self::InvalidDigest { .. }
            | Self::UnapprovedLicense(_)
            | Self::MissingAttribution(_)
            | Self::MissingConfigureFlag(_)
            | Self::ForbiddenConfigureFlag(_)
            | Self::AmbiguousArtifact { .. }
            | Self::ArtifactNotFound
            | Self::DevelopmentPathDisabled
            | Self::BuildProbeFailed
            | Self::BuildProbeTimedOut
            | Self::CancelledBeforeSpawn
            | Self::UnexpectedVersion { .. }
            | Self::MissingConfiguration
            | Self::InvalidLifecyclePolicy
            | Self::DuplicateArtifactId(_)
            | Self::ArtifactPending { .. }
            | Self::ArtifactNotApproved { .. }
            | Self::PendingWithDigest(_)
            | Self::WrongTool { .. }
            | Self::UnverifiedRefusedInRelease
            | Self::OverrideMissing { .. }
            | Self::UnverifiedNotFound
            | Self::StagingFailed(_)
            | Self::MalformedBinary(_)
            | Self::ComponentNotAllowed { .. } => false,
        }
    }
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "media I/O failed: {error}"),
            Self::CatalogJson(error) => write!(f, "artifact catalog is invalid JSON: {error}"),
            Self::UnsupportedCatalogSchema(version) => {
                write!(f, "unsupported artifact catalog schema {version}")
            }
            Self::MissingMetadata(field) => write!(f, "artifact metadata is missing {field}"),
            Self::InvalidDigest { field } => write!(f, "{field} is not a SHA-256 digest"),
            Self::UnapprovedLicense(license) => write!(f, "unapproved FFmpeg license {license}"),
            Self::MissingAttribution(item) => write!(f, "FFmpeg attribution is missing {item}"),
            Self::MissingConfigureFlag(flag) => write!(f, "FFmpeg is missing required flag {flag}"),
            Self::ForbiddenConfigureFlag(flag) => {
                write!(f, "FFmpeg contains forbidden flag {flag}")
            }
            Self::NoApprovedArtifact { os, arch } => {
                write!(f, "no approved FFmpeg artifact for {os}/{arch}")
            }
            Self::AmbiguousArtifact { os, arch } => {
                write!(f, "multiple approved FFmpeg artifacts for {os}/{arch}")
            }
            Self::ArtifactNotFound => f.write_str("approved FFmpeg artifact was not found"),
            Self::DevelopmentPathDisabled => {
                f.write_str("development FFmpeg path requires explicit opt-in")
            }
            Self::DigestMismatch { path } => {
                write!(f, "FFmpeg digest mismatch at {}", path.display())
            }
            Self::BuildProbeFailed => f.write_str("FFmpeg -version exited unsuccessfully"),
            Self::BuildProbeTimedOut => f.write_str("FFmpeg -version timed out"),
            Self::CancelledBeforeSpawn => f.write_str("FFmpeg run was cancelled before spawn"),
            Self::UnexpectedVersion { expected } => {
                write!(f, "FFmpeg version did not match {expected}")
            }
            Self::MissingConfiguration => f.write_str("FFmpeg configuration line is missing"),
            Self::InvalidLifecyclePolicy => {
                f.write_str("timeout, poll interval, and kill grace must be non-zero")
            }
            Self::DuplicateArtifactId(id) => write!(f, "artifact id {id} appears twice"),
            Self::ArtifactPending { id } => write!(
                f,
                "artifact {id} is pending: its binary digest has not been produced (see its `pending.commands`)"
            ),
            Self::ArtifactNotApproved { id } => write!(f, "artifact {id} is not approved"),
            Self::PendingWithDigest(id) => {
                write!(f, "artifact {id} is pending but records a digest or is approved with pending commands")
            }
            Self::WrongTool { expected, found } => {
                write!(f, "expected a {expected} artifact, found {found}")
            }
            Self::UnverifiedRefusedInRelease => f.write_str(
                "an unverified (non-catalog) FFmpeg is refused in release mode",
            ),
            Self::OverrideMissing { path } => {
                write!(f, "explicit FFmpeg override {} is not a file", path.display())
            }
            Self::UnverifiedNotFound => {
                f.write_str("no unverified FFmpeg found (override, environment, or PATH)")
            }
            Self::StagingFailed(why) => write!(f, "externalBin staging failed: {why}"),
            Self::MalformedBinary(why) => write!(f, "malformed binary: {why}"),
            Self::ComponentNotAllowed { variant, component } => write!(
                f,
                "variant {variant} may not link component {component}"
            ),
        }
    }
}

impl std::error::Error for MediaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::CatalogJson(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for MediaError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn validate_digest(field: &'static str, digest: &str) -> Result<(), MediaError> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MediaError::InvalidDigest { field });
    }
    Ok(())
}

fn reject_forbidden_flags(configuration: &str) -> Result<(), MediaError> {
    let lower = configuration.to_ascii_lowercase();
    for flag in FORBIDDEN_CONFIGURE_FLAGS {
        if lower.split_ascii_whitespace().any(|value| value == *flag) {
            return Err(MediaError::ForbiddenConfigureFlag((*flag).to_owned()));
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ResolutionRequest {
    pub override_path: Option<PathBuf>,
    pub bundled_paths: Vec<PathBuf>,
    pub development_path: Option<PathBuf>,
    pub allow_development_path: bool,
}

#[derive(Clone, Debug)]
pub struct ResolvedFfmpeg {
    path: PathBuf,
    artifact: Artifact,
}

impl ResolvedFfmpeg {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }
}

/// Resolve an approved catalog binary (`ffmpeg` or `ffprobe`, per `artifact.tool`):
/// explicit override, then bundled paths, then an explicitly enabled development path.
/// The first present candidate decides; a digest mismatch is an error, never a skip.
pub fn resolve_ffmpeg(
    artifact: &Artifact,
    request: &ResolutionRequest,
) -> Result<ResolvedFfmpeg, MediaError> {
    artifact.validate()?;
    if let Some(path) = request
        .override_path
        .iter()
        .chain(request.bundled_paths.iter())
        .find(|path| path.is_file())
    {
        return verify_resolved_path(artifact, path);
    }
    if let Some(path) = request
        .development_path
        .as_ref()
        .filter(|path| path.is_file())
    {
        if !request.allow_development_path {
            return Err(MediaError::DevelopmentPathDisabled);
        }
        return verify_resolved_path(artifact, path);
    }
    Err(MediaError::ArtifactNotFound)
}

fn verify_resolved_path(artifact: &Artifact, path: &Path) -> Result<ResolvedFfmpeg, MediaError> {
    let expected = artifact.approved_content_sha256()?;
    // A cached pass for this exact file (path, size, mtime, identity, artifact) skips the hash.
    let key = verify_cache::key_for(path, artifact, expected)?;
    if !verify_cache::lookup(&key).digest_ok {
        verify_cache::record_digest_computation();
        verify_content(artifact, path)?;
        verify_cache::remember(key, |entry| entry.digest_ok = true);
    }
    Ok(ResolvedFfmpeg {
        path: path.to_owned(),
        artifact: artifact.clone(),
    })
}

/// Check that the file at `path` is the build `artifact` approves, by content digest.
///
/// A Mach-O or PE must carry the row's architecture; any other file is compared whole. A universal Mach-O passes only when the row's own slice
/// matches and every other slice matches an approved row (same tool, variant, version, OS)
/// for that slice's architecture, so a universal file can never smuggle in an unapproved
/// slice. Malformed binaries are errors, not mismatches to be retried as plain files.
pub fn verify_content(artifact: &Artifact, path: &Path) -> Result<(), MediaError> {
    let mismatch = || MediaError::DigestMismatch {
        path: path.to_owned(),
    };
    let expected = artifact.approved_content_sha256()?;
    let identity = content_identity_file(path)?;
    let own = match identity.format {
        ContentFormat::Plain => identity.slices.first().map(|slice| slice.sha256.as_str()),
        _ => identity.digest_for(&artifact.arch),
    };
    if !own.is_some_and(|digest| digest.eq_ignore_ascii_case(expected)) {
        return Err(mismatch());
    }
    if identity.format == ContentFormat::MachOUniversal {
        let embedded = ArtifactCatalog::embedded()?;
        for slice in &identity.slices {
            let arch = slice.arch.ok_or_else(mismatch)?;
            let approved = embedded
                .artifacts
                .iter()
                .chain(std::iter::once(artifact))
                .filter(|row| {
                    row.status == ArtifactStatus::Approved
                        && row.tool == artifact.tool
                        && row.variant == artifact.variant
                        && row.version == artifact.version
                        && row.os.eq_ignore_ascii_case(&artifact.os)
                        && row.arch.eq_ignore_ascii_case(arch)
                })
                .filter_map(|row| row.content_sha256.as_deref())
                .any(|digest| digest.eq_ignore_ascii_case(&slice.sha256));
            if !approved {
                return Err(mismatch());
            }
        }
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String, MediaError> {
    Ok(rightkit_fs::digest::sha256_file(path)?)
}

pub fn validate_build_probe(
    artifact: &Artifact,
    success: bool,
    output: &str,
) -> Result<(), MediaError> {
    artifact.validate()?;
    if !success {
        return Err(MediaError::BuildProbeFailed);
    }
    let lower = output.to_ascii_lowercase();
    let expected = format!(
        "{} version {}",
        artifact.tool.stem(),
        artifact.version.to_ascii_lowercase()
    );
    if !lower.lines().any(|line| line.starts_with(&expected)) {
        return Err(MediaError::UnexpectedVersion {
            expected: artifact.version.clone(),
        });
    }
    let configuration = lower
        .lines()
        .find(|line| line.trim_start().starts_with("configuration:"))
        .ok_or(MediaError::MissingConfiguration)?;
    reject_forbidden_flags(configuration)?;
    for flag in &artifact.required_configure_flags {
        let expected = flag.to_ascii_lowercase();
        if !configuration
            .split_ascii_whitespace()
            .any(|value| value == expected)
        {
            return Err(MediaError::MissingConfigureFlag(flag.clone()));
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct VerifiedFfmpeg {
    resolved: ResolvedFfmpeg,
}

impl VerifiedFfmpeg {
    pub fn path(&self) -> &Path {
        self.resolved.path()
    }

    pub fn artifact_id(&self) -> &str {
        &self.resolved.artifact.id
    }

    pub fn artifact(&self) -> &Artifact {
        &self.resolved.artifact
    }
}

/// An `ffprobe` that matched its catalog digest and passed the LGPL build probe.
#[derive(Clone, Debug)]
pub struct VerifiedFfprobe {
    resolved: ResolvedFfmpeg,
}

impl VerifiedFfprobe {
    pub fn path(&self) -> &Path {
        self.resolved.path()
    }

    pub fn artifact_id(&self) -> &str {
        &self.resolved.artifact.id
    }

    pub fn artifact(&self) -> &Artifact {
        &self.resolved.artifact
    }
}

impl FfmpegBinary for VerifiedFfprobe {
    fn ffmpeg_path(&self) -> &Path {
        self.path()
    }
}

/// Verify a resolved `ffmpeg`: digest already matched, now the `-version` build probe.
pub fn verify_ffmpeg(
    resolved: ResolvedFfmpeg,
    timeout: Duration,
) -> Result<VerifiedFfmpeg, MediaError> {
    expect_tool(&resolved, Tool::Ffmpeg)?;
    run_build_probe(&resolved, timeout)?;
    Ok(VerifiedFfmpeg { resolved })
}

/// Verify a resolved `ffprobe` exactly like [`verify_ffmpeg`].
pub fn verify_ffprobe(
    resolved: ResolvedFfmpeg,
    timeout: Duration,
) -> Result<VerifiedFfprobe, MediaError> {
    expect_tool(&resolved, Tool::Ffprobe)?;
    run_build_probe(&resolved, timeout)?;
    Ok(VerifiedFfprobe { resolved })
}

fn expect_tool(resolved: &ResolvedFfmpeg, expected: Tool) -> Result<(), MediaError> {
    if resolved.artifact.tool != expected {
        return Err(MediaError::WrongTool {
            expected,
            found: resolved.artifact.tool,
        });
    }
    Ok(())
}

fn run_build_probe(resolved: &ResolvedFfmpeg, timeout: Duration) -> Result<(), MediaError> {
    if timeout.is_zero() {
        return Err(MediaError::InvalidLifecyclePolicy);
    }
    let expected = resolved.artifact.approved_content_sha256()?;
    let key = verify_cache::key_for(&resolved.path, &resolved.artifact, expected)?;
    if verify_cache::lookup(&key).probe_ok {
        return Ok(());
    }
    let output = run_version_probe(&resolved.path, &["-version"], timeout)?;
    validate_build_probe(&resolved.artifact, output.0, &output.1)?;
    verify_cache::remember(key, |entry| entry.probe_ok = true);
    Ok(())
}

/// Run `program args`, bounded by `timeout`, returning (success, stdout+stderr).
pub(crate) fn run_version_probe(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<(bool, String), MediaError> {
    let mut command = OwnedCommand::new(program);
    command
        .command_mut()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.windows_hide();
    let mut child = command.spawn()?;
    let mut stdout = child.take_stdout();
    let mut stderr = child.take_stderr();
    // Drain both pipes concurrently so verbose listings (-encoders) cannot fill a pipe
    // buffer and stall the child until the timeout.
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let (exit_tx, exit_rx) = std::sync::mpsc::channel();
    runner::watch_exit(&child, move |status| {
        let _ = exit_tx.send(status);
    })?;
    // Block until the child exits or the deadline passes; no polling tick sits in between.
    let status = match exit_rx.recv_timeout(timeout) {
        Ok(status) => status?,
        Err(_) => {
            child.terminate_tree()?;
            return Err(MediaError::BuildProbeTimedOut);
        }
    };
    let mut output = out_reader.join().unwrap_or_default();
    output.extend(err_reader.join().unwrap_or_default());
    Ok((
        status.success(),
        String::from_utf8_lossy(&output).into_owned(),
    ))
}

/// `-y -hide_banner -loglevel error -i <input> -vn -ac 1 -ar 16000 -c:a pcm_s16le <output>`:
/// 16-bit mono 16 kHz WAV from any input. It adds `-vn` (drop video) and `-y` (overwrite).
/// Paths stay `OsString`s; nothing is shell-quoted. It needs only the paths, not a binary.
pub fn pcm_s16le_16khz_mono_args(input: &Path, output: &Path) -> Vec<OsString> {
    [
        OsString::from("-y"),
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-i"),
        input.as_os_str().to_owned(),
        OsString::from("-vn"),
        OsString::from("-ac"),
        OsString::from("1"),
        OsString::from("-ar"),
        OsString::from("16000"),
        OsString::from("-c:a"),
        OsString::from("pcm_s16le"),
        output.as_os_str().to_owned(),
    ]
    .into()
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// A token backed by a flag the caller already owns (for example an app-wide
    /// shutdown flag). Setting the flag cancels the token and vice versa.
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    /// The shared flag behind this token, for callers that hand it to other code.
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LifecyclePolicy {
    timeout: Duration,
    poll_interval: Duration,
    kill_grace: Duration,
}

impl LifecyclePolicy {
    pub fn new(
        timeout: Duration,
        poll_interval: Duration,
        kill_grace: Duration,
    ) -> Result<Self, MediaError> {
        if timeout.is_zero() || poll_interval.is_zero() || kill_grace.is_zero() {
            return Err(MediaError::InvalidLifecyclePolicy);
        }
        Ok(Self {
            timeout,
            poll_interval,
            kill_grace,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleDecision {
    Continue,
    Cancel,
    Timeout,
}

pub fn lifecycle_decision(
    elapsed: Duration,
    cancellation: &CancellationToken,
    policy: &LifecyclePolicy,
) -> LifecycleDecision {
    if cancellation.is_cancelled() {
        LifecycleDecision::Cancel
    } else if elapsed >= policy.timeout {
        LifecycleDecision::Timeout
    } else {
        LifecycleDecision::Continue
    }
}

#[non_exhaustive]
#[derive(Debug)]
pub enum ProcessOutcome {
    Exited(ExitStatus),
    Cancelled { status: ExitStatus },
    TimedOut { status: ExitStatus },
}

pub fn run_ffmpeg(
    ffmpeg: &VerifiedFfmpeg,
    args: &[OsString],
    policy: &LifecyclePolicy,
    cancellation: &CancellationToken,
) -> Result<ProcessOutcome, MediaError> {
    run_owned_process(ffmpeg.path(), args, policy, cancellation)
}

fn run_owned_process(
    program: &Path,
    args: &[OsString],
    policy: &LifecyclePolicy,
    cancellation: &CancellationToken,
) -> Result<ProcessOutcome, MediaError> {
    if cancellation.is_cancelled() {
        return Err(MediaError::CancelledBeforeSpawn);
    }
    let mut command = OwnedCommand::new(program.as_os_str());
    command
        .command_mut()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.windows_hide();
    let mut child = command.spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(ProcessOutcome::Exited(status));
        }
        match lifecycle_decision(started.elapsed(), cancellation, policy) {
            LifecycleDecision::Continue => thread::sleep(policy.poll_interval),
            LifecycleDecision::Cancel => {
                let status = child.wait_or_kill(policy.kill_grace)?;
                let status = match status {
                    rightkit_process::WaitOutcome::Exited(status)
                    | rightkit_process::WaitOutcome::Terminated(status) => status,
                };
                return Ok(ProcessOutcome::Cancelled { status });
            }
            LifecycleDecision::Timeout => {
                let status = child.wait_or_kill(policy.kill_grace)?;
                let status = match status {
                    rightkit_process::WaitOutcome::Exited(status)
                    | rightkit_process::WaitOutcome::Terminated(status) => status,
                };
                return Ok(ProcessOutcome::TimedOut { status });
            }
        }
    }
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };

    use super::*;

    fn approved_artifact(sha256: &str) -> Artifact {
        Artifact {
            id: "ffmpeg-7.1.5-windows-x64".into(),
            tool: Tool::Ffmpeg,
            variant: DECODE_VARIANT.into(),
            os: "windows".into(),
            arch: "x86_64".into(),
            version: "7.1.5".into(),
            status: ArtifactStatus::Approved,
            sha256: Some(sha256.into()),
            content_sha256: Some(sha256.into()),
            size: None,
            source_sha256: "de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f"
                .into(),
            attribution: Attribution {
                license_spdx: "LGPL-2.1-or-later".into(),
                source_url: "https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz".into(),
                build_provenance: "BUILD-NOTES.txt".into(),
                notice_files: vec!["COPYING.LGPLv2.1".into(), "BUILD-NOTES.txt".into()],
                components: vec![],
            },
            required_configure_flags: vec![
                "--disable-gpl".into(),
                "--disable-nonfree".into(),
                "--disable-autodetect".into(),
            ],
            pending: None,
        }
    }

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("rightkit-media-{}-{}", std::process::id(), name));
        fs::write(&root, bytes).unwrap();
        root
    }

    #[test]
    fn catalog_selects_one_approved_artifact_and_rejects_stale_or_ambiguous_entries() {
        let approved =
            approved_artifact("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        let mut stale = approved.clone();
        stale.id = "stale".into();
        stale.status = ArtifactStatus::StaleUnverified;
        let catalog = ArtifactCatalog {
            schema_version: 1,
            artifacts: vec![stale, approved.clone()],
        };
        assert_eq!(catalog.approved("windows", "x86_64").unwrap(), &approved);

        let same_id = ArtifactCatalog {
            schema_version: 1,
            artifacts: vec![approved.clone(), approved.clone()],
        };
        assert!(matches!(
            same_id.approved("windows", "x86_64"),
            Err(MediaError::DuplicateArtifactId(_))
        ));
        let mut twin = approved.clone();
        twin.id = "ffmpeg-7.1.5-windows-x64-twin".into();
        let duplicate = ArtifactCatalog {
            schema_version: 1,
            artifacts: vec![approved, twin],
        };
        assert!(matches!(
            duplicate.approved("windows", "x86_64"),
            Err(MediaError::AmbiguousArtifact { .. })
        ));
    }

    #[test]
    fn embedded_catalog_is_valid_and_every_attribution_file_is_packaged() {
        let catalog = ArtifactCatalog::parse(APPROVED_CATALOG_JSON).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for artifact in catalog.artifacts {
            if artifact.status != ArtifactStatus::Approved {
                // Pending notices are produced by the recipe alongside the binary.
                continue;
            }
            for notice in artifact.attribution.notice_files {
                assert!(root.join(&notice).is_file(), "{}: {notice}", artifact.id);
            }
        }
    }

    #[test]
    fn embedded_catalog_approves_exactly_one_artifact_per_shipped_platform() {
        let catalog = ArtifactCatalog::parse(APPROVED_CATALOG_JSON).unwrap();
        for (os, arch, id, digest) in [
            (
                "windows",
                "x86_64",
                "ffmpeg-7.1.5-windows-x64",
                "ded75c0edf344b9eed5876cd6cc469b7366b00c4dd0f25b3965062965c33990d",
            ),
            (
                "macos",
                "aarch64",
                "ffmpeg-7.1.5-macos-arm64",
                "a42a50dbfa8502481db21da247f7fe45ee0c8e8b1bb98bd68558faf64faf4258",
            ),
            (
                "macos",
                "x86_64",
                "ffmpeg-7.1.5-macos-x86_64",
                "2ba456b336d5faa75e2783e4e2c49434d9b26e765621eae6669d10e5b4303929",
            ),
        ] {
            let artifact = catalog.approved(os, arch).unwrap();
            assert_eq!(artifact.id, id);
            assert_eq!(artifact.sha256.as_deref(), Some(digest));
            assert_eq!(artifact.version, "7.1.5");
            assert_eq!(artifact.attribution.license_spdx, "LGPL-2.1-or-later");
        }
    }

    #[test]
    fn catalog_rejects_unknown_fields_instead_of_silently_ignoring_them() {
        let mut value: serde_json::Value = serde_json::from_str(APPROVED_CATALOG_JSON).unwrap();
        value["artifacts"][0]["unreviewedPolicy"] = serde_json::Value::Bool(true);
        let result = ArtifactCatalog::parse(&serde_json::to_string(&value).unwrap());
        assert!(matches!(result, Err(MediaError::CatalogJson(_))));
    }

    #[test]
    fn catalog_metadata_fails_closed_without_digest_lgpl_provenance_and_notices() {
        let mut artifact = approved_artifact("not-a-digest");
        assert!(matches!(
            artifact.validate(),
            Err(MediaError::InvalidDigest { .. })
        ));
        artifact.sha256 =
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into());
        artifact.content_sha256 = artifact.sha256.clone();
        artifact.attribution.license_spdx = "GPL-3.0-only".into();
        assert!(matches!(
            artifact.validate(),
            Err(MediaError::UnapprovedLicense(_))
        ));
        artifact.attribution.license_spdx = "LGPL-2.1-or-later".into();
        artifact.attribution.notice_files.clear();
        assert!(matches!(
            artifact.validate(),
            Err(MediaError::MissingAttribution(_))
        ));
    }

    #[test]
    fn resolution_uses_explicit_order_verifies_digest_and_has_no_ambient_path_fallback() {
        let bundled = temp_file("bundled.bin", b"hello");
        let artifact =
            approved_artifact("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        let resolved = resolve_ffmpeg(
            &artifact,
            &ResolutionRequest {
                override_path: Some(PathBuf::from("missing-override")),
                bundled_paths: vec![bundled.clone()],
                development_path: None,
                allow_development_path: false,
            },
        )
        .unwrap();
        assert_eq!(resolved.path(), bundled.as_path());

        let missing = resolve_ffmpeg(
            &artifact,
            &ResolutionRequest {
                override_path: None,
                bundled_paths: vec![],
                development_path: None,
                allow_development_path: false,
            },
        );
        assert!(matches!(missing, Err(MediaError::ArtifactNotFound)));
        fs::remove_file(bundled).unwrap();
    }

    #[test]
    fn resolution_rejects_a_mismatched_present_candidate_instead_of_skipping_it() {
        let bad = temp_file("bad.bin", b"tampered");
        let good = temp_file("good.bin", b"hello");
        let artifact =
            approved_artifact("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        let result = resolve_ffmpeg(
            &artifact,
            &ResolutionRequest {
                override_path: Some(bad.clone()),
                bundled_paths: vec![good.clone()],
                development_path: None,
                allow_development_path: false,
            },
        );
        assert!(matches!(result, Err(MediaError::DigestMismatch { .. })));
        fs::remove_file(bad).unwrap();
        fs::remove_file(good).unwrap();
    }

    #[test]
    fn development_binary_requires_an_explicit_opt_in_and_still_requires_the_approved_digest() {
        let dev = temp_file("dev.bin", b"hello");
        let artifact =
            approved_artifact("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        let request = ResolutionRequest {
            override_path: None,
            bundled_paths: vec![],
            development_path: Some(dev.clone()),
            allow_development_path: false,
        };
        assert!(matches!(
            resolve_ffmpeg(&artifact, &request),
            Err(MediaError::DevelopmentPathDisabled)
        ));
        let resolved = resolve_ffmpeg(
            &artifact,
            &ResolutionRequest {
                allow_development_path: true,
                ..request
            },
        )
        .unwrap();
        assert_eq!(resolved.path(), dev.as_path());
        fs::remove_file(dev).unwrap();
    }

    #[test]
    fn build_probe_requires_success_version_and_all_approved_lgpl_flags() {
        let artifact =
            approved_artifact("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        let good = "ffmpeg version 7.1.5\nconfiguration: --disable-gpl --disable-nonfree --disable-autodetect";
        assert!(validate_build_probe(&artifact, true, good).is_ok());
        assert!(matches!(
            validate_build_probe(&artifact, false, good),
            Err(MediaError::BuildProbeFailed)
        ));
        assert!(matches!(
            validate_build_probe(&artifact, true, "ffmpeg version 7.1.5\nconfiguration: --enable-gPl --disable-nonfree --disable-autodetect"),
            Err(MediaError::ForbiddenConfigureFlag(_))
        ));
        assert!(matches!(
            validate_build_probe(
                &artifact,
                true,
                "ffmpeg version 7.1.5\nconfiguration: --disable-gpl --disable-nonfree"
            ),
            Err(MediaError::MissingConfigureFlag(_))
        ));
    }

    fn export_artifact() -> Artifact {
        let mut artifact =
            approved_artifact("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        artifact.id = "ffmpeg-7.1.5-export-ass-macos-arm64".into();
        artifact.variant = EXPORT_ASS_VARIANT.into();
        artifact.status = ArtifactStatus::Pending;
        artifact.sha256 = None;
        artifact.content_sha256 = None;
        artifact.pending = Some(PendingBuild {
            reason: "needs a build".into(),
            recipe: "recipes/build-ffmpeg-macos.sh".into(),
            commands: vec!["recipes/build-ffmpeg-macos.sh build ...".into()],
            promote: "record sha256".into(),
        });
        artifact.attribution.license_spdx = "LGPL-2.1-or-later AND ISC".into();
        artifact
            .attribution
            .notice_files
            .push("libass-COPYING.txt".into());
        artifact.attribution.components = vec![Component {
            name: "libass".into(),
            version: "0.17.3".into(),
            license_spdx: "ISC".into(),
            source_url:
                "https://github.com/libass/libass/releases/download/0.17.3/libass-0.17.3.tar.xz"
                    .into(),
            source_sha256: None,
            source_commit: None,
            enable_flag: Some("--enable-libass".into()),
            notice_file: "libass-COPYING.txt".into(),
        }];
        artifact
            .required_configure_flags
            .push("--enable-libass".into());
        artifact
    }

    #[test]
    fn pending_entries_validate_without_a_digest_but_are_never_resolvable() {
        let pending = export_artifact();
        pending.validate().unwrap();
        assert!(matches!(
            pending.approved_sha256(),
            Err(MediaError::ArtifactPending { .. })
        ));
        let bundled = temp_file("pending.bin", b"hello");
        let result = resolve_ffmpeg(
            &pending,
            &ResolutionRequest {
                override_path: None,
                bundled_paths: vec![bundled.clone()],
                development_path: None,
                allow_development_path: false,
            },
        );
        assert!(matches!(result, Err(MediaError::ArtifactPending { .. })));
        fs::remove_file(bundled).unwrap();

        let catalog = ArtifactCatalog {
            schema_version: 2,
            artifacts: vec![pending.clone()],
        };
        assert!(matches!(
            catalog.approved_for(Tool::Ffmpeg, EXPORT_ASS_VARIANT, "windows", "x86_64"),
            Err(MediaError::ArtifactPending { .. })
        ));
        assert_eq!(catalog.pending().count(), 1);

        // A pending entry may not smuggle in a digest, and must say how to produce one.
        let mut with_digest = pending.clone();
        with_digest.sha256 =
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into());
        assert!(matches!(
            with_digest.validate(),
            Err(MediaError::PendingWithDigest(_))
        ));
        let mut without_commands = pending.clone();
        without_commands.pending.as_mut().unwrap().commands.clear();
        assert!(without_commands.validate().is_err());
        let mut approved_without_digest = pending;
        approved_without_digest.status = ArtifactStatus::Approved;
        approved_without_digest.pending = None;
        assert!(matches!(
            approved_without_digest.validate(),
            Err(MediaError::MissingMetadata("contentSha256"))
        ));
        // With the content digest but no provenance digest, the provenance one is missing.
        approved_without_digest.content_sha256 = Some("a".repeat(64));
        assert!(matches!(
            approved_without_digest.validate(),
            Err(MediaError::MissingMetadata("sha256"))
        ));
    }

    #[test]
    fn component_licences_and_enable_flags_are_recorded_and_fail_closed() {
        let base = export_artifact();
        let mut gpl = base.clone();
        gpl.attribution.components[0].license_spdx = "GPL-2.0-or-later".into();
        assert!(matches!(
            gpl.validate(),
            Err(MediaError::UnapprovedLicense(_))
        ));

        let mut unlisted = base.clone();
        unlisted.attribution.license_spdx = "LGPL-2.1-or-later".into();
        assert!(matches!(
            unlisted.validate(),
            Err(MediaError::MissingAttribution(_))
        ));

        let mut not_ffmpeg_first = base.clone();
        not_ffmpeg_first.attribution.license_spdx = "ISC AND LGPL-2.1-or-later".into();
        assert!(not_ffmpeg_first.validate().is_err());

        let mut flag_missing = base.clone();
        flag_missing
            .required_configure_flags
            .retain(|flag| flag != "--enable-libass");
        assert!(matches!(
            flag_missing.validate(),
            Err(MediaError::MissingConfigureFlag(flag)) if flag == "--enable-libass"
        ));

        let mut autodetect = base.clone();
        autodetect
            .required_configure_flags
            .retain(|flag| flag != "--disable-autodetect");
        assert!(matches!(
            autodetect.validate(),
            Err(MediaError::MissingConfigureFlag(flag)) if flag == "--disable-autodetect"
        ));

        let mut approved_unpinned_source = base.clone();
        approved_unpinned_source.status = ArtifactStatus::Approved;
        approved_unpinned_source.pending = None;
        approved_unpinned_source.sha256 =
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into());
        approved_unpinned_source.content_sha256 = approved_unpinned_source.sha256.clone();
        assert!(matches!(
            approved_unpinned_source.validate(),
            Err(MediaError::MissingMetadata("components.sourceSha256"))
        ));

        let mut decode_with_components = base;
        decode_with_components.variant = DECODE_VARIANT.into();
        assert!(decode_with_components.validate().is_err());
    }

    #[test]
    fn wtfpl_is_an_allowed_component_licence_and_unlisted_licences_stay_refused() {
        // zimg (WTFPL) beside libass (ISC): the expression names both, the component carries it.
        let zimg = |license: &str| Component {
            name: "zimg".into(),
            version: "3.0.5".into(),
            license_spdx: license.into(),
            source_url: "https://github.com/sekrit-twc/zimg/archive/refs/tags/release-3.0.5.tar.gz"
                .into(),
            source_sha256: None,
            source_commit: None,
            enable_flag: Some("--enable-libzimg".into()),
            notice_file: "zimg-COPYING.txt".into(),
        };
        let mut with_zimg = export_artifact();
        with_zimg.attribution.license_spdx = "LGPL-2.1-or-later AND ISC AND WTFPL".into();
        with_zimg
            .attribution
            .notice_files
            .push("zimg-COPYING.txt".into());
        with_zimg.attribution.components.push(zimg("WTFPL"));
        with_zimg
            .required_configure_flags
            .push("--enable-libzimg".into());
        with_zimg.validate().unwrap();

        // The licence must be in the entry's SPDX expression too.
        let mut not_in_expression = with_zimg.clone();
        not_in_expression.attribution.license_spdx = "LGPL-2.1-or-later AND ISC".into();
        assert!(matches!(
            not_in_expression.validate(),
            Err(MediaError::MissingAttribution(_))
        ));

        // A licence that is not on the list is still refused, as a component and in the expression.
        let mut unlisted_component = with_zimg.clone();
        unlisted_component.attribution.components[1].license_spdx = "Beerware".into();
        unlisted_component.attribution.license_spdx =
            "LGPL-2.1-or-later AND ISC AND Beerware".into();
        assert!(matches!(
            unlisted_component.validate(),
            Err(MediaError::UnapprovedLicense(license)) if license == "Beerware"
        ));
        let mut unlisted_expression = with_zimg;
        unlisted_expression.attribution.license_spdx =
            "LGPL-2.1-or-later AND ISC AND WTFPL AND Beerware".into();
        assert!(matches!(
            unlisted_expression.validate(),
            Err(MediaError::UnapprovedLicense(license)) if license == "Beerware"
        ));
        assert!(ALLOWED_COMPONENT_LICENSES.contains(&"WTFPL"));
        assert!(!ALLOWED_COMPONENT_LICENSES.contains(&"GPL-2.0-or-later"));
    }

    #[test]
    fn toolchain_runtime_components_are_admitted_in_any_variant_and_other_gpl_stays_refused() {
        const DIGEST: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        const GCC: &str = "GPL-3.0-or-later WITH GCC-exception-3.1";
        let runtime = |name: &str, license: &str, notice: &str| Component {
            name: name.into(),
            version: "16.1.0-5".into(),
            license_spdx: license.into(),
            source_url: "https://mirror.msys2.org/mingw/mingw64/".into(),
            source_sha256: Some(DIGEST.into()),
            source_commit: None,
            enable_flag: None,
            notice_file: notice.into(),
        };
        // A static MinGW-w64 decode build: no library, but the runtime the linker carries in.
        let mut decode = approved_artifact(DIGEST);
        assert_eq!(decode.variant, DECODE_VARIANT);
        decode.attribution.license_spdx = format!(
            "LGPL-2.1-or-later AND LicenseRef-MinGW-w64-runtime AND MIT AND BSD-3-Clause AND {GCC}"
        );
        for &(name, license) in TOOLCHAIN_RUNTIME_COMPONENTS {
            let notice = format!("{name}-notice.txt");
            decode.attribution.notice_files.push(notice.clone());
            decode
                .attribution
                .components
                .push(runtime(name, license, notice.as_str()));
        }
        decode.validate().unwrap();

        // Every term of a conjunction must be named in the entry's expression.
        let mut term_missing = decode.clone();
        term_missing.attribution.license_spdx =
            format!("LGPL-2.1-or-later AND LicenseRef-MinGW-w64-runtime AND MIT AND {GCC}");
        assert!(matches!(
            term_missing.validate(),
            Err(MediaError::MissingAttribution(_))
        ));

        // A runtime component carries its own licence only: plain GPL is refused for libgcc.
        let mut plain_gpl = decode.clone();
        plain_gpl.attribution.components[2].license_spdx = "GPL-3.0-or-later".into();
        assert!(matches!(
            plain_gpl.validate(),
            Err(MediaError::UnapprovedLicense(license)) if license == "GPL-3.0-or-later"
        ));

        // The exception belongs to the GCC runtime: a library cannot be recorded under it.
        let mut library_under_exception = export_artifact();
        library_under_exception.attribution.license_spdx = format!("LGPL-2.1-or-later AND {GCC}");
        library_under_exception.attribution.components[0].license_spdx = GCC.into();
        assert!(matches!(
            library_under_exception.validate(),
            Err(MediaError::UnapprovedLicense(license)) if license == GCC
        ));

        // The decode variant still admits no library.
        let mut decode_with_library = decode.clone();
        decode_with_library
            .attribution
            .license_spdx
            .push_str(" AND ISC");
        decode_with_library
            .attribution
            .notice_files
            .push("libass-COPYING.txt".into());
        let mut libass = export_artifact().attribution.components[0].clone();
        libass.source_sha256 = Some(DIGEST.into());
        decode_with_library.attribution.components.push(libass);
        decode_with_library
            .required_configure_flags
            .push("--enable-libass".into());
        assert!(matches!(
            decode_with_library.validate(),
            Err(MediaError::ComponentNotAllowed { .. })
        ));

        // Every other GPL identifier stays refused, as a component and in the expression.
        for gpl in [
            "GPL-2.0-only",
            "GPL-2.0-or-later",
            "GPL-3.0-only",
            "GPL-3.0-or-later",
            "AGPL-3.0-or-later",
            "GPL-2.0-only WITH GCC-exception-2.0",
        ] {
            assert!(!ALLOWED_COMPONENT_LICENSES.contains(&gpl), "{gpl}");
            let mut in_expression = decode.clone();
            in_expression.attribution.license_spdx.push_str(" AND ");
            in_expression.attribution.license_spdx.push_str(gpl);
            assert!(
                matches!(
                    in_expression.validate(),
                    Err(MediaError::UnapprovedLicense(license)) if license == gpl
                ),
                "{gpl}"
            );
        }
        assert!(ALLOWED_COMPONENT_LICENSES.contains(&GCC));
        assert!(ALLOWED_COMPONENT_LICENSES.contains(&"LicenseRef-MinGW-w64-runtime"));
    }

    #[test]
    fn x264_and_other_gpl_libraries_are_forbidden_in_a_probe() {
        let artifact =
            approved_artifact("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        assert!(matches!(
            validate_build_probe(
                &artifact,
                true,
                "ffmpeg version 7.1.5\nconfiguration: --disable-gpl --disable-nonfree --disable-autodetect --enable-libx264"
            ),
            Err(MediaError::ForbiddenConfigureFlag(_))
        ));
    }

    #[test]
    fn ffprobe_entries_probe_their_own_banner_and_never_verify_as_ffmpeg() {
        let mut artifact =
            approved_artifact("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        artifact.tool = Tool::Ffprobe;
        let flags = "configuration: --disable-gpl --disable-nonfree --disable-autodetect";
        assert!(
            validate_build_probe(&artifact, true, &format!("ffprobe version 7.1.5\n{flags}"))
                .is_ok()
        );
        assert!(matches!(
            validate_build_probe(&artifact, true, &format!("ffmpeg version 7.1.5\n{flags}")),
            Err(MediaError::UnexpectedVersion { .. })
        ));
        let file = temp_file("probe-tool.bin", b"hello");
        let resolved = resolve_ffmpeg(
            &artifact,
            &ResolutionRequest {
                override_path: Some(file.clone()),
                bundled_paths: vec![],
                development_path: None,
                allow_development_path: false,
            },
        )
        .unwrap();
        assert!(matches!(
            verify_ffmpeg(resolved, Duration::from_secs(1)),
            Err(MediaError::WrongTool { .. })
        ));
        fs::remove_file(file).unwrap();
    }

    #[test]
    fn embedded_catalog_lists_ffprobe_and_the_export_ass_variant() {
        let catalog = ArtifactCatalog::embedded().unwrap();
        for arch in ["aarch64", "x86_64"] {
            let probe = catalog
                .approved_for(Tool::Ffprobe, DECODE_VARIANT, "macos", arch)
                .unwrap();
            assert_eq!(probe.version, "7.1.5");
            assert!(probe.sha256.is_some());
            let export = catalog
                .by_id(&format!(
                    "ffmpeg-7.1.5-export-ass-macos-{}",
                    if arch == "aarch64" { "arm64" } else { arch }
                ))
                .expect("export variant entry");
            for flag in [
                "--enable-libass",
                "--enable-libfreetype",
                "--enable-libharfbuzz",
                "--disable-autodetect",
            ] {
                assert!(export.required_configure_flags.iter().any(|f| f == flag));
            }
            assert_eq!(
                export.attribution.license_spdx,
                "LGPL-2.1-or-later AND FTL AND MIT-Modern-Variant AND ISC"
            );
            let names: Vec<_> = export
                .attribution
                .components
                .iter()
                .map(|c| c.name.as_str())
                .collect();
            assert_eq!(names, ["freetype", "fribidi", "harfbuzz", "libass"]);
            assert!(export
                .attribution
                .components
                .iter()
                .all(|c| c.source_sha256.is_some()));
        }
        // arm64 was built, probed and burn-in tested; x86_64 was built but its probes
        // could not run on the arm64 build host, so it stays pending and unresolvable.
        let arm = catalog
            .approved_for(Tool::Ffmpeg, EXPORT_ASS_VARIANT, "macos", "aarch64")
            .unwrap();
        assert_eq!(arm.id, "ffmpeg-7.1.5-export-ass-macos-arm64");
        assert_eq!(
            arm.sha256.as_deref(),
            Some("08f310879e64b348befa7c566c00aa5b6c67e9b64b082fa424f4a181ebd538b8")
        );
        assert_eq!(arm.size, Some(16_092_616));
        assert!(arm.pending.is_none());
        let intel = catalog
            .by_id("ffmpeg-7.1.5-export-ass-macos-x86_64")
            .unwrap();
        assert_eq!(intel.status, ArtifactStatus::Pending);
        assert!(intel.sha256.is_none());
        assert!(intel
            .pending
            .as_ref()
            .unwrap()
            .commands
            .iter()
            .any(|c| c.contains("build-ffmpeg-macos.sh build")));
        assert!(matches!(
            catalog.approved_for(Tool::Ffmpeg, EXPORT_ASS_VARIANT, "macos", "x86_64"),
            Err(MediaError::ArtifactPending { .. })
        ));
        // The Windows ffprobe row is pending: no silent fallback to another platform.
        assert!(catalog
            .approved_for(Tool::Ffprobe, DECODE_VARIANT, "windows", "x86_64")
            .is_err());
    }

    #[test]
    fn size_and_whole_file_sha256_are_provenance_and_the_content_digest_decides() {
        let file = temp_file("sized-tool.bin", b"hello");
        let mut artifact =
            approved_artifact("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        let request = ResolutionRequest {
            override_path: Some(file.clone()),
            bundled_paths: vec![],
            development_path: None,
            allow_development_path: false,
        };
        // A re-signed copy has another length and another whole-file hash: still accepted.
        artifact.size = Some(6);
        artifact.sha256 = Some("0".repeat(64));
        assert!(resolve_ffmpeg(&artifact, &request).is_ok());
        // A different content digest is refused, and an approved row without one is invalid.
        artifact.content_sha256 = Some("1".repeat(64));
        verify_cache_clear();
        assert!(matches!(
            resolve_ffmpeg(&artifact, &request),
            Err(MediaError::DigestMismatch { .. })
        ));
        artifact.content_sha256 = None;
        assert!(matches!(
            artifact.validate(),
            Err(MediaError::MissingMetadata("contentSha256"))
        ));
        fs::remove_file(file).unwrap();
    }

    #[test]
    fn variants_restrict_their_components_and_flags() {
        let mut av1 = approved_artifact(&"a".repeat(64));
        av1.variant = DECODE_AV1_VARIANT.into();
        // Needs dav1d and its flag.
        assert!(av1.validate().is_err());
        av1.required_configure_flags
            .push("--enable-libdav1d".into());
        av1.attribution.license_spdx = "LGPL-2.1-or-later AND BSD-2-Clause".into();
        av1.attribution
            .notice_files
            .push("dav1d-COPYING.txt".into());
        let dav1d = Component {
            name: "dav1d".into(),
            version: "1.5.4".into(),
            license_spdx: "BSD-2-Clause".into(),
            source_url: "https://github.com/videolan/dav1d.git".into(),
            source_sha256: None,
            source_commit: Some("54706fc6bc0cdecab7e9593974a4039cc038fca7".into()),
            enable_flag: Some("--enable-libdav1d".into()),
            notice_file: "dav1d-COPYING.txt".into(),
        };
        av1.attribution.components = vec![dav1d.clone()];
        av1.validate().unwrap();
        // Any other component is refused in this variant, and `decode` still takes none.
        let mut other = av1.clone();
        other.attribution.components[0].name = "libass".into();
        assert!(matches!(
            other.validate(),
            Err(MediaError::ComponentNotAllowed { .. })
        ));
        let mut decode = av1.clone();
        decode.variant = DECODE_VARIANT.into();
        assert!(matches!(
            decode.validate(),
            Err(MediaError::ComponentNotAllowed { .. })
        ));
        // A component must be pinned by exactly one of digest or commit.
        let mut unpinned = av1.clone();
        unpinned.attribution.components[0].source_commit = None;
        assert!(unpinned.validate().is_err());
        let mut both = av1;
        both.attribution.components[0].source_sha256 = Some("b".repeat(64));
        assert!(both.validate().is_err());

        let mut apple = approved_artifact(&"a".repeat(64));
        apple.variant = DECODE_APPLE_VARIANT.into();
        assert!(matches!(
            apple.validate(),
            Err(MediaError::MissingConfigureFlag(_))
        ));
        apple
            .required_configure_flags
            .push("--enable-videotoolbox".into());
        apple
            .required_configure_flags
            .push("--enable-audiotoolbox".into());
        apple.validate().unwrap();
    }

    #[test]
    fn pcm_arguments_are_shell_free_and_identical_for_every_adapter() {
        let args = pcm_s16le_16khz_mono_args(Path::new("in file.mp3"), Path::new("out file.wav"));
        assert_eq!(
            args,
            vec![
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                "in file.mp3",
                "-vn",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-c:a",
                "pcm_s16le",
                "out file.wav"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn cancellation_is_cloneable_and_monotonic() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[test]
    fn cancellation_shares_a_caller_owned_flag_in_both_directions() {
        let flag = Arc::new(AtomicBool::new(false));
        let token = CancellationToken::from_flag(Arc::clone(&flag));
        assert!(!token.is_cancelled());
        flag.store(true, Ordering::Release);
        assert!(token.is_cancelled());

        let other = CancellationToken::new();
        let shared = other.flag();
        assert!(!shared.load(Ordering::Acquire));
        other.cancel();
        assert!(shared.load(Ordering::Acquire));
        assert!(Arc::ptr_eq(&other.flag(), &shared));
    }

    #[test]
    fn lifecycle_policy_rejects_zero_timeout_poll_or_kill_grace() {
        assert!(LifecyclePolicy::new(
            Duration::ZERO,
            Duration::from_millis(10),
            Duration::from_millis(10)
        )
        .is_err());
        assert!(LifecyclePolicy::new(
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::from_millis(10)
        )
        .is_err());
        assert!(LifecyclePolicy::new(
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::ZERO
        )
        .is_err());
    }

    #[test]
    fn lifecycle_state_prioritizes_cancellation_then_timeout() {
        let token = Arc::new(CancellationToken::new());
        let policy = LifecyclePolicy::new(
            Duration::from_secs(5),
            Duration::from_millis(10),
            Duration::from_millis(50),
        )
        .unwrap();
        assert_eq!(
            lifecycle_decision(Duration::from_secs(1), &token, &policy),
            LifecycleDecision::Continue
        );
        token.cancel();
        assert_eq!(
            lifecycle_decision(Duration::from_secs(6), &token, &policy),
            LifecycleDecision::Cancel
        );
        let fresh = CancellationToken::new();
        assert_eq!(
            lifecycle_decision(Duration::from_secs(5), &fresh, &policy),
            LifecycleDecision::Timeout
        );
    }

    #[test]
    fn cancellation_before_spawn_is_reported_without_launching_a_process() {
        let policy = LifecyclePolicy::new(
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(10),
        )
        .unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let result = run_owned_process(
            Path::new("this-program-must-not-be-launched"),
            &[],
            &policy,
            &token,
        );
        assert!(matches!(result, Err(MediaError::CancelledBeforeSpawn)));
    }

    #[cfg(windows)]
    #[test]
    fn owned_process_timeout_is_bounded_and_terminates_the_owned_tree() {
        let policy = LifecyclePolicy::new(
            Duration::from_millis(100),
            Duration::from_millis(10),
            Duration::from_millis(50),
        )
        .unwrap();
        let outcome = run_owned_process(
            Path::new("powershell.exe"),
            &[
                OsString::from("-NoLogo"),
                OsString::from("-NoProfile"),
                OsString::from("-NonInteractive"),
                OsString::from("-Command"),
                OsString::from("Start-Process -WindowStyle Hidden powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 60'; Start-Sleep -Seconds 60"),
            ],
            &policy,
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(matches!(outcome, ProcessOutcome::TimedOut { .. }));
    }

    #[cfg(windows)]
    #[test]
    fn approved_windows_binary_resolves_and_passes_the_real_build_probe() {
        // The retained exe sits in the workspace `vendor/ffmpeg-lgpl`; the depth depends on
        // the checkout layout, so try both. Absent on this host: say so and skip.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(path) = ["../../../vendor", "../../../../vendor"]
            .iter()
            .map(|up| manifest.join(up).join("ffmpeg-lgpl/ffmpeg-win-x64.exe"))
            .find(|path| path.is_file())
        else {
            crate::test_support::skip_missing(
                "approved vendor/ffmpeg-lgpl/ffmpeg-win-x64.exe",
                "vendor/ffmpeg-lgpl under ../../../ or ../../../../ from crates/rightkit-media",
            );
            return;
        };
        // The embedded row: its contentSha256 is the Authenticode hash, not the file hash.
        let catalog = ArtifactCatalog::embedded().unwrap();
        let artifact = catalog.approved("windows", "x86_64").unwrap().clone();
        assert_eq!(artifact.id, "ffmpeg-7.1.5-windows-x64");
        let resolved = resolve_ffmpeg(
            &artifact,
            &ResolutionRequest {
                override_path: None,
                bundled_paths: vec![path],
                development_path: None,
                allow_development_path: false,
            },
        )
        .unwrap();
        let verified = verify_ffmpeg(resolved, Duration::from_secs(5)).unwrap();
        assert_eq!(verified.artifact_id(), artifact.id);
    }

    #[test]
    fn only_content_mismatch_and_missing_row_mean_not_in_catalog() {
        let p = || PathBuf::from("x");
        let s = || String::new();
        let not_in_catalog = [
            MediaError::DigestMismatch { path: p() },
            MediaError::NoApprovedArtifact { os: s(), arch: s() },
        ];
        let other = [
            MediaError::Io(io::Error::other("x")),
            MediaError::CatalogJson(serde_json::from_str::<u8>("x").unwrap_err()),
            MediaError::UnsupportedCatalogSchema(9),
            MediaError::MissingMetadata("x"),
            MediaError::InvalidDigest { field: "x" },
            MediaError::UnapprovedLicense(s()),
            MediaError::MissingAttribution("x"),
            MediaError::MissingConfigureFlag(s()),
            MediaError::ForbiddenConfigureFlag(s()),
            MediaError::AmbiguousArtifact { os: s(), arch: s() },
            MediaError::ArtifactNotFound,
            MediaError::DevelopmentPathDisabled,
            MediaError::BuildProbeFailed,
            MediaError::BuildProbeTimedOut,
            MediaError::CancelledBeforeSpawn,
            MediaError::UnexpectedVersion { expected: s() },
            MediaError::MissingConfiguration,
            MediaError::InvalidLifecyclePolicy,
            MediaError::DuplicateArtifactId(s()),
            MediaError::ArtifactPending { id: s() },
            MediaError::ArtifactNotApproved { id: s() },
            MediaError::PendingWithDigest(s()),
            MediaError::WrongTool {
                expected: Tool::Ffmpeg,
                found: Tool::Ffprobe,
            },
            MediaError::UnverifiedRefusedInRelease,
            MediaError::OverrideMissing { path: p() },
            MediaError::UnverifiedNotFound,
            MediaError::StagingFailed(s()),
            MediaError::MalformedBinary(s()),
            MediaError::ComponentNotAllowed {
                variant: s(),
                component: s(),
            },
        ];
        assert!(not_in_catalog.iter().all(MediaError::is_not_in_catalog));
        assert!(other.iter().all(|e| !e.is_not_in_catalog()), "{other:?}");
        // The method's match has no wildcard, so a new variant fails to compile until it is
        // classified there and listed in one of the arrays above.
    }
}
