//! Non-catalog FFmpeg/FFprobe for development builds, labelled and refused in release.
//!
//! Apps sometimes need a feature the approved catalog build lacks on a developer
//! machine (for example an encoder the approved builds omit before the `export-ass`
//! variant is approved). This module resolves such a binary from, in order:
//!
//! 1. an explicit configured path ([`UnverifiedRequest::config_override`]),
//! 2. an explicit environment variable the app names ([`UnverifiedRequest::env_var`],
//!    e.g. `GENRIGHT_FFMPEG`),
//! 3. `PATH`, only when [`UnverifiedRequest::allow_path_lookup`] is set.
//!
//! The result is an [`UnverifiedTool`], never a [`crate::VerifiedFfmpeg`]: it carries
//! where it came from, says `UNVERIFIED` in its label, and offers
//! [`UnverifiedTool::probe_capabilities`] so callers check for the encoders, decoders,
//! and filters they need instead of assuming them. [`BuildMode::Release`] refuses every
//! source, so a release build can only run catalog-verified binaries. An override that
//! is set but does not point at a file is an error, not a silent fall-through.

use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    resolve_ffmpeg, run_version_probe, verify_ffmpeg, Artifact, FfmpegBinary, MediaError,
    ResolutionRequest, Tool, VerifiedFfmpeg, FORBIDDEN_CONFIGURE_FLAGS,
};

/// Whether non-catalog binaries may run. There is deliberately no `Default`: the app
/// states its mode (for example from `cfg!(debug_assertions)` or its own dev flag).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildMode {
    Development,
    Release,
}

impl BuildMode {
    /// `Development` in builds with debug assertions, otherwise `Release`.
    pub fn from_debug_assertions() -> Self {
        if cfg!(debug_assertions) {
            Self::Development
        } else {
            Self::Release
        }
    }
}

/// Where an unverified binary came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnverifiedSource {
    ConfigOverride,
    EnvOverride { var: String },
    PathLookup,
}

impl fmt::Display for UnverifiedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigOverride => f.write_str("configured override"),
            Self::EnvOverride { var } => write!(f, "${var}"),
            Self::PathLookup => f.write_str("PATH"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct UnverifiedRequest {
    pub tool: Tool,
    /// Explicit path from app configuration; checked first.
    pub config_override: Option<PathBuf>,
    /// Environment variable holding an explicit path, e.g. `GENRIGHT_FFMPEG`.
    pub env_var: Option<String>,
    /// Search `PATH` for `ffmpeg`/`ffprobe` when no override is set.
    pub allow_path_lookup: bool,
}

impl UnverifiedRequest {
    pub fn new(tool: Tool) -> Self {
        Self {
            tool,
            config_override: None,
            env_var: None,
            allow_path_lookup: false,
        }
    }
}

/// A binary that has NOT been checked against the artifact catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedTool {
    tool: Tool,
    path: PathBuf,
    source: UnverifiedSource,
}

