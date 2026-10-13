//! Processes this program did not spawn: a pid read from a pid file, a helper another
//! launcher started, a descendant that outlived its parent.
//!
//! [`AdoptedProcess`](crate::AdoptedProcess) can only say a pid is running. A pid alone is a
//! name the kernel reuses, so signalling a remembered number can reach an unrelated process.
//! [`AdoptedIdentity`] remembers the pid together with the process's start time and executable
//! path, read while the process was known to be the right one, and every signal first checks
//! that the pid still names that process:
//!
//! * a pid that now names a different process (other start time, or a different executable
//!   when both are known) is refused with [`SignalResult::IdentityMismatch`] and nothing is
//!   sent;
//! * a pid that names nothing is [`SignalResult::Gone`];
//! * this process is never signalled, and neither is a pid that cannot be described.
//!
//! The parent pid is deliberately not part of the identity: an orphan is re-parented without
//! becoming a different process.
//!
//! # Limits
//!
//! Unix: the check and the `kill` are two system calls. The pid can change hands in the
//! microseconds between them only if the original process exits and the number wraps, the
//! gap [`identity`](crate::identity) documents; it cannot be closed from user space. Windows
//! has no such gap: the check and `TerminateProcess` use one handle. An exited process that
//! nobody has reaped yet (a Unix zombie) still reads as present until it is reaped.
//!
//! Descendants are found by scanning the process table for parent links, so only descendants
//! whose parent is still alive can be found: once a parent exits, its children are
//! re-parented away. Take the list ([`AdoptedIdentity::descendants`]) while the root is
//! alive, and sweep it ([`sweep`]) later; the identities keep the sweep safe.

use std::{
    io,
    num::NonZeroU32,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

/// When a process started, in the host's own unit (macOS: seconds and microseconds; Linux:
/// clock ticks since boot and 0; Windows: 100 ns ticks since 1601 and 0). Comparable only on
/// the host that produced it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StartToken {
    pub major: u64,
    pub minor: u64,
}

/// A pid together with the start time and executable path that make it a name for exactly one
/// process. See the module documentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdoptedIdentity {
    pid: NonZeroU32,
    start: StartToken,
    exe: Option<PathBuf>,
}

/// What a pid names now, compared with a remembered identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityCheck {
    /// The pid still names the remembered process.
    Same,
    /// No process has this pid.
    Gone,
    /// The pid names a different process.
    Replaced,
}

/// How hard to stop a process. Windows has one way to stop a process, so both are the same
/// there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdoptedSignal {
    /// `SIGTERM`.
    Graceful,
    /// `SIGKILL`.
    Force,
}

/// The outcome of [`AdoptedIdentity::signal`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalResult {
    /// The identity matched and the signal was sent.
    Delivered,
    /// No process has the pid any more.
    Gone,
    /// The pid names a different process; nothing was sent.
    IdentityMismatch,
}

/// The result of [`sweep`], as pids.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SweepReport {
    /// Signalled and no longer present.
    pub terminated: Vec<u32>,
    /// Already absent when the sweep reached them.
    pub gone: Vec<u32>,
    /// The pid names a different process now; nothing was sent.
    pub refused: Vec<u32>,
    /// Signalled (forced) and still present at the end of the sweep.
    pub survivors: Vec<u32>,
    /// The signal could not be sent (for example, permission denied).
    pub failed: Vec<u32>,
}

impl AdoptedIdentity {
    /// Reads the identity of `pid`. `Ok(None)` when no process has that pid. The caller is
    /// responsible for `pid` naming the intended process at this moment (it was just spawned
    /// by a launcher, or it was just read from the owner's own record).
    pub fn capture(pid: NonZeroU32) -> io::Result<Option<Self>> {
        Ok(sys::read(pid.get())?.map(|seen| Self {
            pid,
            start: seen.start,
            exe: seen.exe,
        }))
    }

    /// Rebuilds an identity persisted earlier (a pid file that also records
    /// [`start_token`](Self::start_token) and [`exe`](Self::exe)).
    pub fn from_recorded(pid: NonZeroU32, start: StartToken, exe: Option<PathBuf>) -> Self {
        Self { pid, start, exe }
    }

    pub fn pid(&self) -> u32 {
        self.pid.get()
    }

    pub fn start_token(&self) -> StartToken {
        self.start
    }

    pub fn exe(&self) -> Option<&std::path::Path> {
        self.exe.as_deref()
    }

    fn matches(&self, seen: &sys::Seen) -> bool {
        // The executable is compared only when both sides know it: a zombie or a process of
        // another user may not reveal it, and the start time has already matched.
        seen.start == self.start
            && match (&self.exe, &seen.exe) {
                (Some(remembered), Some(found)) => remembered == found,
                _ => true,
            }
    }

