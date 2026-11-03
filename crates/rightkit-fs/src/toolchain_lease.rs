//! Digest-bound immutable toolchain tree leases (rightkit#838).
//!
//! A native verification that runs a compiler many times against the same runtime and SDK trees
//! cannot hash those trees before every run: that is gigabytes per request. A [`ToolchainLease`]
//! hashes them once, at sealing, and freezes the backing bytes with the operating system's own
//! immutability so that later replays check metadata only.
//!
//! # What sealing does ([`ToolchainLease::seal`])
//!
//! 1. Refuses a root that is or traverses a symlink, a special file (fifo, socket, device), a
//!    symlink whose target does not resolve inside the sealed roots (alias topology), and a file
//!    that has a hard link outside the sealed roots (the link count is checked against the names
//!    found).
//! 2. Marks every file and directory immutable (macOS `UF_IMMUTABLE`, Linux `FS_IMMUTABLE_FL`),
//!    through a descriptor opened with `O_NOFOLLOW` and compared with what the walk saw, so a path
//!    swapped in between is refused, not frozen. A file or directory the OS already protects
//!    (`SF_IMMUTABLE`, `SF_RESTRICTED`) is recorded as such and left alone.
//! 3. Hashes every file through that same descriptor after it is frozen, so the digest is of the
//!    bytes that can no longer change, and re-reads every directory's names after it is frozen.
//! 4. Records the tree digest (SHA-256 over a canonical manifest of paths, kinds, modes, sizes,
//!    file digests and link targets; no device, inode or time), a metadata snapshot (identity,
//!    size, mode, link count, change time, flags, directory membership) and pins each root by an
//!    open descriptor and its ancestor identities.
//!
//! An immutable file cannot be written, truncated, unlinked or renamed over, and an immutable
//! directory cannot gain, lose or rename an entry, even by the same user. Clearing the flag is
//! possible for the owner, and it is detected: it changes the file's change time, which no
//! process can set back.
//!
//! # What a replay does ([`ToolchainLease::verify`])
//!
//! Metadata only, no content read: [`VerifyReport::bytes_hashed`] is 0. It refuses an expired
//! lease, a changed ancestor or pinned root (parent swap), and any entry whose kind, identity,
//! size, mode, link count, change time or flags differ from the snapshot, whose directory
//! membership differs, or whose link target changed (an added, removed or retargeted entry, a
//! direct write, an external hard link). [`ToolchainLease::verify_content`] hashes everything again
//! and compares the tree digest, for the enrollment and final boundaries; its bytes are reported
//! in [`VerifyReport::bytes_hashed`] so a caller's byte ledger accounts for them.
//!
//! # Fail closed
//!
//! Sealing needs the OS to enforce immutability: macOS, or Linux on x86-64, aarch64 or riscv64.
//! Setting the flag needs ownership of the entry (macOS) or `CAP_LINUX_IMMUTABLE` (Linux), or the
//! entry must already be OS-protected. Anywhere else, and on any entry that cannot be frozen,
//! sealing fails with nothing left frozen ([`FsError::Unavailable`] or the failing path), never with
//! a lease that does not hold. Windows is not supported by this module.
//!
//! A lease releases what it froze when released or dropped. The optional journal
//! ([`SealSpec::journal`]) names the paths it is about to freeze (those not already immutable) so [`ToolchainLease::recover`] can clear them
//! after a crash.

use std::ffi::OsString;
use std::fs::File;
#[cfg(unix)]
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::digest::Sha256Hasher;
use crate::error::{FsError, Result};
use crate::file_key::FileKey;

/// What to seal.
#[derive(Clone, Debug)]
pub struct SealSpec {
    /// Directory trees (runtime roots, an SDK) and single regular files (the exact `make`
    /// executable). Each must be an absolute path with no symlink component.
    pub roots: Vec<PathBuf>,
    /// How long the lease stays valid.
    pub ttl: Duration,
    /// Entry cap over all roots; sealing fails with [`FsError::TooManyEntries`] past it.
    pub max_entries: usize,
    /// Where to record the paths this lease froze, for [`ToolchainLease::recover`].
    pub journal: Option<PathBuf>,
}

impl SealSpec {
    pub fn new(roots: Vec<PathBuf>, ttl: Duration) -> Self {
        Self {
            roots,
            ttl,
            max_entries: 2_000_000,
            journal: None,
        }
    }

    pub fn journal(mut self, path: impl Into<PathBuf>) -> Self {
        self.journal = Some(path.into());
        self
    }

    pub fn max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }
}

