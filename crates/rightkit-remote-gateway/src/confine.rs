//! The approved-folder boundary of the remote file tools.
//!
//! Method, for a requested path:
//!
//! 1. The path must be absolute, have no `..` component and lie lexically under an approved folder,
//!    as the folder was written in Settings or as it canonicalizes. All three are decided before
//!    any disk access, so a path outside every folder never reveals whether it exists and a
//!    network path (`\\host\share`, `\\.\`, `\\?\UNC\`, `/net/…`, `/Network/…`) is never
//!    touched, which would open an SMB connection (leaking NTLM credentials) or trigger an
//!    automount. A folder on such a path works only when the user approved it.
//! 2. The deepest part of it that exists is canonicalized, which resolves every symbolic link,
//!    junction and (on Windows) reparse point in that part. That canonical path must lie inside a
//!    canonicalized approved folder **by path components** (`Path::starts_with`, so `/data/ab` is
//!    not inside `/data/a`). Outside is `outside_roots` whether or not the rest exists. Inside
//!    but missing is `not_found`.
//! 3. A credential store is refused as `sensitive_path`: a name below the folder (`.ssh`,
//!    `.gnupg`, `.aws`, `.config/gh`, `.npmrc`, ...), and the user's own `.claude`, `.codex` and
//!    The host app state folders. A folder that is itself one of these, or inside one, is never approved.
//! 4. A file or folder is opened without following a link (`O_NOFOLLOW` and `O_NONBLOCK` on unix,
//!    so a FIFO cannot hang the call; a normal open on Windows, where the handle check below does
//!    the work) and what was **actually opened** is checked: it is a regular file or a folder, as
//!    expected; on unix it has the device and inode the path had just before the open; and the
//!    operating system's own path for the open handle
//!    (`F_GETPATH` on macOS, `/proc/self/fd` elsewhere on unix, `GetFinalPathNameByHandleW` on
//!    Windows) canonicalizes to a place inside the same folder. A path component swapped for a
//!    link between steps 2 and 4 therefore opens something that fails step 4, and the file is
//!    closed unread. Where the open handle's path cannot be read the call is refused.
//!
//! Special files (devices, sockets, FIFOs) are `special_file` and are never opened. A folder
//! given to a file read is `is_directory`. `info`, `find` and a `find --root` folder are opened and
//! verified the same way as a read (`open_any`, `open_dir`); `find` reads each folder after that
//! and, on unix, checks the folder is still the one it opened.
//!
//! Known limits: a hard link inside a folder to a file whose other name is outside it is the same
//! file and is readable; a Finder alias (a regular file holding bookmark data) is not a link to
//! the OS and is read as the bytes it is; case-insensitive spellings of a folder compare as the
//! operating system's canonical spelling, and a spelling the platform does not canonicalize is
//! refused rather than guessed.

use super::settings::Settings;
use super::{Refusal, Result};
use std::fs::{File, Metadata};
use std::path::{Component, Path, PathBuf};

/// An approved folder, canonicalized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    /// Its position in the `remoteRoots` list as written.
    pub index: usize,
    /// The canonical path.
    pub path: PathBuf,
    /// The path as written in Settings, for the lexical check before any disk access.
    pub written: PathBuf,
}

/// A requested path that passed the boundary checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// Position in the `Root` list `approved_roots` returned. `roots[root].index` is the folder's
    /// place in the setting.
    pub root: usize,
    /// The canonical path.
    pub path: PathBuf,
    /// The canonical path below its folder (empty for the folder itself).
    pub relative: PathBuf,
}

/// Folder names that hold credentials, anywhere in a path. Compared without regard to letter
/// case.
const SENSITIVE_NAMES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".password-store",
    "keychains",
];

/// File names that hold credentials, anywhere in a path.
const SENSITIVE_FILES: &[&str] = &[
    ".netrc",
    ".git-credentials",
    ".pgpass",
    ".npmrc",
    ".pypirc",
    ".credentials.json",
];

/// Two names in a row that hold credentials: `gh` and `gcloud` keep tokens in `.config`.
const SENSITIVE_SEQUENCES: &[(&str, &str)] = &[(".config", "gh"), (".config", "gcloud")];

