//! Linux backend: Landlock filesystem confinement + seccomp-BPF, ported from CodeRight
//! `linux_sandbox.rs` / `seccomp_landlock.rs`. Everything is prepared in the parent;
//! `Prepared::apply` (post-fork) performs syscalls only.
//!
//! Fail closed: an unavailable Landlock or seccomp is an error, never an unsandboxed run. The one
//! documented exception is [`LandlockMode::SkipWhenNoPaths`], an explicit opt-in for network-only
//! tiers that list no path at all.
//!
//! Landlock ABI. The ruleset handles whatever the running kernel supports: ABI 2 adds `REFER`
//! (renaming or linking across directories is denied outside the granted trees) and ABI 3 adds
//! `TRUNCATE` (truncating a file needs the right too). On an older kernel those operations keep
//! the kernel's behavior for that ABI; [`landlock_abi_version`] reports what is in force.
//!
//! seccomp. With the network denied the child gets either the default-deny allowlist or the
//! network-only filter ([`SeccompMode`]); both begin with an architecture check (see
//! `seccomp.rs`). NOTE: compiled/tested only where a Linux host is available (see matrix.md).

use crate::deny_read::glob_literal_prefix;
use crate::seccomp::{self, SockFilter};
use crate::{
    path_is_within, ConfinementMode, DenyReadEntry, LandlockMode, NetworkPolicy, SandboxPolicy,
    SeccompMode, SystemRoots, UnixSocketPolicy,
};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};

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
/// ABI 2.
const FS_REFER: u64 = 1 << 13;
/// ABI 3.
const FS_TRUNCATE: u64 = 1 << 14;
const RULE_PATH_BENEATH: u32 = 1;
const CREATE_RULESET_VERSION: u32 = 1 << 0;
const SYS_CREATE_RULESET: libc::c_long = 444;
const SYS_ADD_RULE: libc::c_long = 445;
const SYS_RESTRICT_SELF: libc::c_long = 446;

/// Every ABI-1 right.
const ALL_V1: u64 = FS_EXECUTE
    | FS_WRITE_FILE
    | FS_READ_FILE
    | FS_READ_DIR
    | FS_REMOVE_DIR
    | FS_REMOVE_FILE
    | FS_MAKE_CHAR
    | FS_MAKE_DIR
    | FS_MAKE_REG
    | FS_MAKE_SOCK
    | FS_MAKE_FIFO
    | FS_MAKE_BLOCK
    | FS_MAKE_SYM;
const READ: u64 = FS_READ_FILE | FS_READ_DIR;
const EXEC: u64 = READ | FS_EXECUTE;
const SYSTEM_EXEC: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/proc", "/dev", "/sys",
];

/// The roots Landlock grants read and execute beneath besides the policy's own paths.
fn system_roots_for(policy: &SandboxPolicy) -> Vec<PathBuf> {
    match &policy.system_roots {
        SystemRoots::Default => SYSTEM_EXEC.iter().map(PathBuf::from).collect(),
        SystemRoots::Minimal(roots) => roots.clone(),
        SystemRoots::None => Vec::new(),
    }
}

/// Landlock only adds allowances, so an exec deny can be honoured only when nothing grants
/// execution over the denied path. Refuse (never run with the deny silently unenforced) when a
/// root that grants execution contains, or is contained by, a denied path.
fn check_deny_exec(
    policy: &SandboxPolicy,
    system_roots: &[PathBuf],
    ruleset_applies: bool,
) -> io::Result<()> {
    if policy.deny_exec.is_empty() {
        return Ok(());
    }
    let refuse = |why: String| {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "deny_exec cannot be enforced under Landlock ({why}); grant narrower exec roots, \
                 or use macOS Seatbelt"
            ),
        ))
    };
    if !ruleset_applies {
        return refuse("Landlock is skipped for a policy that lists no path".into());
    }
    if policy.exec_any {
        return refuse("exec_any grants execution everywhere".into());
    }
    let grants: Vec<&Path> = system_roots
        .iter()
        .chain(&policy.exec_paths)
        .chain(&policy.write_paths)
        .map(PathBuf::as_path)
        .collect();
    for entry in &policy.deny_exec {
        let target = match entry {
            DenyReadEntry::Path(path) => path.clone(),
            DenyReadEntry::Glob(pattern) => glob_literal_prefix(pattern),
        };
        for root in &grants {
            if path_is_within(&target, root) || path_is_within(root, &target) {
                return refuse(format!(
                    "{} overlaps the exec grant {}",
                    target.display(),
                    root.display()
                ));
            }
        }
    }
    Ok(())
}