/// The OS mechanism that holds the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "json", derive(serde::Serialize, serde::Deserialize))]
pub enum Enforcement {
    /// macOS `UF_IMMUTABLE` (and OS-protected `SF_IMMUTABLE` / `SF_RESTRICTED` entries).
    MacosImmutableFlag,
    /// Linux `FS_IMMUTABLE_FL`.
    LinuxImmutableAttr,
}

impl Enforcement {
    pub fn label(self) -> &'static str {
        match self {
            Enforcement::MacosImmutableFlag => "macos-uf-immutable",
            Enforcement::LinuxImmutableAttr => "linux-fs-immutable",
        }
    }
}

/// What a check read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct VerifyReport {
    pub entries_checked: u64,
    /// File content bytes hashed: 0 for [`ToolchainLease::verify`].
    pub bytes_hashed: u64,
}

/// Per-root totals in the evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "json", derive(serde::Serialize, serde::Deserialize))]
pub struct RootEvidence {
    pub path: String,
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    pub bytes: u64,
}

/// Content-free evidence a caller can retain and a validator can compare.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "json", derive(serde::Serialize, serde::Deserialize))]
pub struct LeaseEvidence {
    pub schema: u32,
    pub lease_id: String,
    /// SHA-256 hex over the canonical manifest of every sealed root.
    pub tree_digest: String,
    pub enforcement: Enforcement,
    pub roots: Vec<RootEvidence>,
    /// Entries this lease froze itself (the rest were already OS-protected).
    pub frozen_by_lease: u64,
    /// File content bytes hashed while sealing.
    pub bytes_hashed_sealing: u64,
    pub sealed_at_unix: u64,
    pub expires_at_unix: u64,
}

#[derive(Clone, Debug)]
struct Entry {
    root: usize,
    rel: PathBuf,
    kind: Kind,
    key: FileKey,
    size: u64,
    mode: u32,
    nlink: u64,
    digest: Option<String>,
    target: Option<PathBuf>,
    children: Vec<OsString>,
    frozen_by_us: bool,
    ctime: (i64, i64),
    flags: u32,
}

struct Pin {
    file: File,
    key: FileKey,
    ancestors: Vec<(PathBuf, FileKey)>,
}

/// A sealed set of toolchain trees. See the module docs.
pub struct ToolchainLease {
    roots: Vec<PathBuf>,
    entries: Vec<Entry>,
    pins: Vec<Pin>,
    evidence: LeaseEvidence,
    journal: Option<PathBuf>,
    released: bool,
}

