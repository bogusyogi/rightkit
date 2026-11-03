//! Path containment, one module: is a path inside a root, resolve a relative path under a root, and
//! open a file under a root without following links.
//!
//! - [`is_within`]: is an absolute path the root or below it? (rightkit#452)
//! - [`contain`] / [`contain_lenient`]: resolve a caller-supplied relative path under a root and refuse
//!   any escape. (rightkit#373)
//! - [`Roots`] and `open_confined` (feature `fs`): a set of approved folders, a resolver that follows
//!   links but refuses to leave them, and a no-follow open with a handle-path recheck. (rightkit#400,
//!   with the hard-link and sharing options of rightkit#613)
//!
//! All of them resolve through one function, so they agree on the edge cases: symlinks are followed with
//! [`canonicalize_plain`] (a Windows verbatim `\\?\C:` root and a plain `C:` root are equal, and
//! `\\?\UNC\` is folded to `\\`); a path that does not exist yet is resolved through its deepest existing
//! ancestor with the missing tail appended; an existing entry that cannot be resolved (a dangling or
//! looping link) is an error and never counts as absent, so a dangling link cannot be used to create a
//! file outside the root; a `..` in the missing part is refused. Containment compares whole components,
//! never a string prefix (`/data/app2` is not within `/data/app`), compares non-UTF-8 names byte for
//! byte (never through a lossy conversion that could make two different names equal), and ignores case
//! on Windows and macOS only when the filesystem agrees: a path that differs from the root by case
//! alone is inside only if the root and the same-length prefix of the path are the same directory
//! (device and file index), so a case-sensitive APFS volume or a per-directory case flag cannot grant
//! access to a sibling that differs by case. Without feature `fs` a case-only difference is refused. Unicode normalization differences (a macOS name
//! stored in NFD and requested in NFC) compare unequal, which errs toward refusing.
//!
//! # What is refused before the disk is touched
//!
//! A relative path where an absolute one is needed, an absolute or rooted path where a relative one is
//! needed, any `..` component, a NUL byte, and (on Windows) a drive-relative or UNC prefix in a relative
//! path, a reserved device name (`CON`, `NUL`, `COM1`, ...: opening `root\NUL` opens the device, not a
//! file below the root), an alternate-data-stream name (`a:stream`), a name with a Windows-illegal
//! character, and a trailing dot or space (Windows drops it, so the name would address another entry).
//!
//! # What is and is not defended
//!
//! [`is_within`], [`contain`], [`contain_lenient`] and [`Roots::resolve_confined`] are a check at one
//! moment: another process can swap a directory for a symlink between the check and a later open.
//! `open_confined` and `Roots::open_confined` close that race. On Unix they open the root, then each
//! component with `openat(O_NOFOLLOW)`, so no link can be followed at any step; a [`Roots`] set also
//! compares the opened root's device and inode with the ones recorded when the root was added, so a root
//! swapped after approval is refused. On Windows each directory is checked with `symlink_metadata`, the
//! leaf is opened with `FILE_FLAG_OPEN_REPARSE_POINT` (the link itself, never its target). Every
//! directory from the root down is first opened as a held handle without `FILE_SHARE_DELETE`, so it
//! cannot be renamed or swapped for a junction until the leaf is open, and each handle's final path
//! must be below the root before anything is created. The leaf is opened without truncation, its
//! handle path and link count are checked, and only then is it truncated; on Unix the same order
//! applies (`O_TRUNC` is never passed), so a refused open never destroys data. A hard link to a file outside the root is indistinguishable
//! from a file inside it unless `ConfinedOptions::require_single_link` is set. Use `crate::StateDir`
//! where the directory itself must be pinned for writes.