/// Whether any part of `path` names a credential store: a folder or file name from the lists
/// above, `.config/gh`, `.config/gcloud` or Cargo's `.cargo/credentials*`. Works on a relative
/// path, a single name or a canonical path. It looks at names only; `protected` covers the
/// folders that are credential stores by where they are.
pub fn sensitive(path: &Path) -> bool {
    let names: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    names.iter().any(|name| {
        SENSITIVE_NAMES.contains(&name.as_str()) || SENSITIVE_FILES.contains(&name.as_str())
    }) || names.windows(2).any(|pair| {
        SENSITIVE_SEQUENCES
            .iter()
            .any(|(a, b)| pair[0] == *a && pair[1] == *b)
            || (pair[0] == ".cargo" && pair[1].starts_with("credentials"))
    })
}

/// The folders that are credential stores or private records by where they are: the user's
/// `.claude` and `.codex` (their sign-in files and every chat transcript, which `chat_read` alone
/// may return, and only for a selected chat), `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and the host app's own
/// state folder (the chat store, settings and logs; on the Mac `Library/Application
/// Support/Orthic Labs/the host app`, on Windows `%LOCALAPPDATA%\Orthic Labs\the host app`). Each is listed as written and, when it
/// exists, as canonicalized.
fn protected_dirs() -> &'static [PathBuf] {
    use std::sync::OnceLock;
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let account = rightkit_app_host::account_home();
        dirs.push(account.join(".claude"));
        dirs.push(account.join(".codex"));
        // The folders named by the environment stay protected too (a profile's HOME included).
        for home in ["HOME", "USERPROFILE"] {
            if let Some(home) = std::env::var_os(home).filter(|h| !h.is_empty()) {
                let home = PathBuf::from(home);
                dirs.push(home.join(".claude"));
                dirs.push(home.join(".codex"));
            }
        }
        for var in ["CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
            if let Some(dir) = std::env::var_os(var).filter(|d| !d.is_empty()) {
                dirs.push(PathBuf::from(dir));
            }
        }
        if let Ok(state) = rightkit_app_host::state_directory() {
            dirs.push(state);
        }
        let canonical: Vec<PathBuf> = dirs
            .iter()
            .filter_map(|d| std::fs::canonicalize(d).ok())
            .collect();
        dirs.extend(canonical);
        dirs
    })
}

/// Whether `path` is one of the protected folders or inside one.
pub fn protected(path: &Path) -> bool {
    protected_dirs().iter().any(|dir| path.starts_with(dir))
}

/// The approved folders that can be used now. `remote_off` when the master switch is off;
/// `no_roots` when none is set or none of those set can be used (missing, not a folder, a
/// filesystem root such as `/` or `C:\`, or a credential folder or a folder inside one).
pub fn approved_roots(settings: &Settings) -> Result<Vec<Root>> {
    if !settings.enabled {
        return Err(Refusal::new(
            "remote_off",
            "remote tools are turned off in the app's settings (remoteTools)",
        ));
    }
    if settings.roots.is_empty() {
        return Err(Refusal::new(
            "no_roots",
            "no folder is approved; add one in the app's settings (remoteRoots)",
        ));
    }
    let mut roots = Vec::new();
    for (index, text) in settings.roots.iter().enumerate() {
        let path = Path::new(text);
        if !path.is_absolute() {
            continue;
        }
        let Ok(canonical) = std::fs::canonicalize(path) else {
            continue;
        };
        // A filesystem root has no parent; `/` and `C:\` are not approvable folders.
        if canonical.parent().is_none() || !canonical.is_dir() {
            continue;
        }
        // Nor is a credential folder, or anything inside one.
        if sensitive(&canonical) || protected(&canonical) {
            continue;
        }
        roots.push(Root {
            index,
            path: canonical,
            written: path.to_path_buf(),
        });
    }
    if roots.is_empty() {
        return Err(Refusal::new(
            "no_roots",
            "none of the approved folders can be used (each must be an existing folder that is not a filesystem root or a credential folder)",
        ));
    }
    Ok(roots)
}