fn bad(path: &Path, reason: impl Into<String>) -> FsError {
    FsError::Conflict {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

fn entry_path(root: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(unix)]
mod sys {
    use super::*;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    #[cfg(target_os = "macos")]
    pub const ENFORCEMENT: Option<Enforcement> = Some(Enforcement::MacosImmutableFlag);
    #[cfg(all(
        target_os = "linux",
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    ))]
    pub const ENFORCEMENT: Option<Enforcement> = Some(Enforcement::LinuxImmutableAttr);
    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )))]
    pub const ENFORCEMENT: Option<Enforcement> = None;

    pub fn key_of(meta: &fs::Metadata) -> FileKey {
        FileKey {
            volume: meta.dev(),
            index: meta.ino(),
        }
    }

    pub fn mode_of(meta: &fs::Metadata) -> u32 {
        meta.mode() & 0o7777
    }

    pub fn ctime_of(meta: &fs::Metadata) -> (i64, i64) {
        (meta.ctime(), meta.ctime_nsec())
    }

    pub fn nlink_of(meta: &fs::Metadata) -> u64 {
        meta.nlink()
    }

    pub fn open_nofollow(path: &Path, dir: bool) -> std::io::Result<File> {
        let mut flags = libc::O_NOFOLLOW | libc::O_CLOEXEC;
        if dir {
            flags |= libc::O_DIRECTORY;
        }
        OpenOptions::new().read(true).custom_flags(flags).open(path)
    }

    /// `true` when the entry is immutable for ordinary use (already protected or frozen by us).
    #[cfg(target_os = "macos")]
    pub fn is_immutable(flags: u32) -> bool {
        const UF_IMMUTABLE: u32 = 0x0000_0002;
        const SF_IMMUTABLE: u32 = 0x0002_0000;
        const SF_RESTRICTED: u32 = 0x0008_0000;
        flags & (UF_IMMUTABLE | SF_IMMUTABLE | SF_RESTRICTED) != 0
    }

    #[cfg(target_os = "macos")]
    pub fn flags_of(file: &File) -> std::io::Result<u32> {
        use std::os::macos::fs::MetadataExt as _;
        Ok(file.metadata()?.st_flags())
    }

    #[cfg(target_os = "macos")]
    pub fn flags_of_meta(meta: &fs::Metadata) -> Option<u32> {
        use std::os::macos::fs::MetadataExt as _;
        Some(meta.st_flags())
    }

    #[cfg(target_os = "macos")]
    extern "C" {
        fn fchflags(fd: libc::c_int, flags: libc::c_uint) -> libc::c_int;
    }

    /// Set (`on`) or clear the user-immutable flag, keeping every other flag.
    #[cfg(target_os = "macos")]
    pub fn set_user_immutable(file: &File, on: bool) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        const UF_IMMUTABLE: u32 = 0x0000_0002;
        let current = flags_of(file)?;
        let next = if on {
            current | UF_IMMUTABLE
        } else {
            current & !UF_IMMUTABLE
        };
        // SAFETY: `fchflags` is called with a descriptor this `File` owns and plain integers.
        let rc = unsafe { fchflags(file.as_raw_fd(), next) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "linux")]
    const FS_IMMUTABLE_FL: libc::c_int = 0x0000_0010;
    #[cfg(target_os = "linux")]
    const FS_IOC_GETFLAGS: u64 = 0x8008_6601;
    #[cfg(target_os = "linux")]
    const FS_IOC_SETFLAGS: u64 = 0x4008_6602;

    #[cfg(target_os = "linux")]
    pub fn is_immutable(flags: u32) -> bool {
        flags & (FS_IMMUTABLE_FL as u32) != 0
    }

    #[cfg(target_os = "linux")]
    pub fn flags_of(file: &File) -> std::io::Result<u32> {
        use std::os::fd::AsRawFd;
        let mut attr: libc::c_int = 0;
        // SAFETY: `FS_IOC_GETFLAGS` writes one `int` through the pointer, which is a live local.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_GETFLAGS as _, &mut attr) };
        if rc == 0 {
            Ok(attr as u32)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Linux reads the attribute through an ioctl, which needs a descriptor; a metadata check
    /// relies on the change time instead.
    #[cfg(target_os = "linux")]
    pub fn flags_of_meta(_meta: &fs::Metadata) -> Option<u32> {
        None
    }

    #[cfg(target_os = "linux")]
    pub fn set_user_immutable(file: &File, on: bool) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let current = flags_of(file)? as libc::c_int;
        let mut next = if on {
            current | FS_IMMUTABLE_FL
        } else {
            current & !FS_IMMUTABLE_FL
        };
        // SAFETY: `FS_IOC_SETFLAGS` reads one `int` through the pointer, which is a live local.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_SETFLAGS as _, &mut next) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )))]
    pub fn is_immutable(_flags: u32) -> bool {
        false
    }

    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )))]
    pub fn flags_of(_file: &File) -> std::io::Result<u32> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }

    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )))]
    pub fn flags_of_meta(_meta: &fs::Metadata) -> Option<u32> {
        None
    }

    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )))]
    pub fn set_user_immutable(_file: &File, _on: bool) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

#[cfg(unix)]
use sys::*;

fn hash_file(file: &mut File, path: &Path) -> Result<(String, u64)> {
    let mut hasher = Sha256Hasher::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buffer).map_err(|e| FsError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        total += n as u64;
    }
    Ok((hasher.finalize(), total))
}

impl ToolchainLease {
    /// Seal `spec.roots`. On any failure nothing stays frozen.
    #[cfg(unix)]
    pub fn seal(spec: &SealSpec) -> Result<ToolchainLease> {
        let Some(enforcement) = sys::ENFORCEMENT else {
            return Err(FsError::Unavailable(
                "immutable toolchain lease (needs macOS or Linux x86-64/aarch64/riscv64)",
            ));
        };
        if spec.roots.is_empty() {
            return Err(FsError::BadName("a lease needs at least one root".into()));
        }
        let roots = canonical_roots(&spec.roots)?;
        let mut entries = Vec::new();
        for (index, root) in roots.iter().enumerate() {
            walk_root(index, root, &mut entries, spec.max_entries)?;
        }
        check_hard_links(&roots, &entries)?;
        check_link_containment(&roots, &entries)?;
        let ancestors = roots
            .iter()
            .map(|r| ancestor_keys(r))
            .collect::<Result<Vec<_>>>()?;

        let sealing = Sealing::new(spec.journal.clone());
        let bytes_hashed = match sealing.freeze_all(&roots, &mut entries) {
            Ok(n) => n,
            Err(e) => {
                sealing.rollback(&roots, &entries);
                return Err(e);
            }
        };
        let finish = (|| -> Result<(Vec<Pin>, LeaseEvidence)> {
            recheck_membership(&roots, &entries)?;
            snapshot(&roots, &mut entries)?;
            let pins = pin_roots(&roots, &entries, ancestors)?;
            let tree_digest = tree_digest(&roots, &entries);
            let sealed_at = now_unix();
            let evidence = LeaseEvidence {
                schema: 1,
                lease_id: lease_id(&tree_digest, sealed_at),
                tree_digest,
                enforcement,
                roots: root_evidence(&roots, &entries),
                frozen_by_lease: entries.iter().filter(|e| e.frozen_by_us).count() as u64,
                bytes_hashed_sealing: bytes_hashed,
                sealed_at_unix: sealed_at,
                expires_at_unix: sealed_at.saturating_add(spec.ttl.as_secs()),
            };
            Ok((pins, evidence))
        })();
        match finish {
            Ok((pins, evidence)) => Ok(ToolchainLease {
                roots,
                entries,
                pins,
                evidence,
                journal: spec.journal.clone(),
                released: false,
            }),
            Err(e) => {
                sealing.rollback(&roots, &entries);
                Err(e)
            }
        }
    }

