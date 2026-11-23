//! Tie a child the caller already spawned to this process (rightkit#1688).

use std::io;

/// Ties the process group of an already-spawned child to this process: when this process dies,
/// even by `SIGKILL`, the child's whole group gets `SIGTERM` and, two seconds later, `SIGKILL`.
/// It arms the same watchdog as [`OwnedCommand::kill_on_parent_exit`](crate::OwnedCommand::kill_on_parent_exit),
/// for a host that builds and spawns its own commands (a `tokio::process::Command` with its own
/// `pre_exec` hooks, say) and cannot go through `OwnedCommand::spawn`.
///
/// The child must lead its own process group (`tokio_support::lead_session`, `setsid`, or
/// `process_group(0)`): the watchdog signals the group named by the child's pid, so a child that
/// shares its parent's group is refused with [`io::ErrorKind::InvalidInput`] rather than left
/// untied. Call it right after the spawn, while the pid is certainly still the child's; the child
/// runs untied until this returns. On failure the child is left running: the caller decides
/// whether to stop it. Costs one helper process, which exits when the child or this process does.
///
/// macOS and Linux only. Every other platform returns [`io::ErrorKind::Unsupported`], never a
/// silent no-op (Windows callers contain the child in a kill-on-close job instead).
pub fn tie_group_to_parent(child_pid: u32) -> io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let pid = libc::pid_t::try_from(child_pid)
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "child pid out of range"))?;
        // SAFETY: getpgid only reads the process table.
        let group = unsafe { libc::getpgid(pid) };
        if group < 0 {
            return Err(io::Error::last_os_error());
        }
        if group != pid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the child does not lead its own process group; spawn it with lead_session or process_group(0)",
            ));
        }
        crate::process_unix::arm_parent_death_watch(child_pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = child_pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "tie_group_to_parent needs the macOS or Linux parent-death watchdog",
        ))
    }
}
