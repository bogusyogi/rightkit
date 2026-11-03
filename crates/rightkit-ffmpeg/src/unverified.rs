//! Capability probing for catalog-verified FFmpeg builds.
//!
//! [`VerifiedFfmpeg::probe_capabilities`] lists the encoders, decoders and filters a verified
//! build carries, so a pipeline checks for what it needs instead of assuming it. There is no
//! unverified binary, no build mode and no environment override in any build (Adrian,
//! 2026-11-03: "dev mode, is mine ... real data.").

use std::{collections::BTreeSet, path::Path, time::Duration};

use crate::{run_version_probe, MediaError, Tool, VerifiedFfmpeg, FORBIDDEN_CONFIGURE_FLAGS};

impl VerifiedFfmpeg {
    /// Lists the encoders, decoders and filters of a catalog-verified `ffmpeg`. Use it to confirm an encoder or filter the build
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
