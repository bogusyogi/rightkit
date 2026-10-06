//! Linux backend: Landlock filesystem confinement + seccomp-BPF allowlist, ported
//! from CodeRight `linux_sandbox.rs` / `seccomp_landlock.rs`. Everything is
//! prepared in the parent; `Prepared::apply` (post-fork) performs syscalls only.
//! Fail closed: unavailable Landlock/seccomp is an error, never an unsandboxed run.
//! NOTE: compiled/tested only where a Linux host is available (see matrix.md).

use crate::{NetworkPolicy, SandboxPolicy};
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd};

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
const RULE_PATH_BENEATH: u32 = 1;
const SYS_CREATE_RULESET: libc::c_long = 444;
const SYS_ADD_RULE: libc::c_long = 445;
const SYS_RESTRICT_SELF: libc::c_long = 446;

const ALL: u64 = FS_EXECUTE | FS_WRITE_FILE | FS_READ_FILE | FS_READ_DIR | FS_REMOVE_DIR | FS_REMOVE_FILE
    | FS_MAKE_CHAR | FS_MAKE_DIR | FS_MAKE_REG | FS_MAKE_SOCK | FS_MAKE_FIFO | FS_MAKE_BLOCK | FS_MAKE_SYM;
const READ: u64 = FS_READ_FILE | FS_READ_DIR;
const EXEC: u64 = READ | FS_EXECUTE;
const SYSTEM_EXEC: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/proc", "/dev", "/sys"];

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}
#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

pub struct Prepared {
    ruleset: std::fs::File,
    seccomp: Option<Vec<libc::sock_filter>>,
}

impl Prepared {
    pub fn new(policy: &SandboxPolicy) -> std::io::Result<Self> {
        let attr = RulesetAttr { handled_access_fs: ALL };
        // SAFETY: valid attr pointer and size.
        let fd = unsafe { libc::syscall(SYS_CREATE_RULESET, &attr as *const RulesetAttr, std::mem::size_of::<RulesetAttr>(), 0) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            return Err(match err.raw_os_error() {
                Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP) => std::io::Error::other("Landlock unavailable; refusing unsandboxed execution"),
                _ => err,
            });
        }
        // Own the descriptor before any fallible rule installation.
        let ruleset = unsafe { std::fs::File::from_raw_fd(fd as i32) };
        let add = |path: &std::path::Path, access: u64| -> std::io::Result<()> {
            let Ok(c) = CString::new(path.to_string_lossy().into_owned()) else { return Ok(()) };
            let pfd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if pfd < 0 {
                return Ok(()); // missing path grants nothing
            }
            let pa = PathBeneathAttr { allowed_access: access, parent_fd: pfd };
            let r = unsafe { libc::syscall(SYS_ADD_RULE, ruleset.as_raw_fd(), RULE_PATH_BENEATH, &pa as *const PathBeneathAttr, 0) };
            unsafe { libc::close(pfd) };
            if r < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
        };
        for p in SYSTEM_EXEC {
            add(std::path::Path::new(p), EXEC)?;
        }
        for p in &policy.read_paths {
            add(p, READ)?;
        }
        for p in &policy.exec_paths {
            add(p, EXEC)?;
        }
        for p in &policy.write_paths {
            add(p, ALL)?;
        }
        let seccomp = if policy.network == NetworkPolicy::Deny { Some(prepare_seccomp_filter()?) } else { None };
        Ok(Self { ruleset, seccomp })
    }

    /// Post-fork, pre-exec: syscalls only.
    pub fn apply(&self) -> std::io::Result<()> {
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::syscall(SYS_RESTRICT_SELF, self.ruleset.as_raw_fd(), 0) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if let Some(program) = &self.seccomp {
            let filter = libc::sock_fprog { len: program.len() as u16, filter: program.as_ptr().cast_mut() };
            if unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &filter as *const _, 0, 0) } < 0 {
                return Err(std::io::Error::last_os_error());
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

/// Apply a seccomp-BPF allowlist for coding tasks (Linux x86_64).
///
/// Allowed syscalls cover the needs of a compiler/test runner: file I/O,
/// process management, memory, signals, clocks, and IPC primitives. Everything
/// else returns EPERM. ENOSYS/EINVAL from the seccomp() syscall itself means the
/// kernel cannot enforce the requested strict tier, so launch fails closed.
#[cfg(target_os = "linux")]
fn prepare_seccomp_filter() -> std::io::Result<Vec<libc::sock_filter>> {
    use libc;

    // BPF instruction encoding constants (linux/filter.h).
    const BPF_LD: u16 = 0x00;
    const BPF_W: u16 = 0x00;
    const BPF_ABS: u16 = 0x20;
    const BPF_JMP: u16 = 0x05;
    const BPF_JEQ: u16 = 0x10;
    const BPF_K: u16 = 0x00;
    const BPF_RET: u16 = 0x06;

    // seccomp return values.
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const EPERM_VAL: u32 = 1;

    // A requested strict tier cannot silently omit the filter.
    #[cfg(not(target_arch = "x86_64"))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "strict seccomp allowlist is unavailable for this architecture",
    ));

    #[cfg(target_arch = "x86_64")]
    {
        const SYSCALL_NR_OFFSET: u32 = 0;

        // Allowlisted syscall numbers for a compiler/test/editor workload.
        let allowed: &[u32] = ALLOWED_SYSCALLS;

        let n = allowed.len();
        let mut prog: Vec<libc::sock_filter> = Vec::with_capacity(n + 3);

        // Load syscall number from seccomp_data at offset 0.
        prog.push(libc::sock_filter {
            code: (BPF_LD | BPF_W | BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: SYSCALL_NR_OFFSET,
        });

        // For each allowed syscall: if NR matches, jump forward to the ALLOW return.
        // The ALLOW instruction is always the last one; jump distance decreases as we go.
        for (i, &nr) in allowed.iter().enumerate() {
            let jump_to_allow = (n - i) as u8;
            prog.push(libc::sock_filter {
                code: (BPF_JMP | BPF_JEQ | BPF_K) as u16,
                jt: jump_to_allow,
                jf: 0,
                k: nr,
            });
        }

        // Default: deny with EPERM.
        prog.push(libc::sock_filter {
            code: BPF_RET as u16,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ERRNO | EPERM_VAL,
        });

        // Allow.
        prog.push(libc::sock_filter {
            code: BPF_RET as u16,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        });

        Ok(prog)
    }
}