impl UnverifiedTool {
    pub fn tool(&self) -> Tool {
        self.tool
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn source(&self) -> &UnverifiedSource {
        &self.source
    }

    /// Always `false`; present so logs and UI can state it explicitly.
    pub fn is_verified(&self) -> bool {
        false
    }

    /// For logs and diagnostics, e.g.
    /// `UNVERIFIED ffmpeg (not catalog-checked) from $GENRIGHT_FFMPEG: /opt/ffmpeg`.
    pub fn label(&self) -> String {
        format!(
            "UNVERIFIED {} (not catalog-checked) from {}: {}",
            self.tool,
            self.source,
            self.path.display()
        )
    }

    /// Probe `-version`, `-encoders`, `-decoders`, and `-filters` (bounded by `timeout`
    /// per call). `ffprobe` has no encoder or filter listing, so those stay empty.
    pub fn probe_capabilities(&self, timeout: Duration) -> Result<Capabilities, MediaError> {
        probe_capabilities_at(&self.path, self.tool, timeout)
    }
}

impl VerifiedFfmpeg {
    /// The same listing and parsing as [`UnverifiedTool::probe_capabilities`], for a
    /// catalog-verified `ffmpeg`. Use it to confirm an encoder or filter the build
    /// carries before a pipeline depends on it.
    pub fn probe_capabilities(&self, timeout: Duration) -> Result<Capabilities, MediaError> {
        probe_capabilities_at(self.path(), Tool::Ffmpeg, timeout)
    }
}

/// Runs `-version`, `-decoders`, and (for ffmpeg) `-encoders` and `-filters` against
/// `path`, each bounded by `timeout`.
fn probe_capabilities_at(
    path: &Path,
    tool: Tool,
    timeout: Duration,
) -> Result<Capabilities, MediaError> {
    let run = |flag: &str| -> Result<String, MediaError> {
        let (ok, out) = run_version_probe(path, &["-hide_banner", flag], timeout)?;
        if !ok {
            return Err(MediaError::BuildProbeFailed);
        }
        Ok(out)
    };
    let version = run("-version")?;
    let decoders = parse_codec_list(&run("-decoders")?);
    let (encoders, filters) = match tool {
        Tool::Ffmpeg => (
            parse_codec_list(&run("-encoders")?),
            parse_filter_list(&run("-filters")?),
        ),
        Tool::Ffprobe => (BTreeSet::new(), BTreeSet::new()),
    };
    Ok(Capabilities::from_parts(
        &version, encoders, decoders, filters,
    ))
}

impl FfmpegBinary for UnverifiedTool {
    fn ffmpeg_path(&self) -> &Path {
        &self.path
    }
}

/// Licence posture read from a `-version` configuration line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigLicense {
    /// No GPL/nonfree flag present.
    Lgpl,
    /// Contains a flag from [`FORBIDDEN_CONFIGURE_FLAGS`]; never shippable.
    GplOrNonfree,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capabilities {
    /// First line of `-version`, e.g. `ffmpeg version 8.1.2 Copyright ...`.
    pub version_line: String,
    pub configuration: String,
    pub license: ConfigLicense,
    pub encoders: BTreeSet<String>,
    pub decoders: BTreeSet<String>,
    pub filters: BTreeSet<String>,
}

impl Capabilities {
    fn from_parts(
        version: &str,
        encoders: BTreeSet<String>,
        decoders: BTreeSet<String>,
        filters: BTreeSet<String>,
    ) -> Self {
        let configuration = version
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("configuration:"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let lower = configuration.to_ascii_lowercase();
        let license = if lower
            .split_ascii_whitespace()
            .any(|flag| FORBIDDEN_CONFIGURE_FLAGS.contains(&flag))
        {
            ConfigLicense::GplOrNonfree
        } else {
            ConfigLicense::Lgpl
        };
        Self {
            version_line: version.lines().next().unwrap_or_default().trim().to_owned(),
            configuration,
            license,
            encoders,
            decoders,
            filters,
        }
    }

    pub fn has_encoder(&self, name: &str) -> bool {
        self.encoders.contains(name)
    }

    pub fn has_decoder(&self, name: &str) -> bool {
        self.decoders.contains(name)
    }

    pub fn has_filter(&self, name: &str) -> bool {
        self.filters.contains(name)
    }
}

/// Entries of `-encoders`/`-decoders`: lines after the ` ------` separator, formatted
/// ` A....D name  description`.
pub fn parse_codec_list(listing: &str) -> BTreeSet<String> {
    listing
        .lines()
        .skip_while(|line| !line.trim_start().starts_with("------"))
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            let name = parts.next()?;
            (flags.len() == 6).then(|| name.to_owned())
        })
        .collect()
}

/// Entries of `-filters`: ` TSC name  A->A  description` (the I/O column has `->`).
pub fn parse_filter_list(listing: &str) -> BTreeSet<String> {
    listing
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let _flags = parts.next()?;
            let name = parts.next()?;
            let io = parts.next()?;
            io.contains("->").then(|| name.to_owned())
        })
        .collect()
}

fn executable_name(tool: Tool) -> String {
    tool.file_name(if cfg!(windows) {
        "windows"
    } else {
        env::consts::OS
    })
}

/// Resolve a non-catalog binary. Refused in [`BuildMode::Release`].
pub fn resolve_unverified(
    request: &UnverifiedRequest,
    mode: BuildMode,
) -> Result<UnverifiedTool, MediaError> {
    resolve_unverified_with(request, mode, |key| env::var_os(key))
}

fn resolve_unverified_with(
    request: &UnverifiedRequest,
    mode: BuildMode,
    get_env: impl Fn(&str) -> Option<OsString>,
) -> Result<UnverifiedTool, MediaError> {
    if mode == BuildMode::Release {
        return Err(MediaError::UnverifiedRefusedInRelease);
    }
    let found = |path: PathBuf, source: UnverifiedSource| -> Result<UnverifiedTool, MediaError> {
        if !path.is_file() {
            return Err(MediaError::OverrideMissing { path });
        }
        Ok(UnverifiedTool {
            tool: request.tool,
            path,
            source,
        })
    };
    if let Some(path) = &request.config_override {
        return found(path.clone(), UnverifiedSource::ConfigOverride);
    }
    if let Some(var) = &request.env_var {
        if let Some(value) = get_env(var).filter(|value| !value.is_empty()) {
            return found(
                PathBuf::from(value),
                UnverifiedSource::EnvOverride { var: var.clone() },
            );
        }
    }
    if request.allow_path_lookup {
        let name = executable_name(request.tool);
        if let Some(paths) = get_env("PATH") {
            if let Some(path) = env::split_paths(&paths)
                .map(|dir| dir.join(&name))
                .find(|candidate| candidate.is_file())
            {
                return Ok(UnverifiedTool {
                    tool: request.tool,
                    path,
                    source: UnverifiedSource::PathLookup,
                });
            }
        }
    }
    Err(MediaError::UnverifiedNotFound)
}

