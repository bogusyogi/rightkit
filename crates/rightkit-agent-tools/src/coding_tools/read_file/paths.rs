/// Lexically normalize a path, resolving `.` and `..` components without
/// touching the filesystem. A leading `..` that climbs above the base is
/// preserved so the workspace boundary check below can reject it.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Compare filesystem scopes component-wise, treating Windows canonical
/// drive/UNC prefixes as the same paths as their ordinary spellings.
/// Physical symlink/junction containment is checked separately below.
pub fn path_is_within(path: &Path, root: &Path) -> bool {
    fn comparison_path(path: &Path) -> PathBuf {
        #[cfg(windows)]
        {
            use std::path::Prefix;
            let mut components = path.components();
            if let Some(Component::Prefix(prefix)) = components.next() {
                let ordinary = match prefix.kind() {
                    Prefix::VerbatimDisk(drive) => {
                        Some(PathBuf::from(format!("{}:", char::from(drive))))
                    }
                    Prefix::VerbatimUNC(server, share) => {
                        let mut prefix = std::ffi::OsString::from(r"\\");
                        prefix.push(server);
                        prefix.push(r"\");
                        prefix.push(share);
                        Some(PathBuf::from(prefix))
                    }
                    _ => None,
                };
                if let Some(mut ordinary) = ordinary {
                    ordinary.extend(components);
                    return normalize_lexical(&ordinary);
                }
            }
        }
        normalize_lexical(path)
    }
    // Windows paths are case-insensitive: compare components case-folded.
    fn folded(path: PathBuf) -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(path.to_string_lossy().to_lowercase())
        }
        #[cfg(not(windows))]
        {
            path
        }
    }
    !root.as_os_str().is_empty()
        && folded(comparison_path(path)).starts_with(folded(comparison_path(root)))
}

/// Resolve `path_str` and return a path normalized but *not* boundary-checked.
/// Used by read-only helpers (search, git diff). Prefer `resolve_in_workspace`
/// for anything that reads or writes file contents directly.
pub fn resolve_path(path_str: &str, workspace_root: &Path) -> PathBuf {
    let raw = Path::new(path_str);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace_root.join(raw)
    };
    normalize_lexical(&joined)
}

/// Canonicalize `path` for the purpose of a workspace-boundary check. If the
/// path itself doesn't exist yet (a not-yet-created file), canonicalize the
/// deepest existing ancestor directory and re-join the remaining components —
/// this keeps "create a new file at foo/bar.txt" working while still
/// resolving through any symlink/junction ancestor so the check below sees
/// where the path *actually* lands on disk, not just its lexical spelling.
fn canonicalize_for_boundary_check(path: &Path) -> PathBuf {
    if let Ok(canon) = path.canonicalize() {
        return canon;
    }
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if existing.as_os_str().is_empty() {
            return normalize_lexical(path);
        }
        if let Ok(canon) = existing.canonicalize() {
            let mut result = canon;
            for component in tail.iter().rev() {
                result.push(component);
            }
            return result;
        }
        match existing.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                existing = match existing.parent() {
                    Some(p) => p.to_path_buf(),
                    None => return normalize_lexical(path),
                };
            }
            None => return normalize_lexical(path),
        }
    }
}

/// Resolve `path_str` against the workspace root and guarantee the result stays
/// inside the workspace. Absolute paths outside the root and relative `..`
/// traversal that escapes the root are rejected. Symlinks and junctions that
/// resolve outside the workspace are also rejected — without canonicalizing
/// the *final* resolved path (not just the lexical join), a directory
/// junction or file symlink placed inside the workspace can point anywhere on
/// disk and this check would never see it (audit A-4, 2026-07-04). This is
/// the security boundary for the file-content tools — without it an agent
/// could read or write anywhere the daemon user has access (e.g.
/// `/etc/passwd`, `../../.ssh/id_rsa`, or a symlinked/junctioned escape).
pub fn resolve_in_workspace(path_str: &str, workspace_root: &Path) -> Result<PathBuf, String> {
    // Fail closed on an empty workspace root. Otherwise a relative path resolves
    // against the daemon's process cwd, and the escape check below is defeated
    // (an empty lexical root is a prefix of every path), letting an agent read
    // or write into whatever directory the daemon was launched from.
    if workspace_root.as_os_str().is_empty() {
        return Err(format!(
            "Cannot resolve '{path_str}': this session has no workspace root. \
             Open a folder for the session before reading or writing files."
        ));
    }
    let resolved = resolve_path(path_str, workspace_root);
    // Canonicalize the root if it exists; fall back to a lexical normalization
    // so a not-yet-created workspace still resolves predictably.
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexical(workspace_root));
    let lexical_root = normalize_lexical(workspace_root);
    let lexical_ok =
        path_is_within(&resolved, &canonical_root) || path_is_within(&resolved, &lexical_root);
    if !lexical_ok {
        return Err(format!(
            "Path '{path_str}' escapes the workspace root '{}'",
            workspace_root.display()
        ));
    }
    // Lexically inside the workspace is not enough: re-check where the path
    // *actually* resolves on disk once symlinks/junctions are followed.
    let canonical_resolved = canonicalize_for_boundary_check(&resolved);
    if path_is_within(&canonical_resolved, &canonical_root) {
        Ok(resolved)
    } else {
        Err(format!(
            "Path '{path_str}' escapes the workspace root '{}' via a symlink or junction",
            workspace_root.display()
        ))
    }
}

