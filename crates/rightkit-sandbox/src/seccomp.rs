//! seccomp-BPF program assembly for the Linux backend.
//!
//! Pure: this builds instruction vectors and never calls into the kernel, so the programs are
//! unit-tested on every host with a small BPF interpreter (see the tests). `linux.rs` loads them.
//!
//! Every program starts by checking `seccomp_data.arch`. A syscall number means different things
//! per architecture, and a 64-bit process can enter the kernel through the 32-bit (`int 0x80`,
//! i386) or x32 (`0x4000_0000 | nr`) tables: without the architecture check a filter written for
//! x86_64 numbers is bypassed by calling the same service under another number. A foreign
//! architecture kills the process (`SECCOMP_RET_KILL_PROCESS`); on x86_64 the x32 range is
//! denied explicitly because it shares `AUDIT_ARCH_X86_64`.
// Both architecture tables are always built (and tested); a given target loads only one.
#![allow(dead_code)]

use std::io;

/// One classic-BPF instruction (`struct sock_filter`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_JGE: u16 = 0x30;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

pub(crate) const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
pub(crate) const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const EPERM: u32 = 1;
/// What a denied syscall returns: `EPERM`.
pub(crate) const SECCOMP_RET_DENY: u32 = SECCOMP_RET_ERRNO | EPERM;

pub(crate) const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
pub(crate) const AUDIT_ARCH_AARCH64: u32 = 0xC000_00B7;

// `struct seccomp_data { int nr; __u32 arch; __u64 instruction_pointer; __u64 args[6]; }`.
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
/// Low 32 bits of `args[0]` (little-endian, as on x86_64 and aarch64). `socket(int domain, ..)`
/// reads only those bits, so the filter must too.
const OFFSET_ARG0_LOW: u32 = 16;
/// x32 ABI syscalls carry this bit on top of the x86_64 architecture value.
const X32_SYSCALL_BIT: u32 = 0x4000_0000;
const AF_UNIX: u32 = 1;

/// Syscall numbers the network-only filter names, per architecture.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NetworkNumbers {
    pub arch: u32,
    pub socket: u32,
    pub socketpair: u32,
    pub io_uring_setup: u32,
    pub io_uring_enter: u32,
    pub io_uring_register: u32,
    /// Deny the whole x32 range (x86_64 only).
    pub deny_x32: bool,
}

/// x86_64 numbers (`arch/x86/entry/syscalls/syscall_64.tbl`).
pub(crate) const X86_64_NETWORK: NetworkNumbers = NetworkNumbers {
    arch: AUDIT_ARCH_X86_64,
    socket: 41,
    socketpair: 53,
    io_uring_setup: 425,
    io_uring_enter: 426,
    io_uring_register: 427,
    deny_x32: true,
};

/// aarch64 numbers (the generic table, `include/uapi/asm-generic/unistd.h`).
pub(crate) const AARCH64_NETWORK: NetworkNumbers = NetworkNumbers {
    arch: AUDIT_ARCH_AARCH64,
    socket: 198,
    socketpair: 199,
    io_uring_setup: 425,
    io_uring_enter: 426,
    io_uring_register: 427,
    deny_x32: false,
};

fn load(offset: u32) -> SockFilter {
    SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: offset,
    }
}

fn jump_eq(k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt,
        jf,
        k,
    }
}

fn jump_ge(k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter {
        code: BPF_JMP | BPF_JGE | BPF_K,
        jt,
        jf,
        k,
    }
}

fn ret(k: u32) -> SockFilter {
    SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k,
    }
}

/// The three instructions that open every program: kill anything not running `arch`, then load
/// the syscall number.
fn arch_prologue(arch: u32) -> [SockFilter; 4] {
    [
        load(OFFSET_ARCH),
        jump_eq(arch, 1, 0),
        ret(SECCOMP_RET_KILL_PROCESS),
        load(OFFSET_NR),
    ]
}