use crate::plain::{canonicalize_plain, plain_path};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Why a path was refused or could not be resolved.
#[derive(Debug)]
#[non_exhaustive]
pub enum ContainError {
    /// `rel` is empty, absolute, has a root or drive prefix, has a `..` component or a NUL byte, or (on
    /// Windows) a name Windows cannot use as a plain component; or a path ends in `..` or has a `..` in
    /// its not-yet-existing part.
    InvalidRelative,
    /// An absolute path was required (a [`Roots`] method, or a root given to [`Roots::add`]).
    NotAbsolute,
    /// The resolved path is not inside the resolved root (a symlink or junction leads out, the path
    /// is outside every approved root, or an approved root was replaced after it was approved).
    Escapes,
    /// A no-follow open met a symbolic link or reparse point (anywhere, even one that points inside).
    Symlink,
    /// A confined open found something that is not a regular file (a directory, a FIFO, a device).
    NotAFile,
    /// A confined open with `ConfinedOptions::require_single_link` found a file with more than one
    /// hard link.
    HardLinked,
    /// The root, the path, or an entry on the way could not be resolved. This includes a symlink that
    /// dangles or loops: its target is unknown, so it is never trusted.
    Io(io::Error),
}

impl fmt::Display for ContainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRelative => f.write_str("the path is not a plain relative path"),
            Self::NotAbsolute => f.write_str("the path is not absolute"),
            Self::Escapes => f.write_str("the path resolves outside the root"),
            Self::Symlink => f.write_str("the path passes through a symbolic link"),
            Self::NotAFile => f.write_str("the path is not a regular file"),
            Self::HardLinked => f.write_str("the file has more than one hard link"),
            Self::Io(e) => write!(f, "the path cannot be resolved: {e}"),
        }
    }
}

impl std::error::Error for ContainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ContainError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// An `io::Error` of the kind a `std::io` caller expects: `InvalidInput` for a malformed path or a
/// link, `PermissionDenied` for an escape or a hard link; an [`ContainError::Io`] keeps its own error.
impl From<ContainError> for io::Error {
    fn from(e: ContainError) -> Self {
        if let ContainError::Io(inner) = e {
            return inner;
        }
        let kind = match &e {
            ContainError::InvalidRelative
            | ContainError::NotAbsolute
            | ContainError::Symlink
            | ContainError::NotAFile => io::ErrorKind::InvalidInput,
            ContainError::Escapes | ContainError::HardLinked => io::ErrorKind::PermissionDenied,
            ContainError::Io(_) => io::ErrorKind::Other,
        };
        io::Error::new(kind, e)
    }
}

// ---------------------------------------------------------------------------------------------
// Resolution and comparison (shared by everything below)
// ---------------------------------------------------------------------------------------------

/// True when a name that does not exist yet could not be created as a plain component here: on
/// Windows a reserved device name, a stream name, an illegal character, a trailing dot or space, or a
/// name that is not valid Unicode. Always false elsewhere.
fn unusable_new_name(name: &OsStr) -> bool {
    cfg!(windows)
        && name
            .to_str()
            .is_none_or(crate::names::is_unsafe_windows_component)
}

/// `InvalidRelative` when a `..` follows a component that does not exist. Decided from the
/// components of the original path, before the OS collapses `..` lexically (Windows does, so
/// `missing\..` would otherwise resolve to the parent and look contained).
fn refuse_dot_dot_in_missing_part(path: &Path) -> Result<(), ContainError> {
    let mut cur = PathBuf::new();
    let mut exists = true;
    for component in path.components() {
        match component {
            Component::ParentDir if !exists => return Err(ContainError::InvalidRelative),
            Component::ParentDir => cur.push(".."),
            other => {
                cur.push(other.as_os_str());
                if exists && matches!(other, Component::Normal(_)) {
                    exists = std::fs::symlink_metadata(&cur).is_ok();
                }
            }
        }
    }
    Ok(())
}