    #[cfg(not(unix))]
    pub fn seal(_spec: &SealSpec) -> Result<ToolchainLease> {
        Err(FsError::Unavailable(
            "immutable toolchain lease is not supported on this host",
        ))
    }

    /// The evidence to retain: tree digest, enforcement, totals and validity window.
    pub fn evidence(&self) -> &LeaseEvidence {
        &self.evidence
    }

    /// The full-tree digest the lease is bound to (lowercase hex SHA-256).
    pub fn digest(&self) -> &str {
        &self.evidence.tree_digest
    }

    pub fn lease_id(&self) -> &str {
        &self.evidence.lease_id
    }

    pub fn is_expired(&self) -> bool {
        now_unix() >= self.evidence.expires_at_unix
    }

    /// The sealed roots, canonical.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Metadata-only replay check. See the module docs; reads no file content.
    #[cfg(unix)]
    pub fn verify(&self) -> Result<VerifyReport> {
        self.check(false)
    }

    /// Full check for the enrollment and final boundaries: [`ToolchainLease::verify`] plus every
    /// file hashed again and the tree digest compared. The bytes are in the report.
    #[cfg(unix)]
    pub fn verify_content(&self) -> Result<VerifyReport> {
        self.check(true)
    }

    #[cfg(not(unix))]
    pub fn verify(&self) -> Result<VerifyReport> {
        Err(FsError::Unavailable("toolchain lease"))
    }

    #[cfg(not(unix))]
    pub fn verify_content(&self) -> Result<VerifyReport> {
        Err(FsError::Unavailable("toolchain lease"))
    }

    #[cfg(unix)]
    fn check(&self, content: bool) -> Result<VerifyReport> {
        if self.released {
            return Err(bad(&self.roots[0], "the lease was released"));
        }
        if self.is_expired() {
            return Err(bad(&self.roots[0], "the lease expired"));
        }
        for (root, pin) in self.roots.iter().zip(&self.pins) {
            for (path, key) in &pin.ancestors {
                let meta = fs::symlink_metadata(path).map_err(|e| FsError::io(path, e))?;
                if meta.file_type().is_symlink() || key_of(&meta) != *key {
                    return Err(bad(path, "an ancestor of a sealed root was replaced"));
                }
            }
            let meta = pin.file.metadata().map_err(|e| FsError::io(root, e))?;
            let on_path = fs::symlink_metadata(root).map_err(|e| FsError::io(root, e))?;
            if key_of(&meta) != pin.key || key_of(&on_path) != pin.key {
                return Err(bad(root, "a sealed root no longer names the pinned object"));
            }
        }
        let mut report = VerifyReport::default();
        for entry in &self.entries {
            let path = entry_path(&self.roots[entry.root], &entry.rel);
            let meta = fs::symlink_metadata(&path).map_err(|e| FsError::io(&path, e))?;
            let kind = if meta.file_type().is_symlink() {
                Kind::Symlink
            } else if meta.is_dir() {
                Kind::Dir
            } else if meta.is_file() {
                Kind::File
            } else {
                return Err(bad(
                    &path,
                    "entry is no longer a file, directory or symlink",
                ));
            };
            if kind != entry.kind {
                return Err(bad(
                    &path,
                    format!("kind changed from {}", entry.kind.label()),
                ));
            }
            if key_of(&meta) != entry.key {
                return Err(bad(&path, "identity changed (replaced)"));
            }
            if ctime_of(&meta) != entry.ctime {
                return Err(bad(
                    &path,
                    "change time moved (written, relinked or flag cleared)",
                ));
            }
            if mode_of(&meta) != entry.mode || nlink_of(&meta) != entry.nlink {
                return Err(bad(&path, "mode or link count changed"));
            }
            if kind != Kind::Symlink {
                if let Some(flags) = flags_of_meta(&meta) {
                    if flags != entry.flags {
                        return Err(bad(&path, "flags changed"));
                    }
                }
            }
            match kind {
                Kind::File => {
                    if meta.len() != entry.size {
                        return Err(bad(&path, "size changed"));
                    }
                    if content {
                        let mut file =
                            open_nofollow(&path, false).map_err(|e| FsError::io(&path, e))?;
                        let opened = file.metadata().map_err(|e| FsError::io(&path, e))?;
                        if key_of(&opened) != entry.key {
                            return Err(bad(&path, "identity changed while reading"));
                        }
                        let (digest, bytes) = hash_file(&mut file, &path)?;
                        report.bytes_hashed += bytes;
                        if Some(&digest) != entry.digest.as_ref() {
                            return Err(FsError::DigestMismatch {
                                path,
                                expected: entry.digest.clone().unwrap_or_default(),
                                actual: digest,
                            });
                        }
                    }
                }
                Kind::Dir => {
                    if dir_names(&path)? != entry.children {
                        return Err(bad(&path, "directory membership changed"));
                    }
                }
                Kind::Symlink => {
                    let target = fs::read_link(&path).map_err(|e| FsError::io(&path, e))?;
                    if Some(&target) != entry.target.as_ref() {
                        return Err(bad(&path, "symlink retargeted"));
                    }
                }
            }
            report.entries_checked += 1;
        }
        if content && tree_digest(&self.roots, &self.entries) != self.evidence.tree_digest {
            return Err(bad(&self.roots[0], "tree digest does not match the lease"));
        }
        Ok(report)
    }

