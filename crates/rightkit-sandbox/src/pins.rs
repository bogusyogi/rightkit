//! Pinned read roots: interpreter install directories every confined child may read and execute
//! for as long as something holds a pin, instead of a per-command grant.
//!
//! Why a pin and not a grant. An interpreter (Python, Node, ...) lives in a tree the policy author
//! rarely lists, and a per-command ACL grant on a big install tree is rewritten for every child.
//! A pin is taken once and shared: [`Sandbox::pin_read_roots`] returns a [`PinHandle`], a user
//! count per root keeps the entry while any handle lives, and the entry is removed with the last
//! one ([`PinHandle::release_all`] at shutdown). Off Windows, pinned roots are added to the read
//! and execute roots of every confined spawn in the process. On Windows the entry is an
//! ALL APPLICATION PACKAGES read/execute ACE (every AppContainer token satisfies it), which is also
//! why a root is vetted hard: anything pinned is readable by *every* AppContainer app on the
//! machine, not just this crate's children.
//!
//! A root is refused when it is too broad ([`read_root_is_too_broad`]: a filesystem root, the user
//! profile or a folder that holds a user directory or a protected secret) or when it does not
//! look like an interpreter install ([`is_interpreter_dir`]).
// The pin table is process-wide state; `PinTable` documents its bound.

use crate::deny_read::{lexical_path_key, path_is_within, secret_roots, user_profile_root};
use crate::{Sandbox, SandboxError};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Executable stems that mark a directory as an interpreter install (`python`, `python3.12`,
/// `node`, ...). A name matches case-insensitively, with or without `.exe`/`.cmd`/`.bat`/`.com`.
const INTERPRETERS: &[&str] = &[
    "python", "pythonw", "py", "node", "nodejs", "deno", "bun", "ruby", "perl", "php", "java",
    "javaw", "dotnet", "pwsh", "lua", "luajit", "rscript",
];

/// Most entries read from a candidate directory while looking for an interpreter.
const SCAN_LIMIT: usize = 4096;

/// Folders under the user profile whose contents are the user's own data. A pinned root may sit
/// inside one (a virtual environment in `Documents`) but never be, or contain, one.
const USER_DATA_FOLDERS: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Music",
    "Videos",
    "OneDrive",
];

/// The current user's profile and well-known data folders.
pub fn default_user_dirs() -> Vec<PathBuf> {
    let Some(home) = user_profile_root() else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = USER_DATA_FOLDERS.iter().map(|d| home.join(d)).collect();
    dirs.push(home);
    dirs
}

/// Whether `root` is too broad to hand to every confined child as a read root: a filesystem root,
/// a relative or `..` path, the user profile or any of `user_dirs` (or a directory that contains
/// one of them, which would expose it), or a protected secret folder (or a directory containing
/// one). Descendants of the user's folders are fine: that is where per-user interpreters live.
/// Lexical; nothing here touches the filesystem.
pub fn read_root_is_too_broad(root: &Path, user_dirs: &[PathBuf]) -> bool {
    if !root.is_absolute() || root.components().any(|c| matches!(c, Component::ParentDir)) {
        return true;
    }
    if root.parent().is_none() || lexical_path_key(root).trim_matches('/').is_empty() {
        return true;
    }
    let profile = user_profile_root();
    let protected: Vec<PathBuf> = user_dirs
        .iter()
        .cloned()
        .chain(profile)
        .chain(secret_roots())
        .collect();
    // `path_is_within(dir, root)`: `dir` is `root` or below it, so `root` is that folder or an
    // ancestor of it. The reverse (`root` inside a secret) is refused for secrets only.
    protected.iter().any(|dir| path_is_within(dir, root))
        || secret_roots()
            .iter()
            .any(|secret| path_is_within(root, secret))
}

fn is_interpreter_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    let stem = ["exe", "cmd", "bat", "com"]
        .iter()
        .find_map(|ext| lower.strip_suffix(&format!(".{ext}")))
        .unwrap_or(&lower);
    if INTERPRETERS.contains(&stem) {
        return true;
    }
    // `python3`, `python3.12`: the name followed only by digits and dots.
    stem.strip_prefix("python").is_some_and(|rest| {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
    })
}

fn holds_interpreter(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.take(SCAN_LIMIT).flatten().any(|entry| {
            entry.file_type().is_ok_and(|t| !t.is_dir())
                && is_interpreter_name(&entry.file_name().to_string_lossy())
        })
    })
}

/// Whether `dir` looks like an interpreter install: it holds a known interpreter executable
/// itself or in `bin` or `Scripts` (a virtual environment, a `/opt/python/bin` layout).
pub fn is_interpreter_dir(dir: &Path) -> bool {
    holds_interpreter(dir)
        || holds_interpreter(&dir.join("bin"))
        || holds_interpreter(&dir.join("Scripts"))
}