/// The rights the running ABI can express: ABI 2 adds `REFER`, ABI 3 adds `TRUNCATE`.
fn supported_rights(abi: u32) -> u64 {
    let mut all = ALL_V1;
    if abi >= 2 {
        all |= FS_REFER;
    }
    if abi >= 3 {
        all |= FS_TRUNCATE;
    }
    all
}

/// The rights a rule on a non-directory may carry; the kernel rejects directory rights there
/// (`EINVAL`).
fn file_rights(abi: u32) -> u64 {
    let mut rights = FS_EXECUTE | FS_WRITE_FILE | FS_READ_FILE;
    if abi >= 3 {
        rights |= FS_TRUNCATE;
    }
    rights
}

/// Narrow `access` to what a rule on this kind of object may carry.
fn rights_for_object(access: u64, is_dir: bool, abi: u32) -> u64 {
    if is_dir {
        access
    } else {
        access & file_rights(abi)
    }
}

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}
#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// The Landlock ABI version the running kernel offers, or why it offers none.
pub(crate) fn landlock_abi() -> io::Result<u32> {
    // SAFETY: a version query takes a null attribute, size 0 and the VERSION flag.
    let version = unsafe {
        libc::syscall(
            SYS_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    };
    if version < 1 {
        return Err(io::Error::last_os_error());
    }
    Ok(u32::try_from(version).unwrap_or(u32::MAX))
}

/// The Landlock ABI version in force, or `None` when Landlock is unavailable here.
pub(crate) fn landlock_abi_version() -> Option<u32> {
    landlock_abi().ok()
}

pub struct Prepared {
    ruleset: Option<std::fs::File>,
    seccomp: Option<Vec<SockFilter>>,
}

fn add_rule(ruleset: i32, path: &Path, access: u64, abi: u32) -> io::Result<()> {
    let Ok(c) = CString::new(path.to_string_lossy().into_owned()) else {
        return Ok(());
    };
    // SAFETY: `c` is a valid NUL-terminated path.
    let pfd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if pfd < 0 {
        return Ok(()); // a missing path grants nothing
    }
    let result = (|| {
        // SAFETY: `pfd` is a live descriptor and `st` a valid out-pointer; fstat works on O_PATH.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(pfd, &mut st) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let is_dir = (st.st_mode & libc::S_IFMT) == libc::S_IFDIR;
        let access = rights_for_object(access, is_dir, abi);
        if access == 0 {
            return Ok(());
        }
        let pa = PathBeneathAttr {
            allowed_access: access,
            parent_fd: pfd,
        };
        // SAFETY: valid ruleset descriptor and attribute pointer.
        let r = unsafe {
            libc::syscall(
                SYS_ADD_RULE,
                ruleset,
                RULE_PATH_BENEATH,
                &pa as *const PathBeneathAttr,
                0,
            )
        };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })();
    // SAFETY: `pfd` was opened above and is closed exactly once.
    unsafe { libc::close(pfd) };
    result
}

/// Landlock ruleset for `policy`, or `None` when the mode skips it for a path-less policy.
fn build_ruleset(policy: &SandboxPolicy) -> io::Result<Option<std::fs::File>> {
    let paths_listed = !policy.read_paths.is_empty()
        || !policy.write_paths.is_empty()
        || !policy.exec_paths.is_empty();
    if policy.landlock == LandlockMode::SkipWhenNoPaths && !paths_listed {
        return Ok(None);
    }
    let abi = landlock_abi().map_err(|error| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Landlock unavailable ({error}); refusing unsandboxed execution"),
        )
    })?;
    let handled = supported_rights(abi);
    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    // SAFETY: valid attr pointer and size.
    let fd = unsafe {
        libc::syscall(
            SYS_CREATE_RULESET,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // Own the descriptor before any fallible rule installation.
    // SAFETY: `fd` is a fresh descriptor owned by no other value.
    let ruleset = unsafe { std::fs::File::from_raw_fd(fd as i32) };
    let raw = ruleset.as_raw_fd();
    for p in system_roots_for(policy) {
        add_rule(raw, &p, EXEC, abi)?;
    }
    for p in &policy.read_paths {
        add_rule(raw, p, READ, abi)?;
    }
    for p in &policy.exec_paths {
        add_rule(raw, p, EXEC, abi)?;
    }
    for p in &policy.write_paths {
        add_rule(raw, p, handled, abi)?;
    }
    if policy.exec_any {
        // Execution anywhere; reading an image still needs a read grant.
        add_rule(raw, Path::new("/"), FS_EXECUTE, abi)?;
    }
    Ok(Some(ruleset))
}

#[cfg(target_arch = "x86_64")]
fn allowlist_filter() -> io::Result<Vec<SockFilter>> {
    seccomp::allowlist_program(seccomp::AUDIT_ARCH_X86_64, ALLOWED_SYSCALLS)
}

#[cfg(not(target_arch = "x86_64"))]
fn allowlist_filter() -> io::Result<Vec<SockFilter>> {
    // A requested strict tier cannot silently omit the filter.
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "strict seccomp allowlist is unavailable for this architecture",
    ))
}

