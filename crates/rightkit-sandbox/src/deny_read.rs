//! Protected-secret paths, deny-read planning and glob matching.
//!
//! Two jobs, ported from CodeRight `sandbox/deny_read.rs`:
//!
//! 1. A command whose granted root happens to contain a secret folder (a workspace that is the
//!    user profile, say) gets an explicit deny for that folder, so a wide grant never exposes
//!    `~/.ssh` and friends. [`SandboxPolicy::deny_read()`](crate::SandboxPolicy::deny_read)
//!    carries the deny to the backend and [`secret_paths_within`] finds the folders.
//! 2. A denied access to such a folder is reported as a protected secret that no approval
//!    prompt may grant ([`path_is_protected_secret`], [`crate::report_denial`]).
//!
//! Pure path logic: nothing here changes an ACL or a profile. Comparison is lexical after the
//! segments are normalized (`.`/`..` resolved, separators unified), case-insensitive only where
//! the platform's filesystems usually are (Windows, macOS).
// Each backend uses its own half (Seatbelt: regexes; AppContainer: expansion); all of it is tested.
#![allow(dead_code)]

use crate::SandboxError;
use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Secret-bearing folders directly under the user profile root.
pub const USERPROFILE_SECRET_FOLDERS: &[&str] = &[
    ".ssh",
    ".tsh",
    ".brev",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".config",
    ".npm",
    ".pki",
    ".terraform.d",
];

/// Most directory entries [`expand_glob`] visits before refusing, so a glob over a huge tree
/// fails instead of silently denying only part of what it names.
pub(crate) const GLOB_EXPANSION_LIMIT: usize = 20_000;

const CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

/// The current user's profile root (`USERPROFILE` on Windows, `HOME` elsewhere).
pub fn user_profile_root() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// Protected secret folders under `profile` (existing or not).
pub fn secret_roots_under(profile: &Path) -> Vec<PathBuf> {
    USERPROFILE_SECRET_FOLDERS
        .iter()
        .map(|name| profile.join(name))
        .collect()
}

/// Protected secret folders of the current user.
pub fn secret_roots() -> Vec<PathBuf> {
    user_profile_root()
        .map(|profile| secret_roots_under(&profile))
        .unwrap_or_default()
}

/// Strip the `\\?\` verbatim prefix `canonicalize` adds on Windows.
pub(crate) fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix("\\\\?\\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path,
    }
}

/// Resolve `.`/`..` and repeated separators in a `/`-separated string. `..` above the root of
/// an absolute path stays at the root; above the start of a relative one it is kept.
fn normalize_text(text: &str) -> String {
    let rooted = text.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in text.split('/') {
        match segment {
            "" | "." => {}
            ".." => match parts.last() {
                // A bare drive (`c:`) is a root: `..` cannot climb above it.
                Some(last) if last.len() == 2 && last.ends_with(':') && parts.len() == 1 => {}
                Some(last) if *last != ".." => {
                    parts.pop();
                }
                _ if rooted => {}
                _ => parts.push(".."),
            },
            other => parts.push(other),
        }
    }
    let mut out = String::new();
    if rooted {
        out.push('/');
    }
    out.push_str(&parts.join("/"));
    out
}

/// Case-folded (where the platform folds), separator-normalized comparison key for a path.
pub(crate) fn lexical_path_key(path: &Path) -> String {
    let plain = strip_verbatim(path.to_path_buf());
    let text = plain.to_string_lossy();
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    };
    let key = normalize_text(&text);
    if CASE_INSENSITIVE {
        key.to_lowercase()
    } else {
        key
    }
}

/// Whether `path` equals `root` or lies beneath it. Lexical: `.`/`..` are resolved first, so
/// `/home/u/x/../.ssh/key` is inside `/home/u/.ssh`; nothing here follows a symlink.
pub fn path_is_within(path: &Path, root: &Path) -> bool {
    let (path, root) = (lexical_path_key(path), lexical_path_key(root));
    if root.is_empty() {
        return false;
    }
    let prefix = format!("{}/", root.trim_end_matches('/'));
    path == root || path.starts_with(&prefix)
}

