//! Unix transfer primitives: nonblocking `read`/`write` gated by `poll(2)`
//! against an absolute deadline. Every syscall is preceded by a wait, so a
//! transfer never blocks past the deadline, and the descriptor's `O_NONBLOCK`
//! state is checked, never changed, by these primitives.

use std::io;
use std::os::unix::io::RawFd;
use std::time::Instant;

use super::Transfer;

pub(super) type Raw = RawFd;

fn fcntl_flags(fd: Raw) -> io::Result<libc::c_int> {
    // SAFETY: F_GETFL reads descriptor state and takes no pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(flags)
    }
}

/// Fails unless `fd` is nonblocking. Called before every framed transfer.
pub(super) fn check_mode(fd: Raw) -> io::Result<()> {
    if fcntl_flags(fd)? & libc::O_NONBLOCK == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "deadline framing needs a nonblocking descriptor (set O_NONBLOCK, or wrap it with DeadlineFramed::new)",
        ));
    }
    Ok(())
}

/// Sets `O_NONBLOCK` on `fd`, leaving every other status flag as it was.
pub(super) fn set_nonblocking(fd: Raw) -> io::Result<()> {
    let flags = fcntl_flags(fd)?;
    if flags & libc::O_NONBLOCK != 0 {
        return Ok(());
    }
    // SAFETY: F_SETFL with the value just read plus O_NONBLOCK; no pointers.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Waits until `fd` is ready for `events` or `deadline` passes. Returns
/// `Ok(true)` on readiness, including hangup or error: the following
/// `read`/`write` reports the condition. Returns `Ok(false)` at the deadline.
fn wait_ready(fd: Raw, events: libc::c_short, deadline: Instant) -> io::Result<bool> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        // Round up so a sub-millisecond remainder still waits one tick
        // instead of spinning, and clamp to the `poll` argument range.
        let ms = left
            .as_nanos()
            .div_ceil(1_000_000)
            .min(libc::c_int::MAX as u128) as libc::c_int;
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        // SAFETY: `pfd` is one valid element; the caller's borrow keeps `fd` open.
        let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if ready == 0 {
            continue;
        }
        if pfd.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        return Ok(true);
    }
}

/// Reads at least one byte into `out`, or returns `Timeout` at the deadline.
/// `out` must be non-empty.
pub(super) fn read_some(fd: Raw, out: &mut [u8], deadline: Instant) -> io::Result<Transfer> {
    loop {
        if !wait_ready(fd, libc::POLLIN, deadline)? {
            return Ok(Transfer::Timeout);
        }
        // SAFETY: `out` is a valid writable buffer of `out.len()` bytes.
        let n = unsafe { libc::read(fd, out.as_mut_ptr().cast(), out.len()) };
        if n >= 0 {
            return Ok(Transfer::Bytes(n as usize));
        }
        let err = io::Error::last_os_error();
        if !matches!(
            err.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            return Err(err);
        }
    }
}

/// Writes at least one byte of `data`, or returns `Timeout` at the deadline.
/// `data` must be non-empty.
pub(super) fn write_some(fd: Raw, data: &[u8], deadline: Instant) -> io::Result<Transfer> {
    loop {
        if !wait_ready(fd, libc::POLLOUT, deadline)? {
            return Ok(Transfer::Timeout);
        }
        // SAFETY: `data` is a valid readable buffer of `data.len()` bytes.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n >= 0 {
            return Ok(Transfer::Bytes(n as usize));
        }
        let err = io::Error::last_os_error();
        if !matches!(
            err.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            return Err(err);
        }
    }
}