/// The real path of the deepest existing ancestor with the not-yet-existing tail appended.
///
/// `InvalidRelative` when the missing tail holds (or the path ends in) `..`, or (Windows) a name that
/// cannot be a plain component; `Io` when an existing entry cannot be resolved (a dangling or looping
/// link, or a file used as a directory).
fn resolve_lenient(path: &Path) -> Result<PathBuf, ContainError> {
    refuse_dot_dot_in_missing_part(path)?;
    let mut tail: Vec<OsString> = Vec::new();
    let mut head = path.to_path_buf();
    loop {
        let e = match canonicalize_plain(&head) {
            Ok(mut real) => {
                // Names below `real` are only a tail if `real` can hold them. Windows reports a
                // file used as a directory as "not found", so the failed resolve above looked like
                // an absent tail; check the existing prefix is a directory.
                if !tail.is_empty() && !real.is_dir() {
                    return Err(ContainError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a path component is not a directory",
                    )));
                }
                real.extend(tail.iter().rev());
                return Ok(plain_path(real));
            }
            Err(e) => e,
        };
        // NotFound from canonicalize also describes a dangling link; only a name that is really
        // absent may be treated as the not-yet-created tail.
        match std::fs::symlink_metadata(&head) {
            Ok(_) => return Err(ContainError::Io(e)),
            Err(m) if m.kind() == io::ErrorKind::NotFound => {}
            Err(m) => return Err(ContainError::Io(m)),
        }
        // `file_name` is `None` for a trailing `..` and for a root, so neither is skipped over.
        match head.file_name() {
            Some(name) if unusable_new_name(name) => return Err(ContainError::InvalidRelative),
            Some(name) => tail.push(name.to_os_string()),
            None if head.components().next_back() == Some(Component::ParentDir) => {
                return Err(ContainError::InvalidRelative)
            }
            None => return Err(ContainError::Io(e)),
        }
        if !head.pop() {
            return Err(ContainError::Io(e));
        }
    }
}

/// True when `path` resolves to `root` or below, with every existing ancestor of both resolved
/// through links (the tail that does not exist yet is appended). Unlike [`is_within`] a path that
/// cannot be resolved is an error, never "not within", so a caller refusing a descendant fails closed.
#[cfg(feature = "fs")]
pub(crate) fn resolved_within(root: &Path, path: &Path) -> Result<bool, ContainError> {
    let root = resolve_lenient(root)?;
    let path = resolve_lenient(path)?;
    Ok(is_below(&root, &path))
}

/// `\\?\UNC\server\share\x` and `\\server\share\x` name the same place; fold the first to the second.
fn fold_unc(text: &str) -> String {
    match text.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => text.to_string(),
    }
}

#[cfg(any(windows, target_vendor = "apple"))]
const CASE_INSENSITIVE: bool = true;
#[cfg(not(any(windows, target_vendor = "apple")))]
const CASE_INSENSITIVE: bool = false;

/// Components as comparable names. A name that is valid Unicode is lowercased when `fold_case` is set;
/// a name that is not is kept exactly, so two different byte strings never compare equal through a
/// lossy conversion.
fn comparable(path: &Path, fold_case: bool) -> Vec<OsString> {
    let folded: PathBuf = match path.to_str() {
        Some(text) => PathBuf::from(fold_unc(text)),
        None => path.to_path_buf(),
    };
    folded
        .components()
        .map(|c| {
            let name = c.as_os_str();
            match name.to_str() {
                Some(text) if fold_case => OsString::from(text.to_lowercase()),
                _ => name.to_os_string(),
            }
        })
        .collect()
}

/// Whole-component prefix test on resolved paths, with the case rule given.
fn is_below_with(root: &Path, path: &Path, fold_case: bool) -> bool {
    let (root, path) = (comparable(root, fold_case), comparable(path, fold_case));
    path.len() >= root.len() && path[..root.len()] == root[..]
}

#[cfg(feature = "fs")]
/// `path` with a verbatim UNC prefix folded to the plain UNC form, as the comparison sees it.
fn normalized(path: &Path) -> PathBuf {
    match path.to_str() {
        Some(text) => PathBuf::from(fold_unc(text)),
        None => path.to_path_buf(),
    }
}

/// Whole-component prefix test on resolved paths, decided per filesystem.
///
/// An exact (case-sensitive) component match is always "below". Where the platform's default volumes
/// are case-insensitive (Windows, Apple) a match that holds only after case folding is a candidate,
/// never an answer: it counts only when the root and the same-length prefix of `path` are the very
/// same directory (device and file index), which a case-sensitive volume or a per-directory case flag
/// refuses (`/safe/Root` and `/safe/root` are two directories there). Without feature `fs` there is no
/// identity check, so a case-only difference is refused.
fn is_below(root: &Path, path: &Path) -> bool {
    if is_below_with(root, path, false) {
        return true;
    }
    if !CASE_INSENSITIVE || !is_below_with(root, path, true) {
        return false;
    }
    same_directory_prefix(root, path)
}