fn jump_offset(from: usize, to: usize) -> io::Result<u8> {
    to.checked_sub(from + 1)
        .and_then(|distance| u8::try_from(distance).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seccomp program too long for classic BPF byte jumps",
            )
        })
}

/// Default-deny program: only the syscalls in `allowed` return success; everything else returns
/// `EPERM`. `allowed` holds numbers for `arch`; a call made under any other architecture kills
/// the process, and x32 numbers (`nr | 0x4000_0000`) match no entry so they are denied.
pub(crate) fn allowlist_program(arch: u32, allowed: &[u32]) -> io::Result<Vec<SockFilter>> {
    let n = allowed.len();
    let mut prog = Vec::with_capacity(n + 6);
    prog.extend(arch_prologue(arch));
    let base = prog.len();
    // The ALLOW instruction is last, right after DENY; each jump skips the compares after it.
    let allow_at = base + n + 1;
    for (i, &nr) in allowed.iter().enumerate() {
        prog.push(jump_eq(nr, jump_offset(base + i, allow_at)?, 0));
    }
    prog.push(ret(SECCOMP_RET_DENY));
    prog.push(ret(SECCOMP_RET_ALLOW));
    Ok(prog)
}

/// Default-allow program that denies only the creation of network endpoints:
///
/// * `socket` and `socketpair` for every domain except `AF_UNIX` (local IPC keeps working);
/// * `io_uring_setup`/`enter`/`register`, which can open and use sockets without a socket
///   syscall;
/// * on x86_64, the whole x32 syscall range, which would otherwise reach `socket` under a number
///   this filter does not name.
///
/// With `deny_unix`, `AF_UNIX` is denied too: every `socket` and `socketpair` returns `EPERM`.
///
/// Anything else is allowed, including `connect` and `sendto`, which need a socket the process
/// can no longer create (an inherited socket descriptor stays the caller's responsibility).
pub(crate) fn network_only_program(
    nr: &NetworkNumbers,
    deny_unix: bool,
) -> io::Result<Vec<SockFilter>> {
    #[derive(Clone, Copy)]
    enum Goto {
        Deny,
        SocketCheck,
    }
    let mut chain: Vec<(SockFilter, Goto)> = Vec::new();
    if nr.deny_x32 {
        // Placeholder jump fields; resolved below once the layout is known.
        chain.push((jump_ge(X32_SYSCALL_BIT, 0, 0), Goto::Deny));
    }
    for number in [nr.io_uring_setup, nr.io_uring_enter, nr.io_uring_register] {
        chain.push((jump_eq(number, 0, 0), Goto::Deny));
    }
    for number in [nr.socket, nr.socketpair] {
        let target = if deny_unix {
            Goto::Deny
        } else {
            Goto::SocketCheck
        };
        chain.push((jump_eq(number, 0, 0), target));
    }

    let mut prog: Vec<SockFilter> = arch_prologue(nr.arch).to_vec();
    let chain_at = prog.len();
    let allow_default_at = chain_at + chain.len();
    let check_load_at = allow_default_at + 1;
    let check_jump_at = check_load_at + 1;
    let deny_at = check_jump_at + 1;
    let allow_at = deny_at + 1;
    for (i, (mut insn, target)) in chain.into_iter().enumerate() {
        let to = match target {
            Goto::Deny => deny_at,
            Goto::SocketCheck => check_load_at,
        };
        insn.jt = jump_offset(chain_at + i, to)?;
        prog.push(insn);
    }
    prog.push(ret(SECCOMP_RET_ALLOW)); // allow_default_at
    prog.push(load(OFFSET_ARG0_LOW)); // check_load_at
    prog.push(jump_eq(AF_UNIX, jump_offset(check_jump_at, allow_at)?, 0)); // check_jump_at
    prog.push(ret(SECCOMP_RET_DENY)); // deny_at
    prog.push(ret(SECCOMP_RET_ALLOW)); // allow_at
    Ok(prog)
}