#[cfg(windows)]
fn platform_grant(path: &Path) -> std::io::Result<bool> {
    crate::windows_appcontainer::pin_grant(path)
}
#[cfg(not(windows))]
fn platform_grant(_path: &Path) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(windows)]
fn platform_revoke(path: &Path) -> std::io::Result<()> {
    crate::windows_appcontainer::pin_revoke(path)
}
#[cfg(not(windows))]
fn platform_revoke(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

struct PinEntry {
    path: PathBuf,
    users: usize,
    /// This entry added an ACE (Windows) that its last release must remove.
    granted: bool,
}

/// The pinned roots. One entry per distinct root with a live handle, removed with its last
/// handle, so the table is bounded by the pins callers hold.
#[derive(Default)]
struct PinTable {
    /// Bumped by [`PinHandle::release_all`] so handles taken before it release nothing.
    epoch: u64,
    entries: HashMap<String, PinEntry>,
}

fn table() -> &'static Mutex<PinTable> {
    static TABLE: OnceLock<Mutex<PinTable>> = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

/// A shared lease on one or more pinned read roots. Dropping the last handle on a root removes
/// the pin (and, on Windows, its ACE).
#[derive(Debug)]
pub struct PinHandle {
    epoch: u64,
    keys: Vec<String>,
    roots: Vec<PathBuf>,
}

impl PinHandle {
    /// The canonical roots this handle pins.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Release every pin in the process now, whoever holds it, and return how many roots were
    /// pinned. Handles taken earlier become inert. For a daemon's shutdown path: a pin's ACE is
    /// removed here instead of waiting for each handle to drop.
    pub fn release_all() -> usize {
        let mut table = table().lock().unwrap_or_else(|e| e.into_inner());
        table.epoch += 1;
        let drained: Vec<PinEntry> = table.entries.drain().map(|(_, entry)| entry).collect();
        let released = drained.len();
        for entry in drained {
            release_entry(&entry);
        }
        released
    }
}

fn release_entry(entry: &PinEntry) {
    if entry.granted {
        if let Err(error) = platform_revoke(&entry.path) {
            // The ACE stays on a path the user chose to pin; it never widens beyond the pin.
            eprintln!(
                "rightkit-sandbox: could not remove the pin on {}: {error}",
                entry.path.display()
            );
        }
    }
}

impl Drop for PinHandle {
    fn drop(&mut self) {
        let mut table = table().lock().unwrap_or_else(|e| e.into_inner());
        if table.epoch != self.epoch {
            return;
        }
        for key in &self.keys {
            let Some(entry) = table.entries.get_mut(key) else {
                continue;
            };
            entry.users = entry.users.saturating_sub(1);
            if entry.users == 0 {
                if let Some(entry) = table.entries.remove(key) {
                    release_entry(&entry);
                }
            }
        }
    }
}

/// The canonical roots currently pinned, for the spawn paths that add them to a policy. The
/// only non-test caller (`spawn::with_pins`) is compiled off Windows, so Windows builds keep it
/// for tests alone.
#[cfg(any(test, not(windows)))]
pub(crate) fn pinned_read_roots() -> Vec<PathBuf> {
    let table = table().lock().unwrap_or_else(|e| e.into_inner());
    table
        .entries
        .values()
        .map(|entry| entry.path.clone())
        .collect()
}

fn vet(root: &Path, user_dirs: &[PathBuf]) -> Result<PathBuf, SandboxError> {
    let refuse = |why: String| {
        Err(SandboxError::InvalidPolicy(format!(
            "pin_read_roots: {why}"
        )))
    };
    if !root.is_absolute() || root.to_string_lossy().contains('\0') {
        return refuse(format!(
            "{} is not an absolute NUL-free path",
            root.display()
        ));
    }
    if root.components().any(|c| matches!(c, Component::ParentDir)) {
        return refuse(format!("{} contains `..`", root.display()));
    }
    let canonical = std::fs::canonicalize(root)
        .map(crate::deny_read::strip_verbatim)
        .map_err(|e| {
            SandboxError::InvalidPolicy(format!("{} does not resolve: {e}", root.display()))
        })?;
    if !canonical.is_dir() {
        return refuse(format!("{} is not a directory", canonical.display()));
    }
    // Both spellings: the one the caller gave and the one it resolves to (a junction, a
    // symlinked version-manager shim) must each be narrow.
    if read_root_is_too_broad(root, user_dirs) || read_root_is_too_broad(&canonical, user_dirs) {
        return refuse(format!(
            "{} is too broad to expose to every confined child",
            canonical.display()
        ));
    }
    if !is_interpreter_dir(&canonical) {
        return refuse(format!(
            "{} does not hold a known interpreter (python, node, ruby, ...), so it is not an \
             interpreter install",
            canonical.display()
        ));
    }
    Ok(canonical)
}

impl Sandbox {
    /// Pin interpreter install directories as read and execute roots for every confined child in
    /// this process until the returned handle (and every other handle on the same roots) drops, or
    /// [`PinHandle::release_all`] runs. All roots are vetted before any is pinned: a root that is
    /// too broad ([`read_root_is_too_broad`], judged against the user's profile and data
    /// folders), not a directory or not an interpreter install ([`is_interpreter_dir`]) fails the
    /// call and pins nothing. Roots are canonicalized, so a junction pins its target.
    ///
    /// Windows: an ALL APPLICATION PACKAGES read/execute ACE, added when the first handle pins the
    /// root unless the live DACL already allows it, and removed with the last. Elsewhere: the roots
    /// join the read and execute roots of each confined spawn.
    pub fn pin_read_roots(&self, roots: &[PathBuf]) -> Result<PinHandle, SandboxError> {
        let user_dirs = default_user_dirs();
        let mut canonical: Vec<PathBuf> = Vec::with_capacity(roots.len());
        for root in roots {
            let path = vet(root, &user_dirs)?;
            if !canonical.contains(&path) {
                canonical.push(path);
            }
        }
        let mut table = table().lock().unwrap_or_else(|e| e.into_inner());
        let mut taken: Vec<String> = Vec::new();
        for path in &canonical {
            let key = lexical_path_key(path);
            if let Some(entry) = table.entries.get_mut(&key) {
                entry.users += 1;
            } else {
                match platform_grant(path) {
                    Ok(granted) => {
                        table.entries.insert(
                            key.clone(),
                            PinEntry {
                                path: path.clone(),
                                users: 1,
                                granted,
                            },
                        );
                    }
                    Err(error) => {
                        // Undo the roots already taken in this call.
                        for key in &taken {
                            let gone = match table.entries.get_mut(key) {
                                Some(entry) => {
                                    entry.users = entry.users.saturating_sub(1);
                                    entry.users == 0
                                }
                                None => false,
                            };
                            if gone {
                                if let Some(entry) = table.entries.remove(key) {
                                    release_entry(&entry);
                                }
                            }
                        }
                        return Err(SandboxError::Io(error));
                    }
                }
            }
            taken.push(key);
        }
        Ok(PinHandle {
            epoch: table.epoch,
            keys: taken,
            roots: canonical,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn abs(path: &str) -> PathBuf {
        PathBuf::from(path)
    }
    #[cfg(windows)]
    fn abs(path: &str) -> PathBuf {
        PathBuf::from(format!("C:{}", path.replace('/', "\\")))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rk-pins-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    #[test]
    fn filesystem_roots_and_relative_paths_are_too_broad() {
        let root = if cfg!(windows) {
            PathBuf::from("C:\\")
        } else {
            PathBuf::from("/")
        };
        assert!(read_root_is_too_broad(&root, &[]));
        assert!(read_root_is_too_broad(Path::new("relative/python"), &[]));
        assert!(read_root_is_too_broad(&abs("/opt/../etc"), &[]));
    }

    #[test]
    fn a_folder_that_is_or_contains_a_user_dir_is_too_broad() {
        let users = vec![abs("/home/u/Documents"), abs("/home/u")];
        assert!(
            read_root_is_too_broad(&abs("/home/u"), &users),
            "the profile itself"
        );
        assert!(read_root_is_too_broad(&abs("/home/u/Documents"), &users));
        assert!(
            read_root_is_too_broad(&abs("/home"), &users),
            "an ancestor exposes the profile"
        );
        // Descendants are where per-user interpreters live.
        assert!(!read_root_is_too_broad(
            &abs("/home/u/tools/python312"),
            &users
        ));
        assert!(!read_root_is_too_broad(
            &abs("/home/u/Documents/venv"),
            &users
        ));
        // A sibling that merely shares a prefix is unrelated.
        assert!(!read_root_is_too_broad(&abs("/home/u2/python"), &users));
        // A `..` detour does not make a broad root look narrow.
        assert!(read_root_is_too_broad(&abs("/home/u/tools/../.."), &users));
    }

    #[test]
    fn a_secret_folder_or_a_folder_containing_one_is_too_broad() {
        let Some(home) = user_profile_root() else {
            return;
        };
        let no_user_dirs: Vec<PathBuf> = Vec::new();
        assert!(read_root_is_too_broad(&home.join(".ssh"), &no_user_dirs));
        assert!(read_root_is_too_broad(
            &home.join(".aws").join("sso"),
            &no_user_dirs
        ));
        // The profile contains every secret folder.
        assert!(read_root_is_too_broad(&home, &no_user_dirs));
    }

    #[test]
    fn interpreter_names_are_recognized_exactly() {
        for name in [
            "python.exe",
            "Python.EXE",
            "python3",
            "python3.12",
            "python3.12.exe",
            "node",
            "node.exe",
            "pythonw.exe",
            "Rscript.exe",
            "deno",
            "py.exe",
        ] {
            assert!(is_interpreter_name(name), "{name}");
        }
        for name in [
            "pythonista.exe",
            "python-config",
            "python3-config",
            "pythons",
            "nodemon",
            "node-gyp.cmd",
            "notepad.exe",
            "python.txt",
            "",
        ] {
            assert!(!is_interpreter_name(name), "{name}");
        }
    }

    #[test]
    fn an_interpreter_dir_holds_one_directly_or_in_bin_or_scripts() {
        let direct = scratch("direct");
        std::fs::write(direct.join("python3"), "x").unwrap();
        assert!(is_interpreter_dir(&direct));
        let bin = scratch("bin");
        std::fs::create_dir_all(bin.join("bin")).unwrap();
        std::fs::write(bin.join("bin").join("node"), "x").unwrap();
        assert!(is_interpreter_dir(&bin));
        let scripts = scratch("scripts");
        std::fs::create_dir_all(scripts.join("Scripts")).unwrap();
        std::fs::write(scripts.join("Scripts").join("python.exe"), "x").unwrap();
        assert!(is_interpreter_dir(&scripts));
        let neither = scratch("neither");
        std::fs::write(neither.join("notes.txt"), "x").unwrap();
        assert!(!is_interpreter_dir(&neither));
        // A directory named like an interpreter is not one.
        let imposter = scratch("imposter");
        std::fs::create_dir_all(imposter.join("python")).unwrap();
        assert!(!is_interpreter_dir(&imposter));
        assert!(!is_interpreter_dir(&imposter.join("missing")));
    }

    #[test]
    fn pinning_refuses_what_it_must_and_pins_nothing_on_a_partial_failure() {
        let sandbox = Sandbox::new(crate::ResourceAuthority::new(
            crate::ResourceProfile::Expanded,
        ));
        let good = scratch("good");
        std::fs::write(good.join("python3"), "x").unwrap();
        let not_interpreter = scratch("plain");
        std::fs::write(not_interpreter.join("a.txt"), "x").unwrap();
        let error = sandbox
            .pin_read_roots(&[good.clone(), not_interpreter])
            .expect_err("a non-interpreter root fails the whole call");
        assert!(matches!(error, SandboxError::InvalidPolicy(_)), "{error}");
        assert!(
            !pinned_read_roots().contains(&good),
            "the valid root of a failed call must not stay pinned"
        );
        let missing = good.join("does-not-exist");
        assert!(sandbox.pin_read_roots(&[missing]).is_err());
        let relative = PathBuf::from("python");
        assert!(sandbox.pin_read_roots(&[relative]).is_err());
        let root = if cfg!(windows) {
            PathBuf::from("C:\\")
        } else {
            PathBuf::from("/")
        };
        assert!(sandbox.pin_read_roots(&[root]).is_err());
        // A file is not a directory.
        assert!(sandbox.pin_read_roots(&[good.join("python3")]).is_err());
    }

    #[test]
    fn handles_count_users_per_root_and_release_with_the_last() {
        let sandbox = Sandbox::new(crate::ResourceAuthority::new(
            crate::ResourceProfile::Expanded,
        ));
        let root = scratch("count");
        std::fs::write(root.join("node"), "x").unwrap();
        let key = lexical_path_key(&std::fs::canonicalize(&root).unwrap());
        let users = |k: &str| {
            table()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entries
                .get(k)
                .map(|e| e.users)
        };
        let first = sandbox
            .pin_read_roots(std::slice::from_ref(&root))
            .expect("pin");
        let second = sandbox
            .pin_read_roots(&[root.clone(), root.clone()])
            .expect("pin again");
        assert_eq!(first.roots(), second.roots());
        assert_eq!(
            users(&key),
            Some(2),
            "a repeated root in one call counts once"
        );
        // Pinned roots are spelled without the Windows `\\?\` verbatim prefix.
        let canonical = crate::deny_read::strip_verbatim(std::fs::canonicalize(&root).unwrap());
        assert!(pinned_read_roots().contains(&canonical));
        drop(first);
        assert_eq!(users(&key), Some(1));
        drop(second);
        assert_eq!(users(&key), None, "gone with the last handle");
    }
}
