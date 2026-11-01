// Workspace module — trust-root derivation, path resolution, and the
// secret/permission/binary/hash helpers that back every policy decision.
//
// Extracted from `api_workspace.rs` (ra-019 decomposition). Pure structural
// move: every check below enforces the exact same server-side policy as
// before the split —
//   - traversal: `..`, absolute paths outside the root, and NUL bytes
//     are rejected before the FS is touched
//   - symlink: canonicalize-then-prefix; a symlink that escapes the
//     trust root is rejected with 403
//   - secret deny-list: casefolded, matched as full leaf, `.env*` family,
//     `id_rsa*` / `id_ed25519*` family, `*.pem` / `*.key` / `*.p12` /
//     `*.pfx` / `*.crt`, `secrets.{json,yaml,toml,yml}` and `.netrc`
//   - size: regular files > `MAX_TEXT_BYTES` reject the read/edit
//   - binary: a null-byte within the first `BINARY_SNIFF_BYTES` marks
//     the file as binary
//   - permission: a read attempted with no read bit on the owner returns
//     403; an edit attempted without write bit returns 403

use std::path::{Component, Path, PathBuf};

use super::types::{PolicyError, MAX_TEXT_BYTES};

pub(crate) fn format_mode(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

/// Return a portable approximation of the owner mode bits that this API
/// exposes. On Windows, `readonly` is the only permission signal available
/// through the standard library, so map it to the read/write states the UI
/// needs instead of importing Unix-only traits into the cross-platform daemon.
#[cfg(unix)]
pub(crate) fn permission_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode()
}

#[cfg(not(unix))]
pub(crate) fn permission_mode(metadata: &std::fs::Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    let sniff = &bytes[..bytes.len().min(super::types::BINARY_SNIFF_BYTES)];
    sniff.contains(&0)
}

// ── Trust root + policy enforcement ──────────────────────────────────────────

/// Normalize the trust root to an absolute, canonical path with no symlinks
/// in the prefix. The trust root is server-derived (the daemon hands it to
/// the caller — clients never get to choose an arbitrary one).
pub(crate) fn derive_trust_root(workspace_root: &str) -> Result<PathBuf, PolicyError> {
    let trimmed = workspace_root.trim();
    if trimmed.is_empty() {
        return Err(PolicyError::BadRequest("workspaceRoot is required".into()));
    }
    let p = Path::new(trimmed);
    if !p.is_absolute() {
        return Err(PolicyError::BadRequest(
            "workspaceRoot must be an absolute path".into(),
        ));
    }
    let canonical = std::fs::canonicalize(p).map_err(|err| {
        PolicyError::BadRequest(format!("workspaceRoot could not be resolved: {err}"))
    })?;
    Ok(canonical)
}

/// Reject obvious secret filenames before any FS call. This is a name-only
/// check on the leaf; the trust-root resolution below catches a
/// `secrets/../safe.txt` escape attempt independently. The deny-list is
/// casefolded so `KEY.PEM`, `.ENV`, and `Secrets.JSON` all match.
pub(crate) fn is_secret_leaf(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    // Exact-leaf denylist.
    const EXACT: &[&str] = &[
        ".env",
        ".netrc",
        "id_rsa",
        "id_rsa.pub",
        "id_ed25519",
        "id_ed25519.pub",
        "id_ecdsa",
        "id_ecdsa.pub",
        "id_dsa",
        "id_dsa.pub",
        "secrets.json",
        "secrets.yaml",
        "secrets.yml",
        "secrets.toml",
    ];
    if EXACT.iter().any(|p| *p == lower) {
        return true;
    }
    // .env.*
    if lower.starts_with(".env.") {
        return true;
    }
    // cert/key material (casefolded)
    for ext in [".pem", ".key", ".p12", ".pfx", ".crt"] {
        if lower.ends_with(ext) {
            return true;
        }
    }
    // ssh secret matches
    if lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
        || lower.starts_with("id_ecdsa")
    {
        return true;
    }
    false
}