fn has_parent_dir(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

fn is_filesystem_root(path: &Path) -> bool {
    path.is_absolute() && path.parent().is_none()
}

/// Policy-level check of one `deny_read` path: absolute, NUL-free, no `..`, not a filesystem
/// root (a root deny would lock the child out of everything, and an alias must not slip one in).
pub(crate) fn validate_deny_path(path: &Path) -> Result<(), SandboxError> {
    let text = path.to_string_lossy();
    if !path.is_absolute() || text.contains('\0') {
        return Err(SandboxError::InvalidPolicy(format!(
            "deny_read path must be absolute and NUL-free: {}",
            text.escape_debug()
        )));
    }
    if has_parent_dir(path) {
        return Err(SandboxError::InvalidPolicy(format!(
            "deny_read path must not contain `..`: {text}"
        )));
    }
    if is_filesystem_root(path) {
        return Err(SandboxError::InvalidPolicy(format!(
            "refusing to deny reads of a filesystem root: {text}"
        )));
    }
    Ok(())
}

fn glob_is_absolute(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    pattern.starts_with('/')
        || pattern.starts_with("\\\\")
        || (bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\'))
}

/// Policy-level check of one `deny_read` glob: absolute, printable, no `..` segment, no quote
/// (the macOS backend embeds it in a regex literal).
pub(crate) fn validate_glob(pattern: &str) -> Result<(), SandboxError> {
    let bad = |why: &str| {
        Err(SandboxError::InvalidPolicy(format!(
            "deny_read glob {why}: {}",
            pattern.escape_debug()
        )))
    };
    if pattern.is_empty() {
        return bad("is empty");
    }
    if pattern.chars().any(|c| c.is_control() || c == '"') {
        return bad("contains a control character or a quote");
    }
    if !glob_is_absolute(pattern) {
        return bad("must be absolute");
    }
    if pattern.split(['/', '\\']).any(|segment| segment == "..") {
        return bad("must not contain a `..` segment");
    }
    Ok(())
}

/// Build the exact paths that should receive a deny entry.
///
/// Both the lexical path and, when it exists, its canonical target are kept: the lexical path
/// covers the configured spelling, the canonical one covers a symlink/reparse point so the same
/// object cannot be read through its resolved location. Paths must be absolute and free of `..`;
/// filesystem roots are refused after canonicalization, so an alias can never install a deny on a
/// drive root.
pub fn plan_deny_read_paths(paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let mut planned = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |planned: &mut Vec<PathBuf>, path: PathBuf| {
        if seen.insert(lexical_path_key(&path)) {
            planned.push(path);
        }
    };
    for path in paths {
        if !path.is_absolute() || has_parent_dir(path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a deny-read entry must be an absolute path without `..`: {}",
                    path.display()
                ),
            ));
        }
        push(&mut planned, path.to_path_buf());
        let canonical = match std::fs::canonicalize(path) {
            Ok(canonical) => strip_verbatim(canonical),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        push(&mut planned, canonical);
    }
    if let Some(root) = planned.iter().find(|path| is_filesystem_root(path)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to apply a deny-read entry to filesystem root {}",
                root.display()
            ),
        ));
    }
    Ok(planned)
}

/// Infallible protected-set planning: every lexical secret path is always kept, and its canonical
/// target is added when it resolves to something other than a filesystem root. A planning
/// failure (an alias resolving to `/`, an unreadable entry) can therefore never shrink the
/// protected set; it only forgoes the extra canonical spelling.
pub(crate) fn plan_secret_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut planned = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if seen.insert(lexical_path_key(path)) {
            planned.push(path.clone());
        }
        if let Ok(canonical) = std::fs::canonicalize(path).map(strip_verbatim) {
            if !is_filesystem_root(&canonical) && seen.insert(lexical_path_key(&canonical)) {
                planned.push(canonical);
            }
        }
    }
    planned
}

