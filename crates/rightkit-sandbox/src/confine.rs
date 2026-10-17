//! Confining a command the caller owns.
//!
//! [`crate::Sandbox::spawn`] builds, confines and supervises the child in one call. Callers that
//! already own a configured [`std::process::Command`] (or a `tokio::process::Command` through
//! `as_std_mut()`), with its own pipes, process group and spawn path, need the confinement alone:
//! [`confine_command`] registers it on the command and leaves everything else to the caller.

use crate::{SandboxError, SandboxPolicy};
use std::io;
use std::process::Command;

fn to_io(error: SandboxError) -> io::Error {
    match error {
        SandboxError::Io(e) => e,
        SandboxError::InvalidPolicy(message) => {
            io::Error::new(io::ErrorKind::InvalidInput, message)
        }
        SandboxError::Unsupported(message) => io::Error::new(io::ErrorKind::Unsupported, message),
        other => io::Error::other(other.to_string()),
    }
}

/// Confine `cmd` to `policy`: the child applies the confinement itself between `fork` and
/// `exec`, so nothing it runs, and nothing that runs under it, escapes the policy.
///
/// * Linux: Landlock (ABI probed at call time: `REFER` and `TRUNCATE` are handled when the
///   kernel offers them) and, with the network denied, a seccomp filter chosen by
///   [`SandboxPolicy::seccomp()`] that checks the architecture first.
///   [`SandboxPolicy::landlock()`] can skip the filesystem part for a policy without paths.
/// * macOS: the Seatbelt profile for the policy, applied in-process with `sandbox_init`.
/// * Windows: an AppContainer can only be set at process creation, so this returns
///   [`io::ErrorKind::Unsupported`]; use [`crate::Sandbox::spawn`].
///
/// Fails closed: a policy that is invalid, or a primitive the host lacks, is an error here and
/// the command must not be spawned. Everything is prepared in the parent; the closure registered
/// with `pre_exec` makes syscalls only. It runs in registration order, so register caller
/// closures that need the pre-confinement state first.
///
/// Only the confinement is applied. Working directory, environment, stdio and the process group
/// stay the caller's: the policy's `env` is **not** used, so scrub the environment on `cmd` if
/// that matters. With tokio: `confine_command(cmd.as_std_mut(), &policy)`.
pub fn confine_command(cmd: &mut Command, policy: &SandboxPolicy) -> io::Result<()> {
    policy.validate().map_err(to_io)?;
    confine_validated(cmd, policy)
}

#[cfg(target_os = "linux")]
fn confine_validated(cmd: &mut Command, policy: &SandboxPolicy) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let prepared = crate::linux::Prepared::new(policy)?;
    // SAFETY: `Prepared::apply` performs only syscalls on state prepared above.
    unsafe {
        cmd.pre_exec(move || prepared.apply());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn confine_validated(cmd: &mut Command, policy: &SandboxPolicy) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let prepared = crate::seatbelt::Prepared::new(policy)?;
    // SAFETY: the profile was built in the parent; `apply` only calls sandbox_init.
    unsafe {
        cmd.pre_exec(move || prepared.apply());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn confine_validated(_cmd: &mut Command, _policy: &SandboxPolicy) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform confines at process creation; use Sandbox::spawn (Windows) or run \
         supervised without confinement",
    ))
}

/// Whether Landlock can confine a child on this host. `false` off Linux, on a kernel without
/// Landlock, or where it is disabled or filtered.
pub fn is_landlock_available() -> bool {
    landlock_abi_version().is_some()
}

/// The Landlock ABI version the host offers (`REFER` needs 2, `TRUNCATE` needs 3), or `None`.
pub fn landlock_abi_version() -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        crate::linux::landlock_abi_version()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Whether [`SandboxPolicy::deny_read()`] is enforced on this platform: `true` on macOS (Seatbelt)
/// and Windows (explicit deny ACE), `false` on Linux, where Landlock cannot carve a hole out of a
/// granted root. When it is `false`, grant narrower roots instead of relying on the deny.
pub const fn deny_read_enforced() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_policy_is_refused_before_anything_is_registered() {
        let mut cmd = Command::new("/bin/echo");
        let error = confine_command(&mut cmd, &SandboxPolicy::new().allow_read("relative"))
            .expect_err("relative path");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let error = confine_command(&mut cmd, &SandboxPolicy::new().deny_read("relative"))
            .expect_err("relative deny_read");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let policy = SandboxPolicy::new().named_capability("internetClient");
        let error = confine_command(&mut cmd, &policy).expect_err("capability");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_without_landlock_fails_closed_never_unconfined() {
        let mut cmd = Command::new("/bin/true");
        let policy = SandboxPolicy::new().network(crate::NetworkPolicy::Allow);
        match (is_landlock_available(), confine_command(&mut cmd, &policy)) {
            (true, Ok(())) => {}
            (false, Err(error)) => assert_eq!(error.kind(), io::ErrorKind::Unsupported),
            (available, outcome) => {
                panic!("landlock={available} but confine ok={}", outcome.is_ok())
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn unsupported_platforms_refuse() {
        let mut cmd = Command::new("tool");
        let error = confine_command(&mut cmd, &SandboxPolicy::new()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn deny_read_enforcement_is_reported_honestly() {
        assert_eq!(
            deny_read_enforced(),
            cfg!(any(windows, target_os = "macos"))
        );
        #[cfg(target_os = "linux")]
        assert!(!deny_read_enforced());
    }
}