    /// Clear the immutability this lease set. Entries the OS already protected stay as they were.
    pub fn release(mut self) -> Result<()> {
        self.release_inner()
    }

    fn release_inner(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        #[cfg(unix)]
        {
            let result = unfreeze(&self.roots, &self.entries);
            if let Some(journal) = &self.journal {
                let _ = fs::remove_file(journal);
            }
            result
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }

    /// After a crash: clear the immutability recorded in `journal` and remove it. Only the paths
    /// the journal names are touched.
    #[cfg(all(unix, feature = "json"))]
    pub fn recover(journal: &Path) -> Result<u64> {
        let bytes = fs::read(journal).map_err(|e| FsError::io(journal, e))?;
        let record: Journal = serde_json::from_slice(&bytes).map_err(|e| FsError::Corrupt {
            path: journal.to_path_buf(),
            reason: e.to_string(),
        })?;
        let mut cleared = 0;
        let mut first_error = None;
        // Files first, then directories from the outermost in: attribute changes are not blocked
        // by an immutable parent.
        for path in record.sealed.iter().rev() {
            let p = Path::new(path);
            let dir = fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false);
            match open_nofollow(p, dir).and_then(|f| set_user_immutable(&f, false)) {
                Ok(()) => cleared += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => first_error = first_error.or(Some(FsError::io(p, e))),
            }
        }
        if let Some(e) = first_error {
            return Err(e);
        }
        fs::remove_file(journal).map_err(|e| FsError::io(journal, e))?;
        Ok(cleared)
    }
}

impl Drop for ToolchainLease {
    fn drop(&mut self) {
        let _ = self.release_inner();
    }
}

#[cfg(all(unix, feature = "json"))]
#[derive(serde::Serialize, serde::Deserialize)]
struct Journal {
    /// Frozen paths, files before directories, directories deepest first.
    sealed: Vec<String>,
}

#[cfg(unix)]
struct Sealing {
    journal: Option<PathBuf>,
}

#[cfg(unix)]
impl Sealing {
    fn new(journal: Option<PathBuf>) -> Self {
        Self { journal }
    }

