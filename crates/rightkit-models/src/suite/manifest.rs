//! Per-entry manifest: `manifest.json` records id, version, and every file's
//! sha256 + size so lookups can prove a store copy is complete — and name the
//! exact files to re-download when it is not.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::suite::paths::manifest_path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Relative path inside the entry dir (forward slashes).
    pub path: String,
    /// Lowercase trusted SHA-256. Missing hash or size is Unverified &
    /// cannot establish Ready; installed manifests use catalog pins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Store id — the entry dir name (`parakeet_tdt_v3_...`, `pp-ocrv6`).
    pub id: String,
    /// Free-form version label for status display + tool versioning.
    #[serde(default)]
    pub version: String,
    pub files: Vec<ManifestFile>,
}

/// How thoroughly [`verify_dir`] checks each file.
///
/// - `Presence` — exists and is non-empty (zero-byte files count as missing:
///   a truncated copy is not a usable model file).
/// - `Size` — presence plus recorded size match. This is the fast path used
///   when resolving "which dir do we run" on every engine start; hashing
///   ~700 MB per launch is not acceptable, and a same-size silent corruption
///   is still caught by the next Full check (status/repair/onboarding).
/// - `Full` — size plus recorded SHA-256. Used by status, ensure, repair,
///   and migration — anywhere "is this copy proven" is the actual question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    Presence,
    Size,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Ok,
    Missing,
    /// Present but hash (or size) does not match the manifest — re-download.
    Mismatched,
    /// Present without trusted hash/size metadata; never ready.
    Unverified,
}

#[derive(Debug, Clone)]
pub struct FileCheck {
    pub path: String,
    pub state: FileState,
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    pub checks: Vec<FileCheck>,
}

impl VerifyReport {
    /// Every required file has trusted metadata & matches requested depth.
    pub fn is_ready(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|c| c.state == FileState::Ok)
    }

    /// Files that need a (re-)download: missing or mismatched.
    pub fn needed(&self) -> Vec<&FileCheck> {
        self.checks
            .iter()
            .filter(|c| c.state != FileState::Ok)
            .collect()
    }

    pub fn files_ok(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.state == FileState::Ok)
            .count()
    }
}