#[cfg(feature = "fs")]
fn same_directory_prefix(root: &Path, path: &Path) -> bool {
    let count = normalized(root).components().count();
    let prefix: PathBuf = normalized(path).components().take(count).collect();
    match (
        crate::file_key::FileKey::of_path(root),
        crate::file_key::FileKey::of_path(&prefix),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(not(feature = "fs"))]
fn same_directory_prefix(_root: &Path, _path: &Path) -> bool {
    false
}

fn inside(root: &Path, resolved: PathBuf) -> Result<PathBuf, ContainError> {
    if is_below(root, &resolved) {
        Ok(resolved)
    } else {
        Err(ContainError::Escapes)
    }
}

/// `rel` must be a non-empty relative path of plain names; returns those names.
fn plain_components(rel: &Path) -> Result<Vec<&OsStr>, ContainError> {
    if rel.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(ContainError::InvalidRelative);
    }
    let mut names = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => {
                if unusable_new_name(name) {
                    return Err(ContainError::InvalidRelative);
                }
                names.push(name);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ContainError::InvalidRelative)
            }
        }
    }
    if names.is_empty() {
        return Err(ContainError::InvalidRelative);
    }
    Ok(names)
}

// ---------------------------------------------------------------------------------------------
// is_within, contain, contain_lenient
// ---------------------------------------------------------------------------------------------

/// True when `path` is `root` or lies below it. Both must be absolute, otherwise the answer is
/// `false`.
///
/// Refusals, all `false`: a relative `root` or `path`; a `..` component in the part of either path
/// that does not exist (it cannot be resolved without guessing); an entry that exists but cannot be
/// resolved (a dangling or looping symlink), so a dangling link cannot smuggle a path outside; a NUL
/// byte; on Windows a not-yet-existing name that is a reserved device name or otherwise unusable.
///
/// ```
/// let dir = std::env::temp_dir();
/// assert!(rightkit_fs::is_within(&dir, &dir.join("backups").join("a.db")));
/// assert!(!rightkit_fs::is_within(&dir.join("backups"), &dir.join("backups2")));
/// ```
pub fn is_within(root: &Path, path: &Path) -> bool {
    if !root.is_absolute() || !path.is_absolute() {
        return false;
    }
    match (resolve_lenient(root), resolve_lenient(path)) {
        (Ok(root), Ok(path)) => is_below(&root, &path),
        _ => false,
    }
}

/// Resolve `root/rel` through symlinks and return its canonical path, which must exist and lie inside
/// `root`. `root` must exist. `rel` must be non-empty plain names (`.` is ignored; `..`, a root, a
/// drive or UNC prefix and a NUL byte are refused). Errors: [`ContainError::InvalidRelative`],
/// [`ContainError::Escapes`], [`ContainError::Io`] (including a missing path).
pub fn contain(root: &Path, rel: impl AsRef<Path>) -> Result<PathBuf, ContainError> {
    let names = plain_components(rel.as_ref())?;
    let root = canonicalize_plain(root).map_err(ContainError::Io)?;
    let mut joined = root.clone();
    joined.extend(names);
    let full = canonicalize_plain(&joined).map_err(ContainError::Io)?;
    inside(&root, full)
}

/// As [`contain`], but the tail of the path may not exist yet. The deepest existing ancestor of
/// `root/rel` is resolved through symlinks and must lie inside `root`; the names below it (which do not
/// exist, so cannot be links) are appended unchanged. An existing entry that cannot be resolved, such as
/// a dangling symlink, is an error, never treated as absent.
pub fn contain_lenient(root: &Path, rel: impl AsRef<Path>) -> Result<PathBuf, ContainError> {
    let names = plain_components(rel.as_ref())?;
    let root = canonicalize_plain(root).map_err(ContainError::Io)?;
    let mut head = root.clone();
    head.extend(names);
    inside(&root, resolve_lenient(&head)?)
}