    /// Freeze every file, then every directory deepest first; hash each file through the frozen
    /// descriptor. Returns the bytes hashed.
    fn freeze_all(&self, roots: &[PathBuf], entries: &mut [Entry]) -> Result<u64> {
        let mut bytes_hashed = 0u64;
        // The journal first, naming every path we may freeze, so a crash mid-way is recoverable.
        self.write_journal(roots, entries)?;
        for entry in entries.iter_mut().filter(|e| e.kind == Kind::File) {
            let path = entry_path(&roots[entry.root], &entry.rel);
            let mut file = open_nofollow(&path, false).map_err(|e| FsError::io(&path, e))?;
            let meta = file.metadata().map_err(|e| FsError::io(&path, e))?;
            if key_of(&meta) != entry.key || meta.len() != entry.size {
                return Err(bad(&path, "changed while sealing"));
            }
            entry.frozen_by_us = freeze(&file, &path)?;
            let (digest, bytes) = hash_file(&mut file, &path)?;
            if bytes != entry.size {
                return Err(bad(&path, "size differs from the bytes read"));
            }
            bytes_hashed += bytes;
            entry.digest = Some(digest);
        }
        let mut dirs: Vec<usize> = (0..entries.len())
            .filter(|&i| entries[i].kind == Kind::Dir)
            .collect();
        dirs.sort_by_key(|&i| std::cmp::Reverse(entries[i].rel.components().count()));
        for i in dirs {
            let path = entry_path(&roots[entries[i].root], &entries[i].rel);
            let file = open_nofollow(&path, true).map_err(|e| FsError::io(&path, e))?;
            let meta = file.metadata().map_err(|e| FsError::io(&path, e))?;
            if key_of(&meta) != entries[i].key {
                return Err(bad(&path, "changed while sealing"));
            }
            entries[i].frozen_by_us = freeze(&file, &path)?;
        }
        Ok(bytes_hashed)
    }

    fn write_journal(&self, roots: &[PathBuf], entries: &[Entry]) -> Result<()> {
        #[cfg(feature = "json")]
        if let Some(journal) = &self.journal {
            // Only entries that are not already immutable: those are the ones this seal will set,
            // and recovery must not clear a flag somebody else put there.
            let will_freeze = |e: &Entry| {
                let path = entry_path(&roots[e.root], &e.rel);
                match open_nofollow(&path, e.kind == Kind::Dir).and_then(|f| flags_of(&f)) {
                    Ok(flags) => !is_immutable(flags),
                    Err(_) => true,
                }
            };
            let mut sealed: Vec<String> = Vec::new();
            for e in entries
                .iter()
                .filter(|e| e.kind == Kind::File && will_freeze(e))
            {
                sealed.push(entry_path(&roots[e.root], &e.rel).display().to_string());
            }
            let mut dirs: Vec<&Entry> = entries
                .iter()
                .filter(|e| e.kind == Kind::Dir && will_freeze(e))
                .collect();
            dirs.sort_by_key(|e| std::cmp::Reverse(e.rel.components().count()));
            for e in dirs {
                sealed.push(entry_path(&roots[e.root], &e.rel).display().to_string());
            }
            let record = Journal { sealed };
            let bytes = serde_json::to_vec(&record).map_err(|e| FsError::Corrupt {
                path: journal.clone(),
                reason: e.to_string(),
            })?;
            crate::atomic::atomic_write(journal, &bytes)?;
        }
        #[cfg(not(feature = "json"))]
        let _ = (roots, entries, &self.journal);
        Ok(())
    }

    /// Undo whatever this attempt froze. Best effort: sealing already failed.
    fn rollback(&self, roots: &[PathBuf], entries: &[Entry]) {
        let _ = unfreeze(roots, entries);
        if let Some(journal) = &self.journal {
            let _ = fs::remove_file(journal);
        }
    }
}

/// Freeze one open entry. `Ok(true)` when this call set the flag, `Ok(false)` when the OS already protects it.
#[cfg(unix)]
fn freeze(file: &File, path: &Path) -> Result<bool> {
    let flags = flags_of(file).map_err(|e| FsError::io(path, e))?;
    if is_immutable(flags) {
        return Ok(false);
    }
    set_user_immutable(file, true).map_err(|e| FsError::io(path, e))?;
    let after = flags_of(file).map_err(|e| FsError::io(path, e))?;
    if !is_immutable(after) {
        return Err(bad(path, "the immutable flag did not take"));
    }
    Ok(true)
}