    /// Whether the pid still names the remembered process.
    pub fn verify(&self) -> io::Result<IdentityCheck> {
        Ok(match sys::read(self.pid())? {
            None => IdentityCheck::Gone,
            Some(seen) if self.matches(&seen) => IdentityCheck::Same,
            Some(_) => IdentityCheck::Replaced,
        })
    }

    /// Sends `signal` only if the pid still names the remembered process. Never signals this
    /// process: that is `InvalidInput`.
    pub fn signal(&self, signal: AdoptedSignal) -> io::Result<SignalResult> {
        if self.pid() == std::process::id() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to signal this process",
            ));
        }
        sys::signal(self, signal)
    }

    /// The descendants of this process, parents before children, each with its own identity.
    /// Empty when this process is gone or the pid now names a different one. A child is
    /// accepted only if it did not start before its parent, so a stale parent link to a
    /// recycled pid adds nothing.
    pub fn descendants(&self) -> io::Result<Vec<AdoptedIdentity>> {
        if self.verify()? != IdentityCheck::Same {
            return Ok(Vec::new());
        }
        let pairs = sys::process_pairs()?;
        let mut found: Vec<AdoptedIdentity> = Vec::new();
        // (pid, start) of every process whose children are still to be looked up.
        let mut frontier = vec![(self.pid(), self.start)];
        let mut next = 0;
        while next < frontier.len() {
            let (parent, parent_start) = frontier[next];
            next += 1;
            for &(child, claimed_parent) in &pairs {
                if claimed_parent != parent
                    || child == parent
                    || child == self.pid()
                    || frontier.iter().any(|(pid, _)| *pid == child)
                {
                    continue;
                }
                let (Some(pid), Ok(Some(seen))) = (NonZeroU32::new(child), sys::read(child)) else {
                    continue;
                };
                if seen.parent.is_some_and(|actual| actual != parent) || seen.start < parent_start {
                    continue; // the pid changed hands since the table was read
                }
                frontier.push((child, seen.start));
                found.push(AdoptedIdentity {
                    pid,
                    start: seen.start,
                    exe: seen.exe,
                });
            }
        }
        // The root must have stayed itself for the whole scan.
        if self.verify()? != IdentityCheck::Same {
            return Ok(Vec::new());
        }
        Ok(found)
    }
}

const SWEEP_POLL: Duration = Duration::from_millis(20);
const FORCE_WAIT: Duration = Duration::from_millis(500);

/// Stops every process in `targets` that is still the process it was remembered as: a graceful
/// signal to each, up to `grace` for them to leave, then a forced signal to those still
/// present. A target whose pid names a different process is `refused`, never signalled.
/// Targets are signalled in the order given; pass leaves first (the reverse of
/// [`AdoptedIdentity::descendants`]).
pub fn sweep(targets: &[AdoptedIdentity], grace: Duration) -> io::Result<SweepReport> {
    let mut report = SweepReport::default();
    let mut pending: Vec<&AdoptedIdentity> = Vec::new();
    for target in targets {
        match target.signal(AdoptedSignal::Graceful) {
            Ok(SignalResult::Delivered) => pending.push(target),
            Ok(SignalResult::Gone) => report.gone.push(target.pid()),
            Ok(SignalResult::IdentityMismatch) => report.refused.push(target.pid()),
            Err(_) => report.failed.push(target.pid()),
        }
    }
    drain(&mut pending, grace, &mut report)?;
    let mut forced: Vec<&AdoptedIdentity> = Vec::new();
    for target in pending {
        match target.signal(AdoptedSignal::Force) {
            Ok(SignalResult::Delivered) => forced.push(target),
            Ok(SignalResult::Gone) => report.terminated.push(target.pid()),
            Ok(SignalResult::IdentityMismatch) => report.refused.push(target.pid()),
            Err(_) => report.failed.push(target.pid()),
        }
    }
    drain(&mut forced, FORCE_WAIT, &mut report)?;
    report
        .survivors
        .extend(forced.iter().map(|target| target.pid()));
    Ok(report)
}

/// [`sweep`] of `root`'s current descendants, leaves first. The root itself is left alone.
pub fn sweep_descendants(root: &AdoptedIdentity, grace: Duration) -> io::Result<SweepReport> {
    let mut targets = root.descendants()?;
    targets.reverse();
    sweep(&targets, grace)
}