// ---------------------------------------------------------------------------------------------
// Roots: a set of approved folders (rightkit#400)
// ---------------------------------------------------------------------------------------------

/// A set of approved folders. Every path given to its methods must be absolute and must end up inside
/// one of them, or it is refused. Roots are resolved when added, so a root given through a symlink or
/// with a different case compares correctly.
///
/// [`Roots::resolve_confined`] follows links and refuses to leave the roots. With feature `fs`,
/// `Roots::open_confined` additionally never follows a link at open time.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    roots: Vec<PathBuf>,
    /// Device and inode of each root when it was added, parallel to `roots` (Unix opens compare it).
    #[cfg(feature = "fs")]
    keys: Vec<Option<crate::file_key::FileKey>>,
}

impl Roots {
    /// No approved folder yet; every path is refused until one is added.
    pub fn new() -> Self {
        Self::default()
    }

    /// A set built from `roots`. Each must be an absolute path to an existing directory.
    pub fn with_roots<I, P>(roots: I) -> Result<Self, ContainError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut set = Self::new();
        for root in roots {
            set.add(root)?;
        }
        Ok(set)
    }

    /// Approve one more folder. It must be absolute and an existing directory; it is stored resolved.
    pub fn add(&mut self, root: impl AsRef<Path>) -> Result<(), ContainError> {
        let root = root.as_ref();
        if !root.is_absolute() {
            return Err(ContainError::NotAbsolute);
        }
        let real = canonicalize_plain(root)?;
        if !std::fs::metadata(&real)?.is_dir() {
            return Err(ContainError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "an approved root must be a directory",
            )));
        }
        if !self.roots.contains(&real) {
            #[cfg(feature = "fs")]
            self.keys
                .push(crate::file_key::FileKey::of_path(&real).ok());
            self.roots.push(real);
        }
        Ok(())
    }

    /// The approved folders, resolved.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// True when no folder is approved.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// [`is_within`] against any approved folder.
    pub fn contains(&self, path: &Path) -> bool {
        path.is_absolute() && self.resolve_confined_lenient(path).is_ok()
    }

    /// The resolved path of `path`, which must exist, following links, and lie inside an approved
    /// folder. Errors: [`ContainError::NotAbsolute`], [`ContainError::Escapes`], [`ContainError::Io`].
    /// A check at one moment; use `Roots::open_confined` (feature `fs`) to read or write.
    pub fn resolve_confined(&self, path: &Path) -> Result<PathBuf, ContainError> {
        if !path.is_absolute() {
            return Err(ContainError::NotAbsolute);
        }
        self.within_any(canonicalize_plain(path)?)
    }

    /// As [`Roots::resolve_confined`], but the tail of `path` may not exist yet (a file about to be
    /// created). The deepest existing ancestor is resolved and must be inside an approved folder.
    pub fn resolve_confined_lenient(&self, path: &Path) -> Result<PathBuf, ContainError> {
        if !path.is_absolute() {
            return Err(ContainError::NotAbsolute);
        }
        self.within_any(resolve_lenient(path)?)
    }

    fn within_any(&self, resolved: PathBuf) -> Result<PathBuf, ContainError> {
        match self.roots.iter().find(|root| is_below(root, &resolved)) {
            Some(_) => Ok(resolved),
            None => Err(ContainError::Escapes),
        }
    }

    /// The index of the approved root holding `path` and the plain names from that root to it. Links
    /// in the parent directories are followed (and must resolve inside the root); the final name is
    /// never resolved, so a link there is met by the open and refused.
    #[cfg(feature = "fs")]
    fn locate(&self, path: &Path) -> Result<(usize, Vec<OsString>), ContainError> {
        if !path.is_absolute() {
            return Err(ContainError::NotAbsolute);
        }
        if path.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(ContainError::InvalidRelative);
        }
        let name = match path.components().next_back() {
            Some(Component::Normal(name)) => name.to_os_string(),
            _ => return Err(ContainError::InvalidRelative),
        };
        if unusable_new_name(&name) {
            return Err(ContainError::InvalidRelative);
        }
        let parent = path.parent().ok_or(ContainError::InvalidRelative)?;
        let parent = resolve_lenient(parent)?;
        let index = self
            .roots
            .iter()
            .position(|root| is_below(root, &parent))
            .ok_or(ContainError::Escapes)?;
        let skip = self.roots[index].components().count();
        let mut names: Vec<OsString> = parent
            .components()
            .skip(skip)
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        names.push(name);
        Ok((index, names))
    }

    /// Open `path` inside an approved folder without following a link at open time. See
    /// [`open_confined`] for the guarantee. Errors: [`ContainError::NotAbsolute`],
    /// [`ContainError::InvalidRelative`] (the path ends in `..`, holds a NUL byte, or has a name
    /// Windows cannot use), [`ContainError::Escapes`] (also: the approved root was replaced after it
    /// was approved, Unix), [`ContainError::Symlink`], [`ContainError::NotAFile`],
    /// [`ContainError::HardLinked`], [`ContainError::Io`].
    #[cfg(feature = "fs")]
    pub fn open_confined(
        &self,
        path: &Path,
        options: &ConfinedOptions,
    ) -> Result<std::fs::File, ContainError> {
        let (index, names) = self.locate(path)?;
        open_strict(&self.roots[index], &names, options, self.keys[index])
    }
}

