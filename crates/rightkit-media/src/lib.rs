use std::{
    ffi::OsString,
    fmt, fs,
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
use sha2::{Digest, Sha256};

// Runner / probe / progress / cancel / loudness modules (media-runner lane). Kept in
// separate files so the catalog lane's edits to this file stay independent.
pub mod command;
pub mod dev_resolve;
pub mod loudnorm;
pub mod native;
pub mod probe;
pub mod quickprobe;
pub mod runner;
pub mod staging;
pub mod unverified;

pub use command::{
    transcode_audio_command, AudioTranscode, DevFfmpeg, FfmpegArg, FfmpegBinary, FfmpegCommand,
};
pub use runner::{
    run_capture, run_checked, run_with_progress, Captured, ProgressEvent, ProgressParser, RunError,
    RunOptions, RunReport,
};
pub use staging::{
    external_bin_path, stage_external_bin, tauri_target_triple, StagedBinary, VerifiedArtifact,
};
pub use unverified::{
    resolve_ffmpeg_or_unverified, resolve_unverified, AnyFfmpeg, BuildMode, Capabilities,
    ConfigLicense, UnverifiedRequest, UnverifiedSource, UnverifiedTool,
};

pub const APPROVED_CATALOG_JSON: &str = include_str!("../catalog/artifacts.json");

/// The decode-only build every app may ship (native codecs, no external libraries).
pub const DECODE_VARIANT: &str = "decode";
/// Decode build plus libass (subtitle burn-in), zlib (PNG) and
/// VideoToolbox/AudioToolbox, still `--disable-autodetect`. Audio export uses FFmpeg's
/// built-in AAC encoder; there is no MP3 encoder in any catalog variant.
pub const EXPORT_ASS_VARIANT: &str = "export-ass";

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
];

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
    /// SHA-256 of the shipped binary. `None` only while the entry is pending.
    #[serde(default)]
    pub sha256: Option<String>,
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

    /// The approved digest; errors for pending or stale entries.
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
        if self.variant == DECODE_VARIANT && !self.attribution.components.is_empty() {
            return Err(MediaError::MissingAttribution(
                "decode variant links no third-party components",
            ));
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
    /// Source tarball digest; `None` until observed (pending entries only).
    #[serde(default)]
    pub source_sha256: Option<String>,
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
            if !ALLOWED_COMPONENT_LICENSES.contains(&component.license_spdx.as_str()) {
                return Err(MediaError::UnapprovedLicense(
                    component.license_spdx.clone(),
                ));
            }
            if !terms.contains(&component.license_spdx.as_str()) {
                return Err(MediaError::MissingAttribution(
                    "component licence absent from the SPDX expression",
                ));
            }
            if !component.source_url.starts_with("https://") {
                return Err(MediaError::MissingAttribution("component https source URL"));
            }
            match (&component.source_sha256, status) {
                (Some(digest), _) => validate_digest("components.sourceSha256", digest)?,
                (None, ArtifactStatus::Pending) => {}
                (None, _) => return Err(MediaError::MissingMetadata("components.sourceSha256")),
            }
            if !self.notice_files.contains(&component.notice_file) {
                return Err(MediaError::MissingAttribution("component notice file"));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum MediaError {
    Io(io::Error),
    CatalogJson(serde_json::Error),
    UnsupportedCatalogSchema(u32),
    MissingMetadata(&'static str),
    InvalidDigest { field: &'static str },
    UnapprovedLicense(String),
    MissingAttribution(&'static str),
    MissingConfigureFlag(String),
    ForbiddenConfigureFlag(String),
    NoApprovedArtifact { os: String, arch: String },
    AmbiguousArtifact { os: String, arch: String },
    ArtifactNotFound,
    DevelopmentPathDisabled,
    DigestMismatch { path: PathBuf },
    BuildProbeFailed,
    BuildProbeTimedOut,
    CancelledBeforeSpawn,
    UnexpectedVersion { expected: String },
    MissingConfiguration,
    InvalidLifecyclePolicy,
    DuplicateArtifactId(String),
    ArtifactPending { id: String },
    ArtifactNotApproved { id: String },
    PendingWithDigest(String),
    WrongTool { expected: Tool, found: Tool },
    UnverifiedRefusedInRelease,
    OverrideMissing { path: PathBuf },
    UnverifiedNotFound,
    StagingFailed(String),
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
    let expected = artifact.approved_sha256()?;
    let observed = sha256_file(path)?;
    if !observed.eq_ignore_ascii_case(expected) {
        return Err(MediaError::DigestMismatch {
            path: path.to_owned(),
        });
    }
    Ok(ResolvedFfmpeg {
        path: path.to_owned(),
        artifact: artifact.clone(),
    })
}

pub fn sha256_file(path: &Path) -> Result<String, MediaError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
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
    let output = run_version_probe(&resolved.path, &["-version"], timeout)?;
    validate_build_probe(&resolved.artifact, output.0, &output.1)
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
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            child.terminate_tree()?;
            return Err(MediaError::BuildProbeTimedOut);
        }
        thread::sleep(Duration::from_millis(5));
    };
    let mut output = out_reader.join().unwrap_or_default();
    output.extend(err_reader.join().unwrap_or_default());
    Ok((
        status.success(),
        String::from_utf8_lossy(&output).into_owned(),
    ))
}

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
        artifact.pending = Some(PendingBuild {
            reason: "needs a build".into(),
            recipe: "recipes/build-ffmpeg-macos.sh".into(),
            commands: vec!["recipes/build-ffmpeg-macos.sh build ...".into()],
            promote: "record sha256".into(),
        });
        artifact.attribution.license_spdx =
            "LGPL-2.1-or-later AND ISC".into();
        artifact
            .attribution
            .notice_files
            .push("libass-COPYING.txt".into());
        artifact.attribution.components = vec![
            Component {
                name: "libass".into(),
                version: "0.17.3".into(),
                license_spdx: "ISC".into(),
                source_url:
                    "https://github.com/libass/libass/releases/download/0.17.3/libass-0.17.3.tar.xz"
                        .into(),
                source_sha256: None,
                enable_flag: Some("--enable-libass".into()),
                notice_file: "libass-COPYING.txt".into(),
            },
        ];
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
        not_ffmpeg_first.attribution.license_spdx =
            "ISC AND LGPL-2.1-or-later".into();
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
        assert!(matches!(
            approved_unpinned_source.validate(),
            Err(MediaError::MissingMetadata("components.sourceSha256"))
        ));

        let mut decode_with_components = base;
        decode_with_components.variant = DECODE_VARIANT.into();
        assert!(decode_with_components.validate().is_err());
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
    fn embedded_catalog_lists_ffprobe_and_the_pending_export_variant_with_commands() {
        let catalog = ArtifactCatalog::embedded().unwrap();
        for arch in ["aarch64", "x86_64"] {
            let probe = catalog
                .approved_for(Tool::Ffprobe, DECODE_VARIANT, "macos", arch)
                .unwrap();
            assert_eq!(probe.version, "7.1.5");
            assert!(probe.sha256.is_some());
            let export = catalog.by_id(&format!(
                "ffmpeg-7.1.5-export-ass-macos-{}",
                if arch == "aarch64" { "arm64" } else { arch }
            ));
            let export = export.expect("export variant entry");
            assert_eq!(export.status, ArtifactStatus::Pending);
            assert!(export.sha256.is_none());
            let pending = export.pending.as_ref().unwrap();
            assert!(pending
                .commands
                .iter()
                .any(|c| c.contains("build-ffmpeg-macos.sh build")));
            for flag in [
                "--enable-libass",
                "--disable-autodetect",
            ] {
                assert!(export.required_configure_flags.iter().any(|f| f == flag));
            }
            assert!(export
                .attribution
                .license_spdx
                .starts_with("LGPL-2.1-or-later AND FTL"));
            assert!(matches!(
                catalog.approved_for(Tool::Ffmpeg, EXPORT_ASS_VARIANT, "macos", arch),
                Err(MediaError::ArtifactPending { .. })
            ));
        }
        // Windows has no ffprobe artifact yet: no silent fallback to another platform.
        assert!(catalog
            .approved_for(Tool::Ffprobe, DECODE_VARIANT, "windows", "x86_64")
            .is_err());
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
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../vendor/ffmpeg-lgpl/ffmpeg-win-x64.exe");
        let artifact =
            approved_artifact("ded75c0edf344b9eed5876cd6cc469b7366b00c4dd0f25b3965062965c33990d");
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
}
