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

pub const APPROVED_CATALOG_JSON: &str = include_str!("../catalog/artifacts.json");

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

    pub fn validate(&self) -> Result<(), MediaError> {
        if self.schema_version != 1 {
            return Err(MediaError::UnsupportedCatalogSchema(self.schema_version));
        }
        for artifact in &self.artifacts {
            artifact.validate()?;
        }
        Ok(())
    }

    pub fn approved(&self, os: &str, arch: &str) -> Result<&Artifact, MediaError> {
        self.validate()?;
        let mut matches = self.artifacts.iter().filter(|artifact| {
            artifact.status == ArtifactStatus::Approved
                && artifact.os.eq_ignore_ascii_case(os)
                && artifact.arch.eq_ignore_ascii_case(arch)
        });
        let artifact = matches
            .next()
            .ok_or_else(|| MediaError::NoApprovedArtifact {
                os: os.to_owned(),
                arch: arch.to_owned(),
            })?;
        if matches.next().is_some() {
            return Err(MediaError::AmbiguousArtifact {
                os: os.to_owned(),
                arch: arch.to_owned(),
            });
        }
        Ok(artifact)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub os: String,
    pub arch: String,
    pub version: String,
    pub status: ArtifactStatus,
    pub sha256: String,
    pub source_sha256: String,
    pub attribution: Attribution,
    pub required_configure_flags: Vec<String>,
}

impl Artifact {
    pub fn validate(&self) -> Result<(), MediaError> {
        for (field, value) in [
            ("id", self.id.as_str()),
            ("os", self.os.as_str()),
            ("arch", self.arch.as_str()),
            ("version", self.version.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(MediaError::MissingMetadata(field));
            }
        }
        validate_digest("sha256", &self.sha256)?;
        validate_digest("sourceSha256", &self.source_sha256)?;
        self.attribution.validate()?;
        if self.required_configure_flags.is_empty() {
            return Err(MediaError::MissingMetadata("requiredConfigureFlags"));
        }
        let required = self
            .required_configure_flags
            .iter()
            .map(|flag| flag.to_ascii_lowercase())
            .collect::<Vec<_>>();
        for flag in ["--disable-gpl", "--disable-nonfree"] {
            if !required.iter().any(|candidate| candidate == flag) {
                return Err(MediaError::MissingConfigureFlag(flag.to_owned()));
            }
        }
        reject_forbidden_flags(&required.join(" "))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactStatus {
    Approved,
    StaleUnverified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Attribution {
    pub license_spdx: String,
    pub source_url: String,
    pub build_provenance: String,
    pub notice_files: Vec<String>,
}

impl Attribution {
    pub fn validate(&self) -> Result<(), MediaError> {
        if self.license_spdx != "LGPL-2.1-or-later" {
            return Err(MediaError::UnapprovedLicense(self.license_spdx.clone()));
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
    for flag in ["--enable-gpl", "--enable-nonfree"] {
        if lower.split_ascii_whitespace().any(|value| value == flag) {
            return Err(MediaError::ForbiddenConfigureFlag(flag.to_owned()));
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
}

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
    let observed = sha256_file(path)?;
    if !observed.eq_ignore_ascii_case(&artifact.sha256) {
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
    let expected = format!("ffmpeg version {}", artifact.version.to_ascii_lowercase());
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
}

pub fn verify_ffmpeg(
    resolved: ResolvedFfmpeg,
    timeout: Duration,
) -> Result<VerifiedFfmpeg, MediaError> {
    if timeout.is_zero() {
        return Err(MediaError::InvalidLifecyclePolicy);
    }
    let mut command = OwnedCommand::new(&resolved.path);
    command
        .command_mut()
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.windows_hide();
    let mut child = command.spawn()?;
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
    let mut output = Vec::new();
    if let Some(mut stdout) = child.take_stdout() {
        stdout.read_to_end(&mut output)?;
    }
    if let Some(mut stderr) = child.take_stderr() {
        stderr.read_to_end(&mut output)?;
    }
    let output = String::from_utf8_lossy(&output);
    validate_build_probe(&resolved.artifact, status.success(), &output)?;
    Ok(VerifiedFfmpeg { resolved })
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
            os: "windows".into(),
            arch: "x86_64".into(),
            version: "7.1.5".into(),
            status: ArtifactStatus::Approved,
            sha256: sha256.into(),
            source_sha256: "de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f"
                .into(),
            attribution: Attribution {
                license_spdx: "LGPL-2.1-or-later".into(),
                source_url: "https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz".into(),
                build_provenance: "BUILD-NOTES.txt".into(),
                notice_files: vec!["COPYING.LGPLv2.1".into(), "BUILD-NOTES.txt".into()],
            },
            required_configure_flags: vec![
                "--disable-gpl".into(),
                "--disable-nonfree".into(),
                "--disable-autodetect".into(),
            ],
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

        let duplicate = ArtifactCatalog {
            schema_version: 1,
            artifacts: vec![approved.clone(), approved],
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
            for notice in artifact.attribution.notice_files {
                assert!(root.join(notice).is_file());
            }
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
        artifact.sha256 = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into();
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
