//! POSIX PTY: `openpty`, the slave as the child's stdio and controlling terminal, the master
//! registered with the tokio reactor.

use crate::PtySize;
use rightkit_child_process::{OwnedChild, OwnedCommand};
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    task::{Context, Poll, ready},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf, unix::AsyncFd};

fn winsize(size: PtySize) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    }
}

/// A new pty pair: (master, slave), both close-on-exec.
fn open_pty(size: PtySize) -> io::Result<(OwnedFd, OwnedFd)> {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let window = winsize(size);
    // SAFETY: the out pointers are valid; the name and termios arguments are null (not wanted,
    // defaults); `window` is a valid winsize that openpty only reads.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &window,
        )
    };
    if opened != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openpty returned two new descriptors that nothing else owns.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    set_flag(&master, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC)?;
    set_flag(&slave, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC)?;
    Ok((master, slave))
}

/// `get`/`set` fcntl pair that ORs `bit` into a descriptor's flags.
fn set_flag(fd: &OwnedFd, get: libc::c_int, set: libc::c_int, bit: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor this function's caller owns, with plain integer arguments.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), get) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd.as_raw_fd(), set, flags | bit) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The terminal's end-of-file character (`VEOF`, Ctrl-D unless the terminal says otherwise).
fn eof_char(master: RawFd) -> u8 {
    // SAFETY: an all-zero termios is valid storage, and tcgetattr fills it.
    let mut attributes: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: valid descriptor and out pointer.
    if unsafe { libc::tcgetattr(master, &mut attributes) } == 0 {
        let value = attributes.c_cc[libc::VEOF];
        if value != 0 {
            return value;
        }
    }
    0x04
}

pub(crate) fn spawn(
    mut command: OwnedCommand,
    size: PtySize,
) -> io::Result<(OwnedChild, Terminal)> {
    let (master, slave) = open_pty(size)?;
    set_flag(&master, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK)?;
    let tty = || slave.try_clone().map(Stdio::from);
    command
        .command_mut()
        .stdin(tty()?)
        .stdout(tty()?)
        .stderr(tty()?);
    // setsid, then TIOCSCTTY on stdin: the pty becomes the child's controlling terminal and the
    // child leads a session, so group signals reach its whole tree.
    command.unix_controlling_terminal(true);
    let child = command.spawn()?;
    // The parent's slave must close now: end of file on the master needs every slave closed.
    drop(slave);
    let eof = eof_char(master.as_raw_fd());
    // SAFETY: `master` is an owned, open descriptor that AsyncFd now owns until it drops; nothing
    // else closes it or swaps it for another descriptor.
    let master = unsafe { AsyncFd::register(master) }.map_err(|e| e.into_parts().1)?;
    Ok((
        child,
        Terminal {
            master: Arc::new(master),
            eof,
        },
    ))
}

#[derive(Debug)]
pub(crate) struct Terminal {
    master: Arc<AsyncFd<OwnedFd>>,
    eof: u8,
}

impl Terminal {
    pub(crate) fn reader(&self) -> io::Result<RawReader> {
        Ok(RawReader {
            master: Arc::clone(&self.master),
        })
    }

    pub(crate) fn writer(&self) -> io::Result<RawWriter> {
        Ok(RawWriter {
            master: Arc::clone(&self.master),
        })
    }

    pub(crate) fn eof_sequence(&self) -> Vec<u8> {
        vec![self.eof]
    }

    pub(crate) fn resize(&self, size: PtySize) -> io::Result<()> {
        let window = winsize(size);
        // SAFETY: TIOCSWINSZ reads one winsize through the pointer; the master is open.
        if unsafe {
            libc::ioctl(
                self.master.get_ref().as_raw_fd(),
                libc::TIOCSWINSZ as _,
                &window,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Nothing to do: the master reads end of file by itself once the slave side is closed.
    pub(crate) fn child_exited(&self) {}
}

#[derive(Debug)]
pub(crate) struct RawReader {
    master: Arc<AsyncFd<OwnedFd>>,
}

impl AsyncRead for RawReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            let mut guard = ready!(this.master.poll_read_ready(cx))?;
            let spare = buf.initialize_unfilled();
            let read = guard.try_io(|inner| {
                // SAFETY: `spare` is a writable, initialised buffer of the length passed.
                let count = unsafe {
                    libc::read(
                        inner.get_ref().as_raw_fd(),
                        spare.as_mut_ptr().cast(),
                        spare.len(),
                    )
                };
                if count < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(count as usize)
                }
            });
            match read {
                Ok(Ok(count)) => {
                    buf.advance(count);
                    return Poll::Ready(Ok(()));
                }
                // Linux reports "every slave closed" as EIO: that is end of file here.
                Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => {
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                // Spurious readiness: the guard cleared it, wait again.
                Err(_would_block) => continue,
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct RawWriter {
    master: Arc<AsyncFd<OwnedFd>>,
}

impl AsyncWrite for RawWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        loop {
            let mut guard = ready!(this.master.poll_write_ready(cx))?;
            let written = guard.try_io(|inner| {
                // SAFETY: `buf` is a readable buffer of the length passed.
                let count = unsafe {
                    libc::write(inner.get_ref().as_raw_fd(), buf.as_ptr().cast(), buf.len())
                };
                if count < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(count as usize)
                }
            });
            match written {
                Ok(Ok(count)) => return Poll::Ready(Ok(count)),
                // Writing to a terminal whose slave side is gone.
                Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => {
                    return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
                }
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    /// The terminal has no half-close: use `PtyWriter::send_eof`.
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