#[cfg(target_arch = "x86_64")]
fn network_only_filter(deny_unix: bool) -> io::Result<Vec<SockFilter>> {
    seccomp::network_only_program(&seccomp::X86_64_NETWORK, deny_unix)
}

#[cfg(target_arch = "aarch64")]
fn network_only_filter(deny_unix: bool) -> io::Result<Vec<SockFilter>> {
    seccomp::network_only_program(&seccomp::AARCH64_NETWORK, deny_unix)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn network_only_filter(_deny_unix: bool) -> io::Result<Vec<SockFilter>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the network-only seccomp filter is unavailable for this architecture",
    ))
}

impl Prepared {
    pub fn new(policy: &SandboxPolicy) -> io::Result<Self> {
        if policy.mode == ConfinementMode::WriteOnly {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "ConfinementMode::WriteOnly is implemented for macOS only; refusing to run with a \
                 different boundary",
            ));
        }
        // `deny_read` is not enforceable under Landlock (allow-only): see `deny_read_enforced`.
        let ruleset = build_ruleset(policy)?;
        check_deny_exec(policy, &system_roots_for(policy), ruleset.is_some())?;
        if policy.network == NetworkPolicy::Deny {
            if let UnixSocketPolicy::Allow(paths) = &policy.unix_sockets {
                if !paths.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "UnixSocketPolicy::Allow cannot be enforced on Linux: seccomp cannot \
                         filter connect() by path; use UnixSocketPolicy::Deny or Local",
                    ));
                }
            }
        }
        let deny_unix = matches!(policy.unix_sockets, UnixSocketPolicy::Deny)
            || matches!(&policy.unix_sockets, UnixSocketPolicy::Allow(paths) if paths.is_empty());
        let seccomp = if policy.network == NetworkPolicy::Deny {
            Some(match policy.seccomp {
                SeccompMode::Allowlist => allowlist_filter()?,
                SeccompMode::NetworkOnly => network_only_filter(deny_unix)?,
            })
        } else {
            None
        };
        Ok(Self { ruleset, seccomp })
    }

    /// Post-fork, pre-exec: syscalls only.
    pub fn apply(&self) -> io::Result<()> {
        if self.ruleset.is_none() && self.seccomp.is_none() {
            return Ok(());
        }
        // SAFETY: plain prctl with constant arguments.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if let Some(ruleset) = &self.ruleset {
            // SAFETY: valid ruleset descriptor.
            if unsafe { libc::syscall(SYS_RESTRICT_SELF, ruleset.as_raw_fd(), 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        if let Some(program) = &self.seccomp {
            let filter = libc::sock_fprog {
                len: program.len() as u16,
                filter: program.as_ptr().cast::<libc::sock_filter>().cast_mut(),
            };
            // SAFETY: `filter` points at `program`, which outlives the call; `SockFilter` has
            // the layout of `struct sock_filter`.
            if unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &filter as *const _, 0, 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

/// Syscalls the confined process may make, by x86_64 syscall number.
///
/// This is an allowlist: anything absent returns EPERM. Network syscalls
/// (socket/connect/bind/sendto/…) are deliberately omitted, which is what
/// denies egress on Linux — there is no separate network rule to read, so
/// `network_syscalls_are_never_allowlisted` guards the property directly.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
const ALLOWED_SYSCALLS: &[u32] = &[
    0,   // read
    1,   // write
    2,   // open
    3,   // close
    4,   // stat
    5,   // fstat
    6,   // lstat
    7,   // poll
    8,   // lseek
    9,   // mmap
    10,  // mprotect
    11,  // munmap
    12,  // brk
    13,  // rt_sigaction
    14,  // rt_sigprocmask
    15,  // rt_sigreturn
    16,  // ioctl
    17,  // pread64
    18,  // pwrite64
    19,  // readv
    20,  // writev
    21,  // access
    22,  // pipe
    23,  // select
    24,  // sched_yield
    25,  // mremap
    26,  // msync
    28,  // madvise
    32,  // dup
    33,  // dup2
    34,  // pause
    35,  // nanosleep
    37,  // alarm
    39,  // getpid
    40,  // sendfile
    56,  // clone
    57,  // fork
    58,  // vfork
    59,  // execve
    60,  // exit
    61,  // wait4
    62,  // kill
    63,  // uname
    72,  // fcntl
    73,  // flock
    74,  // fsync
    75,  // fdatasync
    76,  // truncate
    77,  // ftruncate
    78,  // getdents
    79,  // getcwd
    80,  // chdir
    81,  // fchdir
    82,  // rename
    83,  // mkdir
    84,  // rmdir
    85,  // creat
    86,  // link
    87,  // unlink
    88,  // symlink
    89,  // readlink
    90,  // chmod
    91,  // fchmod
    95,  // umask
    96,  // gettimeofday
    97,  // getrlimit
    98,  // getrusage
    99,  // sysinfo
    100, // times
    102, // getuid
    104, // getgid
    107, // geteuid
    108, // getegid
    110, // getppid
    111, // getpgrp
    112, // setsid
    131, // sigaltstack
    158, // arch_prctl
    186, // gettid
    200, // tkill
    202, // futex
    204, // sched_getaffinity
    217, // getdents64
    218, // set_tid_address
    228, // clock_gettime
    229, // clock_getres
    230, // clock_nanosleep
    231, // exit_group
    233, // epoll_ctl
    234, // tgkill
    257, // openat
    258, // mkdirat
    262, // newfstatat
    263, // unlinkat
    264, // renameat
    268, // fchmodat
    269, // faccessat
    281, // epoll_pwait
    285, // fallocate
    290, // eventfd2
    291, // epoll_create1
    292, // dup3
    293, // pipe2
    302, // prlimit64
    316, // renameat2
    318, // getrandom
    332, // statx
    334, // rseq (modern libc thread-local initialization)
    435, // clone3
    436, // close_range
    437, // openat2
    439, // faccessat2
];