/// Existing protected secret paths that lie inside `granted_root`: the carve-outs a wide grant
/// must not expose. Planned through [`plan_deny_read_paths`] so symlinked spellings are covered.
/// Missing secret folders are not materialized.
pub fn secret_paths_within(granted_root: &Path) -> Vec<PathBuf> {
    let root = strip_verbatim(
        std::fs::canonicalize(granted_root).unwrap_or_else(|_| granted_root.to_path_buf()),
    );
    let existing: Vec<PathBuf> = secret_roots().into_iter().filter(|p| p.exists()).collect();
    plan_secret_paths(&existing)
        .into_iter()
        .filter(|secret| path_is_within(secret, &root))
        .collect()
}

fn protected_under(path: &Path, secrets: &[PathBuf]) -> bool {
    let canonical = std::fs::canonicalize(path)
        .map(strip_verbatim)
        .unwrap_or_else(|_| path.to_path_buf());
    secrets
        .iter()
        .any(|secret| path_is_within(path, secret) || path_is_within(&canonical, secret))
}

/// Whether `path` is (inside) a protected secret folder. Never grantable. The check is lexical
/// on the given spelling and on the canonical target when the path exists, so neither a `..`
/// detour nor a symlink hides a secret.
pub fn path_is_protected_secret(path: &Path) -> bool {
    let planned = plan_secret_paths(&secret_roots());
    protected_under(path, &planned)
}

// ---------------------------------------------------------------------------------------
// Globs
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Lit(char),
    /// `?`: one character other than `/`.
    One,
    /// `*`: any run within one segment.
    Star,
    /// `**`: any run, across segments.
    DoubleStar,
    /// `**/`: zero or more whole segments.
    DoubleStarSlash,
}

fn normalize_glob_text(pattern: &str) -> String {
    if cfg!(windows) {
        pattern.replace('\\', "/")
    } else {
        pattern.to_string()
    }
}

fn tokenize(pattern: &str) -> Vec<Tok> {
    let chars: Vec<char> = normalize_glob_text(pattern).chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                let mut run = 0;
                while chars.get(i + run) == Some(&'*') {
                    run += 1;
                }
                i += run;
                if run == 1 {
                    tokens.push(Tok::Star);
                } else if chars.get(i) == Some(&'/') {
                    i += 1;
                    tokens.push(Tok::DoubleStarSlash);
                } else {
                    tokens.push(Tok::DoubleStar);
                }
            }
            '?' => {
                tokens.push(Tok::One);
                i += 1;
            }
            c => {
                tokens.push(Tok::Lit(c));
                i += 1;
            }
        }
    }
    tokens
}

/// Whether `text` (an absolute path spelling) matches the absolute glob `pattern`. The match is
/// anchored at both ends and runs in `O(pattern * text)`, so no pattern can stall it.
// The rows are indexed by text position on purpose: each cell depends on its left neighbour.
#[allow(clippy::needless_range_loop)]
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    let fold = |c: char| {
        if CASE_INSENSITIVE {
            c.to_lowercase().next().unwrap_or(c)
        } else {
            c
        }
    };
    let tokens = tokenize(pattern);
    let text: Vec<char> = normalize_glob_text(text).chars().collect();
    let n = text.len();
    // row[j]: the tokens consumed so far match text[..j].
    let mut row = vec![false; n + 1];
    row[0] = true;
    for tok in tokens {
        let mut next = vec![false; n + 1];
        match tok {
            Tok::Lit(c) => {
                let c = fold(c);
                for j in 0..n {
                    next[j + 1] = row[j] && fold(text[j]) == c;
                }
            }
            Tok::One => {
                for j in 0..n {
                    next[j + 1] = row[j] && text[j] != '/';
                }
            }
            Tok::Star | Tok::DoubleStar => {
                let any_char = tok == Tok::DoubleStar;
                for j in 0..=n {
                    next[j] = row[j] || (j > 0 && next[j - 1] && (any_char || text[j - 1] != '/'));
                }
            }
            Tok::DoubleStarSlash => {
                let mut seen = false;
                for j in 0..=n {
                    next[j] = row[j] || (j > 0 && text[j - 1] == '/' && seen);
                    seen |= row[j];
                }
            }
        }
        row = next;
    }
    row[n]
}

