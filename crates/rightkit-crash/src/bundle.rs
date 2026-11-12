//! User-initiated support bundle: a ZIP the user can inspect and choose to send.
//! Nothing here uploads. Every text file is key-redacted and scrubbed.

use crate::{create_zip, list_pending, CrashReport};
use rightkit_logs::{
    collect_diagnostics, scrub_value_counted, system, BundleMetadata, CollectionPolicy, LogSource,
    RedactionCounts,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SupportBundleOptions {
    pub app_name: String,
    pub app_version: String,
    pub build: String,
    pub crash_dir: PathBuf,
    pub log_sources: Vec<LogSource>,
    /// App-provided, content-free settings snapshot (counts, flags; no user content).
    pub settings: Value,
    pub policy: CollectionPolicy,
    /// Also include native samples / minidumps. They can hold memory contents,
    /// so this is off unless the user explicitly ticks it.
    pub include_native_dumps: bool,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleManifest {
    pub destination: PathBuf,
    pub crash_reports: usize,
    pub native_artifacts: usize,
    pub log_events: usize,
    /// One entry per app section passed to [`build_support_bundle_with_sections`], in order.
    pub sections: Vec<SectionDigest>,
    /// Redactions made while building the bundle, per kind (#793): the diagnostics collection
    /// (settings and log events), `summary.json`, and every app section, merged.
    pub redactions: RedactionCounts,
}

/// Digest of one app section as written into the bundle (#793).
#[derive(Debug, Clone, Serialize)]
pub struct SectionDigest {
    pub name: String,
    pub bytes: usize,
    pub sha256: String,
}

/// One raw file an app adds to its bundle (#1028). The bytes are copied as they are: the app owns
/// scrubbing and consent for them, exactly as it does for the app sections' source data.
#[derive(Debug, Clone)]
pub struct ExtraFile {
    /// Path inside the bundle root: one or more `/`-separated components, each 1 to 128 ASCII
    /// letters, digits, `.`, `-` or `_`, none starting with `.`. It may not collide with a
    /// file or directory the bundle writes itself.
    pub name: String,
    /// File to copy. It must be a regular file; a missing or unreadable source fails the bundle.
    pub source: PathBuf,
}

/// Layout switches for apps that want their whole archive from the owner (#1028). The default is
/// the layout [`build_support_bundle`] has always written.
#[derive(Debug, Clone)]
pub struct BundleExtras {
    /// Raw files added to the bundle root.
    pub extra_files: Vec<ExtraFile>,
    /// Name of the single top-level directory; `None` keeps `<app>-support-bundle`. 1 to 64
    /// ASCII letters, digits, `.`, `-` or `_`, not starting with `.`.
    pub root_dir_name: Option<String>,
    /// Write `diagnostics.json` (default true).
    pub include_diagnostics: bool,
    /// Write `summary.json` (default true).
    pub include_summary: bool,
    /// Write the `crash/` folder (default true). Off: no crash artifact is read at all.
    pub include_crash_artifacts: bool,
}

impl Default for BundleExtras {
    fn default() -> Self {
        Self {
            extra_files: Vec::new(),
            root_dir_name: None,
            include_diagnostics: true,
            include_summary: true,
            include_crash_artifacts: true,
        }
    }
}

/// File names written by [`admit_crash_artifacts`], relative to the staging directory.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AdmittedCrashArtifacts {
    /// Finished `crash_*.json` reports (pending and `submitted/`), each written through
    /// [`CrashReport::to_scrubbed_value`].
    pub reports: Vec<String>,
    /// `dump_*` and `sample_*` files; empty unless `include_native_dumps`.
    pub native: Vec<String>,
    /// `dump_*` files that could not be copied into `staging`, left out and reported here. The
    /// rest of the call carries on.
    pub skipped: Vec<String>,
}

/// Admits crash artifacts into a caller's staging directory under the same rules the support
/// bundle uses, so an app that builds its own archive never re-implements them (#1028).
///
/// * Finished reports from `crash_dir` and `crash_dir/submitted` are parsed and written as the
///   scrubbed value; a file that is not a readable report is skipped.
/// * With `include_native_dumps`, `dump_*` files are copied and `sample_*` text is scrubbed again
///   on the way in. A sample that cannot be read or scrubbed is replaced by a short withheld
///   marker, never by the raw bytes. A leftover raw sample never matches.
/// * Failure rule: a `dump_*` file that cannot be copied is skipped (any partial copy removed)
///   and named in [`AdmittedCrashArtifacts::skipped`]; one bad dump does not cost the rest of
///   the bundle. A report or sample that cannot be written into `staging`, or a `staging` that
///   cannot be created, fails the whole call with an error naming it. Files already written
///   stay in `staging` for the caller to discard.
///
/// `staging` is created when missing. Names are returned in admission order.
pub fn admit_crash_artifacts(
    crash_dir: &Path,
    staging: &Path,
    include_native_dumps: bool,
) -> Result<AdmittedCrashArtifacts, String> {
    let mut admitted = AdmittedCrashArtifacts::default();
    // Already-queued reports live under submitted/; include both generations.
    let mut report_paths = list_pending(crash_dir);
    report_paths.extend(list_pending(&crash_dir.join("submitted")));
    for path in report_paths {
        let Some(report) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CrashReport>(&bytes).ok())
        else {
            continue;
        };
        std::fs::create_dir_all(staging)
            .map_err(|e| format!("create {}: {e}", staging.display()))?;
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        write_json(&staging.join(&name), &report.to_scrubbed_value())?;
        admitted.reports.push(name);
    }

    if include_native_dumps {
        if let Ok(entries) = std::fs::read_dir(crash_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !(is_native_artifact_name(&name) && entry.path().is_file()) {
                    continue;
                }
                std::fs::create_dir_all(staging)
                    .map_err(|e| format!("create {}: {e}", staging.display()))?;
                let target = staging.join(&name);
                if name.starts_with("sample_") {
                    // Text sample: scrub again on the way in (a file an older release left
                    // unscrubbed must not reach the bundle). Bytes go straight from memory to
                    // the staging file; on any failure the marker is written, never the raw.
                    let text = match std::fs::read(entry.path()) {
                        Ok(bytes) => crate::native::scrub_for_bundle(&bytes),
                        Err(_) => crate::native::WITHHELD_MARKER.to_string(),
                    };
                    std::fs::write(&target, text)
                        .map_err(|e| format!("write {}: {e}", target.display()))?;
                } else {
                    if std::fs::copy(entry.path(), &target).is_err() {
                        let _ = std::fs::remove_file(&target);
                        admitted.skipped.push(name);
                        continue;
                    }
                }
                admitted.native.push(name);
            }
        }
    }
    Ok(admitted)
}

