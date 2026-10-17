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
/// Anything else is allowed, including `connect` and `sendto`, which need a socket the process
/// can no longer create (an inherited socket descriptor stays the caller's responsibility).
pub(crate) fn network_only_program(nr: &NetworkNumbers) -> io::Result<Vec<SockFilter>> {
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
        chain.push((jump_eq(number, 0, 0), Goto::SocketCheck));
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
    debug_assert_eq!(prog.len(), allow_at + 1);
    Ok(prog)
}

#[cfg(test)]
pub(crate) mod interpreter {
    //! A minimal classic-BPF evaluator over `seccomp_data`, for the program tests.
    use super::*;

    /// Run `prog` for a syscall `nr` made under `arch` with first argument `arg0`.
    pub(crate) fn run(prog: &[SockFilter], arch: u32, nr: u32, arg0: u64) -> u32 {
        let mut acc = 0u32;
        let mut pc = 0usize;
        // A classic-BPF program cannot loop; the bound is only a guard against a broken test.
        for _ in 0..prog.len() + 1 {
            let insn = prog[pc];
            pc += 1;
            match insn.code {
                c if c == BPF_LD | BPF_W | BPF_ABS => {
                    acc = match insn.k {
                        OFFSET_NR => nr,
                        OFFSET_ARCH => arch,
                        OFFSET_ARG0_LOW => arg0 as u32,
                        other => panic!("filter reads unexpected seccomp_data offset {other}"),
                    };
                }
                c if c == BPF_JMP | BPF_JEQ | BPF_K => {
                    pc += usize::from(if acc == insn.k { insn.jt } else { insn.jf });
                }
                c if c == BPF_JMP | BPF_JGE | BPF_K => {
                    pc += usize::from(if acc >= insn.k { insn.jt } else { insn.jf });
                }
                c if c == BPF_RET | BPF_K => return insn.k,
                other => panic!("unsupported BPF opcode {other:#x}"),
            }
            assert!(pc < prog.len(), "jump past the end of the program");
        }
        panic!("program did not terminate");
    }
}

#[cfg(test)]
mod tests {
    use super::interpreter::run;
    use super::*;

    const I386: u32 = 0x4000_0003;
    const ARM: u32 = 0x4000_0028;

    fn net(prog: &[SockFilter], arch: u32, nr: u32, arg0: u64) -> u32 {
        run(prog, arch, nr, arg0)
    }

    #[test]
    fn allowlist_allows_listed_and_denies_the_rest() {
        let prog = allowlist_program(AUDIT_ARCH_X86_64, &[0, 1, 60]).unwrap();
        for nr in [0, 1, 60] {
            assert_eq!(
                run(&prog, AUDIT_ARCH_X86_64, nr, 0),
                SECCOMP_RET_ALLOW,
                "{nr}"
            );
        }
        for nr in [2, 41, 42, 59, 425] {
            assert_eq!(
                run(&prog, AUDIT_ARCH_X86_64, nr, 0),
                SECCOMP_RET_DENY,
                "{nr}"
            );
        }
    }

    #[test]
    fn allowlist_kills_a_foreign_architecture_even_for_a_listed_number() {
        let prog = allowlist_program(AUDIT_ARCH_X86_64, &[0, 1, 60]).unwrap();
        // i386 `exit` is 1: the same number is `write` on x86_64. Under another architecture it
        // must never reach the allowlist.
        for arch in [I386, ARM, AUDIT_ARCH_AARCH64, 0] {
            assert_eq!(
                run(&prog, arch, 1, 0),
                SECCOMP_RET_KILL_PROCESS,
                "arch {arch:#x}"
            );
        }
    }

    #[test]
    fn allowlist_denies_the_x32_alias_of_an_allowed_number() {
        let prog = allowlist_program(AUDIT_ARCH_X86_64, &[0, 1, 60]).unwrap();
        for nr in [0u32, 1, 60] {
            assert_eq!(
                run(&prog, AUDIT_ARCH_X86_64, nr | X32_SYSCALL_BIT, 0),
                SECCOMP_RET_DENY,
                "{nr}"
            );
        }
    }

    #[test]
    fn allowlist_of_the_whole_real_table_fits_byte_jumps() {
        let all: Vec<u32> = (0..250).collect();
        assert!(allowlist_program(AUDIT_ARCH_X86_64, &all).is_ok());
        // One more than a byte can span: refuse instead of wrapping a jump.
        let too_many: Vec<u32> = (0..300).collect();
        assert!(allowlist_program(AUDIT_ARCH_X86_64, &too_many).is_err());
    }

    #[test]
    fn network_only_allows_ordinary_syscalls() {
        let prog = network_only_program(&X86_64_NETWORK).unwrap();
        for nr in [0u32, 1, 2, 3, 59, 60, 231, 257, 332] {
            assert_eq!(
                net(&prog, AUDIT_ARCH_X86_64, nr, 0),
                SECCOMP_RET_ALLOW,
                "{nr}"
            );
        }
    }