pub(crate) fn policy_target_path(path: &Path, workspace_root: &Path) -> String {
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexical(workspace_root));
    let candidate = path
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexical(path));

    candidate
        .strip_prefix(&canonical_root)
        .or_else(|_| candidate.strip_prefix(&normalize_lexical(workspace_root)))
        .unwrap_or(&candidate)
        .to_string_lossy()
        .replace('\\', "/")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathAccess {
    Read,
    Write,
}

fn default_path_deny() -> &'static rightkit_agent_policy::DenyGlobSet {
    static DEFAULT: std::sync::OnceLock<rightkit_agent_policy::DenyGlobSet> = std::sync::OnceLock::new();
    DEFAULT.get_or_init(rightkit_agent_policy::DenyGlobSet::new)
}

/// Enforce the configured `deny-read-globs` / `deny-write-globs` (plus the
/// always-on defaults) on `path` before any IO. The path is tested in every
/// spelling an attacker could steer between: the lexical workspace-relative
/// form and the fully canonical form (symlinks/junctions followed, including
/// through the deepest existing ancestor of a not-yet-created target). A match
/// on ANY form denies; no form can lift a deny. Paths that land outside the
/// workspace are tested by their absolute canonical spelling.
pub(crate) fn enforce_path_deny(
    ctx: &ToolContext,
    path: &Path,
    access: PathAccess,
) -> Result<(), String> {
    let deny = ctx
        .path_deny
        .as_deref()
        .unwrap_or_else(|| default_path_deny());
    let root = &ctx.workspace_root;
    let canonical_root = root
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexical(root));
    let lexical = if path.is_absolute() {
        normalize_lexical(path)
    } else {
        normalize_lexical(&root.join(path))
    };
    let canonical = canonicalize_for_boundary_check(&lexical);
    let mut spellings: Vec<String> = Vec::new();
    for candidate in [&lexical, &canonical] {
        let rel = candidate
            .strip_prefix(&canonical_root)
            .or_else(|_| candidate.strip_prefix(&normalize_lexical(root)))
            .ok();
        match rel {
            Some(rel) => spellings.push(rel.to_string_lossy().replace('\\', "/")),
            None => spellings.push(candidate.to_string_lossy().replace('\\', "/")),
        }
    }
    for spelling in spellings {
        let hit = match access {
            PathAccess::Read => deny.first_read_match(&spelling).map(|g| ("read", g)),
            PathAccess::Write => deny.first_write_match(&spelling).map(|g| ("write", g)),
        };
        if let Some((kind, glob)) = hit {
            return Err(format!(
                "Permission denied: '{spelling}' matches {kind} deny-glob '{glob}' (policy: deny-glob:{kind})"
            ));
        }
    }
    Ok(())
}

