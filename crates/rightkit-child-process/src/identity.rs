//! Who a pid names. The one place this crate decides that a pid it remembers still belongs to
//! the process it was remembered for.
//!
//! # The class of defect
//!
//! A pid, and the process group it leads, is only a name. Once the process it named is reaped
//! the kernel may give the number to an unrelated process, and a later `kill`, `waitpid` or
//! `waitid` on the remembered number then reaches that process: it signals unrelated work or
//! steals the exit status of an unrelated child. This process cannot stop *other* code in it
//! (a `waitpid(-1)` loop, a foreign `Child` wrapper) from reaping our child, so "the pid is
//! one of my children" proves nothing.
//!
//! # The rule
//!
//! Every pid this crate remembers is remembered together with an [`Identity`] read from the
//! kernel while the process was alive and ours. Before every signal, every wait-class system
//! call and every membership probe that names the pid, the crate calls [`describe`] and acts
//! only if the description is unchanged ([`Origin::check`]). A different or missing
//! description means the pid changed hands: the caller forgets the process and never touches
//! the number again. A process whose identity cannot be established is never acted on by pid;
//! on a host that has no identity source ([`SUPPORTED`] is false) the older rule applies
//! alone (see `unix_child`).
//!
//! The description of a zombie is readable on macOS (`proc_pidinfo` with `arg = 1`, checked
//! on Darwin 27: a zombie child returns its parent and start time, `arg = 0` returns nothing)
//! and on Linux (`/proc/<pid>/stat`), so the proof also holds at the moment of the reap.
//!
//! # Known limit (not closed in this release)
//!
//! Proof and act are two separate system calls: [`Origin::check`], then the `kill`, `waitid`
//! or `waitpid` that names the pid. Foreign code that reaps our child inside that gap, and a
//! replacement process that then takes the same pid, defeat the proof for that one call. The
//! window is microseconds wide and needs the pid space to wrap, but it is not zero, and no
//! user-space API available here closes it (macOS has no `kill` or `waitpid` conditional on a
//! start time; this crate does not use pidfds for signalling or for proof).
//!
//! Two related gaps have the same cause:
//!
//! * The identity is read right after the child is spawned, not by the spawn itself. Foreign
//!   code that reaps the child before that read, and a replacement that takes the pid first,
//!   would hand the replacement's description over as the baseline.
//! * `OwnedChild::while_unreaped` proves the leader and then runs a caller's closure that
//!   signals by pid (a descendant sweep). The closure runs under the child lock, so this
//!   crate cannot reap meanwhile, but foreign code still can.
//!
//! What the rule does guarantee: no path acts on a remembered pid without the proof above,
//! none blocks on a pid with the lock released, and a group signal is never sent after this
//! crate has itself reaped the leader. An application that embeds a foreign reaper
//! (`waitpid(-1)` loops) in the same process needs that reaper isolated from the children
//! this crate owns; another identity check cannot close it.

use std::io;

/// A process's parent and start time: together with its pid, a name that a recycled pid
/// cannot reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) parent: u32,
    /// macOS: start time, seconds. Linux: start time in clock ticks since boot.
    pub(crate) started_sec: u64,
    /// macOS: microseconds. Linux: 0.
    pub(crate) started_usec: u64,
}

/// Whether this host can describe a process at all.
pub(crate) const SUPPORTED: bool = cfg!(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android"
));

/// What the crate knows about the process behind a remembered pid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// Described while it was ours.
    Known(Identity),
    /// The host has no identity source; only the unreaped-child rule applies.
    Unavailable,
    /// The host has one but would not describe the process at spawn. Nothing may be done to
    /// the pid by number.
    Missing,
}

/// The verdict of [`Origin::check`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Proof {
    /// The pid still names the remembered process (or the host cannot tell).
    Same,
    /// The pid names another process, or none: it changed hands or was reaped.
    Changed,
}

impl Origin {
    /// Describes `pid` now, while it is still known to be ours.
    pub(crate) fn capture(pid: u32) -> Self {
        if !SUPPORTED {
            return Self::Unavailable;
        }
        match describe(pid) {
            Ok(Some(identity)) => Self::Known(identity),
            _ => Self::Missing,
        }
    }

    /// Whether `pid` still names the remembered process. `Err` when the kernel cannot be asked
    /// or the process was never identified: the caller must then do nothing to the pid.
    pub(crate) fn check(&self, pid: u32) -> io::Result<Proof> {
        match self {
            Self::Unavailable => Ok(Proof::Same),
            Self::Missing => Err(io::Error::other(
                "the child could not be identified at spawn; refusing to act on its pid",
            )),
            Self::Known(expected) => Ok(match describe(pid)? {
                Some(found) if found == *expected => Proof::Same,
                _ => Proof::Changed,
            }),
        }
    }
}

/// The identity of `pid` (a live process or an unreaped zombie), `Ok(None)` when the kernel
/// has no such process, `Err` when it cannot say.
#[cfg(target_os = "macos")]
pub(crate) fn describe(pid: u32) -> io::Result<Option<Identity>> {
    // SAFETY: an all-zero `proc_bsdinfo` is a valid value (plain integers and char arrays).
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: errno is thread-local and writable.
    unsafe { *libc::__error() = 0 };
    // SAFETY: `info` is a writable buffer of exactly `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::pid_t,
            libc::PROC_PIDTBSDINFO,
            // xnu reads `arg` as "zombies are allowed" for this flavor: with 0 an exited but
            // unreaped child is invisible, with 1 it is described from the zombie list.
            1,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if written == size {
        return Ok((info.pbi_pid == pid).then_some(Identity {
            parent: info.pbi_ppid,
            started_sec: info.pbi_start_tvsec,
            started_usec: info.pbi_start_tvusec,
        }));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) | Some(0) | None if written <= 0 => Ok(None),
        _ => Err(error),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn describe(pid: u32) -> io::Result<Option<Identity>> {
    let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => text,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            // No such process, unless /proc itself is not there.
            return if std::path::Path::new("/proc/self/stat").exists() {
                Ok(None)
            } else {
                Err(error)
            };
        }
        Err(error) => return Err(error),
    };
    // `pid (comm) state ppid ...`: comm may hold spaces and parentheses, so split after the
    // last ')'. starttime is field 22 overall, index 19 after the state.
    let malformed = || io::Error::new(io::ErrorKind::InvalidData, "unreadable /proc stat");
    let rest = text.rsplit_once(')').ok_or_else(malformed)?.1;
    let mut fields = rest.split_whitespace();
    let _state = fields.next().ok_or_else(malformed)?;
    let parent = fields
        .next()
        .and_then(|f| f.parse().ok())
        .ok_or_else(malformed)?;
    let started = fields
        .nth(17)
        .and_then(|f| f.parse().ok())
        .ok_or_else(malformed)?;
    Ok(Some(Identity {
        parent,
        started_sec: started,
        started_usec: 0,
    }))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
pub(crate) fn describe(_pid: u32) -> io::Result<Option<Identity>> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}