/// Verify `entry_dir` against `manifest` at `mode` depth: file-by-file
/// presence (non-empty), recorded size, and (in `Full` mode, where recorded)
/// SHA-256.
pub fn verify_dir(entry_dir: &Path, manifest: &Manifest, mode: VerifyMode) -> VerifyReport {
    let checks = manifest
        .files
        .iter()
        .map(|file| {
            let Some(rel) = safe_rel_path(&file.path) else {
                // A manifest path that escapes the entry dir is a defect, not
                // a file to trust — count it as mismatched so it is repaired.
                return FileCheck {
                    path: file.path.clone(),
                    state: FileState::Mismatched,
                    size: 0,
                };
            };
            let full = entry_dir.join(rel);
            match fs::metadata(&full) {
                Err(_) => FileCheck {
                    path: file.path.clone(),
                    state: FileState::Missing,
                    size: 0,
                },
                Ok(meta) if !meta.is_file() || meta.len() == 0 => FileCheck {
                    path: file.path.clone(),
                    state: FileState::Missing,
                    size: meta.len(),
                },
                Ok(meta) => {
                    let size = meta.len();
                    if mode != VerifyMode::Presence {
                        if let Some(expected_size) = file.size {
                            if expected_size != size {
                                return FileCheck {
                                    path: file.path.clone(),
                                    state: FileState::Mismatched,
                                    size,
                                };
                            }
                        }
                    }
                    if file.size.is_none()
                        || !file.sha256.as_deref().is_some_and(|hash| {
                            hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                    {
                        return FileCheck {
                            path: file.path.clone(),
                            state: FileState::Unverified,
                            size,
                        };
                    }
                    match (mode, file.sha256.as_deref()) {
                        (_, None) => FileCheck {
                            path: file.path.clone(),
                            state: FileState::Unverified,
                            size,
                        },
                        (VerifyMode::Full, Some(expected)) => match sha256_file(&full) {
                            Ok(got) if got.eq_ignore_ascii_case(expected) => FileCheck {
                                path: file.path.clone(),
                                state: FileState::Ok,
                                size,
                            },
                            _ => FileCheck {
                                path: file.path.clone(),
                                state: FileState::Mismatched,
                                size,
                            },
                        },
                        // Trusted identity supplies the hash; this mode checks
                        // live presence/size. Full checks actual bytes too.
                        (_, Some(_)) => FileCheck {
                            path: file.path.clone(),
                            state: FileState::Ok,
                            size,
                        },
                    }
                }
            }
        })
        .collect();
    VerifyReport { checks }
}

/// A local record cannot choose its own identity or weaken trusted pins.
pub fn matches_trusted(actual: &Manifest, trusted: &Manifest) -> bool {
    if actual.id != trusted.id
        || actual.version != trusted.version
        || actual.files.len() != trusted.files.len()
        || trusted.files.is_empty()
    {
        return false;
    }
    let mut seen = std::collections::BTreeSet::new();
    actual.files.iter().all(|file| {
        seen.insert(&file.path)
            && trusted.files.iter().any(|pin| {
                file.path == pin.path
                    && file.sha256 == pin.sha256
                    && file.size == pin.size
                    && pin.size.is_some()
                    && pin.sha256.is_some()
            })
    })
}

/// Read `manifest.json` inside `entry_dir`. `Ok(None)` when absent.
pub fn read_manifest(entry_dir: &Path) -> Result<Option<Manifest>, String> {
    let path = manifest_path(entry_dir);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let manifest =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(Some(manifest))
}

/// Write `manifest.json` inside `entry_dir` atomically (tmp + rename).
pub fn write_manifest(entry_dir: &Path, manifest: &Manifest) -> Result<(), String> {
    fs::create_dir_all(entry_dir).map_err(|e| format!("create {}: {e}", entry_dir.display()))?;
    let path = manifest_path(entry_dir);
    let json =
        serde_json::to_vec_pretty(manifest).map_err(|e| format!("serialize manifest: {e}"))?;
    write_atomic(&path, &json)
}

/// Durable replacement with a private temporary file for each writer
/// (`rightkit_fs::atomic_write`: temp, fsync, rename, directory fsync).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("atomic write needs parent directory")?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    rightkit_fs::atomic_write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Build a manifest by hashing every file currently inside `entry_dir`
/// (recursive, relative paths, `manifest.json` itself excluded). Used after a
/// verified local copy is migrated into the store so future lookups verify
/// against the recorded hashes.
pub fn manifest_from_dir(entry_dir: &Path, id: &str, version: &str) -> Result<Manifest, String> {
    let mut files = Vec::new();
    collect_files(entry_dir, entry_dir, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let files = files
        .into_iter()
        .filter(|f| f.path != "manifest.json")
        .map(|f| {
            let full = entry_dir.join(rel_path(&f));
            let sha256 = Some(sha256_file(&full)?);
            let size = Some(
                fs::metadata(&full)
                    .map_err(|e| format!("stat {}: {e}", full.display()))?
                    .len(),
            );
            Ok(ManifestFile {
                path: f.path,
                sha256,
                size,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Manifest {
        id: id.to_string(),
        version: version.to_string(),
        files,
    })
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<ManifestFile>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry in {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else if path.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(ManifestFile {
                path: rel,
                sha256: None,
                size: None,
            });
        }
    }
    Ok(())
}

fn rel_path(file: &ManifestFile) -> PathBuf {
    file.path.split('/').collect()
}

/// Reject manifest paths that would escape the entry dir (`..`, absolute,
/// Windows prefixes). Called before any manifest-listed path is joined.
pub fn safe_rel_path(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('\\')
            || part.contains(':')
        {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// sha256 hex of a file, lowercase.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("open {} for hashing: {e}", path.display()))?;
    rightkit_fs::digest::sha256_reader(&mut file)
        .map_err(|e| format!("hash {}: {e}", path.display()))
}

pub fn hex_digest(bytes: &[u8]) -> String {
    rightkit_fs::digest::hex(bytes)
}

/// Live trusted readiness for managed entries & compatible external copies.
pub fn readiness_report_at(
    dir: &Path,
    spec: &crate::suite::catalog::ModelSpec,
) -> crate::suite::VerifyReport {
    use crate::suite::manifest::{self, FileCheck, FileState};
    let trusted = spec.expected_manifest();
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let managed = [
        crate::suite::paths::models_root(),
        crate::suite::paths::legacy_suite_root().join("models"),
    ]
    .iter()
    .any(|root| {
        dir.starts_with(root)
            || canonical.starts_with(std::fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
    });
    let identity_ok = match manifest::read_manifest(dir) {
        Ok(Some(actual)) => manifest::matches_trusted(&actual, &trusted),
        Ok(None) => !managed,
        Err(_) => false,
    };
    if !identity_ok {
        return crate::suite::VerifyReport {
            checks: vec![FileCheck {
                path: "manifest.json".into(),
                state: FileState::Unverified,
                size: 0,
            }],
        };
    }
    manifest::verify_dir(dir, &trusted, crate::suite::VerifyMode::Full)
}
