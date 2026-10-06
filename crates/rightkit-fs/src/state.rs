//! A descriptor-pinned state directory.
//!
//! On unix every operation goes through one verified directory descriptor: the directory is
//! opened component by component with `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`, `fstat`-checked
//! (a directory, owned by the effective user, not writable by group/others) and compared with an
//! `lstat` taken before the walk. Reads, writes, renames, unlinks and listings are `*at`
//! operations relative to that descriptor, so renaming or swapping the path afterwards (or
//! planting a symlink at a child name) cannot redirect anything. Donor: Cockpit
//! `core/src/store.rs` (pinned descriptor, tested against swaps).
//!
//! Non-unix targets use a path-based fallback that refuses symlinks and reparse points on each
//! operation. Cockpit's NT handle-relative Windows implementation has not been ported or
//! verified here; it remains the reference when a Windows host is available.

use crate::atomic::TEMP_MARKER;
use crate::error::{FsError, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_NAME: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub struct EntryStat {
    pub kind: EntryKind,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub modified_unix_ms: u64,
}

/// Entry names are single path components: no separators, no NUL, not `.`/`..`.
pub fn validate_name(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name.len() > MAX_NAME
        || name == "."
        || name == ".."
        || name.bytes().any(|b| b == b'/' || b == b'\\' || b == 0);
    if bad {
        return Err(FsError::BadName(name.to_string()));
    }
    Ok(())
}

#[derive(Debug)]
pub struct StateDir {
    dir: sys::Dir,
    path: PathBuf,
}

impl StateDir {
    /// Pin an existing directory.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            dir: sys::Dir::pin(path, false)?,
            path: path.to_path_buf(),
        })
    }

    /// Create the directory (and missing ancestors, owner-only, through descriptor-relative
    /// `mkdirat`) if needed, then pin it.
    pub fn create(path: &Path) -> Result<Self> {
        Ok(Self {
            dir: sys::Dir::pin(path, true)?,
            path: path.to_path_buf(),
        })
    }

    /// Like [`create`](Self::create) but resolves symlinks in the *parent* first (for example
    /// macOS `/var` -> `/private/var`). The final component must still be a real directory.
    pub fn create_in_resolved_parent(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .ok_or_else(|| FsError::BadName(path.display().to_string()))?;
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => {
                p.canonicalize().map_err(|e| FsError::io(p, e))?
            }
            _ => std::env::current_dir().map_err(|e| FsError::io(path, e))?,
        };
        Self::create(&parent.join(name))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create-or-open a direct child directory (owner-only), pinned relative to this one.
    pub fn child(&self, name: &str) -> Result<StateDir> {
        validate_name(name)?;
        Ok(Self {
            dir: self.dir.child(name)?,
            path: self.path.join(name),
        })
    }

    pub fn stat(&self, name: &str) -> Result<Option<EntryStat>> {
        validate_name(name)?;
        self.dir.stat(name)
    }

    pub fn exists(&self, name: &str) -> Result<bool> {
        Ok(self.stat(name)?.is_some())
    }

    /// Read a regular file (never through a symlink), refusing anything over `max_bytes`.
    pub fn read(&self, name: &str, max_bytes: u64) -> Result<Vec<u8>> {
        validate_name(name)?;
        let path = self.path.join(name);
        let mut file = self.dir.open_read(name)?;
        let meta = file.metadata().map_err(|e| FsError::io(&path, e))?;
        if !meta.is_file() {
            return Err(FsError::NotAFile(path));
        }
        if meta.len() > max_bytes {
            return Err(FsError::TooLarge {
                path,
                size: meta.len(),
                limit: max_bytes,
            });
        }
        let mut out = Vec::with_capacity(meta.len() as usize);
        (&mut file)
            .take(max_bytes + 1)
            .read_to_end(&mut out)
            .map_err(|e| FsError::io(&path, e))?;
        if out.len() as u64 > max_bytes {
            return Err(FsError::TooLarge {
                path,
                size: out.len() as u64,
                limit: max_bytes,
            });
        }
        Ok(out)
    }

    /// Like [`read`](Self::read) but `Ok(None)` when the entry does not exist.
    pub fn read_opt(&self, name: &str, max_bytes: u64) -> Result<Option<Vec<u8>>> {
        match self.read(name, max_bytes) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Atomically create or replace `name` (owner-only, `fsync`ed file and directory).
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.publish(name, bytes, true)
    }

    /// Atomically create `name`, failing with [`FsError::AlreadyExists`] rather than replacing.
    pub fn write_new(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.publish(name, bytes, false)
    }

    fn publish(&self, name: &str, bytes: &[u8], replace: bool) -> Result<()> {
        validate_name(name)?;
        let final_path = self.path.join(name);
        let (temp, mut file) = self.create_temp(name)?;
        let result = (|| -> Result<()> {
            file.write_all(bytes)
                .map_err(|e| FsError::io(&final_path, e))?;
            file.sync_all().map_err(|e| FsError::io(&final_path, e))?;
            drop(file);
            if replace {
                // Never replace a symlinked or non-file destination through the rename.
                if let Some(existing) = self.dir.stat(name)? {
                    if existing.kind != EntryKind::File {
                        return Err(FsError::NotAFile(final_path.clone()));
                    }
                }
                self.dir.rename_replace(&temp, name)?;
            } else {
                self.dir.link_new(&temp, name).map_err(|e| match e {
                    FsError::AlreadyExists(_) => FsError::AlreadyExists(final_path.clone()),
                    other => other,
                })?;
                let _ = self.dir.unlink(&temp);
            }
            self.dir.sync();
            Ok(())
        })();
        if result.is_err() {
            let _ = self.dir.unlink(&temp);
        }
        result
    }

    fn create_temp(&self, name: &str) -> Result<(String, File)> {
        for attempt in 0..32u32 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let temp = format!(".{name}{TEMP_MARKER}{}-{n}-{attempt}", std::process::id());
            match self.dir.create_new(&temp) {
                Ok(file) => return Ok((temp, file)),
                Err(FsError::AlreadyExists(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(FsError::AlreadyExists(self.path.join(name)))
    }

    /// Remove a file (a symlink is removed itself, never followed). Missing is `Ok(false)`.
    pub fn remove(&self, name: &str) -> Result<bool> {
        validate_name(name)?;
        match self.dir.unlink(name) {
            Ok(()) => Ok(true),
            Err(e) if e.is_not_found() => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Names of direct children, capped at `limit` entries (error past the cap).
    pub fn list(&self, limit: usize) -> Result<Vec<String>> {
        self.dir.names(limit)
    }

    /// Remove temp files left by writers that were killed mid-write (pid gone, or older than
    /// `max_age`). Returns how many were removed.
    pub fn sweep_temps(&self, max_age: std::time::Duration) -> Result<usize> {
        let mut removed = 0;
        for name in self.dir.names(usize::MAX)? {
            let Some(idx) = name.find(TEMP_MARKER) else {
                continue;
            };
            let pid: Option<u32> = name[idx + TEMP_MARKER.len()..]
                .split('-')
                .next()
                .and_then(|p| p.parse().ok());
            let Some(pid) = pid else { continue };
            let Some(stat) = self.dir.stat(&name)? else {
                continue;
            };
            if stat.kind != EntryKind::File {
                continue;
            }
            let age_ms = now_ms().saturating_sub(stat.modified_unix_ms);
            let old = age_ms >= max_age.as_millis() as u64;
            if (pid != std::process::id() && !crate::process_alive(pid)) || old {
                if self.dir.unlink(&name).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(unix)]
mod sys {
    use super::{EntryKind, EntryStat};
    use crate::error::{FsError, Result};
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
    use std::path::Path;

    #[derive(Debug)]
    pub struct Dir(File);

    fn cname(name: &str) -> Result<CString> {
        CString::new(name).map_err(|_| FsError::BadName(name.to_string()))
    }

    fn errno_is(e: &io::Error, code: i32) -> bool {
        e.raw_os_error() == Some(code)
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    unsafe fn reset_errno() {
        unsafe { *libc::__error() = 0 };
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    unsafe fn reset_errno() {
        unsafe { *libc::__errno_location() = 0 };
    }

    fn kind_of(mode: u32) -> EntryKind {
        match mode & libc::S_IFMT as u32 {
            m if m == libc::S_IFREG as u32 => EntryKind::File,
            m if m == libc::S_IFDIR as u32 => EntryKind::Dir,
            m if m == libc::S_IFLNK as u32 => EntryKind::Symlink,
            _ => EntryKind::Other,
        }
    }

    fn fstat(file: &File) -> io::Result<libc::stat> {
        // SAFETY: zeroed stat is a valid out-parameter and fd is open.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(file.as_raw_fd(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(st)
    }

    fn open_dir_at(base: RawFd, name: &CStr, shown: &Path) -> Result<File> {
        // SAFETY: base is an open directory fd (or AT_FDCWD) and name is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                base,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let e = io::Error::last_os_error();
            if errno_is(&e, libc::ELOOP) {
                return Err(FsError::Symlink(shown.to_path_buf()));
            }
            if errno_is(&e, libc::ENOTDIR) {
                // A symlink to a directory reports ENOTDIR with O_DIRECTORY|O_NOFOLLOW on some
                // kernels; distinguish it from a plain non-directory.
                // SAFETY: as above, with a valid out-parameter.
                let mut st: libc::stat = unsafe { std::mem::zeroed() };
                let rc = unsafe {
                    libc::fstatat(base, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW)
                };
                if rc == 0 && kind_of(st.st_mode as u32) == EntryKind::Symlink {
                    return Err(FsError::Symlink(shown.to_path_buf()));
                }
                return Err(FsError::NotADirectory(shown.to_path_buf()));
            }
            return Err(FsError::io(shown, e));
        }
        // SAFETY: fd was just returned by openat and is owned here.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn mkdir_at(base: RawFd, name: &CStr, shown: &Path) -> Result<()> {
        // SAFETY: valid fd and C string.
        if unsafe { libc::mkdirat(base, name.as_ptr(), 0o700) } != 0 {
            let e = io::Error::last_os_error();
            if !errno_is(&e, libc::EEXIST) {
                return Err(FsError::io(shown, e));
            }
        }
        Ok(())
    }

    fn verify(file: &File, shown: &Path) -> Result<libc::stat> {
        let st = fstat(file).map_err(|e| FsError::io(shown, e))?;
        if kind_of(st.st_mode as u32) != EntryKind::Dir {
            return Err(FsError::NotADirectory(shown.to_path_buf()));
        }
        // SAFETY: geteuid has no preconditions.
        if st.st_uid != unsafe { libc::geteuid() } {
            return Err(FsError::NotOwned(shown.to_path_buf()));
        }
        if st.st_mode as u32 & 0o022 != 0 {
            return Err(FsError::TooPermissive(shown.to_path_buf()));
        }
        Ok(st)
    }

    impl Dir {
        fn fd(&self) -> RawFd {
            self.0.as_raw_fd()
        }

        pub fn pin(path: &Path, create: bool) -> Result<Dir> {
            use std::path::Component;
            let root = CString::new("/").unwrap();
            let dot = CString::new(".").unwrap();
            let mut current = open_dir_at(
                libc::AT_FDCWD,
                if path.is_absolute() { &root } else { &dot },
                path,
            )?;
            let mut walked = std::path::PathBuf::new();
            for component in path.components() {
                let name = match component {
                    Component::RootDir => {
                        walked.push("/");
                        continue;
                    }
                    Component::CurDir => continue,
                    Component::ParentDir | Component::Prefix(_) => {
                        return Err(FsError::BadName(path.display().to_string()))
                    }
                    Component::Normal(n) => n,
                };
                walked.push(name);
                let c = CString::new(name.as_bytes())
                    .map_err(|_| FsError::BadName(path.display().to_string()))?;
                current = match open_dir_at(current.as_raw_fd(), &c, &walked) {
                    Ok(next) => next,
                    Err(FsError::Io { source, .. })
                        if create && errno_is(&source, libc::ENOENT) =>
                    {
                        mkdir_at(current.as_raw_fd(), &c, &walked)?;
                        open_dir_at(current.as_raw_fd(), &c, &walked)?
                    }
                    Err(e) => return Err(e),
                };
            }
            let st = verify(&current, path)?;
            // Defence in depth: the pinned inode must be the one the path names right now.
            match std::fs::symlink_metadata(path) {
                Ok(before)
                    if before.dev() == st.st_dev as u64 && before.ino() == st.st_ino as u64 => {}
                Ok(_) => return Err(FsError::IdentityChanged(path.to_path_buf())),
                Err(e) => return Err(FsError::io(path, e)),
            }
            Ok(Dir(current))
        }

        pub fn child(&self, name: &str) -> Result<Dir> {
            let c = cname(name)?;
            let shown = Path::new(name);
            let file = match open_dir_at(self.fd(), &c, shown) {
                Ok(f) => f,
                Err(FsError::Io { source, .. }) if errno_is(&source, libc::ENOENT) => {
                    mkdir_at(self.fd(), &c, shown)?;
                    open_dir_at(self.fd(), &c, shown)?
                }
                Err(e) => return Err(e),
            };
            verify(&file, shown)?;
            Ok(Dir(file))
        }

        pub fn stat(&self, name: &str) -> Result<Option<EntryStat>> {
            let c = cname(name)?;
            // SAFETY: valid fd, C string, and zeroed out-parameter.
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstatat(self.fd(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) }
                == 0
            {
                return Ok(Some(EntryStat {
                    kind: kind_of(st.st_mode as u32),
                    size: st.st_size as u64,
                    dev: st.st_dev as u64,
                    ino: st.st_ino as u64,
                    mode: st.st_mode as u32 & 0o7777,
                    modified_unix_ms: (st.st_mtime as u64) * 1000
                        + (st.st_mtime_nsec as u64) / 1_000_000,
                }));
            }
            let e = io::Error::last_os_error();
            if errno_is(&e, libc::ENOENT) || errno_is(&e, libc::ENOTDIR) {
                Ok(None)
            } else {
                Err(FsError::io(name, e))
            }
        }

        pub fn open_read(&self, name: &str) -> Result<File> {
            let c = cname(name)?;
            // SAFETY: valid fd and C string.
            let fd = unsafe {
                libc::openat(
                    self.fd(),
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let e = io::Error::last_os_error();
                if errno_is(&e, libc::ELOOP) {
                    return Err(FsError::Symlink(name.into()));
                }
                return Err(FsError::io(name, e));
            }
            // SAFETY: fd just returned by openat.
            Ok(unsafe { File::from_raw_fd(fd) })
        }

        pub fn create_new(&self, name: &str) -> Result<File> {
            let c = cname(name)?;
            // SAFETY: valid fd and C string; O_EXCL|O_NOFOLLOW means a planted symlink is never followed.
            let fd = unsafe {
                libc::openat(
                    self.fd(),
                    c.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600 as libc::c_uint,
                )
            };
            if fd < 0 {
                let e = io::Error::last_os_error();
                if errno_is(&e, libc::EEXIST) {
                    return Err(FsError::AlreadyExists(name.into()));
                }
                return Err(FsError::io(name, e));
            }
            // SAFETY: fd just returned by openat.
            Ok(unsafe { File::from_raw_fd(fd) })
        }

        pub fn rename_replace(&self, from: &str, to: &str) -> Result<()> {
            let (cf, ct) = (cname(from)?, cname(to)?);
            // SAFETY: valid fds and C strings.
            if unsafe { libc::renameat(self.fd(), cf.as_ptr(), self.fd(), ct.as_ptr()) } != 0 {
                return Err(FsError::io(to, io::Error::last_os_error()));
            }
            Ok(())
        }

        /// No-replace publication: a hard link fails with EEXIST if `to` exists.
        pub fn link_new(&self, from: &str, to: &str) -> Result<()> {
            let (cf, ct) = (cname(from)?, cname(to)?);
            // SAFETY: valid fds and C strings.
            if unsafe { libc::linkat(self.fd(), cf.as_ptr(), self.fd(), ct.as_ptr(), 0) } != 0 {
                let e = io::Error::last_os_error();
                if errno_is(&e, libc::EEXIST) {
                    return Err(FsError::AlreadyExists(to.into()));
                }
                return Err(FsError::io(to, e));
            }
            Ok(())
        }

        pub fn unlink(&self, name: &str) -> Result<()> {
            let c = cname(name)?;
            // SAFETY: valid fd and C string.
            if unsafe { libc::unlinkat(self.fd(), c.as_ptr(), 0) } != 0 {
                return Err(FsError::io(name, io::Error::last_os_error()));
            }
            Ok(())
        }

        pub fn sync(&self) {
            let _ = self.0.sync_all();
        }

        pub fn names(&self, limit: usize) -> Result<Vec<String>> {
            let dup = self
                .0
                .try_clone()
                .map_err(|e| FsError::io(".", e))?
                .into_raw_fd();
            // SAFETY: dup is a fresh owned fd; on success fdopendir takes ownership of it.
            let raw = unsafe { libc::fdopendir(dup) };
            if raw.is_null() {
                let e = io::Error::last_os_error();
                // SAFETY: fdopendir failed, so dup is still ours to close.
                unsafe { libc::close(dup) };
                return Err(FsError::io(".", e));
            }
            struct DirGuard(*mut libc::DIR);
            impl Drop for DirGuard {
                fn drop(&mut self) {
                    // SAFETY: pointer came from a successful fdopendir.
                    unsafe { libc::closedir(self.0) };
                }
            }
            let guard = DirGuard(raw);
            // The dup shares the file offset with the pinned fd; start from the beginning.
            // SAFETY: valid DIR*.
            unsafe { libc::rewinddir(guard.0) };
            let mut out = Vec::new();
            loop {
                // SAFETY: readdir signals end vs error through errno, which we clear first.
                unsafe { reset_errno() };
                let entry = unsafe { libc::readdir(guard.0) };
                if entry.is_null() {
                    let e = io::Error::last_os_error();
                    return if e.raw_os_error() == Some(0) {
                        Ok(out)
                    } else {
                        Err(FsError::io(".", e))
                    };
                }
                // SAFETY: d_name is a NUL-terminated array inside a live dirent.
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
                    .to_string_lossy()
                    .into_owned();
                if name != "." && name != ".." {
                    if out.len() >= limit {
                        return Err(FsError::TooManyEntries(limit));
                    }
                    out.push(name);
                }
            }
        }
    }
}

#[cfg(not(unix))]
mod sys {
    use super::{EntryKind, EntryStat};
    use crate::error::{FsError, Result};
    use std::fs::{self, File, OpenOptions};
    use std::path::{Path, PathBuf};

    #[derive(Debug)]
    pub struct Dir(PathBuf);

    fn is_link(meta: &fs::Metadata) -> bool {
        if meta.file_type().is_symlink() {
            return true;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return true; // FILE_ATTRIBUTE_REPARSE_POINT
            }
        }
        false
    }

    fn reject_links(path: &Path) -> Result<()> {
        for ancestor in path.ancestors() {
            match fs::symlink_metadata(ancestor) {
                Ok(m) if is_link(&m) => return Err(FsError::Symlink(path.to_path_buf())),
                Ok(_) | Err(_) => {}
            }
        }
        Ok(())
    }

    impl Dir {
        pub fn pin(path: &Path, create: bool) -> Result<Dir> {
            reject_links(path)?;
            if create {
                fs::create_dir_all(path).map_err(|e| FsError::io(path, e))?;
            }
            reject_links(path)?;
            if !fs::metadata(path)
                .map_err(|e| FsError::io(path, e))?
                .is_dir()
            {
                return Err(FsError::NotADirectory(path.to_path_buf()));
            }
            Ok(Dir(path.to_path_buf()))
        }
        pub fn child(&self, name: &str) -> Result<Dir> {
            let p = self.0.join(name);
            Dir::pin(&p, true)
        }
        pub fn stat(&self, name: &str) -> Result<Option<EntryStat>> {
            let p = self.0.join(name);
            match fs::symlink_metadata(&p) {
                Ok(m) => Ok(Some(EntryStat {
                    kind: if is_link(&m) {
                        EntryKind::Symlink
                    } else if m.is_dir() {
                        EntryKind::Dir
                    } else if m.is_file() {
                        EntryKind::File
                    } else {
                        EntryKind::Other
                    },
                    size: m.len(),
                    dev: 0,
                    ino: 0,
                    mode: 0,
                    modified_unix_ms: m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0),
                })),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(FsError::io(p, e)),
            }
        }
        pub fn open_read(&self, name: &str) -> Result<File> {
            let p = self.0.join(name);
            if let Ok(m) = fs::symlink_metadata(&p) {
                if is_link(&m) {
                    return Err(FsError::Symlink(p));
                }
            }
            File::open(&p).map_err(|e| FsError::io(p, e))
        }
        pub fn create_new(&self, name: &str) -> Result<File> {
            let p = self.0.join(name);
            let f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&p)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        FsError::AlreadyExists(p.clone())
                    } else {
                        FsError::io(&p, e)
                    }
                })?;
            crate::private::restrict_owner_only(&p)?;
            Ok(f)
        }
        pub fn rename_replace(&self, from: &str, to: &str) -> Result<()> {
            fs::rename(self.0.join(from), self.0.join(to))
                .map_err(|e| FsError::io(self.0.join(to), e))
        }
        pub fn link_new(&self, from: &str, to: &str) -> Result<()> {
            fs::hard_link(self.0.join(from), self.0.join(to)).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    FsError::AlreadyExists(self.0.join(to))
                } else {
                    FsError::io(self.0.join(to), e)
                }
            })
        }
        pub fn unlink(&self, name: &str) -> Result<()> {
            fs::remove_file(self.0.join(name)).map_err(|e| FsError::io(self.0.join(name), e))
        }
        pub fn sync(&self) {}
        pub fn names(&self, limit: usize) -> Result<Vec<String>> {
            let mut out = Vec::new();
            for entry in fs::read_dir(&self.0).map_err(|e| FsError::io(&self.0, e))? {
                let entry = entry.map_err(|e| FsError::io(&self.0, e))?;
                if out.len() >= limit {
                    return Err(FsError::TooManyEntries(limit));
                }
                out.push(entry.file_name().to_string_lossy().into_owned());
            }
            Ok(out)
        }
    }
}
