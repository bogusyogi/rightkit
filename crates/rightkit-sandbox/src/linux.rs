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

use crate::seccomp::{self, SockFilter};
use crate::{ConfinementMode, LandlockMode, NetworkPolicy, SandboxPolicy, SeccompMode};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;

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
    for p in SYSTEM_EXEC {
        add_rule(raw, Path::new(p), EXEC, abi)?;
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
fn network_only_filter() -> io::Result<Vec<SockFilter>> {
    seccomp::network_only_program(&seccomp::X86_64_NETWORK)
}

#[cfg(target_arch = "aarch64")]
fn network_only_filter() -> io::Result<Vec<SockFilter>> {
    seccomp::network_only_program(&seccomp::AARCH64_NETWORK)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn network_only_filter() -> io::Result<Vec<SockFilter>> {
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
        let seccomp = if policy.network == NetworkPolicy::Deny {
            Some(match policy.seccomp {
                SeccompMode::Allowlist => allowlist_filter()?,
                SeccompMode::NetworkOnly => network_only_filter()?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seccomp::interpreter::run;

    /// x86_64 syscall numbers that can open or use a network connection.
    const NETWORK_SYSCALLS: &[(u32, &str)] = &[
        (41, "socket"),
        (42, "connect"),
        (43, "accept"),
        (44, "sendto"),
        (45, "recvfrom"),
        (46, "sendmsg"),
        (47, "recvmsg"),
        (49, "bind"),
        (50, "listen"),
        (53, "socketpair"),
        (288, "accept4"),
        (299, "recvmmsg"),
        (307, "sendmmsg"),
        (425, "io_uring_setup"),
        (426, "io_uring_enter"),
        (427, "io_uring_register"),
    ];

    // Linux egress isolation is enforced by omission: the seccomp filter is an allowlist, so a
    // network syscall is denied precisely because it is absent. That makes the property invisible
    // in the source, and a future "just add socketpair so tool X works" edit would silently open
    // egress. This test is the guard.
    #[test]
    fn network_syscalls_are_never_allowlisted() {
        for (nr, name) in NETWORK_SYSCALLS {
            assert!(
                !ALLOWED_SYSCALLS.contains(nr),
                "syscall {nr} ({name}) is allowlisted; the Linux sandbox would permit network egress"
            );
        }
    }

    #[test]
    fn allowlist_has_no_duplicate_entries() {
        let mut seen = ALLOWED_SYSCALLS.to_vec();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicate syscall numbers in allowlist");
    }

    #[test]
    fn allowlist_is_non_empty_and_covers_core_io() {
        assert!(!ALLOWED_SYSCALLS.is_empty());
        for (nr, name) in [(0u32, "read"), (1, "write"), (3, "close")] {
            assert!(
                ALLOWED_SYSCALLS.contains(&nr),
                "core syscall {nr} ({name}) must stay allowlisted"
            );
        }
    }

    #[test]
    fn the_real_allowlist_program_checks_the_architecture_first() {
        let prog = seccomp::allowlist_program(seccomp::AUDIT_ARCH_X86_64, ALLOWED_SYSCALLS)
            .expect("the real allowlist fits byte jumps");
        for &nr in ALLOWED_SYSCALLS {
            assert_eq!(
                run(&prog, seccomp::AUDIT_ARCH_X86_64, nr, 0),
                seccomp::SECCOMP_RET_ALLOW,
                "{nr}"
            );
            // The same number under i386 or as an x32 alias never reaches the allowlist.
            assert_eq!(
                run(&prog, 0x4000_0003, nr, 0),
                seccomp::SECCOMP_RET_KILL_PROCESS,
                "i386 {nr}"
            );
            assert_ne!(
                run(&prog, seccomp::AUDIT_ARCH_X86_64, nr | 0x4000_0000, 0),
                seccomp::SECCOMP_RET_ALLOW,
                "x32 {nr}"
            );
        }
        for (nr, name) in NETWORK_SYSCALLS {
            assert_ne!(
                run(&prog, seccomp::AUDIT_ARCH_X86_64, *nr, 2),
                seccomp::SECCOMP_RET_ALLOW,
                "{name}"
            );
        }
    }

    #[test]
    fn rights_follow_the_abi() {
        assert_eq!(supported_rights(1), ALL_V1);
        assert_eq!(supported_rights(2), ALL_V1 | FS_REFER);
        assert_eq!(supported_rights(3), ALL_V1 | FS_REFER | FS_TRUNCATE);
        assert_eq!(supported_rights(9), supported_rights(3));
        // Write grants carry REFER and TRUNCATE exactly when they are handled, so a handled-but-
        // ungranted right (read-only roots) stays denied.
        for abi in 1..=3 {
            let handled = supported_rights(abi);
            assert_eq!(READ & !handled, 0);
            assert_eq!(EXEC & !handled, 0);
            assert_eq!(handled & FS_REFER != 0, abi >= 2, "abi {abi}");
            assert_eq!(handled & FS_TRUNCATE != 0, abi >= 3, "abi {abi}");
        }
        // Read and exec roots never carry REFER or TRUNCATE.
        assert_eq!(EXEC & (FS_REFER | FS_TRUNCATE), 0);
    }

    #[test]
    fn a_rule_on_a_file_drops_the_directory_rights() {
        // The kernel answers EINVAL for directory rights on a file; the mask must remove them.
        let on_file = rights_for_object(READ | FS_EXECUTE, false, 1);
        assert_eq!(on_file, FS_READ_FILE | FS_EXECUTE);
        let write = rights_for_object(supported_rights(3), false, 3);
        assert_eq!(
            write,
            FS_EXECUTE | FS_WRITE_FILE | FS_READ_FILE | FS_TRUNCATE
        );
        assert_eq!(
            rights_for_object(supported_rights(1), false, 1) & FS_TRUNCATE,
            0
        );
        // A directory keeps everything it was given.
        assert_eq!(
            rights_for_object(supported_rights(3), true, 3),
            supported_rights(3)
        );
        // Nothing left means nothing to add.
        assert_eq!(rights_for_object(FS_READ_DIR | FS_MAKE_DIR, false, 3), 0);
    }

    #[test]
    fn write_only_mode_is_refused_not_approximated() {
        let policy = SandboxPolicy::new().mode(ConfinementMode::WriteOnly);
        let error = Prepared::new(&policy).err().expect("refused");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn a_pathless_network_only_policy_prepares_no_landlock_when_asked() {
        let policy = SandboxPolicy::new()
            .landlock(LandlockMode::SkipWhenNoPaths)
            .seccomp(SeccompMode::NetworkOnly);
        let prepared = Prepared::new(&policy).expect("network-only tier prepares anywhere");
        assert!(prepared.ruleset.is_none());
        assert!(prepared.seccomp.is_some());
        // No network restriction and no paths: nothing to apply at all.
        let open = SandboxPolicy::new()
            .landlock(LandlockMode::SkipWhenNoPaths)
            .network(NetworkPolicy::Allow);
        let prepared = Prepared::new(&open).unwrap();
        assert!(prepared.ruleset.is_none() && prepared.seccomp.is_none());
        prepared
            .apply()
            .expect("an empty confinement applies nothing");
    }

    #[test]
    fn a_listed_path_turns_landlock_on_even_in_skip_mode() {
        let policy = SandboxPolicy::new()
            .landlock(LandlockMode::SkipWhenNoPaths)
            .allow_read(std::env::temp_dir());
        match (landlock_abi_version(), Prepared::new(&policy)) {
            (Some(_), Ok(prepared)) => assert!(prepared.ruleset.is_some()),
            // No Landlock here: the launch must fail, never fall back to unconfined.
            (None, Err(error)) => assert_eq!(error.kind(), io::ErrorKind::Unsupported),
            (abi, outcome) => panic!("abi {abi:?} but prepare gave ok={}", outcome.is_ok()),
        }
    }

    #[test]
    fn the_abi_probe_agrees_with_availability() {
        assert_eq!(
            crate::is_landlock_available(),
            landlock_abi_version().is_some_and(|abi| abi >= 1)
        );
    }
}