/// Whether `path` is `root` or below it, by components.
pub fn inside(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

fn outside(why: &str) -> Refusal {
    Refusal::new(
        "outside_roots",
        format!("that path is not inside an approved folder ({why})"),
    )
}

/// Whether `path` is `root` or below it, compared name by name as written, with no disk access.
/// Windows compares letter case loosely, because the check only decides whether to look at all;
/// the boundary itself is the canonical comparison.
fn lexically_under(root: &Path, path: &Path) -> bool {
    fn same(a: Component, b: Component) -> bool {
        if cfg!(windows) {
            a.as_os_str().to_string_lossy().to_lowercase()
                == b.as_os_str().to_string_lossy().to_lowercase()
        } else {
            a == b
        }
    }
    let mut mine = path.components();
    root.components()
        .all(|r| mine.next().is_some_and(|m| same(r, m)))
}

/// Steps 1 to 3 of the module's method.
pub fn resolve(roots: &[Root], requested: &str) -> Result<Resolved> {
    if requested.is_empty() || requested.contains('\0') {
        return Err(Refusal::new("invalid_request", "a path is needed"));
    }
    let path = Path::new(requested);
    if !path.is_absolute() {
        return Err(outside("give an absolute path"));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(outside("it uses .."));
    }
    // Before any disk access: the path as written must be under a folder as written or as it
    // canonicalizes. Anything else (a network path, an automount, another alias) is never
    // canonicalized.
    if !roots
        .iter()
        .any(|r| lexically_under(&r.written, path) || lexically_under(&r.path, path))
    {
        return Err(outside(
            "it is not under an approved folder as written; network paths are never opened",
        ));
    }
    let (anchor, missing) = match canonical_prefix(path) {
        Prefix::Found(anchor, missing) => (anchor, missing),
        // Under an approved folder as written, but a link loop or a permission stops the resolve.
        Prefix::Unreadable => {
            return Err(Refusal::new(
                "read_failed",
                "that path could not be resolved (permission denied or a link loop)",
            ));
        }
        Prefix::None => return Err(outside("it does not resolve")),
    };
    let Some(root) = roots
        .iter()
        .enumerate()
        .find(|(_, r)| inside(&r.path, &anchor))
        .map(|(position, _)| position)
    else {
        return Err(outside("after links are resolved"));
    };
    let relative = anchor
        .strip_prefix(&roots[root].path)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    if sensitive(&relative) || protected(&anchor) {
        return Err(Refusal::new(
            "sensitive_path",
            "that path is a credential or private folder (.ssh, .gnupg, .aws, .claude, .codex, the app's own folder, ...) that remote tools never read",
        ));
    }
    if missing > 0 {
        return Err(Refusal::new("not_found", "no such file or folder"));
    }
    Ok(Resolved {
        root,
        path: anchor,
        relative,
    })
}

enum Prefix {
    /// The canonical form of the deepest existing part, and how many names were cut to reach it.
    Found(PathBuf, usize),
    /// Resolving failed for a reason other than "not there".
    Unreadable,
    /// Not even the top exists.
    None,
}

fn canonical_prefix(path: &Path) -> Prefix {
    let mut current = path.to_path_buf();
    let mut missing = 0usize;
    loop {
        match std::fs::canonicalize(&current) {
            Ok(canonical) => return Prefix::Found(canonical, missing),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                if !current.pop() {
                    return Prefix::None;
                }
                missing += 1;
            }
            Err(_) => return Prefix::Unreadable,
        }
    }
}

fn special() -> Refusal {
    Refusal::new(
        "special_file",
        "that is a device, socket or pipe, which remote tools never open",
    )
}

fn io_refusal(error: &std::io::Error, what: &str) -> Refusal {
    match error.kind() {
        std::io::ErrorKind::NotFound => Refusal::new("not_found", "no such file or folder"),
        _ => Refusal::new("read_failed", format!("{what}: {}", error.kind())),
    }
}

/// A symbolic link, or on Windows a junction too (std counts every name-surrogate reparse point
/// as a link; a cloud-sync placeholder is not one).
fn is_link(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// What `open_checked` may open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    /// A regular file; a folder is `is_directory`.
    File,
    /// A regular file or a folder.
    Any,
}

/// Step 4: opens the regular file at `resolved` for reading and verifies what was opened.
/// Returns the open file and its metadata. A folder is `is_directory`.
pub fn open_regular(roots: &[Root], resolved: &Resolved) -> Result<(File, Metadata)> {
    open_checked(&roots[resolved.root], &resolved.path, Want::File)
}

/// Step 4 for a file or a folder (`info`, `find --root`): the handle and its metadata, which the
/// caller reports from instead of reading the path again.
pub fn open_any(roots: &[Root], resolved: &Resolved) -> Result<(File, Metadata)> {
    open_checked(&roots[resolved.root], &resolved.path, Want::Any)
}