/// Clear what a lease froze: directories from the outermost in, then files. Reports the first failure after trying all.
#[cfg(unix)]
fn unfreeze(roots: &[PathBuf], entries: &[Entry]) -> Result<()> {
    let mut first = None;
    let mut order: Vec<&Entry> = entries.iter().filter(|e| e.frozen_by_us).collect();
    order.sort_by_key(|e| (e.kind == Kind::File, e.rel.components().count()));
    for entry in order {
        let path = entry_path(&roots[entry.root], &entry.rel);
        let outcome = open_nofollow(&path, entry.kind == Kind::Dir)
            .and_then(|f| set_user_immutable(&f, false));
        if let Err(e) = outcome {
            if e.kind() != std::io::ErrorKind::NotFound && first.is_none() {
                first = Some(FsError::io(&path, e));
            }
        }
    }
    match first {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(unix)]
fn canonical_roots(given: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for root in given {
        if !root.is_absolute() {
            return Err(FsError::BadName(format!(
                "lease root {} is not absolute",
                root.display()
            )));
        }
        let meta = fs::symlink_metadata(root).map_err(|e| FsError::io(root, e))?;
        if meta.file_type().is_symlink() {
            return Err(FsError::Symlink(root.clone()));
        }
        if !meta.is_dir() && !meta.is_file() {
            return Err(FsError::NotAFile(root.clone()));
        }
        let canonical = fs::canonicalize(root).map_err(|e| FsError::io(root, e))?;
        if canonical != *root {
            return Err(FsError::Symlink(root.clone()));
        }
        for other in &out {
            if canonical.starts_with(other) || other.starts_with(&canonical) {
                return Err(bad(root, "lease roots overlap"));
            }
        }
        out.push(canonical);
    }
    Ok(out)
}

#[cfg(unix)]
fn dir_names(path: &Path) -> Result<Vec<OsString>> {
    let mut names = Vec::new();
    for item in fs::read_dir(path).map_err(|e| FsError::io(path, e))? {
        names.push(item.map_err(|e| FsError::io(path, e))?.file_name());
    }
    names.sort();
    Ok(names)
}

#[cfg(unix)]
fn walk_root(index: usize, root: &Path, entries: &mut Vec<Entry>, max: usize) -> Result<()> {
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let path = entry_path(root, &rel);
        let meta = fs::symlink_metadata(&path).map_err(|e| FsError::io(&path, e))?;
        if entries.len() >= max {
            return Err(FsError::TooManyEntries(max));
        }
        let ft = meta.file_type();
        let kind = if ft.is_symlink() {
            Kind::Symlink
        } else if ft.is_dir() {
            Kind::Dir
        } else if ft.is_file() {
            Kind::File
        } else {
            return Err(FsError::NotAFile(path));
        };
        let mut entry = Entry {
            root: index,
            rel: rel.clone(),
            kind,
            key: key_of(&meta),
            size: meta.len(),
            mode: mode_of(&meta),
            nlink: nlink_of(&meta),
            digest: None,
            target: None,
            children: Vec::new(),
            frozen_by_us: false,
            ctime: ctime_of(&meta),
            flags: 0,
        };
        match kind {
            Kind::Symlink => {
                entry.target = Some(fs::read_link(&path).map_err(|e| FsError::io(&path, e))?);
            }
            Kind::Dir => {
                entry.children = dir_names(&path)?;
                for name in entry.children.iter().rev() {
                    stack.push(rel.join(name));
                }
            }
            Kind::File => {}
        }
        entries.push(entry);
    }
    Ok(())
}

/// Every name of a file with more than one link must be inside the sealed roots: the links seen
/// must account for the whole link count, else another path reaches the same bytes.
#[cfg(unix)]
fn check_hard_links(roots: &[PathBuf], entries: &[Entry]) -> Result<()> {
    use std::collections::HashMap;
    let mut seen: HashMap<FileKey, u64> = HashMap::new();
    for e in entries.iter().filter(|e| e.kind == Kind::File) {
        *seen.entry(e.key).or_insert(0) += 1;
    }
    for e in entries
        .iter()
        .filter(|e| e.kind == Kind::File && e.nlink > 1)
    {
        if seen.get(&e.key).copied().unwrap_or(0) != e.nlink {
            return Err(FsError::HardLinked(entry_path(&roots[e.root], &e.rel)));
        }
    }
    Ok(())
}

