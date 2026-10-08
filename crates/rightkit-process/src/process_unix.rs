use nix::libc;
use std::{io, os::fd::RawFd};

pub(crate) struct Inheritance {
    allowed: Vec<RawFd>,
    upper_bound: RawFd,
}

impl Inheritance {
    pub(crate) fn new(mut allowed: Vec<RawFd>) -> io::Result<Self> {
        if allowed.iter().any(|fd| *fd < 0) {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        allowed.sort_unstable();
        allowed.dedup();
        for &fd in &allowed {
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let maximum = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
        if maximum < 0 {
            return Err(io::Error::last_os_error());
        }
        // Include descriptors above a subsequently lowered soft limit.
        let highest = ["/proc/self/fd", "/dev/fd"]
            .into_iter()
            .find_map(|path| {
                std::fs::read_dir(path).ok().map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter_map(|entry| entry.file_name().to_str()?.parse::<RawFd>().ok())
                        .max()
                        .unwrap_or(2)
                })
            })
            .unwrap_or(2);
        let upper_bound = maximum.min(i64::from(i32::MAX) as libc::c_long) as RawFd;
        Ok(Self {
            allowed,
            upper_bound: upper_bound.max(highest.saturating_add(1)),
        })
    }

    // Called after fork: no allocation, directory traversal, locks or destructors.
    pub(crate) fn apply(&self) -> io::Result<()> {
        let mut first = 3u32;
        for &fd in self.allowed.iter().filter(|fd| **fd >= 3) {
            self.exclude(first, fd as u32 - 1)?;
            first = fd as u32 + 1;
        }
        self.exclude(first, u32::MAX)?;
        for &fd in self.allowed.iter().filter(|fd| **fd >= 3) {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn exclude(&self, first: u32, last: u32) -> io::Result<()> {
        if first > last {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            // Keep std's CLOEXEC exec-error pipe alive until exec: closing it
            // in pre_exec would make failed exec look like successful spawn.
            // CLOSE_RANGE_CLOEXEC excludes every unwanted fd atomically at exec.
            const CLOSE_RANGE_CLOEXEC: libc::c_uint = 4;
            if unsafe { libc::syscall(libc::SYS_close_range, first, last, CLOSE_RANGE_CLOEXEC) }
                == 0
            {
                return Ok(());
            }
        }
        let end = last.min(self.upper_bound.saturating_sub(1) as u32);
        for fd in first..=end {
            let fd = fd as RawFd;
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EBADF) {
                    return Err(error);
                }
            } else if flags & libc::FD_CLOEXEC == 0 {
                // All preexisting CLOEXEC fds (including std's error pipe)
                // already disappear at exec; only inheritable fds need close.
                // Even if close is interrupted on a host that leaves fd open,
                // CLOEXEC guarantees it cannot escape into the executed child.
                if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                unsafe {
                    libc::close(fd);
                }
            }
        }
        Ok(())
    }
}
