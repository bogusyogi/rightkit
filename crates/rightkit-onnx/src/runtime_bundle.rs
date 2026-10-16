//! Verified runtime bundles: a directory holding ONNX Runtime, its provider
//! libraries and models, described by `manifest.json`. Every listed file is
//! hashed in full before the bundle is reported as verified. Nothing here loads
//! a library or creates a session; callers pass the verified paths on.
//!
//! Layout (the directory given to [`verify_runtime_manifest`] is `runtime/`,
//! and the manifest sits inside it):
//!
//! ```text
//! runtime/
//!   manifest.json                              not listed in its own manifest
//!   onnxruntime.dll                            listed (libonnxruntime.dylib on macOS)
//!   DirectML.dll                               listed
//!   models/face_detection_yunet_2023mar.onnx   listed
//! ```
//!
//! Rules enforced:
//! - `manifest.json` must be valid JSON with exactly the fields of schema 1 and
//!   no others; `schema` must be the integer `1`.
//! - Every `files[].path` is a relative, `/`-separated, normalised path with no
//!   `..`, no `.`, no leading `/` or drive prefix, no `\`, no trailing `/`, and
//!   it may not name `manifest.json`. A path that resolves outside the directory
//!   is rejected.
//! - Paths are unique. Each listed file must exist as a regular file with the
//!   listed byte count and SHA-256 (hex, case-insensitive in the manifest,
//!   lowercase in the result).
//! - Extra files: every regular file anywhere under the directory, except
//!   `manifest.json` at its top level, must be listed. Directories may exist and
//!   hold listed files. Symbolic links and other special entries are rejected.

use std::collections::HashSet;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{runtime_filename, RuntimeError};

const MANIFEST_FILE: &str = "manifest.json";
const SUPPORTED_SCHEMA: u32 = 1;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// A bundle whose manifest and every listed file were verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRuntime {
    /// Canonical absolute path of the bundle directory (the one holding `manifest.json`).
    pub dir: PathBuf,
    /// Manifest platform identifier, for example `windows-x64`.
    pub platform: String,
    pub onnxruntime: OnnxRuntimeIdentity,
    /// Every manifest entry, in manifest order, all verified.
    pub files: Vec<VerifiedFile>,
}

/// The ONNX Runtime build the bundle claims to contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnnxRuntimeIdentity {
    /// Runtime version, for example `1.24.4`.
    pub version: String,
    /// Build variant, for example `directml`.
    pub variant: String,
    /// Identifier of the native-library catalog row this build came from. Recorded,
    /// not checked here.
    pub catalog_row: String,
}

/// One verified manifest entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFile {
    /// Path relative to the bundle directory, `/`-separated, as listed.
    pub path: String,
    /// Lowercase hex SHA-256 of the complete file.
    pub sha256: String,
    pub bytes: u64,
}