/// A symlink must resolve, through the filesystem, to a path inside a sealed root.
#[cfg(unix)]
fn check_link_containment(roots: &[PathBuf], entries: &[Entry]) -> Result<()> {
    for e in entries.iter().filter(|e| e.kind == Kind::Symlink) {
        let path = entry_path(&roots[e.root], &e.rel);
        let resolved = fs::canonicalize(&path).map_err(|_| {
            bad(
                &path,
                "symlink does not resolve, so its containment cannot be shown",
            )
        })?;
        if !roots.iter().any(|r| resolved.starts_with(r)) {
            return Err(bad(
                &path,
                format!(
                    "symlink resolves outside the sealed roots ({})",
                    resolved.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn ancestor_keys(root: &Path) -> Result<Vec<(PathBuf, FileKey)>> {
    let mut out = Vec::new();
    for ancestor in root.ancestors().skip(1) {
        let meta = fs::symlink_metadata(ancestor).map_err(|e| FsError::io(ancestor, e))?;
        if meta.file_type().is_symlink() {
            return Err(FsError::Symlink(ancestor.to_path_buf()));
        }
        out.push((ancestor.to_path_buf(), key_of(&meta)));
    }
    Ok(out)
}

/// After freezing, every directory must still list exactly the names the walk saw.
#[cfg(unix)]
fn recheck_membership(roots: &[PathBuf], entries: &[Entry]) -> Result<()> {
    for e in entries.iter().filter(|e| e.kind == Kind::Dir) {
        let path = entry_path(&roots[e.root], &e.rel);
        if dir_names(&path)? != e.children {
            return Err(bad(&path, "directory membership changed while sealing"));
        }
    }
    Ok(())
}

/// The metadata every later check compares with, taken after the flags are in place (setting a flag
/// moves the change time).
#[cfg(unix)]
fn snapshot(roots: &[PathBuf], entries: &mut [Entry]) -> Result<()> {
    for e in entries.iter_mut() {
        let path = entry_path(&roots[e.root], &e.rel);
        let meta = fs::symlink_metadata(&path).map_err(|e| FsError::io(&path, e))?;
        if key_of(&meta) != e.key || meta.len() != e.size && e.kind == Kind::File {
            return Err(bad(&path, "changed while sealing"));
        }
        e.ctime = ctime_of(&meta);
        e.mode = mode_of(&meta);
        e.nlink = nlink_of(&meta);
        e.flags = if e.kind == Kind::Symlink {
            0
        } else {
            flags_of_meta(&meta).unwrap_or(0)
        };
    }
    Ok(())
}

#[cfg(unix)]
fn pin_roots(
    roots: &[PathBuf],
    entries: &[Entry],
    ancestors: Vec<Vec<(PathBuf, FileKey)>>,
) -> Result<Vec<Pin>> {
    let mut pins = Vec::new();
    for (index, (root, ancestors)) in roots.iter().zip(ancestors).enumerate() {
        let top = entries
            .iter()
            .find(|e| e.root == index && e.rel.as_os_str().is_empty())
            .ok_or_else(|| bad(root, "root missing from the walk"))?;
        let file = open_nofollow(root, top.kind == Kind::Dir).map_err(|e| FsError::io(root, e))?;
        let key = key_of(&file.metadata().map_err(|e| FsError::io(root, e))?);
        if key != top.key {
            return Err(bad(root, "changed while sealing"));
        }
        pins.push(Pin {
            file,
            key,
            ancestors,
        });
    }
    Ok(pins)
}

/// SHA-256 over a canonical manifest: no device, inode or time, so the digest of the same bytes is
/// the same on every host. Per entry: root index, relative path, kind, permission bits, size, then
/// the file digest or the link target.
fn tree_digest(roots: &[PathBuf], entries: &[Entry]) -> String {
    let mut order: Vec<&Entry> = entries.iter().collect();
    order.sort_by(|a, b| (a.root, &a.rel).cmp(&(b.root, &b.rel)));
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"rightkit-toolchain-lease-v1\n");
    hasher.update(format!("roots\t{}\n", roots.len()).as_bytes());
    for e in order {
        let detail = match e.kind {
            Kind::File => e.digest.clone().unwrap_or_default(),
            Kind::Symlink => e
                .target
                .as_ref()
                .map(|t| t.display().to_string())
                .unwrap_or_default(),
            Kind::Dir => String::new(),
        };
        let line = format!(
            "{}\t{}\t{}\t{:o}\t{}\t{}\n",
            e.root,
            e.rel.display().to_string().replace('\\', "/"),
            e.kind.label(),
            e.mode,
            e.size,
            detail
        );
        hasher.update(line.as_bytes());
    }
    hasher.finalize()
}

fn lease_id(tree_digest: &str, sealed_at: u64) -> String {
    let mut hasher = Sha256Hasher::new();
    hasher.update(tree_digest.as_bytes());
    hasher.update(format!("\t{sealed_at}\t{}", std::process::id()).as_bytes());
    hasher.finalize()[..32].to_string()
}

fn root_evidence(roots: &[PathBuf], entries: &[Entry]) -> Vec<RootEvidence> {
    roots
        .iter()
        .enumerate()
        .map(|(index, root)| {
            let mut ev = RootEvidence {
                path: root.display().to_string(),
                files: 0,
                directories: 0,
                symlinks: 0,
                bytes: 0,
            };
            for e in entries.iter().filter(|e| e.root == index) {
                match e.kind {
                    Kind::File => {
                        ev.files += 1;
                        ev.bytes += e.size;
                    }
                    Kind::Dir => ev.directories += 1,
                    Kind::Symlink => ev.symlinks += 1,
                }
            }
            ev
        })
        .collect()
}