pub fn build_support_bundle(options: &SupportBundleOptions) -> Result<BundleManifest, String> {
    build_support_bundle_with_sections(options, &[])
}

/// Build the support bundle with app-owned JSON sections (a session transcript, config,
/// health). Each section is redacted with the same scrub as the rest of the bundle, written
/// to `sections/<name>.json`, and digested (SHA-256 of the written bytes) in the manifest and
/// in `sections/index.json`. A section name is 1 to 64 ASCII letters, digits, `-` or `_`, and
/// names must be unique; anything else is refused before any file is written.
pub fn build_support_bundle_with_sections(
    options: &SupportBundleOptions,
    sections: &[(String, Value)],
) -> Result<BundleManifest, String> {
    build_support_bundle_with_extras(options, sections, &BundleExtras::default())
}

/// [`build_support_bundle_with_sections`] plus the layout switches and raw files of
/// [`BundleExtras`] (#1028). Every name and path in `extras` is validated before any file is
/// written.
pub fn build_support_bundle_with_extras(
    options: &SupportBundleOptions,
    sections: &[(String, Value)],
    extras: &BundleExtras,
) -> Result<BundleManifest, String> {
    validate_extras(extras)?;
    let mut seen = std::collections::BTreeSet::new();
    for (name, _) in sections {
        let valid = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !valid {
            return Err(format!("invalid bundle section name: {name:?}"));
        }
        if !seen.insert(name.as_str()) {
            return Err(format!("duplicate bundle section name: {name}"));
        }
    }
    let parent = options
        .destination
        .parent()
        .ok_or_else(|| "bundle destination has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create bundle directory: {e}"))?;
    let stem = match &extras.root_dir_name {
        Some(name) => name.clone(),
        None => format!(
            "{}-support-bundle",
            options.app_name.replace(['/', '\\', ' '], "-")
        ),
    };
    let staging = parent.join(format!(".{stem}-staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let root = staging.join(&stem);
    std::fs::create_dir_all(&root).map_err(|e| format!("create staging: {e}"))?;
    let outcome = stage_and_zip(options, &root, sections, extras);
    let _ = std::fs::remove_dir_all(&staging);
    outcome
}

fn stage_and_zip(
    options: &SupportBundleOptions,
    root: &std::path::Path,
    sections: &[(String, Value)],
    extras: &BundleExtras,
) -> Result<BundleManifest, String> {
    let identity = system::platform_identity();
    let diagnostics = collect_diagnostics(
        &options.log_sources,
        BundleMetadata {
            app_name: options.app_name.clone(),
            app_version: options.app_version.clone(),
            build: options.build.clone(),
            os: std::env::consts::OS.to_string(),
            settings: options.settings.clone(),
        },
        &options.policy,
    );
    let log_events: usize = diagnostics.sources.iter().map(|s| s.events.len()).sum();
    let mut diagnostics_value = serde_json::to_value(&diagnostics).map_err(|e| e.to_string())?;
    let mut redactions = diagnostics.redaction_summary();
    redactions.merge(&scrub_value_counted(&mut diagnostics_value));
    if extras.include_diagnostics {
        write_json(&root.join("diagnostics.json"), &diagnostics_value)?;
    }

    let mut summary = json!({
        "app": { "name": options.app_name, "version": options.app_version, "build": options.build },
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "os_version": identity.os_version,
            "os_build": identity.os_build,
            "chip": identity.chip,
            "hw_model": identity.hw_model,
            "memory_total_mb": identity.memory_total_mb,
        },
        "resources": system::resource_snapshot(),
        "privacy": "Redacted on this device. Contains no transcripts, window titles, clipboard, credentials or user names. Nothing is sent unless you send it.",
    });
    redactions.merge(&scrub_value_counted(&mut summary));
    if extras.include_summary {
        write_json(&root.join("summary.json"), &summary)?;
    }

    let mut section_digests = Vec::with_capacity(sections.len());
    if !sections.is_empty() {
        let sections_dir = root.join("sections");
        std::fs::create_dir_all(&sections_dir).map_err(|e| format!("create sections: {e}"))?;
        for (name, value) in sections {
            let mut value = value.clone();
            redactions.merge(&scrub_value_counted(&mut value));
            let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
            let digest: String = rightkit_crypto::sha256(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let path = sections_dir.join(format!("{name}.json"));
            std::fs::write(&path, &bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
            section_digests.push(SectionDigest {
                name: name.clone(),
                bytes: bytes.len(),
                sha256: digest,
            });
        }
        let index = serde_json::to_value(&section_digests).map_err(|e| e.to_string())?;
        write_json(&sections_dir.join("index.json"), &index)?;
    }

    let (crash_reports, native_artifacts) = if extras.include_crash_artifacts {
        let admitted = admit_crash_artifacts(
            &options.crash_dir,
            &root.join("crash"),
            options.include_native_dumps,
        )?;
        (admitted.reports.len(), admitted.native.len())
    } else {
        (0, 0)
    };

    for extra in &extras.extra_files {
        let target = root.join(&extra.name);
        if target.exists() {
            return Err(format!(
                "extra file collides with a bundle file: {}",
                extra.name
            ));
        }
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        std::fs::copy(&extra.source, &target)
            .map_err(|e| format!("copy extra file {}: {e}", extra.source.display()))?;
    }

    write_json(
        &root.join("redactions.json"),
        &serde_json::to_value(redactions).map_err(|e| e.to_string())?,
    )?;

    create_zip(root, &options.destination)?;
    Ok(BundleManifest {
        destination: options.destination.clone(),
        crash_reports,
        native_artifacts,
        log_events,
        sections: section_digests,
        redactions,
    })
}

/// Published native artifacts only. A leftover raw sample (`.sample_raw_*.raw`, see
/// `native::RAW_PREFIX`) does not match, and `sample_*` text is scrubbed again on copy.
fn is_native_artifact_name(name: &str) -> bool {
    name.starts_with("dump_") || name.starts_with("sample_")
}

fn write_json(path: &std::path::Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

fn name_ok(component: &str, max: usize) -> bool {
    !component.is_empty()
        && component.len() <= max
        && !component.starts_with('.')
        && component
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn validate_extras(extras: &BundleExtras) -> Result<(), String> {
    if let Some(root) = &extras.root_dir_name {
        if !name_ok(root, 64) {
            return Err(format!("invalid bundle root directory name: {root:?}"));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for extra in &extras.extra_files {
        let path = Path::new(&extra.name);
        let plain = !extra.name.contains('\\')
            && path
                .components()
                .all(|c| matches!(c, Component::Normal(n) if name_ok(&n.to_string_lossy(), 128)))
            && path.components().count() > 0;
        if !plain {
            return Err(format!("invalid extra file name: {:?}", extra.name));
        }
        let top = extra.name.split('/').next().unwrap_or_default();
        let reserved = matches!(top, "crash" | "sections")
            || extra.name == "redactions.json"
            || (extra.name == "diagnostics.json" && extras.include_diagnostics)
            || (extra.name == "summary.json" && extras.include_summary);
        if reserved {
            return Err(format!(
                "extra file name is reserved by the bundle: {}",
                extra.name
            ));
        }
        if !seen.insert(extra.name.as_str()) {
            return Err(format!("duplicate extra file name: {}", extra.name));
        }
        if !extra.source.is_file() {
            return Err(format!(
                "extra file source is not a file: {}",
                extra.source.display()
            ));
        }
    }
    Ok(())
}