/// Step 4 for a folder `find` is about to read, queued by path: opened without following a link
/// and verified. Keep the handle until the folder has been read, then call `still_here`.
pub fn open_dir(root: &Root, path: &Path) -> Result<File> {
    let (file, meta) = open_checked(root, path, Want::Any)?;
    if !meta.is_dir() {
        return Err(outside("it stopped being a folder"));
    }
    Ok(file)
}

/// Whether `path` still names the object `handle` was opened on: the same device and inode on
/// unix, so a folder swapped for another between the check and its read is noticed. Windows has
/// no cheap equivalent here and answers true: the handle's final path was already verified.
pub fn still_here(handle: &File, path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (handle.metadata(), std::fs::symlink_metadata(path)) {
            (Ok(held), Ok(now)) => (held.dev(), held.ino()) == (now.dev(), now.ino()),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (handle, path);
        true
    }
}

fn open_checked(root: &Root, path: &Path, want: Want) -> Result<(File, Metadata)> {
    let before = std::fs::symlink_metadata(path)
        .map_err(|e| io_refusal(&e, "the file or folder could not be opened"))?;
    if is_link(&before) {
        return Err(outside("it became a link"));
    }
    let folder = before.is_dir();
    if folder && want == Want::File {
        return Err(Refusal::new(
            "is_directory",
            "that is a folder; read a file in it",
        ));
    }
    if !folder && !before.is_file() {
        return Err(special());
    }
    let opened = if folder {
        open_dir_no_follow(path)
    } else {
        open_no_follow(path)
    };
    let file = opened.map_err(|e| io_refusal(&e, "the file or folder could not be opened"))?;
    let meta = file
        .metadata()
        .map_err(|e| io_refusal(&e, "the file or folder could not be read"))?;
    if meta.is_dir() != folder || (!folder && !meta.is_file()) {
        return Err(outside("it changed while it was opened"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (meta.dev(), meta.ino()) != (before.dev(), before.ino()) {
            return Err(outside("it changed while it was opened"));
        }
    }
    // What was actually opened must be inside the folder, whatever the path said.
    let Some(opened) = handle_path(&file) else {
        return Err(Refusal::new(
            "outside_roots",
            "the opened file's location could not be verified, so it was not read",
        ));
    };
    match std::fs::canonicalize(&opened) {
        Ok(real) if inside(&root.path, &real) => Ok((file, meta)),
        _ => Err(outside("what was opened is somewhere else")),
    }
}

#[cfg(unix)]
fn open_dir_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(path)
}

/// A folder handle on Windows needs backup semantics; a link swapped in since is caught by the
/// handle's final path.
#[cfg(windows)]
fn open_dir_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_dir_no_follow(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

/// Windows opens normally: the path is already link-free and was just checked, a link swapped in
/// since is caught by the open handle's final path below, and opening a cloud-sync placeholder
/// as a reparse point would stop it from loading.
#[cfg(windows)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new().read(true).open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_no_follow(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

/// The operating system's path for an open file.
#[cfg(target_os = "macos")]
fn handle_path(file: &File) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    let mut buffer = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most PATH_MAX bytes into `buffer`.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) };
    if rc == -1 {
        return None;
    }
    let length = buffer.iter().position(|b| *b == 0)?;
    buffer.truncate(length);
    Some(PathBuf::from(OsString::from_vec(buffer)))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn handle_path(file: &File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFinalPathNameByHandleW(
        file: *mut std::ffi::c_void,
        path: *mut u16,
        length: u32,
        flags: u32,
    ) -> u32;
}

#[cfg(windows)]
fn handle_path(file: &File) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: the handle is live for the call and the buffer holds `length` UTF-16 units; flags 0
    // is FILE_NAME_NORMALIZED with the DOS volume name.
    let written = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle().cast(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    if written == 0 || written >= buffer.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&buffer[..written])))
}

#[cfg(not(any(unix, windows)))]
fn handle_path(_file: &File) -> Option<PathBuf> {
    None
}

/// A path as a person reads it: Windows' `\\?\C:\x` as `C:\x`, `\\?\UNC\h\s` as `\\h\s`.
pub fn display(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    text.into_owned()
}