/// An anchored regular expression equal to `pattern` (POSIX extended syntax, as Seatbelt's
/// `regex` filter reads it). Letters match in both cases where the platform's filesystems
/// usually fold case.
pub fn glob_to_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    for tok in tokenize(pattern) {
        match tok {
            Tok::Lit(c) if c.is_ascii_alphabetic() && CASE_INSENSITIVE => {
                out.push('[');
                out.push(c.to_ascii_lowercase());
                out.push(c.to_ascii_uppercase());
                out.push(']');
            }
            Tok::Lit(c) => {
                if !c.is_ascii_alphanumeric() && c != '/' && c != '_' && c != '-' {
                    out.push('\\');
                }
                out.push(c);
            }
            Tok::One => out.push_str("[^/]"),
            Tok::Star => out.push_str("[^/]*"),
            Tok::DoubleStar => out.push_str(".*"),
            Tok::DoubleStarSlash => out.push_str("(.*/)?"),
        }
    }
    out.push('$');
    out
}

/// As [`glob_to_regex`], but a match also covers everything below it: a glob that names a
/// directory denies the directory's contents too.
pub(crate) fn glob_to_deny_regex(pattern: &str) -> String {
    let regex = glob_to_regex(pattern);
    let body = regex.strip_suffix('$').unwrap_or(&regex);
    format!("{body}(/.*)?$")
}

/// The literal directory a glob starts from: every leading segment with no wildcard.
pub(crate) fn glob_literal_prefix(pattern: &str) -> PathBuf {
    let normalized = normalize_glob_text(pattern);
    let mut prefix = String::new();
    for (index, segment) in normalized.split('/').enumerate() {
        if segment.contains(['*', '?']) {
            break;
        }
        if index > 0 {
            prefix.push('/');
        }
        prefix.push_str(segment);
    }
    if prefix.is_empty() {
        prefix.push('/');
    }
    PathBuf::from(prefix)
}

