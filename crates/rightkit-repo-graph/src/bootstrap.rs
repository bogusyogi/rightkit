//! Native port of `blueprint/src/graph/bootstrap.mjs` and the portable
//! manifest validation of `blueprint/src/graph/portable-manifest.mjs`.
//!
//! If `.agent/manifest.json` exists in the repo, it is the authoritative
//! descriptor for the canonical graph generation. Bootstrap verifies the
//! recorded `repo.sourceHash` against the current checkout; on match, it
//! returns the tracked generation descriptor. On mismatch, it reports
//! `Stale` so callers fall back to a full rebuild.
//!
//! Reuses `graph::is_canonical_ignored_dir` / `graph::is_canonical_ignored_file`
//! for the source-universe walk (shared canonical ignore policy, read-only
//! use of an existing public function — `graph.rs` itself is untouched) and
//! `identity::content_digest` for the xxh3-128 content hashing algorithm the
//! legacy `sourceHash` also uses.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::graph::RepoIgnore;
use crate::identity::content_digest;

pub const AGENT_DIR_NAME: &str = ".agent";
pub const MANIFEST_NAME: &str = "manifest.json";

/// A file gathered during the source scan: its repo-relative path and the
/// content digest of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: String,
    pub content_hash: String,
}

/// Result of [`bootstrap_from_tracked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapOutcome {
    /// No tracked manifest exists at `.agent/manifest.json`.
    Missing { manifest_path: Option<PathBuf> },
    /// A tracked manifest exists but failed to parse or validate.
    Corrupt {
        manifest_path: PathBuf,
        errors: Vec<String>,
    },
    /// A tracked manifest exists but the recorded sourceHash does not match
    /// the current checkout.
    Stale {
        manifest_path: PathBuf,
        manifest: Value,
        recorded_hash: Option<String>,
        current_hash: String,
    },
    /// The tracked manifest is authoritative over the current checkout.
    Ready {
        manifest_path: PathBuf,
        descriptor: Value,
        file_count: usize,
        content_hash: String,
    },
}

fn normalize_path(value: &str) -> String {
    let mut v = value.replace('\\', "/");
    while let Some(rest) = v.strip_prefix("./") {
        v = rest.to_string();
    }
    v
}

