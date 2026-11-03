//! What a release must bundle to ship the catalog's FFmpeg variants with an app (rightkit#246).
//!
//! [`ArtifactCatalog::bundle_plan`] answers, for the tools and variant an app ships and the
//! platforms it builds for: which retained archive to fetch (url, archive digest, member), the
//! content digest the shipped file must match, where Tauri expects the file
//! (`<stem>-<triple>[.exe]`), every notice file that must ship beside it, and the text of the source
//! availability statement ([`BundlePlan::source_offer`]). The release tool (`right-release`,
//! stage-sidecar `sources`) consumes it; nothing here downloads or copies.
//!
//! Nothing is guessed: a requested tool, variant and platform without an approved row is an error
//! (a platform the catalog has no row for, such as Linux today, is refused, never skipped), and a
//! row with no retained archive is listed as `archive: None` so the release owner sees that bytes
//! must come from somewhere else. No digest is ever invented.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::archive::ArchiveRef;
use crate::{ArtifactCatalog, MediaError, Tool};

/// One shipped file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleEntry {
    /// Catalog row id.
    pub id: String,
    pub tool: Tool,
    pub variant: String,
    pub os: String,
    pub arch: String,
    pub version: String,
    /// Rust target triple Tauri names this platform by.
    pub target_triple: &'static str,
    /// File name inside the archive and beside the app (`ffmpeg`, `ffmpeg.exe`).
    pub file_name: String,
    /// `<tool>-<triple>[.exe]`, the `externalBin` staging name.
    pub external_bin_name: String,
    /// Where the retained bytes are fetched; `None` where the catalog holds none.
    pub archive: Option<ArchiveRef>,
    /// The signature-independent digest the shipped file must match.
    pub content_sha256: String,
    /// SPDX expression of the binary.
    pub license_spdx: String,
    /// Official FFmpeg source tarball and its digest.
    pub source_url: String,
    pub source_sha256: String,
    /// Notice files that ship with the binary, relative to the crate root.
    pub notice_files: Vec<String>,
    /// The build-notes file that records the configuration.
    pub build_provenance: String,
}

/// Everything to bundle for one app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundlePlan {
    pub entries: Vec<BundleEntry>,
    /// Every notice file any entry needs, de-duplicated, in order.
    pub notice_files: Vec<String>,
}

impl ArtifactCatalog {
    /// The bundle plan for `parts` (tool and variant pairs, for example
    /// `[(Tool::Ffmpeg, "decode"), (Tool::Ffprobe, "decode")]`) on each `(os, arch)` target.
    /// Every combination must be approved in the catalog, or the whole plan is refused with the
    /// catalog's own error.
    pub fn bundle_plan(
        &self,
        parts: &[(Tool, &str)],
        targets: &[(&str, &str)],
    ) -> Result<BundlePlan, MediaError> {
        let mut entries = Vec::new();
        let mut notices: Vec<String> = Vec::new();
        for &(os, arch) in targets {
            let triple = crate::tauri_target_triple(os, arch).ok_or_else(|| {
                MediaError::NoApprovedArtifact {
                    os: os.to_owned(),
                    arch: arch.to_owned(),
                }
            })?;
            for &(tool, variant) in parts {
                let row = self.approved_for(tool, variant, os, arch)?;
                let content_sha256 = row.approved_content_sha256()?.to_owned();
                for notice in &row.attribution.notice_files {
                    if !notices.contains(notice) {
                        notices.push(notice.clone());
                    }
                }
                let exe = if triple.contains("windows") {
                    ".exe"
                } else {
                    ""
                };
                entries.push(BundleEntry {
                    id: row.id.clone(),
                    tool,
                    variant: row.variant.clone(),
                    os: row.os.clone(),
                    arch: row.arch.clone(),
                    version: row.version.clone(),
                    target_triple: triple,
                    file_name: tool.file_name(os),
                    external_bin_name: format!("{}-{triple}{exe}", tool.stem()),
                    archive: row.archive.clone(),
                    content_sha256,
                    license_spdx: row.attribution.license_spdx.clone(),
                    source_url: row.attribution.source_url.clone(),
                    source_sha256: row.source_sha256.clone(),
                    notice_files: row.attribution.notice_files.clone(),
                    build_provenance: row.attribution.build_provenance.clone(),
                });
            }
        }
        Ok(BundlePlan {
            entries,
            notice_files: notices,
        })
    }
}

impl BundlePlan {
    /// The text of the source availability statement to ship with the app, for the FFmpeg
    /// versions in this plan: the official source tarball and digest, the build-notes file that
    /// records the configuration, and every statically linked component's source. `contact` is the
    /// app's address for a source request. This states what the catalog records; it is not legal
    /// advice and does not replace the notice files, which ship beside it.
    pub fn source_offer(&self, product: &str, contact: &str, catalog: &ArtifactCatalog) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{product} ships FFmpeg, licensed LGPL-2.1-or-later (see the shipped notice files for the complete text and the components' licences)."
        );
        let _ = writeln!(
            out,
            "The FFmpeg source this build was made from, and how it was configured, are recorded below. For anything else you need to rebuild or replace the FFmpeg binaries, write to {contact}.\n"
        );
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for entry in &self.entries {
            if !seen.insert(entry.id.clone()) {
                continue;
            }
            let _ = writeln!(
                out,
                "{} ({}, {} {}):",
                entry.id, entry.variant, entry.os, entry.arch
            );
            let _ = writeln!(out, "  source: {}", entry.source_url);
            let _ = writeln!(out, "  source sha256: {}", entry.source_sha256);
            let _ = writeln!(out, "  build configuration: {}", entry.build_provenance);
            if let Some(row) = catalog.by_id(&entry.id) {
                let _ = writeln!(
                    out,
                    "  required configure flags: {}",
                    row.required_configure_flags.join(" ")
                );
                for component in &row.attribution.components {
                    let pin = match (&component.source_sha256, &component.source_commit) {
                        (Some(digest), _) => format!("sha256 {digest}"),
                        (None, Some(commit)) => format!("commit {commit}"),
                        (None, None) => "no pin recorded".to_owned(),
                    };
                    let _ = writeln!(
                        out,
                        "  component {} {} ({}): {} ({pin})",
                        component.name,
                        component.version,
                        component.license_spdx,
                        component.source_url
                    );
                }
            }
            let _ = writeln!(out);
        }
        out
    }
}
