//! Read-only bridge to the enrolled-repository identity in the Cortex state root.
//!
//! The registry file (`project-registry.json`) is unchanged by the Pulse cutover, so ids read
//! here stay byte-identical to the ones the old Membrane runtime produced. Enrolment is
//! Pulse's concern; this module never writes the registry and never derives an id locally.
//! Path and schema are transcribed from `context/engine/crates/membrane-runtime/src/
//! authorization.rs` (`default_registry_path`, `load_installation_registry`, `parse_registry`,
//! `parse_binding`), which this crate no longer links.

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalRepositoryScope {
    pub root: String,
    pub repository_id: String,
    pub scope_id: String,
    pub scope_descriptor: Option<Value>,
}

/// One `bindings[<root>]` registry row (only the fields the host app reads).
#[derive(Debug, Clone, PartialEq)]
struct Binding {
    root: String,
    repository_id: String,
    scope_id: String,
    scope_descriptor: Option<Value>,
}

/// `MEMBRANE_PROJECT_REGISTRY` overrides, else `APPDATA` or `HOME` + `Cortex/project-registry.json`.
fn registry_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MEMBRANE_PROJECT_REGISTRY") {
        return Some(PathBuf::from(path));
    }
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    Some(
        PathBuf::from(base)
            .join("Cortex")
            .join("project-registry.json"),
    )
}

/// A missing registry file is an empty registry; a malformed one is an error (fail closed).
fn load_bindings() -> Result<Vec<Binding>, String> {
    let Some(path) = registry_path() else {
        return Err("repository registry path is unresolvable: no APPDATA/HOME base".into());
    };
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "repository registry unavailable {}: {error}",
                path.display()
            ))
        }
    };
    let value: Value = serde_json::from_slice(&raw).map_err(|error| {
        format!(
            "repository registry unavailable {}: {error}",
            path.display()
        )
    })?;
    parse_bindings(&value).map_err(|detail| {
        format!(
            "repository registry {} is malformed: {detail}",
            path.display()
        )
    })
}

fn parse_bindings(value: &Value) -> Result<Vec<Binding>, String> {
    if !matches!(
        value.get("schema_version").and_then(Value::as_u64),
        Some(1) | Some(2)
    ) {
        return Err("schema_version must be 1 or 2".into());
    }
    let bindings = value
        .get("bindings")
        .and_then(Value::as_object)
        .ok_or_else(|| "bindings must be an object".to_string())?;
    let mut parsed = Vec::with_capacity(bindings.len());
    for (root, binding) in bindings {
        if root.trim().is_empty() {
            return Err("registry root is malformed".into());
        }
        let object = binding
            .as_object()
            .ok_or_else(|| "binding must be an object".to_string())?;
        let field = |key: &str| {
            object
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("binding {key} is required"))
        };
        parsed.push(Binding {
            root: root.clone(),
            repository_id: field("repository_id")?,
            scope_id: field("scope_id")?,
            scope_descriptor: object
                .get("scope_descriptor")
                .cloned()
                .filter(Value::is_object),
        });
    }
    Ok(parsed)
}

/// Resolve a selected workspace root to its exact existing registry binding. This never
/// enrolls a repository or derives an identity locally.
pub fn resolve_canonical_repository_scope(
    selected_root: &Path,
) -> Result<Option<CanonicalRepositoryScope>, String> {
    let selected = selected_root
        .canonicalize()
        .map_err(|error| format!("selected repository root is unavailable: {error}"))?;
    if !selected.is_dir() {
        return Err("selected repository root is not a directory".into());
    }
    let bindings = load_bindings()?;
    resolve_from_bindings(&selected, &bindings)
}

/// Resolve a session workspace: the workspace itself, else - for a linked git worktree (a
/// session worktree under `<state-dir>/worktrees`) - the source repository it was created
/// from. Both go through the same registry; the worktree mapping comes from git's own `.git`
/// file, never a local identity.
pub fn resolve_canonical_repository_scope_for_workspace(
    workspace: &Path,
) -> Result<Option<CanonicalRepositoryScope>, String> {
    if let Some(scope) = resolve_canonical_repository_scope(workspace)? {
        return Ok(Some(scope));
    }
    match linked_worktree_source(workspace) {
        Some(source) => resolve_canonical_repository_scope(&source),
        None => Ok(None),
    }
}

/// `<worktree>/.git` is a file `gitdir: <repo>/.git/worktrees/<name>`.
fn linked_worktree_source(workspace: &Path) -> Option<PathBuf> {
    let marker = std::fs::read_to_string(workspace.join(".git")).ok()?;
    let gitdir = PathBuf::from(marker.trim().strip_prefix("gitdir:")?.trim());
    let worktrees = gitdir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    worktrees.parent()?.parent().map(Path::to_path_buf)
}

/// Existing enrolled roots, resolved through the one registry. Unavailable roots are omitted;
/// duplicate/ambiguous bindings fail closed.
pub fn enrolled_repository_scopes() -> Result<Vec<CanonicalRepositoryScope>, String> {
    let bindings = load_bindings()?;
    let mut scopes = Vec::new();
    for binding in &bindings {
        let Ok(root) = PathBuf::from(&binding.root).canonicalize() else {
            continue;
        };
        if !root.is_dir() {
            continue;
        }
        if let Some(scope) = resolve_from_bindings(&root, &bindings)? {
            if !scopes
                .iter()
                .any(|existing: &CanonicalRepositoryScope| existing.root == scope.root)
            {
                scopes.push(scope);
            }
        }
    }
    scopes.sort_by(|left, right| left.root.cmp(&right.root));
    Ok(scopes)
}

fn resolve_from_bindings(
    selected: &Path,
    bindings: &[Binding],
) -> Result<Option<CanonicalRepositoryScope>, String> {
    let mut matches = Vec::new();
    for binding in bindings {
        let Ok(root) = PathBuf::from(&binding.root).canonicalize() else {
            continue;
        };
        let same = if cfg!(windows) {
            root.to_string_lossy()
                .eq_ignore_ascii_case(&selected.to_string_lossy())
        } else {
            root == selected
        };
        if same {
            // `canonicalize` yields a verbatim `\\?\` path on Windows; the registry compares
            // plain paths.
            let root = root.to_string_lossy();
            let root = root.strip_prefix(r"\\?\").unwrap_or(&root).to_string();
            matches.push(CanonicalRepositoryScope {
                root,
                repository_id: binding.repository_id.clone(),
                scope_id: binding.scope_id.clone(),
                scope_descriptor: binding.scope_descriptor.clone(),
            });
        }
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err("selected repository has ambiguous registry bindings".into()),
    }
}