// ---------------------------------------------------------------------------------------------
// No-follow open (feature `fs`)
// ---------------------------------------------------------------------------------------------

/// How [`open_confined`] opens the file: the std `OpenOptions` flags that matter, plus the checks of
/// [`crate::handle::ReadOptions`], as a plain value. With no access mode set, the open is refused as
/// `InvalidInput`. A new file is created owner-only (`0600`) on Unix, with default access on Windows.
#[cfg(feature = "fs")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConfinedOptions {
    read: bool,
    write: bool,
    append: bool,
    create: bool,
    create_new: bool,
    truncate: bool,
    require_single_link: bool,
    share_read_only: bool,
}

#[cfg(feature = "fs")]
impl ConfinedOptions {
    /// Read-only.
    pub fn read_only() -> Self {
        Self::default().read(true)
    }
    /// Open for reading.
    pub fn read(mut self, yes: bool) -> Self {
        self.read = yes;
        self
    }
    /// Open for writing.
    pub fn write(mut self, yes: bool) -> Self {
        self.write = yes;
        self
    }
    /// Open for appending.
    pub fn append(mut self, yes: bool) -> Self {
        self.append = yes;
        self
    }
    /// Create the file when missing (needs `write` or `append`).
    pub fn create(mut self, yes: bool) -> Self {
        self.create = yes;
        self
    }
    /// Create the file and fail when it exists (needs `write` or `append`).
    pub fn create_new(mut self, yes: bool) -> Self {
        self.create_new = yes;
        self
    }
    /// Truncate to empty on open (needs `write`).
    pub fn truncate(mut self, yes: bool) -> Self {
        self.truncate = yes;
        self
    }
    /// Refuse a file with more than one hard link ([`ContainError::HardLinked`]), checked on the
    /// opened handle. For a read of a file the folder's owner did not make, where a hard link could
    /// name a file outside the folder.
    pub fn require_single_link(mut self, yes: bool) -> Self {
        self.require_single_link = yes;
        self
    }
    /// Windows: open with `FILE_SHARE_READ` only (a read-only open; see
    /// [`crate::handle::ReadOptions::share_read_only`]). Ignored on Unix.
    pub fn share_read_only(mut self, yes: bool) -> Self {
        self.share_read_only = yes;
        self
    }

    fn check(&self) -> Result<(), ContainError> {
        let bad = |why: &'static str| {
            Err(ContainError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                why,
            )))
        };
        if !(self.read || self.write || self.append) {
            return bad("no access mode set");
        }
        if (self.create || self.create_new) && !(self.write || self.append) {
            return bad("create needs write or append");
        }
        if self.truncate && !self.write {
            return bad("truncate needs write");
        }
        if self.share_read_only && (self.write || self.append) {
            return bad("share_read_only is for a read-only open");
        }
        Ok(())
    }
}