/// Waits until `deadline` for each pending target to stop being the remembered process; those
/// that did are moved to `report.terminated`.
fn drain(
    pending: &mut Vec<&AdoptedIdentity>,
    wait: Duration,
    report: &mut SweepReport,
) -> io::Result<()> {
    let deadline = Instant::now().checked_add(wait);
    loop {
        let mut still = Vec::new();
        for target in pending.drain(..) {
            if target.verify()? == IdentityCheck::Same {
                still.push(target);
            } else {
                report.terminated.push(target.pid());
            }
        }
        *pending = still;
        let expired = deadline.is_none_or(|limit| Instant::now() >= limit);
        if pending.is_empty() || expired {
            return Ok(());
        }
        thread::sleep(SWEEP_POLL);
    }
}

mod sys {
    use super::StartToken;
    #[cfg(not(windows))]
    use super::{AdoptedIdentity, AdoptedSignal, SignalResult};
    #[cfg(not(windows))]
    use std::io;
    use std::path::PathBuf;

    /// One look at a process.
    pub(super) struct Seen {
        pub(super) start: StartToken,
        pub(super) exe: Option<PathBuf>,
        /// The parent pid when the host reports it with the process (Unix); Windows reads it
        /// from the process table only.
        pub(super) parent: Option<u32>,
    }

    #[cfg(unix)]
    pub(super) fn read(pid: u32) -> io::Result<Option<Seen>> {
        Ok(crate::identity::describe(pid)?.map(|identity| Seen {
            start: StartToken {
                major: identity.started_sec,
                minor: identity.started_usec,
            },
            exe: exe_path(pid),
            parent: Some(identity.parent),
        }))
    }

