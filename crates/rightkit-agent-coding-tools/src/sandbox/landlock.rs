// RIGHTKIT-SERIOUS: Linux filesystem confinement stays local (report T-2). `rightkit_sandbox::confine_command` (0.5.0)
// always adds read and execute roots for `/usr /bin /sbin /lib /lib64 /etc /proc /dev /sys` to the Landlock
// ruleset (`SYSTEM_EXEC`, linux.rs:64, applied unconditionally at linux.rs:215) with no way to turn them off.
// `/proc` lets a confined child open `/proc/<pid>/environ` of every same-user process (API keys, tokens).
// The seccomp half (allowlist, architecture check, x32 denial, network-only filter) IS RightKit's; see
// `SandboxConfig::confine`.
//
// Landlock only (Linux, kernel 5.13+): prepared in the parent, applied in the child between
// fork and exec with syscalls only. Fails closed: when Landlock is unavailable and a path is
// configured the launch is refused, never run unconfined.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};

// Landlock ABI v1 access-rights bitflags (linux/landlock.h).
const FS_EXECUTE: u64 = 1 << 0;
const FS_WRITE_FILE: u64 = 1 << 1;
const FS_READ_FILE: u64 = 1 << 2;
const FS_READ_DIR: u64 = 1 << 3;
const FS_REMOVE_DIR: u64 = 1 << 4;
const FS_REMOVE_FILE: u64 = 1 << 5;
const FS_MAKE_CHAR: u64 = 1 << 6;
const FS_MAKE_DIR: u64 = 1 << 7;
const FS_MAKE_REG: u64 = 1 << 8;
const FS_MAKE_SOCK: u64 = 1 << 9;
const FS_MAKE_FIFO: u64 = 1 << 10;
const FS_MAKE_BLOCK: u64 = 1 << 11;
const FS_MAKE_SYM: u64 = 1 << 12;
// ABI v2 (kernel 5.19) cross-directory rename/link; ABI v3 (6.2) truncate.
const FS_REFER: u64 = 1 << 13;
const FS_TRUNCATE: u64 = 1 << 14;
const RULE_PATH_BENEATH: u32 = 1;
const CREATE_RULESET_VERSION: u32 = 1;

// Landlock syscall numbers (x86_64 and aarch64 share them).
const SYS_CREATE_RULESET: libc::c_long = 444;
const SYS_ADD_RULE: libc::c_long = 445;
const SYS_RESTRICT_SELF: libc::c_long = 446;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// A Landlock ruleset built in the parent. `apply` performs syscalls only.
pub(crate) struct PreparedLandlock {
    ruleset: Option<std::fs::File>,
}

