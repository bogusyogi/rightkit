//! User-initiated support bundle: a ZIP the user can inspect and choose to send.
//! Nothing here uploads. Every text file is key-redacted and scrubbed.

use crate::{create_zip, list_pending, CrashReport};
use rightkit_logs::{
    collect_diagnostics, scrub_value, system, BundleMetadata, CollectionPolicy, LogSource,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;

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
}

pub fn build_support_bundle(options: &SupportBundleOptions) -> Result<BundleManifest, String> {
    let parent = options
        .destination
        .parent()
        .ok_or_else(|| "bundle destination has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create bundle directory: {e}"))?;
    let stem = format!("{}-support-bundle", options.app_name.replace(['/', '\\', ' '], "-"));
    let staging = parent.join(format!(".{stem}-staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let root = staging.join(&stem);
    std::fs::create_dir_all(&root).map_err(|e| format!("create staging: {e}"))?;
    let outcome = stage_and_zip(options, &root);
    let _ = std::fs::remove_dir_all(&staging);
    outcome
}

fn stage_and_zip(options: &SupportBundleOptions, root: &std::path::Path) -> Result<BundleManifest, String> {
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
    scrub_value(&mut diagnostics_value);
    write_json(&root.join("diagnostics.json"), &diagnostics_value)?;

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
    scrub_value(&mut summary);
    write_json(&root.join("summary.json"), &summary)?;

    let mut crash_reports = 0;
    let crash_out = root.join("crash");
    // Already-queued reports live under submitted/; include both generations.
    let mut report_paths = list_pending(&options.crash_dir);
    report_paths.extend(list_pending(&options.crash_dir.join("submitted")));
    for path in report_paths {
        let Some(report) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CrashReport>(&bytes).ok())
        else {
            continue;
        };
        std::fs::create_dir_all(&crash_out).map_err(|e| e.to_string())?;
        let name = path.file_name().unwrap_or_default();
        write_json(&crash_out.join(name), &report.to_scrubbed_value())?;
        crash_reports += 1;
    }

    let mut native_artifacts = 0;
    if options.include_native_dumps {
        if let Ok(entries) = std::fs::read_dir(&options.crash_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if (name.starts_with("dump_") || name.starts_with("sample_")) && entry.path().is_file() {
                    std::fs::create_dir_all(&crash_out).map_err(|e| e.to_string())?;
                    std::fs::copy(entry.path(), crash_out.join(&name)).map_err(|e| e.to_string())?;
                    native_artifacts += 1;
                }
            }
        }
    }

    create_zip(root, &options.destination)?;
    Ok(BundleManifest {
        destination: options.destination.clone(),
        crash_reports,
        native_artifacts,
        log_events,
    })
}

fn write_json(path: &std::path::Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}