/// Open `root/rel` without following a link, and return the file. `root` must exist; `rel` must be
/// non-empty plain names (as for [`contain`]). A symbolic link or reparse point at any step, including
/// one that points inside the root, is [`ContainError::Symlink`]. Something that is not a regular file
/// is [`ContainError::NotAFile`] (a FIFO is never waited on). Other errors: `InvalidRelative`,
/// `Escapes` (Windows handle-path recheck), `HardLinked`, `Io` (including a missing parent, and a
/// missing file unless `create` is set).
///
/// Unlike [`contain_lenient`] followed by an open, nothing can be swapped for a link between the check
/// and the open (see the module docs for the per-platform mechanism).
#[cfg(feature = "fs")]
pub fn open_confined(
    root: &Path,
    rel: impl AsRef<Path>,
    options: &ConfinedOptions,
) -> Result<std::fs::File, ContainError> {
    let names: Vec<OsString> = plain_components(rel.as_ref())?
        .into_iter()
        .map(OsStr::to_os_string)
        .collect();
    let root = canonicalize_plain(root)?;
    // No recorded identity for a root given per call; the Unix open still refuses a root that is a
    // link and the walk below it never follows one.
    open_strict(&root, &names, options, None)
}

#[cfg(all(feature = "fs", unix))]
fn open_strict(
    root: &Path,
    names: &[OsString],
    options: &ConfinedOptions,
    expected_root: Option<crate::file_key::FileKey>,
) -> Result<std::fs::File, ContainError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    options.check()?;
    let cstr = |bytes: &[u8]| {
        CString::new(bytes).map_err(|_| {
            ContainError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains a NUL byte",
            ))
        })
    };
    let (last, parents) = names.split_last().ok_or(ContainError::InvalidRelative)?;

    let root_c = cstr(root.as_os_str().as_bytes())?;
    // O_NOFOLLOW: the resolved root has no link in it, so a link here means it was swapped since.
    // SAFETY: `root_c` is a valid NUL-terminated string.
    let fd = unsafe {
        libc::open(
            root_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        return Err(if e.raw_os_error() == Some(libc::ELOOP) {
            ContainError::Symlink
        } else {
            ContainError::Io(e)
        });
    }
    // SAFETY: `fd` is a fresh descriptor this function owns.
    let mut dir = unsafe { OwnedFd::from_raw_fd(fd) };
    if let Some(expected) = expected_root {
        // SAFETY: zeroed `stat` is a valid out-parameter and `dir` is an open descriptor.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(dir.as_raw_fd(), &mut st) } != 0 {
            return Err(ContainError::Io(io::Error::last_os_error()));
        }
        if st.st_dev as u64 != expected.volume || st.st_ino as u64 != expected.index {
            return Err(ContainError::Escapes);
        }
    }

    // Why a component failed: a link is reported as `Symlink`, anything else as the OS error.
    let refuse = |dir: &OwnedFd, name: &std::ffi::CStr, err: io::Error| -> ContainError {
        // SAFETY: zeroed `stat` is a valid out-parameter; `dir` and `name` are valid.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
            ContainError::Symlink
        } else {
            ContainError::Io(err)
        }
    };

    for name in parents {
        let c = cstr(name.as_bytes())?;
        // SAFETY: `dir` is a live directory descriptor and `c` a valid string.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(refuse(&dir, c.as_c_str(), io::Error::last_os_error()));
        }
        // SAFETY: fresh descriptor owned here; the previous `dir` is dropped (closed) on assignment.
        dir = unsafe { OwnedFd::from_raw_fd(fd) };
    }

    let c = cstr(last.as_bytes())?;
    let mut flags = match (options.read, options.write || options.append) {
        (true, false) => libc::O_RDONLY,
        (false, true) => libc::O_WRONLY,
        _ => libc::O_RDWR,
    };
    // O_NONBLOCK: a FIFO or device planted at the name must not hang the open. It is cleared again
    // below once the handle is known to be a regular file.
    flags |= libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    if options.append {
        flags |= libc::O_APPEND;
    }
    if options.create_new {
        flags |= libc::O_CREAT | libc::O_EXCL;
    } else if options.create {
        flags |= libc::O_CREAT;
    }
    // O_TRUNC is deliberately absent: truncation is destructive, so it happens only after the
    // handle is proven to be a regular file with an acceptable link count (see below).
    // SAFETY: `dir` is a live directory descriptor, `c` a valid string; the mode is read only with O_CREAT.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), flags, 0o600 as libc::c_uint) };
    if fd < 0 {
        return Err(refuse(&dir, c.as_c_str(), io::Error::last_os_error()));
    }
    // SAFETY: fresh descriptor owned here.
    let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(ContainError::NotAFile);
    }
    crate::handle::clear_nonblock(&file)?;
    if options.require_single_link && meta.nlink() != 1 {
        return Err(ContainError::HardLinked);
    }
    if options.truncate {
        file.set_len(0)?;
    }
    Ok(file)
}