impl VerifiedRuntime {
    /// Path of the ONNX Runtime library inside the bundle, when the manifest
    /// lists it (`<dir>/<runtime_filename()>`). The returned file is verified.
    pub fn onnxruntime_library_path(&self) -> Option<PathBuf> {
        let name = runtime_filename();
        self.files
            .iter()
            .any(|file| file.path == name)
            .then(|| self.dir.join(name))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    platform: String,
    onnxruntime: ManifestOnnxRuntime,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ManifestOnnxRuntime {
    version: String,
    variant: String,
    catalog_row: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    path: String,
    sha256: String,
    bytes: u64,
}

/// Verify `<dir>/manifest.json` and every file it lists. `dir` must be absolute.
pub fn verify_runtime_manifest(dir: &Path) -> Result<VerifiedRuntime, RuntimeError> {
    if !dir.is_absolute() {
        return Err(RuntimeError::UnsafePath {
            path: dir.to_path_buf(),
        });
    }
    let dir = dir
        .canonicalize()
        .map_err(|error| RuntimeError::Canonicalize {
            path: dir.to_path_buf(),
            message: error.to_string(),
        })?;
    let manifest_path = dir.join(MANIFEST_FILE);
    let text =
        std::fs::read(&manifest_path).map_err(|error| manifest_error(&manifest_path, error))?;
    let manifest: Manifest =
        serde_json::from_slice(&text).map_err(|error| manifest_error(&manifest_path, error))?;

    if manifest.schema != SUPPORTED_SCHEMA {
        return Err(manifest_error(
            &manifest_path,
            format!(
                "unsupported schema {}; expected {SUPPORTED_SCHEMA}",
                manifest.schema
            ),
        ));
    }
    if !valid_platform(&manifest.platform) {
        return Err(manifest_error(
            &manifest_path,
            format!("invalid platform {:?}", manifest.platform),
        ));
    }
    let onnxruntime = OnnxRuntimeIdentity {
        version: required(
            &manifest_path,
            "onnxruntime.version",
            manifest.onnxruntime.version,
        )?,
        variant: required(
            &manifest_path,
            "onnxruntime.variant",
            manifest.onnxruntime.variant,
        )?,
        catalog_row: required(
            &manifest_path,
            "onnxruntime.catalogRow",
            manifest.onnxruntime.catalog_row,
        )?,
    };

    let mut listed = Vec::with_capacity(manifest.files.len());
    let mut seen = HashSet::with_capacity(manifest.files.len());
    for entry in manifest.files {
        let (key, relative) = parse_listed_path(&entry.path)?;
        if !seen.insert(key.clone()) {
            return Err(manifest_error(
                &manifest_path,
                format!("path {key:?} is listed more than once"),
            ));
        }
        let sha256 = normalize_sha256(&entry.sha256).ok_or_else(|| {
            manifest_error(
                &manifest_path,
                format!("sha256 for {key:?} must be 64 hex digits"),
            )
        })?;
        listed.push((
            VerifiedFile {
                path: key,
                sha256,
                bytes: entry.bytes,
            },
            relative,
        ));
    }

    reject_unlisted_entries(&dir, &seen)?;

    let mut files = Vec::with_capacity(listed.len());
    for (expected, relative) in listed {
        files.push(verify_file(&dir, &relative, expected)?);
    }
    Ok(VerifiedRuntime {
        dir,
        platform: manifest.platform,
        onnxruntime,
        files,
    })
}

/// Verify one library against the `files[]` list of the tolerant `manifest.json`
/// that sits in the library's own directory (Orthic-Labs/rightkit#178). `lib` must
/// be an absolute path to the library; its file name is the manifest key.
///
/// For apps whose `runtime/` holds more than ONNX Runtime. Tolerant of the
/// manifest around the entry: schema, platform and `onnxruntime` fields, unknown
/// fields, other `files` entries (valid, malformed or absent on disk) and unlisted
/// files in the directory are all ignored. Strict about the library itself: the
/// manifest must exist with a `files` array that lists it exactly once with a
/// 64-hex `sha256` and a `bytes` count, and the file must be a regular file inside
/// the directory whose size and full SHA-256 match. A digest or size mismatch is
/// never tolerated.
pub fn verify_library_in_manifest(lib: &Path) -> Result<VerifiedFile, RuntimeError> {
    if !lib.is_absolute() {
        return Err(RuntimeError::UnsafePath {
            path: lib.to_path_buf(),
        });
    }
    let name = lib
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| RuntimeError::UnsafePath {
            path: lib.to_path_buf(),
        })?;
    let (key, relative) = parse_listed_path(name)?;
    let parent = lib.parent().ok_or_else(|| RuntimeError::UnsafePath {
        path: lib.to_path_buf(),
    })?;
    let dir = parent
        .canonicalize()
        .map_err(|error| RuntimeError::Canonicalize {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;
    let manifest_path = dir.join(MANIFEST_FILE);
    let text =
        std::fs::read(&manifest_path).map_err(|error| manifest_error(&manifest_path, error))?;
    let value: serde_json::Value =
        serde_json::from_slice(&text).map_err(|error| manifest_error(&manifest_path, error))?;
    let entries = value
        .get("files")
        .and_then(|files| files.as_array())
        .ok_or_else(|| manifest_error(&manifest_path, "\"files\" is missing or not an array"))?;

    let mut matches = entries
        .iter()
        .filter(|entry| entry.get("path").and_then(|p| p.as_str()) == Some(key.as_str()));
    let entry = matches
        .next()
        .ok_or_else(|| manifest_error(&manifest_path, format!("library {key:?} is not listed")))?;
    if matches.next().is_some() {
        return Err(manifest_error(
            &manifest_path,
            format!("path {key:?} is listed more than once"),
        ));
    }
    let sha256 = entry
        .get("sha256")
        .and_then(|s| s.as_str())
        .and_then(normalize_sha256)
        .ok_or_else(|| {
            manifest_error(
                &manifest_path,
                format!("sha256 for {key:?} must be 64 hex digits"),
            )
        })?;
    let bytes = entry
        .get("bytes")
        .and_then(|b| b.as_u64())
        .ok_or_else(|| {
            manifest_error(
                &manifest_path,
                format!("bytes for {key:?} must be a non-negative integer"),
            )
        })?;

    verify_file(
        &dir,
        &relative,
        VerifiedFile {
            path: key,
            sha256,
            bytes,
        },
    )
}

fn manifest_error(path: &Path, error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::BundleManifest {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn required(manifest_path: &Path, field: &str, value: String) -> Result<String, RuntimeError> {
    if value.trim().is_empty() {
        return Err(manifest_error(manifest_path, format!("{field} is empty")));
    }
    Ok(value)
}

/// `<os>-<arch>` in lowercase ASCII letters, digits and `-`, with an inner `-`.
pub(crate) fn valid_platform(raw: &str) -> bool {
    raw.contains('-')
        && !raw.starts_with('-')
        && !raw.ends_with('-')
        && raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn normalize_sha256(raw: &str) -> Option<String> {
    (raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| raw.to_ascii_lowercase())
}

/// Validate one manifest path. Returns the normalised key (as listed) and the
/// relative path to join onto the bundle directory.
fn parse_listed_path(raw: &str) -> Result<(String, PathBuf), RuntimeError> {
    let reject = |reason: &'static str| RuntimeError::BundlePath {
        path: raw.to_owned(),
        reason,
    };
    if raw.is_empty() {
        return Err(reject("empty path"));
    }
    if raw.contains('\\') {
        return Err(reject("use '/' as the separator"));
    }
    if raw.contains('\0') {
        return Err(reject("contains a NUL byte"));
    }
    let mut relative = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(name) => relative.push(name),
            Component::ParentDir => return Err(reject("'..' escapes the runtime directory")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(reject("absolute paths are not allowed"))
            }
            Component::CurDir => return Err(reject("'.' components are not allowed")),
        }
    }
    // Components drop repeated and trailing separators, so a spelling that
    // differs from its normalised form is not canonical.
    if raw.ends_with('/') || raw.contains("//") {
        return Err(reject("not a normalised relative path"));
    }
    if raw == MANIFEST_FILE {
        return Err(reject("manifest.json cannot list itself"));
    }
    Ok((raw.to_owned(), relative))
}

/// Walk the bundle and fail on the first symlink, special entry or regular file
/// that the manifest does not list. `manifest.json` at the top level is exempt.
fn reject_unlisted_entries(dir: &Path, listed: &HashSet<String>) -> Result<(), RuntimeError> {
    let mut found = Vec::new();
    collect_files(dir, dir, &mut found)?;
    found.sort();
    for (key, path) in found {
        let exempt = key.as_deref() == Some(MANIFEST_FILE);
        let is_listed = key.as_deref().is_some_and(|k| listed.contains(k));
        if !exempt && !is_listed {
            return Err(RuntimeError::BundleFileUnlisted { path });
        }
    }
    Ok(())
}

fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(Option<String>, PathBuf)>,
) -> Result<(), RuntimeError> {
    let read_failed = |error: std::io::Error| RuntimeError::BundleFileRead {
        path: dir.to_path_buf(),
        message: error.to_string(),
    };
    for entry in std::fs::read_dir(dir).map_err(read_failed)? {
        let entry = entry.map_err(read_failed)?;
        let path = entry.path();
        // DirEntry::file_type does not follow symlinks.
        let file_type = entry.file_type().map_err(read_failed)?;
        if file_type.is_symlink() || !(file_type.is_dir() || file_type.is_file()) {
            return Err(RuntimeError::BundleEntryUnsupported { path });
        }
        if file_type.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            out.push((relative_key(root, &path), path));
        }
    }
    Ok(())
}

/// `/`-separated relative key, or `None` when a component is not valid UTF-8.
fn relative_key(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => parts.push(name.to_str()?),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn verify_file(
    root: &Path,
    relative: &Path,
    expected: VerifiedFile,
) -> Result<VerifiedFile, RuntimeError> {
    let path = root.join(relative);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(RuntimeError::BundleFileMissing { path })
        }
        Err(error) => {
            return Err(RuntimeError::BundleFileRead {
                path,
                message: error.to_string(),
            })
        }
    };
    if !metadata.is_file() {
        return Err(RuntimeError::BundleFileRead {
            path,
            message: "not a regular file".to_owned(),
        });
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| RuntimeError::Canonicalize {
            path: path.clone(),
            message: error.to_string(),
        })?;
    if !canonical.starts_with(root) {
        return Err(RuntimeError::BundlePath {
            path: expected.path,
            reason: "resolves outside the runtime directory",
        });
    }
    if metadata.len() != expected.bytes {
        return Err(RuntimeError::BundleFileSize {
            path,
            expected: expected.bytes,
            actual: metadata.len(),
        });
    }
    let (actual_bytes, digest) = hash_file(&path)?;
    if actual_bytes != expected.bytes {
        // The file changed size between the stat and the read.
        return Err(RuntimeError::BundleFileSize {
            path,
            expected: expected.bytes,
            actual: actual_bytes,
        });
    }
    if digest != expected.sha256 {
        return Err(RuntimeError::BundleFileHash {
            path,
            expected: expected.sha256,
            actual: digest,
        });
    }
    Ok(expected)
}

/// Streams the whole file through SHA-256. Returns the byte count and lowercase hex digest.
fn hash_file(path: &Path) -> Result<(u64, String), RuntimeError> {
    let read_failed = |error: std::io::Error| RuntimeError::BundleFileRead {
        path: path.to_path_buf(),
        message: error.to_string(),
    };
    let mut file = File::open(path).map_err(read_failed)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                hasher.update(&buffer[..read]);
                total += read as u64;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(read_failed(error)),
        }
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((total, digest))
}