/// Either a catalog-verified `ffmpeg` or, in development only, an unverified one.
// Resolved once per app session; boxing the verified handle buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum AnyFfmpeg {
    Verified(VerifiedFfmpeg),
    Unverified {
        tool: UnverifiedTool,
        /// Why the catalog path was not used.
        catalog_error: String,
    },
}

impl AnyFfmpeg {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }

    pub fn label(&self) -> String {
        match self {
            Self::Verified(v) => format!("catalog-verified {}", v.artifact_id()),
            Self::Unverified {
                tool,
                catalog_error,
            } => format!("{} [catalog: {catalog_error}]", tool.label()),
        }
    }
}

impl FfmpegBinary for AnyFfmpeg {
    fn ffmpeg_path(&self) -> &Path {
        match self {
            Self::Verified(v) => v.path(),
            Self::Unverified { tool, .. } => tool.path(),
        }
    }
}

/// Catalog first; in [`BuildMode::Development`] fall back to [`resolve_unverified`]
/// when the catalog path fails for any reason. In release the catalog error is final.
pub fn resolve_ffmpeg_or_unverified(
    artifact: &Artifact,
    catalog: &ResolutionRequest,
    unverified: &UnverifiedRequest,
    mode: BuildMode,
    timeout: Duration,
) -> Result<AnyFfmpeg, MediaError> {
    let verified = resolve_ffmpeg(artifact, catalog).and_then(|r| verify_ffmpeg(r, timeout));
    match (verified, mode) {
        (Ok(v), _) => Ok(AnyFfmpeg::Verified(v)),
        (Err(error), BuildMode::Release) => Err(error),
        (Err(error), BuildMode::Development) => Ok(AnyFfmpeg::Unverified {
            tool: resolve_unverified(unverified, mode)?,
            catalog_error: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn env_of(pairs: Vec<(&'static str, OsString)>) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn release_mode_refuses_every_unverified_source() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ffmpeg");
        fs::write(&file, b"x").unwrap();
        let request = UnverifiedRequest {
            tool: Tool::Ffmpeg,
            config_override: Some(file.clone()),
            env_var: Some("APP_FFMPEG".into()),
            allow_path_lookup: true,
        };
        let env = env_of(vec![
            ("APP_FFMPEG", file.clone().into_os_string()),
            ("PATH", dir.path().as_os_str().to_owned()),
        ]);
        assert!(matches!(
            resolve_unverified_with(&request, BuildMode::Release, &env),
            Err(MediaError::UnverifiedRefusedInRelease)
        ));
    }

    #[test]
    fn development_order_is_config_then_env_then_path_and_is_labelled() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config-ffmpeg");
        let from_env = dir.path().join("env-ffmpeg");
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let on_path = bin.join(executable_name(Tool::Ffmpeg));
        for file in [&config, &from_env, &on_path] {
            fs::write(file, b"x").unwrap();
        }
        let env = env_of(vec![
            ("APP_FFMPEG", from_env.clone().into_os_string()),
            (
                "PATH",
                env::join_paths([dir.path().join("missing"), bin.clone()]).unwrap(),
            ),
        ]);
        let mut request = UnverifiedRequest {
            tool: Tool::Ffmpeg,
            config_override: Some(config.clone()),
            env_var: Some("APP_FFMPEG".into()),
            allow_path_lookup: true,
        };
        let got = resolve_unverified_with(&request, BuildMode::Development, &env).unwrap();
        assert_eq!(
            (got.path(), got.source()),
            (config.as_path(), &UnverifiedSource::ConfigOverride)
        );
        assert!(!got.is_verified());
        assert!(got
            .label()
            .starts_with("UNVERIFIED ffmpeg (not catalog-checked)"));

        request.config_override = None;
        let got = resolve_unverified_with(&request, BuildMode::Development, &env).unwrap();
        assert_eq!(got.path(), from_env.as_path());
        assert!(got.label().contains("$APP_FFMPEG"));

        request.env_var = None;
        let got = resolve_unverified_with(&request, BuildMode::Development, &env).unwrap();
        assert_eq!(
            (got.path(), got.source()),
            (on_path.as_path(), &UnverifiedSource::PathLookup)
        );

        request.allow_path_lookup = false;
        assert!(matches!(
            resolve_unverified_with(&request, BuildMode::Development, &env),
            Err(MediaError::UnverifiedNotFound)
        ));
    }

    #[test]
    fn a_set_override_that_is_not_a_file_fails_instead_of_falling_through() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join(executable_name(Tool::Ffprobe)), b"x").unwrap();
        let env = env_of(vec![
            ("APP_FFPROBE", dir.path().join("nope").into_os_string()),
            ("PATH", bin.into_os_string()),
        ]);
        let request = UnverifiedRequest {
            tool: Tool::Ffprobe,
            config_override: None,
            env_var: Some("APP_FFPROBE".into()),
            allow_path_lookup: true,
        };
        assert!(matches!(
            resolve_unverified_with(&request, BuildMode::Development, &env),
            Err(MediaError::OverrideMissing { .. })
        ));
    }

    #[test]
    fn codec_and_filter_listings_parse_names_only() {
        let encoders = "Encoders:\n V..... = Video\n A..... = Audio\n ------\n V....D mpeg4                MPEG-4 part 2\n A....D libmp3lame           libmp3lame MP3 (MPEG audio layer 3) (codec mp3)\n A....D aac                  AAC (Advanced Audio Coding)\n";
        let set = parse_codec_list(encoders);
        assert_eq!(
            set.into_iter().collect::<Vec<_>>(),
            vec!["aac", "libmp3lame", "mpeg4"]
        );
        let filters = "Filters:\n  T.. = Timeline support\n  A = Audio input/output\n  | = Source or sink filter\n TSC acompressor        A->A       Audio compressor.\n ... ass               V->V       Render ASS subtitles.\n ..C ebur128           A->N       EBU R128 scanner.\n ... anullsrc          |->A       Null audio source.\n";
        assert_eq!(
            parse_filter_list(filters).into_iter().collect::<Vec<_>>(),
            vec!["acompressor", "anullsrc", "ass", "ebur128"]
        );
    }

    #[test]
    fn configuration_licence_flags_gpl_builds() {
        let gpl = Capabilities::from_parts(
            "ffmpeg version 8.1\nconfiguration: --enable-gpl --enable-libx264\n",
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeSet::new(),
        );
        assert_eq!(gpl.license, ConfigLicense::GplOrNonfree);
        let lgpl = Capabilities::from_parts(
            "ffmpeg version 7.1.5\nconfiguration: --disable-gpl --disable-nonfree\n",
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeSet::new(),
        );
        assert_eq!(lgpl.license, ConfigLicense::Lgpl);
        assert_eq!(lgpl.version_line, "ffmpeg version 7.1.5");
    }

    #[cfg(unix)]
    #[test]
    fn capability_probe_reads_fixture_listings_for_ffmpeg_and_skips_them_for_ffprobe() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-tool");
        // `run_version_probe` passes `-hide_banner <flag>`, so the flag is `$2`.
        fs::write(
            &script,
            r#"#!/bin/sh
case "$2" in
  -version) printf 'ffmpeg version 7.1.5 Copyright\nconfiguration: --disable-gpl --disable-nonfree --disable-autodetect\n' ;;
  -decoders) printf 'Decoders:\n V..... = Video\n ------\n V....D h264  H.264\n A....D aac   AAC\n' ;;
  -encoders) printf 'Encoders:\n ------\n A....D aac   AAC\n' ;;
  -filters) printf 'Filters:\n ... anullsrc  |->A  Null\n ... ass V->V  Render\n' ;;
  *) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let caps = probe_capabilities_at(&script, Tool::Ffmpeg, Duration::from_secs(10)).unwrap();
        assert_eq!(caps.version_line, "ffmpeg version 7.1.5 Copyright");
        assert_eq!(caps.license, ConfigLicense::Lgpl);
        assert!(caps.has_decoder("h264") && caps.has_decoder("aac"));
        assert!(caps.has_encoder("aac") && !caps.has_encoder("h264"));
        assert!(caps.has_filter("anullsrc") && caps.has_filter("ass"));
        assert!(!caps.has_filter("acompressor"));

        let probe_caps =
            probe_capabilities_at(&script, Tool::Ffprobe, Duration::from_secs(10)).unwrap();
        assert!(probe_caps.encoders.is_empty() && probe_caps.filters.is_empty());
        assert!(probe_caps.has_decoder("h264"));
    }
}