    #[cfg(target_os = "macos")]
    fn exe_path(pid: u32) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        // PROC_PIDPATHINFO_MAXSIZE is 4 * MAXPATHLEN.
        let mut buffer = vec![0u8; 4096];
        // SAFETY: the buffer is writable for exactly `buffer.len()` bytes.
        let written = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
            )
        };
        if written <= 0 {
            return None;
        }
        buffer.truncate(written as usize);
        Some(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn exe_path(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "linux", target_os = "android"))
    ))]
    fn exe_path(_pid: u32) -> Option<PathBuf> {
        None
    }

    #[cfg(unix)]
    pub(super) fn signal(
        identity: &AdoptedIdentity,
        signal: AdoptedSignal,
    ) -> io::Result<SignalResult> {
        use super::IdentityCheck;
        match identity.verify()? {
            IdentityCheck::Gone => return Ok(SignalResult::Gone),
            IdentityCheck::Replaced => return Ok(SignalResult::IdentityMismatch),
            IdentityCheck::Same => {}
        }
        let pid = i32::try_from(identity.pid()).map_err(io::Error::other)?;
        let number = match signal {
            AdoptedSignal::Graceful => libc::SIGTERM,
            AdoptedSignal::Force => libc::SIGKILL,
        };
        // SAFETY: kill takes two integers; the pid was proven to be the remembered process
        // just above (see the module documentation for the residual gap).
        if unsafe { libc::kill(pid, number) } == 0 {
            return Ok(SignalResult::Delivered);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(SignalResult::Gone),
            _ => Err(error),
        }
    }

    /// Every (pid, parent pid) pair in the process table.
    #[cfg(unix)]
    pub(super) fn process_pairs() -> io::Result<Vec<(u32, u32)>> {
        let mut pairs = Vec::new();
        for pid in list_pids()? {
            if let Ok(Some(identity)) = crate::identity::describe(pid) {
                pairs.push((pid, identity.parent));
            }
        }
        Ok(pairs)
    }

    #[cfg(target_os = "macos")]
    fn list_pids() -> io::Result<Vec<u32>> {
        /// `PROC_ALL_PIDS` from `<libproc.h>`.
        const PROC_ALL_PIDS: u32 = 1;
        let size = std::mem::size_of::<libc::pid_t>();
        // SAFETY: a null buffer asks only for the size needed.
        let needed = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Err(io::Error::last_os_error());
        }
        // Room for processes that start between the two calls.
        let mut pids = vec![0 as libc::pid_t; needed as usize / size + 64];
        // SAFETY: the buffer holds `pids.len()` pids and the byte size passed says so.
        let written = unsafe {
            libc::proc_listpids(
                PROC_ALL_PIDS,
                0,
                pids.as_mut_ptr().cast(),
                (pids.len() * size) as libc::c_int,
            )
        };
        if written <= 0 {
            return Err(io::Error::last_os_error());
        }
        pids.truncate(written as usize / size);
        Ok(pids
            .into_iter()
            .filter(|pid| *pid > 0)
            .map(|pid| pid as u32)
            .collect())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn list_pids() -> io::Result<Vec<u32>> {
        Ok(std::fs::read_dir("/proc")?
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .collect())
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "linux", target_os = "android"))
    ))]
    fn list_pids() -> io::Result<Vec<u32>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    #[cfg(windows)]
    mod windows_impl {
        use super::super::{AdoptedIdentity, AdoptedSignal, SignalResult, StartToken};
        use super::Seen;
        use crate::process_windows::os_error;
        use std::{io, path::PathBuf};
        use windows::{
            core::PWSTR,
            Win32::{
                Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE},
                System::{
                    Diagnostics::ToolHelp::{
                        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                        TH32CS_SNAPPROCESS,
                    },
                    Threading::{
                        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
                        WaitForSingleObject, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32,
                        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
                    },
                },
            },
        };

        /// A kernel handle closed on drop.
        struct Owned(HANDLE);
        impl Drop for Owned {
            fn drop(&mut self) {
                // SAFETY: the handle came from a successful open or snapshot and is closed once.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }

        fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> io::Result<Option<Owned>> {
            // SAFETY: OpenProcess takes plain values and returns a new handle.
            match unsafe { OpenProcess(access, false, pid) } {
                Ok(handle) => Ok(Some(Owned(handle))),
                Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => Ok(None),
                Err(error) => Err(os_error(error)),
            }
        }

        /// `None` once the process has exited (its handle can outlive it).
        fn observe(process: &Owned) -> io::Result<Option<Seen>> {
            // SAFETY: the handle is open with SYNCHRONIZE.
            if unsafe { WaitForSingleObject(process.0, 0) }.0 == 0 {
                return Ok(None);
            }
            let (mut created, mut exited, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            // SAFETY: the handle is open with QUERY_LIMITED_INFORMATION; the outputs are valid.
            unsafe {
                GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user)
            }
            .map_err(os_error)?;
            let ticks =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            let mut buffer = vec![0u16; 32768];
            let mut length = buffer.len() as u32;
            // SAFETY: the buffer holds `length` UTF-16 units.
            let exe = unsafe {
                QueryFullProcessImageNameW(
                    process.0,
                    PROCESS_NAME_WIN32,
                    PWSTR(buffer.as_mut_ptr()),
                    &mut length,
                )
            }
            .ok()
            .map(|()| PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize])));
            Ok(Some(Seen {
                start: StartToken {
                    major: ticks,
                    minor: 0,
                },
                exe,
                parent: None,
            }))
        }

        pub(in super::super) fn read(pid: u32) -> io::Result<Option<Seen>> {
            let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
            match open(pid, access)? {
                Some(process) => observe(&process),
                None => Ok(None),
            }
        }

        pub(in super::super) fn signal(
            identity: &AdoptedIdentity,
            _signal: AdoptedSignal,
        ) -> io::Result<SignalResult> {
            let access =
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE;
            let Some(process) = open(identity.pid(), access)? else {
                return Ok(SignalResult::Gone);
            };
            // Proof and act use the same handle: the pid cannot change hands in between.
            let Some(seen) = observe(&process)? else {
                return Ok(SignalResult::Gone);
            };
            if !identity.matches(&seen) {
                return Ok(SignalResult::IdentityMismatch);
            }
            // SAFETY: the handle is open with PROCESS_TERMINATE.
            unsafe { TerminateProcess(process.0, 1) }.map_err(os_error)?;
            Ok(SignalResult::Delivered)
        }

        pub(in super::super) fn process_pairs() -> io::Result<Vec<(u32, u32)>> {
            // SAFETY: a snapshot request takes plain values.
            let snapshot = Owned(
                unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(os_error)?,
            );
            // SAFETY: PROCESSENTRY32W is plain data; dwSize is set below as the API requires.
            let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            // SAFETY: the snapshot is open and `entry` is a valid, sized entry.
            unsafe { Process32FirstW(snapshot.0, &mut entry) }.map_err(os_error)?;
            let mut pairs = Vec::new();
            loop {
                pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
                // SAFETY: as above.
                if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
                    break;
                }
            }
            Ok(pairs)
        }
    }

    #[cfg(windows)]
    pub(super) use windows_impl::{process_pairs, read, signal};

    #[cfg(not(any(unix, windows)))]
    pub(super) fn read(_pid: u32) -> io::Result<Option<Seen>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    #[cfg(not(any(unix, windows)))]
    pub(super) fn signal(_: &AdoptedIdentity, _: AdoptedSignal) -> io::Result<SignalResult> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    #[cfg(not(any(unix, windows)))]
    pub(super) fn process_pairs() -> io::Result<Vec<(u32, u32)>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