/// Expand an absolute glob against the filesystem into concrete existing paths, for backends
/// that need an object to attach a deny to. A directory that matches covers everything below it
/// and is not descended into; symlinks are never followed. Fails (rather than under-deny) when
/// more than `limit` directory entries would have to be visited.
pub(crate) fn expand_glob(pattern: &str, limit: usize) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut visited = 0usize;
    let mut stack = vec![glob_literal_prefix(pattern)];
    // A glob with no wildcard names one path.
    if !pattern.contains(['*', '?']) {
        let path = PathBuf::from(pattern);
        return Ok(if path.exists() { vec![path] } else { found });
    }
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            visited += 1;
            if visited > limit {
                return Err(io::Error::other(format!(
                    "deny_read glob {pattern} would visit more than {limit} directory entries"
                )));
            }
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if glob_matches(pattern, &path.to_string_lossy()) {
                found.push(path);
            } else if is_dir {
                stack.push(path);
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rk-deny-read-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    #[cfg(unix)]
    fn abs(path: &str) -> PathBuf {
        PathBuf::from(path)
    }
    #[cfg(windows)]
    fn abs(path: &str) -> PathBuf {
        PathBuf::from(format!("C:{}", path.replace('/', "\\")))
    }

    #[test]
    fn secret_roots_under_names_every_protected_folder() {
        let roots = secret_roots_under(&abs("/home/u"));
        assert_eq!(roots.len(), USERPROFILE_SECRET_FOLDERS.len());
        for name in [
            ".ssh", ".aws", ".gnupg", ".kube", ".docker", ".config", ".npm",
        ] {
            assert!(roots.contains(&abs("/home/u").join(name)), "{name}");
        }
    }

    #[test]
    fn within_is_component_wise_not_a_string_prefix() {
        assert!(path_is_within(&abs("/home/u/.ssh"), &abs("/home/u/.ssh")));
        assert!(path_is_within(
            &abs("/home/u/.ssh/id_rsa"),
            &abs("/home/u/.ssh")
        ));
        // `.ssh-keys` shares a string prefix with `.ssh` but is another folder.
        assert!(!path_is_within(
            &abs("/home/u/.ssh-keys/x"),
            &abs("/home/u/.ssh")
        ));
        assert!(!path_is_within(&abs("/home/u"), &abs("/home/u/.ssh")));
        assert!(!path_is_within(&abs("/home/u/x"), Path::new("")));
    }

    #[test]
    fn within_resolves_dot_dot_so_a_detour_cannot_hide_a_secret() {
        assert!(path_is_within(
            &abs("/home/u/work/../.ssh/id_rsa"),
            &abs("/home/u/.ssh")
        ));
        assert!(!path_is_within(
            &abs("/home/u/.ssh/../work/file"),
            &abs("/home/u/.ssh")
        ));
        // `..` cannot climb above the root.
        assert!(path_is_within(
            &abs("/../home/u/.ssh/k"),
            &abs("/home/u/.ssh")
        ));
    }

    #[test]
    fn within_root_covers_everything() {
        let root = if cfg!(windows) {
            Path::new("C:\\")
        } else {
            Path::new("/")
        };
        assert!(path_is_within(&abs("/any/where"), root));
    }

    #[test]
    fn protected_check_sees_dot_dot_detours_on_missing_paths() {
        let secrets = vec![abs("/home/u/.ssh")];
        assert!(protected_under(&abs("/home/u/.ssh/id_rsa"), &secrets));
        assert!(protected_under(&abs("/home/u/a/../.ssh/id_rsa"), &secrets));
        assert!(!protected_under(&abs("/home/u/work/file"), &secrets));
    }

    #[cfg(unix)]
    #[test]
    fn protected_check_follows_a_symlink_to_a_secret() {
        let dir = scratch("protected-link");
        let secret = dir.join(".ssh");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("id"), "k").unwrap();
        let link = dir.join("innocent");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let planned = plan_deny_read_paths(std::slice::from_ref(&secret)).unwrap();
        assert!(protected_under(&link.join("id"), &planned));
        assert!(!protected_under(&dir.join("other"), &planned));
    }

    #[test]
    fn plan_keeps_lexical_and_canonical_without_duplicates() {
        let dir = scratch("plan");
        let secret = dir.join(".ssh");
        std::fs::create_dir_all(&secret).unwrap();
        let planned = plan_deny_read_paths(&[secret.clone(), secret.clone()]).unwrap();
        // Already canonical: one entry, listed once.
        assert_eq!(planned, vec![secret]);
    }

    #[cfg(unix)]
    #[test]
    fn plan_adds_the_canonical_target_of_a_symlink() {
        let dir = scratch("plan-link");
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let planned = plan_deny_read_paths(std::slice::from_ref(&link)).unwrap();
        assert_eq!(planned, vec![link, real]);
    }

    #[test]
    fn plan_keeps_a_missing_path_lexically_only() {
        let dir = scratch("plan-missing");
        let missing = dir.join("not-yet");
        assert_eq!(
            plan_deny_read_paths(std::slice::from_ref(&missing)).unwrap(),
            vec![missing]
        );
    }

    #[test]
    fn plan_refuses_filesystem_roots_relative_paths_and_dot_dot() {
        let root = if cfg!(windows) {
            PathBuf::from("C:\\")
        } else {
            PathBuf::from("/")
        };
        assert!(plan_deny_read_paths(&[root]).is_err());
        assert!(plan_deny_read_paths(&[PathBuf::from("relative/.ssh")]).is_err());
        assert!(plan_deny_read_paths(&[abs("/home/u/../..")]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn secret_plan_keeps_lexical_paths_even_when_an_alias_resolves_to_root() {
        let secrets = vec![abs("/definitely/not/there/.ssh"), abs("/")];
        let planned = plan_secret_paths(&secrets);
        assert!(planned.contains(&secrets[0]));
        assert!(!planned.is_empty());
        assert!(plan_deny_read_paths(&secrets).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn plan_refuses_a_symlink_that_resolves_to_the_filesystem_root() {
        let dir = scratch("plan-root-alias");
        let alias = dir.join("everything");
        std::os::unix::fs::symlink("/", &alias).unwrap();
        let error = plan_deny_read_paths(&[alias]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("filesystem root"), "{error}");
    }

    #[test]
    fn validate_deny_path_rejects_the_hostile_spellings() {
        assert!(validate_deny_path(&abs("/home/u/.ssh")).is_ok());
        assert!(validate_deny_path(Path::new("relative")).is_err());
        assert!(validate_deny_path(&abs("/home/../etc")).is_err());
        let with_nul = PathBuf::from(format!("{}/a\0b", abs("/x").display()));
        assert!(validate_deny_path(&with_nul).is_err());
        let root = if cfg!(windows) {
            PathBuf::from("C:\\")
        } else {
            PathBuf::from("/")
        };
        assert!(validate_deny_path(&root).is_err());
    }

    #[test]
    fn validate_glob_rejects_the_hostile_spellings() {
        assert!(validate_glob("/home/u/.ssh/**").is_ok());
        assert!(validate_glob("C:\\Users\\u\\.ssh\\*").is_ok());
        for bad in [
            "",
            "relative/*",
            "/a/../b/*",
            "/a/\"quote\"",
            "/a/new\nline",
            "/a/nul\0",
        ] {
            assert!(validate_glob(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn glob_star_stays_inside_one_segment() {
        assert!(glob_matches("/h/*/key", "/h/u/key"));
        assert!(!glob_matches("/h/*/key", "/h/u/v/key"));
        assert!(glob_matches("/h/*.pem", "/h/a.pem"));
        assert!(!glob_matches("/h/*.pem", "/h/sub/a.pem"));
        assert!(glob_matches("/h/id_*", "/h/id_"));
    }

    #[test]
    fn glob_double_star_crosses_segments_and_may_match_zero_directories() {
        assert!(glob_matches("/h/**", "/h/a/b/c"));
        assert!(glob_matches("/h/**/key", "/h/a/b/key"));
        assert!(glob_matches("/h/**/key", "/h/key"));
        assert!(!glob_matches("/h/**/key", "/h/keys"));
        assert!(!glob_matches("/h/**/key", "/h/a/b/keys"));
        assert!(glob_matches("/h/**", "/h/"));
    }

    #[test]
    fn glob_question_mark_is_one_non_separator() {
        assert!(glob_matches("/h/k?y", "/h/key"));
        assert!(!glob_matches("/h/k?y", "/h/ky"));
        assert!(!glob_matches("/h/a?b", "/h/a/b"));
    }

    #[test]
    fn glob_is_anchored_and_literal_elsewhere() {
        assert!(!glob_matches("/h/key", "/h/key2"));
        assert!(!glob_matches("/h/key", "/x/h/key"));
        // Regex metacharacters in the pattern are literal characters.
        assert!(glob_matches("/h/a.b(c)", "/h/a.b(c)"));
        assert!(!glob_matches("/h/a.b", "/h/aXb"));
    }

    #[test]
    fn glob_matching_is_linear_on_pathological_patterns() {
        let pattern = format!("/{}", "*a".repeat(60));
        let text = format!("/{}", "a".repeat(2000));
        // Would take exponential time with naive backtracking; must simply answer.
        assert!(glob_matches(&pattern, &text));
        assert!(!glob_matches(&format!("{pattern}b"), &text));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn glob_to_regex_translates_each_token() {
        let regex = glob_to_regex("/h/**/*.k?");
        if CASE_INSENSITIVE {
            assert_eq!(regex, "^/[hH]/(.*/)?[^/]*\\.[kK][^/]$");
        } else {
            assert_eq!(regex, "^/h/(.*/)?[^/]*\\.k[^/]$");
        }
        assert!(glob_to_regex("/a/**").ends_with(".*$"));
        // A path with regex metacharacters is escaped, never interpreted.
        assert!(glob_to_regex("/a/b+c").contains("\\+"));
    }

    #[test]
    fn deny_regex_also_covers_the_children_of_a_match() {
        let regex = glob_to_deny_regex("/h/.ssh*");
        assert!(
            regex.starts_with('^') && regex.ends_with("(/.*)?$"),
            "{regex}"
        );
        assert_eq!(regex.matches('$').count(), 1, "{regex}");
    }

    #[test]
    fn literal_prefix_stops_at_the_first_wildcard_segment() {
        assert_eq!(
            glob_literal_prefix("/h/u/.ssh/*"),
            PathBuf::from("/h/u/.ssh")
        );
        assert_eq!(glob_literal_prefix("/h/*/x"), PathBuf::from("/h"));
        assert_eq!(glob_literal_prefix("/*"), PathBuf::from("/"));
    }

    #[test]
    fn expand_glob_finds_existing_matches_and_does_not_descend_into_a_match() {
        let dir = scratch("expand");
        std::fs::create_dir_all(dir.join("keys/inner")).unwrap();
        std::fs::write(dir.join("keys/a.pem"), "x").unwrap();
        std::fs::write(dir.join("keys/inner/b.pem"), "x").unwrap();
        std::fs::write(dir.join("keys/c.txt"), "x").unwrap();
        let pattern = format!("{}/keys/**/*.pem", dir.display());
        let mut found = expand_glob(&pattern, 1000).unwrap();
        found.sort();
        assert_eq!(
            found,
            vec![dir.join("keys/a.pem"), dir.join("keys/inner/b.pem")]
        );
        // A glob with no wildcard names one path, existing or not.
        let single = dir.join("keys/c.txt").to_string_lossy().into_owned();
        assert_eq!(
            expand_glob(&single, 10).unwrap(),
            vec![dir.join("keys/c.txt")]
        );
        let missing = dir.join("keys/none").to_string_lossy().into_owned();
        assert!(expand_glob(&missing, 10).unwrap().is_empty());
    }

    #[test]
    fn expand_glob_fails_instead_of_under_denying_a_huge_tree() {
        let dir = scratch("expand-limit");
        for i in 0..20 {
            std::fs::write(dir.join(format!("f{i}.txt")), "x").unwrap();
        }
        let pattern = format!("{}/*.pem", dir.display());
        assert!(expand_glob(&pattern, 5).is_err());
        assert!(expand_glob(&pattern, 100).unwrap().is_empty());
    }

    #[test]
    fn deny_read_entries_validate_through_the_policy() {
        use crate::SandboxPolicy;
        assert!(SandboxPolicy::new()
            .deny_read(abs("/home/u/.ssh"))
            .validate()
            .is_ok());
        assert!(SandboxPolicy::new()
            .deny_read("relative")
            .validate()
            .is_err());
        assert!(SandboxPolicy::new()
            .deny_read_glob("nope/*")
            .validate()
            .is_err());
        assert!(SandboxPolicy::new()
            .deny_read_glob(format!("{}/*.pem", abs("/h").display()))
            .validate()
            .is_ok());
    }
}
