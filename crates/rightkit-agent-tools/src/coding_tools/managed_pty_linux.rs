//! Linux PTY transport under the existing managed process/sandbox authorities.
//! Allocates CLOEXEC descriptors in the parent; installs controlling terminal
//! and prepared Landlock/seccomp in the child before exec. No renderer.

use super::finish_unix_spawn;
use portable_pty::MasterPty;
use rightkit_sandbox::execution::{
    CommandLaunchSpec, EffectExecutionError, ManagedChild, PtySize, SandboxProcessProof,
};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn spawn(
    spec: &CommandLaunchSpec,
    size: PtySize,
    program: &str,
    argv: &[String],
    proof: SandboxProcessProof,
    tier: Option<crate::coding_tools::sandbox::SandboxTier>,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let sandbox = tier.map(|tier| {
        crate::coding_tools::shell::local_sandbox_config_for_tier(
            Path::new(&spec.context.workspace_root),
            tier,
        )
    });
    let (master, slave) = open_pair().map_err(|_| EffectExecutionError::SpawnFailed)?;
    let native_size = portable_pty::PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    };
    master
        .resize(native_size)
        .map_err(|_| EffectExecutionError::SpawnFailed)?;
    let reader = master
        .try_clone_reader()
        .map_err(|_| EffectExecutionError::SpawnFailed)?;
    let writer = master
        .take_writer()
        .map_err(|_| EffectExecutionError::SpawnFailed)?;
    let stdout = slave
        .try_clone()
        .map_err(|_| EffectExecutionError::SpawnFailed)?;
    let stderr = slave
        .try_clone()
        .map_err(|_| EffectExecutionError::SpawnFailed)?;
    // Linux pty path: no Windows console involved.
    #[allow(clippy::disallowed_methods)]
    let mut command = std::process::Command::new(program);
    command
        .args(argv)
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().cloned())
        .stdin(slave)
        .stdout(stdout)
        .stderr(stderr);
    // SAFETY: this callback contains no allocation, locks or formatting.
    // Rust has connected fd 0 to the slave before invoking pre_exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            for signal in [
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTSTP,
                libc::SIGTTIN,
                libc::SIGTTOU,
            ] {
                libc::signal(signal, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    // RightKit prepares Landlock/seccomp here in the parent and registers its own
    // syscall-only `pre_exec` hook after the terminal setup above (registration order is
    // execution order, so the controlling terminal is installed first). A failure refuses
    // the launch; it never runs unconfined.
    if let Some(sandbox) = &sandbox {
        sandbox
            .confine(&mut command)
            .map_err(|_| EffectExecutionError::SandboxMissing)?;
    }
    let child = command.spawn().map_err(|_| {
        if tier.is_some() {
            EffectExecutionError::SandboxMissing
        } else {
            EffectExecutionError::SpawnFailed
        }
    })?;
    // Drop the parent copies of slave stdio before exposing the master.
    drop(command);
    finish_unix_spawn(
        Box::new(master),
        Box::new(child),
        reader,
        writer,
        size,
        proof,
    )
}

struct LinuxMaster {
    fd: File,
    tty: PathBuf,
    writer_taken: AtomicBool,
}

fn open_pair() -> io::Result<(LinuxMaster, File)> {
    // Open with CLOEXEC atomically: another concurrent spawn cannot inherit
    // the master or slave while this transport is being assembled.
    let fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { File::from_raw_fd(fd) };
    if unsafe { libc::grantpt(fd) } < 0 || unsafe { libc::unlockpt(fd) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut name = [0 as libc::c_char; 256];
    let result = unsafe { libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    let slave = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return Err(io::Error::last_os_error());
    }
    let slave = unsafe { File::from_raw_fd(slave) };
    let tty = PathBuf::from(
        unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    );
    Ok((
        LinuxMaster {
            fd: master,
            tty,
            writer_taken: AtomicBool::new(false),
        },
        slave,
    ))
}

impl MasterPty for LinuxMaster {
    fn resize(&self, size: portable_pty::PtySize) -> anyhow::Result<()> {
        let size = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.pixel_width,
            ws_ypixel: size.pixel_height,
        };
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
    fn get_size(&self) -> anyhow::Result<portable_pty::PtySize> {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), libc::TIOCGWINSZ, &mut size) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(portable_pty::PtySize {
            rows: size.ws_row,
            cols: size.ws_col,
            pixel_width: size.ws_xpixel,
            pixel_height: size.ws_ypixel,
        })
    }
    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn io::Read + Send>> {
        Ok(Box::new(self.fd.try_clone()?))
    }
    fn take_writer(&self) -> anyhow::Result<Box<dyn io::Write + Send>> {
        anyhow::ensure!(
            !self.writer_taken.swap(true, Ordering::AcqRel),
            "PTY writer already taken"
        );
        Ok(Box::new(self.fd.try_clone()?))
    }
    fn process_group_leader(&self) -> Option<libc::pid_t> {
        let group = unsafe { libc::tcgetpgrp(self.fd.as_raw_fd()) };
        (group > 0).then_some(group)
    }
    fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
        Some(self.fd.as_raw_fd())
    }
    fn tty_name(&self) -> Option<PathBuf> {
        Some(self.tty.clone())
    }
}
