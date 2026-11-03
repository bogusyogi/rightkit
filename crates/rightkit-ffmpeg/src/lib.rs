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

use rightkit_child_process::OwnedCommand;
use serde::{Deserialize, Serialize};

// Runner / probe / progress / cancel / loudness modules (media-runner lane). Kept in
// separate files so the catalog lane's edits to this file stay independent.
pub mod archive;
pub mod bundle;
pub mod bundled;
pub mod command;
pub mod content_digest;
pub mod decode;
pub mod ebur128;
pub mod encoders;
pub mod h264_rule;
pub mod handle;
pub mod loudnorm;
#[cfg(feature = "native")]
pub mod native;
pub mod probe;
pub mod quickprobe;
pub mod runner;
pub mod staging;
pub mod unverified;
pub mod verify_cache;

pub use bundle::{BundleEntry, BundlePlan};
pub use bundled::{
    verify_bundled_ffmpeg, verify_bundled_ffmpeg_in, verify_bundled_ffprobe,
    verify_bundled_ffprobe_in,
};
pub use command::{
    extract_args, transcode_audio_command, AudioTranscode, ExtractMode, ExtractOptions, FfmpegArg,
    FfmpegBinary, FfmpegCommand,
};
pub use content_digest::{
    content_identity, content_identity_file, ContentFormat, ContentIdentity, SliceDigest,
};
pub use encoders::{pick_h264, EncoderInventory};
pub use handle::{
    bundled_tool_candidates, bundled_tool_candidates_with_dirs, resolve_ffmpeg_handle,
    resolve_ffmpeg_handle_with, resolve_ffprobe_handle, resolve_ffprobe_handle_with,
    verify_ffmpeg_handle, verify_ffprobe_handle, verify_first_of, FfmpegHandle, FfprobeHandle,
    HandleResolution, ResolveOptions, SkippedCandidate, ToolHandle,
};
#[cfg(feature = "native")]
pub use native::{
    verified_libraries, verified_libraries_in, verified_library, verified_library_in, HostProof,
    VerifiedLibrary,
};
/// Re-exports of the `rightkit-child-process` types that appear in this crate's public signatures
/// ([`runner::spawn_streaming`] returns an [`OwnedChild`]; its wait methods return
/// [`WaitOutcome`] and [`ReapOutcome`]), so consumers need no direct dependency to name them.
pub use rightkit_child_process::{
    OwnedChild, ProcessPriority, ReapOutcome, TerminationOptions, WaitOutcome,
};
pub use runner::{
    run_capture, run_checked, run_with_progress, spawn_streaming, spawn_streaming_in,
    spawn_streaming_with_priority, Captured, ProgressEvent, ProgressParser, RunError, RunOptions,
    RunReport, StdioMode, StdioPolicy,
};
pub use staging::{
    external_bin_path, stage_external_bin, tauri_target_triple, StagedBinary, VerifiedArtifact,
};
pub use unverified::{Capabilities, ConfigLicense};
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
    /// Where the retained bytes can be fetched: an https archive pinned by its own sha256 whose
    /// `member` is the tool's bare file name (`ffmpeg`, `ffmpeg.exe`) holding exactly the bytes
    /// `sha256` names. Absent where the bytes are not published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<archive::ArchiveRef>,
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
        if let Some(archive) = &self.archive {
            let invalid = |reason| MediaError::InvalidArchive {
                id: self.id.clone(),
                reason,
            };
            if self.sha256.is_none() {
                return Err(invalid("needs the sha256 of the bytes it holds"));
            }
            if !archive.url.starts_with("https://") || archive.url.contains(char::is_whitespace) {
                return Err(invalid("url must be an https:// URL"));
            }
            validate_digest("archive.sha256", &archive.sha256)?;
            if archive.member.as_deref() != Some(self.tool.file_name(&self.os).as_str()) {
                return Err(invalid(
                    "member must be the tool's bare file name for the row's os",
                ));
            }
        }
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
    OverrideMissing {
        path: PathBuf,
    },
    StagingFailed(String),
    /// A Mach-O or PE whose structure does not add up; never treated as a plain file.
    MalformedBinary(String),
    ComponentNotAllowed {
        variant: String,
        component: String,
    },
    /// A catalog row's `archive` cannot fetch the row's bytes as written.
    InvalidArchive {
        id: String,
        reason: &'static str,
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
            | Self::OverrideMissing { .. }
            | Self::StagingFailed(_)
            | Self::MalformedBinary(_)
            | Self::ComponentNotAllowed { .. }
            | Self::InvalidArchive { .. } => false,
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
            Self::InvalidArchive { id, reason } => write!(f, "{id}: archive {reason}"),
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
            Self::OverrideMissing { path } => {
                write!(f, "explicit FFmpeg override {} is not a file", path.display())
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
                    rightkit_child_process::WaitOutcome::Exited(status)
                    | rightkit_child_process::WaitOutcome::Terminated(status) => status,
                };
                return Ok(ProcessOutcome::Cancelled { status });
            }
            LifecycleDecision::Timeout => {
                let status = child.wait_or_kill(policy.kill_grace)?;
                let status = match status {
                    rightkit_child_process::WaitOutcome::Exited(status)
                    | rightkit_child_process::WaitOutcome::Terminated(status) => status,
                };
                return Ok(ProcessOutcome::TimedOut { status });
            }
        }
    }
}

#[cfg(feature = "agent_media")]
pub mod agent_media;