impl PreparedLandlock {
    /// Build the ruleset for `read_paths` (read + execute) and `write_paths` (everything).
    /// With no path at all there is no ruleset (a network-only request). `withheld` lists existing
    /// paths beneath those roots that must stay unreadable (see [`add_root`]).
    pub(crate) fn new(
        read_paths: &[PathBuf],
        write_paths: &[PathBuf],
        withheld: &[PathBuf],
    ) -> io::Result<Self> {
        if read_paths.is_empty() && write_paths.is_empty() {
            return Ok(Self { ruleset: None });
        }
        // Probe the kernel ABI so truncate/refer are handled (denied outside allowed paths)
        // wherever supported; unknown bits would be EINVAL.
        // SAFETY: documented version query (null attr, size 0, version flag).
        let abi = unsafe {
            libc::syscall(
                SYS_CREATE_RULESET,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        let mut extra_rights: u64 = 0;
        if abi >= 2 {
            extra_rights |= FS_REFER;
        }
        if abi >= 3 {
            extra_rights |= FS_TRUNCATE;
        }
        let handled = extra_rights
            | FS_EXECUTE
            | FS_READ_FILE
            | FS_READ_DIR
            | FS_WRITE_FILE
            | FS_REMOVE_DIR
            | FS_REMOVE_FILE
            | FS_MAKE_CHAR
            | FS_MAKE_DIR
            | FS_MAKE_REG
            | FS_MAKE_SOCK
            | FS_MAKE_FIFO
            | FS_MAKE_BLOCK
            | FS_MAKE_SYM;
        let attr = RulesetAttr {
            handled_access_fs: handled,
        };
        // SAFETY: syscall with a valid attribute pointer and size.
        let fd = unsafe {
            libc::syscall(
                SYS_CREATE_RULESET,
                &attr as *const RulesetAttr,
                std::mem::size_of::<RulesetAttr>(),
                0,
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP) => Err(io::Error::other(
                    "Landlock unavailable; refusing unsandboxed execution",
                )),
                _ => Err(error),
            };
        }
        // Own the descriptor before any fallible rule installation.
        // SAFETY: `fd` is a fresh descriptor owned by no other value.
        let ruleset = unsafe { std::fs::File::from_raw_fd(fd as i32) };
        let raw = ruleset.as_raw_fd();

        let withheld: Vec<PathBuf> = withheld
            .iter()
            .filter_map(|path| std::fs::canonicalize(path).ok())
            .collect();
        // One rule budget per pass: the workspace is both a read and a write root, so a shared
        // budget would count every entry of a split folder twice.
        let mut read_budget = 0usize;
        let read_rights = FS_READ_FILE | FS_READ_DIR | FS_EXECUTE;
        for path in read_paths {
            add_root(raw, path, read_rights, abi, &withheld, &mut read_budget)?;
        }
        let mut write_budget = 0usize;
        let write_rights = handled;
        for path in write_paths {
            add_root(raw, path, write_rights, abi, &withheld, &mut write_budget)?;
        }
        Ok(Self {
            ruleset: Some(ruleset),
        })
    }

    /// Post-fork, pre-exec: syscalls only.
    pub(crate) fn apply(&self) -> io::Result<()> {
        let Some(ruleset) = &self.ruleset else {
            return Ok(());
        };
        // SAFETY: plain prctl with constant arguments.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: valid ruleset descriptor.
        if unsafe { libc::syscall(SYS_RESTRICT_SELF, ruleset.as_raw_fd(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Rights a rule on a non-directory may carry; the kernel rejects directory rights there
/// (`EINVAL`).
fn file_rights(abi: i64) -> u64 {
    let mut rights = FS_EXECUTE | FS_WRITE_FILE | FS_READ_FILE;
    if abi >= 3 {
        rights |= FS_TRUNCATE;
    }
    rights
}

/// Most rules one ruleset may carry when withheld paths force a directory to be split.
const MAX_RULES: usize = 4096;

/// Add the rule for root `path`, keeping every `withheld` path beneath it unreadable.
///
/// Landlock rights are a union across ancestor rules, so a withheld file cannot be carved
/// out of a granted directory. Instead the directory keeps its rights minus
/// `READ_FILE | EXECUTE` and each of its entries gets a rule of its own with the full rights
/// (recursively, down to the directory holding the withheld path, which is simply left
/// without a rule). Rules are bound to inodes, so a rename or hard link of a withheld file
/// stays unreadable. Files created during the command in a split folder are writable but not
/// readable until the next launch. The split directory keeps WRITE_FILE, TRUNCATE and
/// REMOVE_FILE, so the shell can still truncate or delete the withheld file; file tools deny
/// that separately. Symlink entries get no rule: `add_rule` opens with `O_PATH` and would
/// follow a link to the withheld file. A directory that cannot be listed gets nothing
/// beneath it (fail closed). With no `withheld` path this is exactly one `add_rule`.
fn add_root(
    ruleset: i32,
    path: &Path,
    rights: u64,
    abi: i64,
    withheld: &[PathBuf],
    budget: &mut usize,
) -> io::Result<()> {
    if withheld.is_empty() {
        return add_rule(ruleset, path, rights, abi);
    }
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if withheld.contains(&path) {
        return Ok(());
    }
    *budget += 1;
    if *budget > MAX_RULES {
        return Err(io::Error::other(
            "too many entries beside withheld secret files to confine; refusing the launch",
        ));
    }
    if !withheld.iter().any(|w| w.starts_with(&path)) {
        return add_rule(ruleset, &path, rights, abi);
    }
    add_rule(ruleset, &path, rights & !(FS_READ_FILE | FS_EXECUTE), abi)?;
    let Ok(entries) = std::fs::read_dir(&path) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(kind) if !kind.is_symlink() => {}
            _ => continue,
        }
        add_root(ruleset, &entry.path(), rights, abi, withheld, budget)?;
    }
    Ok(())
}

fn add_rule(ruleset: i32, path: &Path, access: u64, abi: i64) -> io::Result<()> {
    let Ok(c_path) = CString::new(path.to_string_lossy().into_owned()) else {
        return Ok(());
    };
    // SAFETY: `c_path` is a valid NUL-terminated path.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        // A missing path grants nothing.
        return Ok(());
    }
    let result = (|| {
        // SAFETY: `fd` is a live descriptor and `st` a valid out-pointer; fstat works on O_PATH.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let is_dir = (st.st_mode & libc::S_IFMT) == libc::S_IFDIR;
        let access = if is_dir {
            access
        } else {
            access & file_rights(abi)
        };
        if access == 0 {
            return Ok(());
        }
        let rule = PathBeneathAttr {
            allowed_access: access,
            parent_fd: fd,
        };
        // SAFETY: valid ruleset descriptor and attribute pointer.
        let ret = unsafe {
            libc::syscall(
                SYS_ADD_RULE,
                ruleset,
                RULE_PATH_BENEATH,
                &rule as *const PathBeneathAttr,
                0,
            )
        };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })();
    // SAFETY: `fd` was opened above and is closed exactly once.
    unsafe { libc::close(fd) };
    result
}