/// Walk `root`, skipping the canonical ignored dirs/files, and return every
/// file's repo-relative path with its xxh3-128 content digest. Files larger
/// than 2 MiB or unreadable are skipped, mirroring the legacy scanner's
/// size cap and best-effort error handling.
pub fn scan_source_files(root: &Path) -> Vec<ScannedFile> {
    const MAX_BYTES: u64 = 2 * 1024 * 1024;
    let ignore = RepoIgnore::for_root(root);
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = match path.strip_prefix(root) {
                Ok(p) => normalize_path(&p.to_string_lossy()),
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if ignore.dir_ignored(&relative, &name) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if ignore.file_ignored(&relative, &name) {
                continue;
            }
            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if metadata.len() > MAX_BYTES {
                continue;
            }
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            out.push(ScannedFile {
                path: relative,
                content_hash: content_digest(&bytes),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// `xxh128:<hex>` over `"{path}:{contentHash}"` for every file, newline
/// joined and sorted by path — matches the legacy `sourceHash` formula
/// (minus the `isGeneratedDoc` filter, which lives entirely in doc-domain
/// providers this port does not depend on).
pub fn source_hash(files: &[ScannedFile]) -> String {
    let joined = files
        .iter()
        .map(|f| format!("{}:{}", f.path, f.content_hash))
        .collect::<Vec<_>>()
        .join("\n");
    content_digest(joined.as_bytes())
}

const ABSOLUTE_INDICATOR_WINDOWS_DRIVE: &str = "windows_drive";
const ABSOLUTE_INDICATOR_MAC_USERS: &str = "mac_users";
const ABSOLUTE_INDICATOR_LINUX_HOME: &str = "linux_home";
const ABSOLUTE_INDICATOR_MAC_VOLUMES: &str = "mac_volumes";
const ABSOLUTE_INDICATOR_UNC_PATH: &str = "unc_path";

/// Scan the JSON text of `value` for substrings that indicate a
/// machine-local absolute path leaked into a supposedly portable manifest.
pub fn find_absolute_path_indicators(value: &Value) -> Vec<&'static str> {
    let raw = value.to_string();
    let mut findings = Vec::new();
    if windows_drive_pattern(&raw) {
        findings.push(ABSOLUTE_INDICATOR_WINDOWS_DRIVE);
    }
    if raw.contains("/Users/") {
        findings.push(ABSOLUTE_INDICATOR_MAC_USERS);
    }
    if raw.contains("/home/") {
        findings.push(ABSOLUTE_INDICATOR_LINUX_HOME);
    }
    if raw.contains("/Volumes/") {
        findings.push(ABSOLUTE_INDICATOR_MAC_VOLUMES);
    }
    if raw.contains("\\\\") {
        findings.push(ABSOLUTE_INDICATOR_UNC_PATH);
    }
    findings
}

fn windows_drive_pattern(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        let letter = bytes[i];
        if letter.is_ascii_alphabetic() && bytes[i + 1] == b':' {
            if let Some(&next) = bytes.get(i + 2) {
                if next == b'\\' || next == b'/' {
                    return true;
                }
            }
        }
    }
    false
}

pub fn validate_portable_relative_path(path: &str, field_name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let raw = path.trim();
    if raw.is_empty() {
        errors.push(format!("{field_name} is empty"));
        return errors;
    }
    let normalized = raw.replace('\\', "/");
    let is_windows_absolute = {
        let bytes = normalized.as_bytes();
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
    };
    if normalized.starts_with('/') || is_windows_absolute {
        errors.push(format!("{field_name} is absolute"));
    }
    if has_parent_traversal(&normalized) {
        errors.push(format!("{field_name} contains parent traversal"));
    }
    if normalized.contains('\0') {
        errors.push(format!("{field_name} contains a null byte"));
    }
    errors
}

fn has_parent_traversal(normalized: &str) -> bool {
    normalized.split('/').any(|segment| segment == "..")
}

/// Port of `validatePortableManifest`: returns the empty vec when the
/// manifest is valid, else every violation found.
pub fn validate_portable_manifest(manifest: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if !manifest.is_object() {
        return vec!["manifest is not a JSON object".to_string()];
    }
    if manifest.get("schemaVersion").and_then(Value::as_i64) != Some(1) {
        let found = manifest
            .get("schemaVersion")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "undefined".to_string());
        errors.push(format!("unsupported schemaVersion {found}; expected 1"));
    }
    let path_findings = find_absolute_path_indicators(manifest);
    if !path_findings.is_empty() {
        errors.push(format!(
            "manifest contains absolute machine path indicators: {}",
            path_findings.join(", ")
        ));
    }
    if let Some(artifacts) = manifest.get("artifacts").and_then(Value::as_object) {
        for (key, value) in artifacts {
            if let Some(path) = value.as_str() {
                errors.extend(validate_portable_relative_path(
                    path,
                    &format!("artifacts.{key}"),
                ));
            }
        }
    }
    if let Some(human_docs) = manifest.get("humanDocs").and_then(Value::as_array) {
        for (index, value) in human_docs.iter().enumerate() {
            if let Some(path) = value.as_str() {
                errors.extend(validate_portable_relative_path(
                    path,
                    &format!("humanDocs[{index}]"),
                ));
            }
        }
    }
    if let Some(entrypoint) = manifest.get("entrypoint") {
        if let Some(path) = entrypoint.as_str() {
            errors.extend(validate_portable_relative_path(path, "entrypoint"));
        }
    }
    let has_source_hash = manifest
        .get("repo")
        .and_then(|repo| repo.get("sourceHash"))
        .map(|v| !v.is_null())
        .unwrap_or(false);
    if !has_source_hash {
        errors.push("manifest is missing repo.sourceHash".to_string());
    }
    errors
}

fn manifest_path_for(repo_root: &Path) -> PathBuf {
    repo_root.join(AGENT_DIR_NAME).join(MANIFEST_NAME)
}

/// Attempt to load the tracked portable contract for `repo_root`. Never
/// fails: absent, malformed, or drifted manifests are reported as typed
/// outcomes rather than errors.
pub fn bootstrap_from_tracked(repo_root: &Path) -> BootstrapOutcome {
    let manifest_path = manifest_path_for(repo_root);
    if !manifest_path.exists() {
        return BootstrapOutcome::Missing {
            manifest_path: None,
        };
    }
    let raw = match fs::read_to_string(&manifest_path) {
        Ok(r) => r,
        Err(_) => {
            return BootstrapOutcome::Missing {
                manifest_path: Some(manifest_path),
            }
        }
    };
    let manifest: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(error) => {
            return BootstrapOutcome::Corrupt {
                manifest_path,
                errors: vec![error.to_string()],
            };
        }
    };
    if !manifest.is_object() {
        return BootstrapOutcome::Corrupt {
            manifest_path,
            errors: vec!["manifest is not a JSON object".to_string()],
        };
    }
    let validation_errors = validate_portable_manifest(&manifest);
    if !validation_errors.is_empty() {
        return BootstrapOutcome::Corrupt {
            manifest_path,
            errors: validation_errors,
        };
    }
    let path_findings = find_absolute_path_indicators(&manifest);
    if !path_findings.is_empty() {
        return BootstrapOutcome::Corrupt {
            manifest_path,
            errors: vec![format!(
                "manifest contains absolute machine path indicators: {}",
                path_findings.join(", ")
            )],
        };
    }

    let files = scan_source_files(repo_root);
    let current_hash = source_hash(&files);
    let recorded_hash = manifest
        .get("repo")
        .and_then(|r| r.get("sourceHash"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    if recorded_hash.as_deref() == Some(current_hash.as_str()) {
        let generation = manifest
            .get("generation")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let descriptor = serde_json::json!({
            "schemaVersion": 1,
            "id": generation.get("id").cloned().unwrap_or(Value::Null),
            "revision": generation.get("id").cloned().unwrap_or(Value::Null),
            "indexedAt": manifest.get("generatedAt").cloned().unwrap_or(Value::Null),
            "stale": false,
            "sourceKind": "agent-tracked",
            "toolVersions": generation.get("toolVersions").cloned().unwrap_or_else(|| Value::Object(Default::default())),
            "providerCapabilities": generation.get("providerCapabilities").cloned()
                .or_else(|| manifest.get("capabilities").and_then(|c| c.get("outputs")).cloned())
                .unwrap_or_else(|| Value::Array(vec![])),
            "supportedEdgeTypes": generation.get("supportedEdgeTypes").cloned().unwrap_or_else(|| Value::Array(vec![])),
            "supportedLanguages": generation.get("supportedLanguages").cloned().unwrap_or_else(|| Value::Array(vec![])),
            "fileCount": files.len(),
            "contentHash": current_hash,
            "frozen": true,
            "manifestPath": manifest_path.to_string_lossy(),
            "manifest": manifest,
        });
        return BootstrapOutcome::Ready {
            manifest_path,
            descriptor,
            file_count: files.len(),
            content_hash: current_hash,
        };
    }

    BootstrapOutcome::Stale {
        manifest_path,
        manifest,
        recorded_hash,
        current_hash,
    }
}