    #[test]
    fn network_only_denies_every_non_unix_socket_domain() {
        let prog = network_only_program(&X86_64_NETWORK).unwrap();
        // AF_INET, AF_AX25, AF_IPX, AF_APPLETALK, AF_INET6, AF_NETLINK, AF_PACKET, AF_VSOCK,
        // AF_BLUETOOTH, AF_TIPC, AF_ALG, AF_XDP, and out-of-range values.
        for domain in [2u64, 3, 4, 5, 10, 16, 17, 40, 31, 30, 38, 44, 0, 45, 255] {
            for nr in [X86_64_NETWORK.socket, X86_64_NETWORK.socketpair] {
                assert_eq!(
                    net(&prog, AUDIT_ARCH_X86_64, nr, domain),
                    SECCOMP_RET_DENY,
                    "nr {nr} domain {domain}"
                );
            }
        }
    }

    #[test]
    fn network_only_keeps_unix_sockets() {
        let prog = network_only_program(&X86_64_NETWORK).unwrap();
        for nr in [X86_64_NETWORK.socket, X86_64_NETWORK.socketpair] {
            assert_eq!(
                net(&prog, AUDIT_ARCH_X86_64, nr, 1),
                SECCOMP_RET_ALLOW,
                "{nr}"
            );
        }
    }

    #[test]
    fn network_only_judges_the_domain_by_its_low_32_bits_like_the_kernel() {
        let prog = network_only_program(&X86_64_NETWORK).unwrap();
        // `int domain`: the kernel ignores the upper half of the register. Garbage up there must
        // neither turn AF_INET into AF_UNIX nor AF_UNIX into a denial.
        let garbage = 0xdead_beef_u64 << 32;
        assert_eq!(
            net(&prog, AUDIT_ARCH_X86_64, X86_64_NETWORK.socket, garbage | 2),
            SECCOMP_RET_DENY
        );
        assert_eq!(
            net(&prog, AUDIT_ARCH_X86_64, X86_64_NETWORK.socket, garbage | 1),
            SECCOMP_RET_ALLOW
        );
    }

    #[test]
    fn network_only_denies_io_uring() {
        let prog = network_only_program(&X86_64_NETWORK).unwrap();
        for nr in [425u32, 426, 427] {
            assert_eq!(
                net(&prog, AUDIT_ARCH_X86_64, nr, 0),
                SECCOMP_RET_DENY,
                "{nr}"
            );
        }
    }

    #[test]
    fn network_only_kills_a_foreign_architecture() {
        // 32-bit `socketcall` is 102 on i386, and its numbers differ: a filter that skipped the
        // architecture check would let this through as "not socket".
        let prog = network_only_program(&X86_64_NETWORK).unwrap();
        for arch in [I386, ARM, AUDIT_ARCH_AARCH64, 0] {
            for nr in [0u32, 41, 102, 359] {
                assert_eq!(
                    net(&prog, arch, nr, 2),
                    SECCOMP_RET_KILL_PROCESS,
                    "arch {arch:#x} nr {nr}"
                );
            }
        }
    }

    #[test]
    fn network_only_denies_the_whole_x32_range() {
        let prog = network_only_program(&X86_64_NETWORK).unwrap();
        // x32 `socket` is 0x4000_0029: not 41, so only the range check stops it.
        for nr in [41u32, 0, 1, 425] {
            assert_eq!(
                net(&prog, AUDIT_ARCH_X86_64, nr | X32_SYSCALL_BIT, 2),
                SECCOMP_RET_DENY,
                "x32 {nr}"
            );
        }
        assert_eq!(
            net(&prog, AUDIT_ARCH_X86_64, 0x7fff_ffff, 0),
            SECCOMP_RET_DENY,
            "far above the table"
        );
    }

    #[test]
    fn network_only_on_aarch64_uses_its_own_numbers_and_has_no_x32_rule() {
        let prog = network_only_program(&AARCH64_NETWORK).unwrap();
        assert_eq!(net(&prog, AUDIT_ARCH_AARCH64, 198, 2), SECCOMP_RET_DENY);
        assert_eq!(net(&prog, AUDIT_ARCH_AARCH64, 198, 1), SECCOMP_RET_ALLOW);
        assert_eq!(net(&prog, AUDIT_ARCH_AARCH64, 199, 2), SECCOMP_RET_DENY);
        assert_eq!(net(&prog, AUDIT_ARCH_AARCH64, 426, 0), SECCOMP_RET_DENY);
        // x86_64's `socket` number is another syscall here and stays allowed.
        assert_eq!(net(&prog, AUDIT_ARCH_AARCH64, 41, 2), SECCOMP_RET_ALLOW);
        // An x86_64 caller is the wrong architecture for this program.
        assert_eq!(
            net(&prog, AUDIT_ARCH_X86_64, 41, 2),
            SECCOMP_RET_KILL_PROCESS
        );
    }

    #[test]
    fn programs_have_every_jump_inside_and_end_in_a_return() {
        for prog in [
            network_only_program(&X86_64_NETWORK).unwrap(),
            network_only_program(&AARCH64_NETWORK).unwrap(),
            allowlist_program(AUDIT_ARCH_X86_64, &[0, 1, 2]).unwrap(),
        ] {
            assert_eq!(prog.last().unwrap().code, BPF_RET | BPF_K);
            for (i, insn) in prog.iter().enumerate() {
                if insn.code & 0x07 == BPF_JMP {
                    assert!(i + 1 + usize::from(insn.jt) < prog.len(), "jt at {i}");
                    assert!(i + 1 + usize::from(insn.jf) < prog.len(), "jf at {i}");
                }
            }
            assert!(prog.len() < 4096, "BPF_MAXINSNS");
        }
    }
}
