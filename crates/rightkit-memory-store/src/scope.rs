//! Multi-project scope model — a faithful Rust port of `mem.py`'s scope functions.
//!
//! A project's `scope_id` is Claude's own project-slug: each `:` `\` `/` in the cwd becomes `-`,
//! with NO collapsing, and the leading Windows drive letter normalized to uppercase so a
//! lowercase-drive cwd can't fork a project's memories into a second scope. Recall is chain-scoped
//! (self + ancestors that exist + global), never siblings.

use std::path::{Path, PathBuf};

pub const MAX_VIRTUAL_SCOPE_DEPTH: usize = 8;

/// Explicit caller scope. Legacy strings remain filesystem-only; virtual scopes must use this
/// tagged form so punctuation in an opaque id can never create path-like ancestors.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ScopeDescriptorV1 {
    #[serde(rename = "filesystem")]
    Filesystem { path: String },
    #[serde(rename = "virtual")]
    Virtual {
        id: String,
        #[serde(rename = "tenant_id")]
        tenant_id: String,
        #[serde(default)]
        parents: Vec<String>,
        #[serde(default, rename = "inherit_global")]
        inherit_global: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScopeDescriptorError {
    #[error("scope descriptor path is required")]
    MissingPath,
    #[error("virtual scope id and tenant_id are required")]
    MissingVirtualIdentity,
    #[error("virtual scope parent chain exceeds depth 8")]
    Depth,
    #[error("virtual scopes cannot inherit global")]
    GlobalInheritance,
    #[error("virtual scope parent is invalid")]
    InvalidParent,
    #[error("virtual scope parent chain contains a duplicate or cycle")]
    Cycle,
    #[error("virtual scope parent is not registered for this tenant")]
    UnknownParent,
}

fn virtual_scope_key(tenant_id: &str, id: &str) -> String {
    format!("virtual:{tenant_id}:{id}")
}

impl ScopeDescriptorV1 {
    pub fn filesystem(path: impl Into<String>) -> Self {
        Self::Filesystem { path: path.into() }
    }

    pub fn virtual_scope(
        id: impl Into<String>,
        tenant_id: impl Into<String>,
        parents: Vec<String>,
        inherit_global: bool,
    ) -> Self {
        Self::Virtual {
            id: id.into(),
            tenant_id: tenant_id.into(),
            parents,
            inherit_global,
        }
    }

    /// Resolve a descriptor into exact recall scope ids. Virtual parents are opaque ids in the
    /// same tenant, ordered nearest-first; they are never slugged, split, or given implicit global.
    pub fn resolve_chain(&self, existing: &[String]) -> Result<Vec<String>, ScopeDescriptorError> {
        match self {
            Self::Filesystem { path } => {
                if path.trim().is_empty() {
                    return Err(ScopeDescriptorError::MissingPath);
                }
                Ok(canonical_scope_chain(path, existing))
            }
            Self::Virtual {
                id,
                tenant_id,
                parents,
                inherit_global,
            } => {
                if id.trim().is_empty() || tenant_id.trim().is_empty() {
                    return Err(ScopeDescriptorError::MissingVirtualIdentity);
                }
                if parents.len() + 1 > MAX_VIRTUAL_SCOPE_DEPTH {
                    return Err(ScopeDescriptorError::Depth);
                }
                if *inherit_global {
                    return Err(ScopeDescriptorError::GlobalInheritance);
                }
                let own = virtual_scope_key(tenant_id, id);
                let mut seen = std::collections::HashSet::from([own.clone()]);
                let mut chain = vec![own];
                for parent in parents {
                    if parent.trim().is_empty() || parent.starts_with("virtual:") {
                        return Err(ScopeDescriptorError::InvalidParent);
                    }
                    let key = virtual_scope_key(tenant_id, parent);
                    if !seen.insert(key.clone()) {
                        return Err(ScopeDescriptorError::Cycle);
                    }
                    if !existing.iter().any(|scope| scope == &key) {
                        return Err(ScopeDescriptorError::UnknownParent);
                    }
                    chain.push(key);
                }
                Ok(chain)
            }
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MembraneWorkspaceConfig {
    schema_version: u32,
    workspace_root: PathBuf,
}

fn validated_workspace_root(value: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value.trim());
    (path.is_absolute() && path.is_dir())
        .then(|| path.canonicalize().ok())
        .flatten()
}

pub(crate) fn workspace_root_from(
    explicit: Option<&str>,
    config_path: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(value) = explicit.filter(|value| !value.trim().is_empty()) {
        return validated_workspace_root(value);
    }
    let payload = std::fs::read(config_path?).ok()?;
    let config: MembraneWorkspaceConfig = serde_json::from_slice(&payload).ok()?;
    if config.schema_version != 3 {
        return None;
    }
    validated_workspace_root(config.workspace_root.to_str()?)
}

/// The account's own home folder, resolved as the host's `home::account_home` does: `HOME`
/// (`USERPROFILE` on Windows), unless on Unix it is `/` or lies inside `<account>/ClaudeProfiles/`
/// (a shell inside Claude B), where the password database's home wins.
pub fn account_home() -> Option<PathBuf> {
    let env = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    }
    .map(PathBuf::from);
    #[cfg(unix)]
    if let Some(account) = passwd_home() {
        let profiles = account.join("ClaudeProfiles");
        let inside = env
            .as_deref()
            .is_none_or(|env| env == Path::new("/") || env.starts_with(&profiles));
        if inside {
            return Some(account);
        }
    }
    env
}

/// The home directory from the password database for the current uid, or `None` when unavailable.
#[cfg(unix)]
fn passwd_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: `entry` and `buffer` are valid out-parameters that outlive the call.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: `pw_dir` is a NUL-terminated string stored inside `buffer`, read before it drops.
    let dir = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

pub fn workspace_root() -> Option<PathBuf> {
    let explicit = crate::qa_env::workspace_root().and_then(|value| value.into_string().ok());
    let configured = crate::qa_env::workspace_config()
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| account_home().map(|home| home.join(".config/membrane/workspace.json")));
    workspace_root_from(explicit.as_deref(), configured.as_deref())
}

/// Uppercase a leading Windows drive token so `d--Claude-x` and `D--Claude-x` never fork.
/// The slug form of a drive is one letter followed by `--` (from `d:` -> `d-`, joined with the next `-`).
pub fn normalize_scope(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b'-' && b[0].is_ascii_alphabetic() && b[0].is_ascii_lowercase() {
        let mut out = String::with_capacity(s.len());
        out.push((b[0] as char).to_ascii_uppercase());
        out.push_str(&s[1..]);
        out
    } else {
        s.to_string()
    }
}

/// Claude's project-slug: each `:` `\` `/` -> `-`, NO collapsing, drive letter normalized.
/// `D:\Claude\myproject` and `d:\Claude\myproject` both -> `D--Claude-myproject`.
pub fn path_to_scope(cwd: &str) -> String {
    let cwd = cwd.strip_prefix(r"\\?\").unwrap_or(cwd);
    let slug: String = cwd
        .chars()
        .map(|c| {
            if c == ':' || c == '\\' || c == '/' {
                '-'
            } else {
                c
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        return "workspace".to_string();
    }
    normalize_scope(slug)
}

/// A scope sees itself + its ancestors (path-slug prefixes that actually hold rows) + `global`,
/// never siblings. `existing` is the set of scope_ids present in the store. Self always leads, even
/// if it has no rows yet. `D--Claude-coderight` -> `[D--Claude-coderight, D--Claude, global]`.
pub fn scope_chain(scope_id: &str, existing: &[String]) -> Vec<String> {
    let parts: Vec<&str> = scope_id.split('-').collect();
    // prefixes: longest (self) down to the first segment
    let mut candidates: Vec<String> = Vec::with_capacity(parts.len() + 2);
    candidates.push(scope_id.to_string());
    for i in (1..=parts.len()).rev() {
        candidates.push(parts[..i].join("-"));
    }
    let has = |p: &str| existing.iter().any(|e| e == p);
    let mut chain: Vec<String> = Vec::new();
    for p in candidates {
        if chain.contains(&p) {
            continue;
        }
        if p == scope_id || has(&p) {
            chain.push(p);
        }
    }
    if has("global") && !chain.iter().any(|c| c == "global") {
        chain.push("global".to_string());
    }
    chain
}

/// THE one canonical entry point for a caller-supplied scope string (2026-07-16, Sol audit P0).
/// Clients send whatever they have — a filesystem path (`D:\Claude\heardright`, `/Users/x/claude`),
/// an already-slugged scope (`D--Claude`), or `global` — and every retrieval surface must resolve
/// it identically. Before this existed, the federation memory lane passed raw paths straight into
/// `recall_scored`, which matched NO project scope rows: the rich path silently recalled from the
/// global corpus only. Path detection keys on separators that can never appear in a slug.
pub fn canonical_scope_chain(raw: &str, existing: &[String]) -> Vec<String> {
    let raw = raw.trim();
    let slug = if raw.contains(':') || raw.contains('\\') || raw.contains('/') {
        path_to_scope(raw)
    } else if raw.is_empty() {
        "global".to_string()
    } else {
        normalize_scope(raw)
    };
    scope_chain(&slug, existing)
}

/// Windows drive cwd slug: `D--Claude` / `D--Claude-heardright` (drive letter + `--` + rest).
fn is_windows_drive_slug(scope: &str) -> bool {
    let b = scope.as_bytes();
    b.len() >= 4
        && b[0].is_ascii_alphabetic()
        && b[1] == b'-'
        && b[2] == b'-'
        && b[3].is_ascii_alphanumeric()
}

/// Unix-style cwd slug from path_to_scope: `Users-adrdsouza-claude`, `Volumes-D-claude`.
/// Requires a known absolute-root first segment, or ≥3 non-empty hyphen segments.
fn is_unix_path_slug(scope: &str) -> bool {
    let segments: Vec<&str> = scope.split('-').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return false;
    }
    let first = segments[0];
    let known_root = matches!(
        first,
        "Users" | "home" | "Volumes" | "private" | "var" | "tmp" | "opt" | "root"
    );
    known_root || segments.len() >= 3
}

/// Write-path scope gate (P1 scope-typo hardening).
///
/// Allows `proposed` / `global` / `workspace`, filesystem paths, Windows drive slugs
/// (`D--Claude-…`), Unix cwd slugs (`Users-…` / multi-segment path slugs), and typed
/// `virtual:…` ids. Refuses single-token hand-typed names like `heardright` that silently
/// fork the corpus (2026-07-05 mirror incident).
pub fn validate_write_scope(raw: &str) -> Result<(), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("scope must be non-empty".into());
    }
    if matches!(raw, "proposed" | "global" | "workspace") {
        return Ok(());
    }
    if raw.starts_with("virtual:") {
        return Ok(());
    }
    // Paths always OK — callers normalize via path_to_scope / normalize_scope.
    if raw.contains('/') || raw.contains('\\') || raw.contains(':') {
        return Ok(());
    }
    let normalized = normalize_scope(raw);
    if is_windows_drive_slug(&normalized) || is_unix_path_slug(&normalized) {
        return Ok(());
    }
    Err(format!(
        "refusing hand-typed scope {raw:?}: use proposed, global, a cwd path, or a cwd slug \
         (e.g. D--Claude-heardright / Users-adrdsouza-claude); a novel single-token scope forks the corpus"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn path_to_scope_normalizes_windows_drive_case_and_separators() {
        assert_eq!(path_to_scope(r"D:\Claude\myproject"), "D--Claude-myproject");
        assert_eq!(path_to_scope(r"d:\Claude\myproject"), "D--Claude-myproject");
        assert_eq!(path_to_scope(r"\\?\d:\Claude"), "D--Claude");
    }

    #[test]
    fn path_to_scope_trims_edges_and_falls_back_to_workspace() {
        assert_eq!(path_to_scope("/Users/x/claude"), "Users-x-claude");
        assert_eq!(path_to_scope("/"), "workspace");
        assert_eq!(path_to_scope(""), "workspace");
    }

    #[test]
    fn normalize_scope_only_uppercases_a_leading_lowercase_drive_token() {
        assert_eq!(normalize_scope("d--Claude-x"), "D--Claude-x");
        assert_eq!(normalize_scope("D--Claude-x"), "D--Claude-x");
        assert_eq!(normalize_scope("global"), "global");
        assert_eq!(normalize_scope("a"), "a");
    }

    #[test]
    fn scope_chain_includes_self_existing_ancestors_and_global_but_not_siblings() {
        let existing = strings(&["D--Claude", "D--Claude-other", "global"]);
        assert_eq!(
            scope_chain("D--Claude-coderight", &existing),
            strings(&["D--Claude-coderight", "D--Claude", "global"])
        );
    }

    #[test]
    fn scope_chain_always_leads_with_self_even_without_rows() {
        assert_eq!(scope_chain("proj-a", &[]), strings(&["proj-a"]));
    }

    #[test]
    fn canonical_scope_chain_resolves_paths_slugs_and_empty_input_alike() {
        let existing = strings(&["D--Claude", "global"]);
        let expected = strings(&["D--Claude-heardright", "D--Claude", "global"]);
        assert_eq!(
            canonical_scope_chain(r"d:\Claude\heardright", &existing),
            expected
        );
        assert_eq!(
            canonical_scope_chain("d--Claude-heardright", &existing),
            expected
        );
        assert_eq!(canonical_scope_chain("  ", &existing), strings(&["global"]));
    }

    #[test]
    fn validate_write_scope_accepts_known_names_paths_and_slugs() {
        for ok in [
            "global",
            "proposed",
            "workspace",
            "virtual:tenant:id",
            "/home/me/proj",
            r"D:\Claude",
            "D--Claude-heardright",
            "d--Claude",
            "Users-adrdsouza-claude",
            "my-long-slug",
        ] {
            assert!(validate_write_scope(ok).is_ok(), "{ok} should be accepted");
        }
    }

    #[test]
    fn validate_write_scope_rejects_empty_and_single_token_typos() {
        assert_eq!(
            validate_write_scope("   ").unwrap_err(),
            "scope must be non-empty"
        );
        let err = validate_write_scope("heardright").unwrap_err();
        assert!(err.contains("refusing hand-typed scope"), "got {err}");
        assert!(validate_write_scope("two-parts").is_err());
    }

    #[test]
    fn virtual_chain_is_nearest_first_and_requires_registered_parents() {
        let descriptor =
            ScopeDescriptorV1::virtual_scope("leaf", "t1", strings(&["mid", "root"]), false);
        let existing = strings(&["virtual:t1:mid", "virtual:t1:root"]);
        assert_eq!(
            descriptor.resolve_chain(&existing).unwrap(),
            strings(&["virtual:t1:leaf", "virtual:t1:mid", "virtual:t1:root"])
        );
        assert_eq!(
            descriptor.resolve_chain(&[]).unwrap_err(),
            ScopeDescriptorError::UnknownParent
        );
    }

    #[test]
    fn virtual_chain_rejects_invalid_descriptors() {
        let existing = strings(&["virtual:t:p"]);
        let resolve = |d: ScopeDescriptorV1| d.resolve_chain(&existing).unwrap_err();
        assert_eq!(
            resolve(ScopeDescriptorV1::virtual_scope(" ", "t", vec![], false)),
            ScopeDescriptorError::MissingVirtualIdentity
        );
        assert_eq!(
            resolve(ScopeDescriptorV1::virtual_scope("a", "t", vec![], true)),
            ScopeDescriptorError::GlobalInheritance
        );
        assert_eq!(
            resolve(ScopeDescriptorV1::virtual_scope(
                "a",
                "t",
                strings(&["virtual:t:p"]),
                false
            )),
            ScopeDescriptorError::InvalidParent
        );
        assert_eq!(
            resolve(ScopeDescriptorV1::virtual_scope(
                "a",
                "t",
                strings(&["p", "p"]),
                false
            )),
            ScopeDescriptorError::Cycle
        );
        assert_eq!(
            resolve(ScopeDescriptorV1::virtual_scope(
                "a",
                "t",
                strings(&["a"]),
                false
            )),
            ScopeDescriptorError::Cycle
        );
        let too_deep: Vec<String> = (0..MAX_VIRTUAL_SCOPE_DEPTH)
            .map(|i| format!("p{i}"))
            .collect();
        assert_eq!(
            resolve(ScopeDescriptorV1::virtual_scope("a", "t", too_deep, false)),
            ScopeDescriptorError::Depth
        );
    }

    #[test]
    fn filesystem_descriptor_requires_path_and_uses_canonical_chain() {
        assert_eq!(
            ScopeDescriptorV1::filesystem("  ")
                .resolve_chain(&[])
                .unwrap_err(),
            ScopeDescriptorError::MissingPath
        );
        let existing = strings(&["global"]);
        assert_eq!(
            ScopeDescriptorV1::filesystem("/Users/x/claude")
                .resolve_chain(&existing)
                .unwrap(),
            strings(&["Users-x-claude", "global"])
        );
    }

    #[test]
    fn descriptor_deserializes_tagged_forms_with_defaults_and_rejects_unknown_fields() {
        let parsed: ScopeDescriptorV1 =
            serde_json::from_str(r#"{"kind":"virtual","id":"a","tenant_id":"t"}"#).unwrap();
        assert_eq!(
            parsed,
            ScopeDescriptorV1::virtual_scope("a", "t", vec![], false)
        );
        let fs: ScopeDescriptorV1 =
            serde_json::from_str(r#"{"kind":"filesystem","path":"/x"}"#).unwrap();
        assert_eq!(fs, ScopeDescriptorV1::filesystem("/x"));
        assert!(serde_json::from_str::<ScopeDescriptorV1>(
            r#"{"kind":"filesystem","path":"/x","extra":1}"#
        )
        .is_err());
    }

    fn unique_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rightkit-memory-store-scope-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn workspace_root_from_prefers_valid_explicit_directory() {
        let dir = unique_dir("explicit");
        let expected = dir.canonicalize().unwrap();
        assert_eq!(
            workspace_root_from(dir.to_str(), None),
            Some(expected.clone())
        );
        assert_eq!(
            workspace_root_from(dir.join("missing").to_str(), None),
            None,
            "an invalid explicit root must not fall through to config"
        );
        assert_eq!(workspace_root_from(Some("relative/dir"), None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_root_from_reads_config_and_enforces_schema_version() {
        let dir = unique_dir("config");
        let root = dir.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let config = dir.join("workspace.json");

        let write = |version: u32| {
            let body = serde_json::json!({
                "schemaVersion": version,
                "workspaceRoot": root.to_str().unwrap(),
            });
            std::fs::write(&config, body.to_string()).unwrap();
        };
        write(3);
        assert_eq!(
            workspace_root_from(None, Some(config.as_path())),
            Some(root.canonicalize().unwrap())
        );
        write(2);
        assert_eq!(workspace_root_from(None, Some(config.as_path())), None);
        assert_eq!(workspace_root_from(Some("  "), None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