// Audit A-4 (2026-07-04): reproduction tests settling the resolve_in_workspace /
// search resolve_path symlink-escape dispute. Windows junctions need no elevated
// privilege (unlike symlinks, which need Developer Mode or admin), so junctions
// are the primary repro vehicle; a file symlink repro is attempted too and
// skipped with a printed note if the sandbox can't create one.
#[cfg(all(test, windows))]
mod symlink_escape_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn windows_scope_equivalence_preserves_boundaries() {
        let workspace = tempdir().unwrap();
        let canonical = workspace.path().canonicalize().unwrap();
        let ordinary = canonical
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
        let target = Path::new(&ordinary).join("new-file.txt");
        assert!(path_is_within(&target, &canonical));
        assert!(path_is_within(
            &canonical.join("new-file.txt"),
            Path::new(&ordinary)
        ));
        assert!(resolve_in_workspace(target.to_str().unwrap(), &canonical).is_ok());
        assert!(!path_is_within(
            &canonical.join(r"..\outside.txt"),
            &canonical
        ));
        assert!(!path_is_within(
            Path::new(&format!("{ordinary}-other/file")),
            &canonical
        ));
        assert!(path_is_within(
            Path::new(r"\\server\share\repo\file"),
            Path::new(r"\\?\UNC\server\share\repo")
        ));
        assert!(!path_is_within(
            Path::new(r"\\server\other\repo\file"),
            Path::new(r"\\?\UNC\server\share\repo")
        ));
    }

    /// Create a directory junction (`mklink /J`) with no admin/Developer-Mode
    /// requirement. Returns true on success.
    fn make_junction(link: &Path, target: &Path) -> bool {
        rightkit_child_process::command_std("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Attempt a file symlink; returns true on success (needs Developer Mode or
    /// admin on Windows — expected to fail in most CI/sandbox contexts).
    fn make_file_symlink(link: &Path, target: &Path) -> bool {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }

    // 1a: workspace/link -> junction to a directory OUTSIDE the workspace;
    // resolve_in_workspace through the link — does it escape?
    #[test]
    fn resolve_in_workspace_junction_dir_escape() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

        let link = workspace.path().join("link");
        assert!(
            make_junction(&link, outside.path()),
            "mklink /J should not require elevation"
        );

        let result = resolve_in_workspace("link/secret.txt", workspace.path());
        match result {
            Ok(resolved) => {
                // If this resolves, it MUST still canonicalize to inside the
                // workspace tree. A path landing inside `outside` proves the
                // escape: the boundary check only compared lexical prefixes,
                // never the canonical (post-junction) location.
                let canonical_outside = outside.path().canonicalize().unwrap();
                assert!(
                    !resolved
                        .canonicalize()
                        .map(|c| c.starts_with(&canonical_outside))
                        .unwrap_or(false),
                    "ESCAPE CONFIRMED: resolve_in_workspace returned {resolved:?} which \
                     canonicalizes outside the workspace root via junction 'link'"
                );
            }
            Err(_) => {
                // Rejected outright — no escape via this path.
            }
        }
    }

    // 1b: same escape attempt via a file symlink (rather than a junction).
    // File symlinks need Developer Mode or admin on Windows; if creation
    // fails, that itself is the documented Windows answer for this variant.
    #[test]
    fn resolve_in_workspace_file_symlink_escape() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("secret.txt");
        std::fs::write(&outside_file, "top secret").unwrap();

        let link = workspace.path().join("link.txt");
        if !make_file_symlink(&link, &outside_file) {
            eprintln!(
                "SKIPPED (documented Windows answer): file symlink creation failed — \
                 requires Developer Mode or admin privilege in this environment."
            );
            return;
        }

        let result = resolve_in_workspace("link.txt", workspace.path());
        if let Ok(resolved) = result {
            let canonical_outside_file = outside_file.canonicalize().unwrap();
            assert!(
                resolved
                    .canonicalize()
                    .map(|c| c != canonical_outside_file)
                    .unwrap_or(true),
                "ESCAPE CONFIRMED: resolve_in_workspace returned {resolved:?} which \
                 canonicalizes to the outside secret file via symlink 'link.txt'"
            );
        }
    }

    // 1c: search tool's resolve_path (not boundary-checked) with a `../outside`
    // path argument — does it return a path outside the workspace root at all?
    #[test]
    fn search_resolve_path_dotdot_escapes_boundary() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

        let outside_name = outside.path().file_name().unwrap().to_str().unwrap();
        let traversal = format!("../{outside_name}");

        let resolved = resolve_path(&traversal, workspace.path());

        // resolve_path performs NO boundary check by design (see its doc
        // comment) — this test locks that a caller relying on it alone (as
        // search.rs does for its `path` argument) gets a path that has
        // genuinely left the workspace root, proving the boundary gap is real
        // and not hypothetical.
        assert!(
            !resolved.starts_with(workspace.path()),
            "resolve_path('{traversal}') should escape the workspace root when the \
             caller does no boundary check, but returned {resolved:?}"
        );
        assert_eq!(
            resolved.canonicalize().ok(),
            outside.path().canonicalize().ok(),
            "resolve_path should land exactly on the outside directory"
        );
    }

    // Also lock the junction variant used by search's `path` argument: a
    // junction *inside* the workspace pointing outside, resolved as `path`.
    #[test]
    fn search_resolve_path_junction_inside_workspace_escapes() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

        let link = workspace.path().join("link");
        assert!(make_junction(&link, outside.path()));

        let resolved = resolve_path("link", workspace.path());
        // Lexically this still starts with the workspace root (it's just
        // "<workspace>/link"), but walking it (as search.rs's WalkBuilder
        // does) follows the junction onto disk and reads `outside`'s
        // contents. Confirm the junction really does resolve on disk to the
        // outside directory, which is what makes the search escape real.
        assert_eq!(
            resolved.canonicalize().ok(),
            Some(link.canonicalize().unwrap()),
        );
        assert_eq!(
            link.canonicalize().ok(),
            outside.path().canonicalize().ok(),
            "junction 'link' should canonicalize straight through to the outside dir, \
             proving a walker rooted at resolve_path's unchecked result escapes"
        );
    }
}