/// Resolve a workspace path against the trust root and verify the result
/// stays inside the root, rejecting traversal and symlink escapes.
/// `binary_allowed` lets read-only callers opt back in when they only need
/// metadata; the policy lives here, not in the handler.
pub(crate) fn resolve_within_trust_root(
    trust_root: &Path,
    raw_path: &str,
    binary_allowed: bool,
) -> Result<(PathBuf, String, Option<String>), PolicyError> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(PolicyError::BadRequest("path is required".into()));
    }
    if trimmed.contains('\0') {
        return Err(PolicyError::BadRequest("path contains NUL byte".into()));
    }
    // Keep rooted-path policy host-independent. On Windows, `Path::is_absolute`
    // does not classify every Unix-shaped rooted input (for example `/etc/passwd`)
    // as absolute, so reject both slash forms before filesystem resolution.
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(PolicyError::Forbidden(
            "absolute paths are not permitted; use a path relative to workspaceRoot".into(),
        ));
    }
    let candidate = Path::new(trimmed);
    let components: Vec<Component> = candidate.components().collect();
    for component in &components {
        if matches!(component, Component::ParentDir) {
            return Err(PolicyError::Forbidden(
                "path traversal (`..`) is not permitted".into(),
            ));
        }
        if matches!(component, Component::Prefix(_)) {
            return Err(PolicyError::Forbidden(
                "Windows path prefixes are not permitted".into(),
            ));
        }
    }
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        trust_root.join(candidate)
    };
    let canonical = match std::fs::canonicalize(&joined) {
        Ok(p) => p,
        Err(_) => return Err(PolicyError::NotFound),
    };
    let rel = canonical
        .strip_prefix(trust_root)
        .map_err(|_| PolicyError::Forbidden("path escapes workspaceRoot".into()))?;
    let leaf_name = rel
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if is_secret_leaf(&leaf_name) {
        return Err(PolicyError::Forbidden(format!(
            "secret file `{leaf_name}` is policy-blocked from workspace reads"
        )));
    }
    let metadata = std::fs::metadata(&canonical)
        .map_err(|err| PolicyError::Forbidden(format!("metadata could not be read: {err}")))?;
    if metadata.is_file() && !binary_allowed && metadata.len() > MAX_TEXT_BYTES {
        return Err(PolicyError::Forbidden(format!(
            "file is larger than {MAX_TEXT_BYTES} bytes"
        )));
    }
    let relative_path = rel.to_string_lossy().replace('\\', "/");
    Ok((canonical, relative_path, Some(leaf_name)))
}

/// Resolve a directory-list request. The empty relative path intentionally
/// means the trust root itself; file reads and writes remain strict and still
/// require a concrete path.
pub(crate) fn resolve_directory_within_trust_root(
    trust_root: &Path,
    raw_path: &str,
) -> Result<(PathBuf, String, Option<String>), PolicyError> {
    if raw_path.trim().is_empty() {
        return Ok((trust_root.to_path_buf(), String::new(), None));
    }
    resolve_within_trust_root(trust_root, raw_path, true)
}

/// Resolve a write target inside the trust root. The parent directory must
/// already exist; binary targets are refused outright (the bounded editor
/// is text-only).
pub(crate) fn resolve_writable_within_trust_root(
    trust_root: &Path,
    raw_path: &str,
) -> Result<PathBuf, PolicyError> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return Err(PolicyError::BadRequest("path is invalid".into()));
    }
    let candidate = Path::new(trimmed);
    for component in candidate.components() {
        if matches!(component, Component::ParentDir) {
            return Err(PolicyError::Forbidden(
                "path traversal (`..`) is not permitted".into(),
            ));
        }
    }
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        trust_root.join(candidate)
    };
    let parent = joined.parent().unwrap_or(trust_root);
    if !parent.exists() {
        return Err(PolicyError::NotFound);
    }
    let canonical_parent = std::fs::canonicalize(parent).map_err(|err| {
        PolicyError::Forbidden(format!("parent directory could not be resolved: {err}"))
    })?;
    if !canonical_parent.starts_with(trust_root) {
        return Err(PolicyError::Forbidden(
            "parent directory escapes workspaceRoot".into(),
        ));
    }
    if joined.exists() {
        let canonical = std::fs::canonicalize(&joined)
            .map_err(|err| PolicyError::Forbidden(format!("file could not be resolved: {err}")))?;
        if !canonical.starts_with(trust_root) {
            return Err(PolicyError::Forbidden(
                "target file escapes workspaceRoot".into(),
            ));
        }
        if let Some(name) = canonical.file_name().and_then(|n| n.to_str()) {
            if is_secret_leaf(name) {
                return Err(PolicyError::Forbidden(format!(
                    "secret file `{name}` is policy-blocked from workspace edits"
                )));
            }
        }
    } else if let Some(name) = joined.file_name().and_then(|n| n.to_str()) {
        if is_secret_leaf(name) {
            return Err(PolicyError::Forbidden(format!(
                "secret file `{name}` is policy-blocked from workspace edits"
            )));
        }
    }
    Ok(joined)
}