#[cfg(all(feature = "fs", windows))]
fn open_strict(
    root: &Path,
    names: &[OsString],
    options: &ConfinedOptions,
    _expected_root: Option<crate::file_key::FileKey>,
) -> Result<std::fs::File, ContainError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    /// Open the reparse point itself instead of following it.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    /// Lets `CreateFileW` open a directory.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    const FILE_SHARE_READ: u32 = 0x1;
    /// A pinned directory: readers and writers may share it, but nobody may rename or delete it (no
    /// `FILE_SHARE_DELETE`), so it cannot be replaced by a junction while the handle is held.
    const FILE_SHARE_PIN: u32 = 0x3;
    const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x7;

    options.check()?;
    let (last, parents) = names.split_last().ok_or(ContainError::InvalidRelative)?;

    // Every directory from the root down is opened (never created) as a reparse-point-aware handle
    // and held until the leaf is open. A held directory cannot be renamed, deleted or swapped for a
    // junction, so the leaf path below cannot be redirected, and nothing is created or truncated
    // until every ancestor is proven to be a real directory below the root.
    let pin = |path: &Path| -> Result<std::fs::File, ContainError> {
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_PIN)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let meta = dir.metadata()?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(ContainError::Symlink);
        }
        if !meta.is_dir() {
            return Err(ContainError::Io(io::Error::new(
                io::ErrorKind::NotADirectory,
                "a path component is not a directory",
            )));
        }
        if !is_below(root, &crate::handle::final_path(&dir)?) {
            return Err(ContainError::Escapes);
        }
        Ok(dir)
    };
    let mut pins = vec![pin(root)?];
    let mut cur = root.to_path_buf();
    for name in parents {
        cur.push(name);
        pins.push(pin(&cur)?);
    }
    cur.push(last);
    // No truncation here: it is applied only after the handle is validated.
    let file = std::fs::OpenOptions::new()
        .read(options.read)
        .write(options.write)
        .append(options.append)
        .create(options.create)
        .create_new(options.create_new)
        .share_mode(if options.share_read_only {
            FILE_SHARE_READ
        } else {
            FILE_SHARE_READ_WRITE_DELETE
        })
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&cur)?;
    let meta = file.metadata()?;
    if meta.file_type().is_symlink() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ContainError::Symlink);
    }
    if !meta.is_file() {
        return Err(ContainError::NotAFile);
    }
    // The handle-path recheck: wherever the open really landed must still be below the root.
    let real = crate::handle::final_path(&file)?;
    if !is_below(root, &real) {
        return Err(ContainError::Escapes);
    }
    if options.require_single_link && crate::handle::link_count(&file)? != 1 {
        return Err(ContainError::HardLinked);
    }
    if options.truncate {
        file.set_len(0)?;
    }
    drop(pins);
    Ok(file)
}

#[cfg(all(feature = "fs", not(any(unix, windows))))]
fn open_strict(
    _root: &Path,
    _names: &[OsString],
    _options: &ConfinedOptions,
    _expected_root: Option<crate::file_key::FileKey>,
) -> Result<std::fs::File, ContainError> {
    Err(ContainError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "a confined open is not available on this platform",
    )))
}
