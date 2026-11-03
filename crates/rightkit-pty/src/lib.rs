//! An async managed PTY child (rightkit#557).
//!
//! [`PtyCommand`] starts a program on a pseudo-terminal and returns a [`PtyChild`]:
//!
//! * the child is an owned tree, built on [`rightkit_child_process::OwnedCommand`]: on Unix it
//!   leads its own session with the pty slave as controlling terminal, so every signal of this
//!   workspace's process-tree code reaches all of it; on Windows it is created suspended,
//!   assigned to a Job Object and attached to a ConPTY pseudoconsole before it runs. Dropping a
//!   [`PtyChild`] runs the staged kill of an `OwnedChild`.
//! * [`PtyChild::take_reader`] and [`PtyChild::take_writer`] give tokio `AsyncRead` and
//!   `AsyncWrite` streams of the terminal. Input is bounded: a single `poll_write` accepts at
//!   most [`INPUT_CHUNK`] (8 KiB) and [`PtyWriter::write_input`] returns only after every chunk
//!   was accepted by the terminal, one acknowledged chunk at a time.
//! * [`PtyWriter::send_eof`] ends the input the way the platform's canonical mode does:
//!   Unix writes the terminal's own `VEOF` character (Ctrl-D by default), Windows writes
//!   `0x1a` and a carriage return (Ctrl-Z, Enter).
//! * [`PtyChild::resize`] changes rows, columns and pixel size.
//! * [`PtyChild::wait`] resolves with the exit status; [`PtyChild::cancel`] is the staged kill of
//!   the whole tree (and, on Unix, of every other process group of the session).
//!
//! # Composing with a sandbox
//!
//! Hooks for a confinement layer (`rightkit-sandbox`) that wants the PTY child to run confined:
//!
//! * [`PtyCommand::from_argv`] takes an argv that is already wrapped (a Seatbelt
//!   `sandbox-exec -p <profile> -- <program> ...` prefix, a `bwrap` prefix): element 0 is what is
//!   executed.
//! * [`PtyCommand::pre_exec_last`] (Unix, `unsafe`) runs a closure in the forked child after
//!   `setsid`, the controlling-terminal ioctl and the descriptor sweep, as the last step
//!   before `exec` (a seccomp or landlock filter belongs here).
//! * [`PtyCommand::owned_mut`] is the underlying [`OwnedCommand`]: set its Job limits, priority,
//!   environment allowlist (`env_allowlist_with_defaults`) and the other options it offers. A
//!   Windows AppContainer token is not expressible through it; a Windows sandbox runs the
//!   program it wraps through `from_argv` instead.
//!
//! # Requirements and limits
//!
//! * Call [`PtyCommand::spawn`] inside a tokio runtime.
//! * The default `TERM` is `xterm-256color`, set only when the command has no `TERM` of its own.
//! * Unix: a terminal read after the last slave closes ends with end of file (`EIO` on Linux is
//!   mapped to it). Windows: the output pipe of a ConPTY ends only when the pseudoconsole is
//!   closed, which this crate does when the child has exited ([`PtyChild::wait`], [`PtyChild::cancel`])
//!   or is dropped; output written before that is delivered first.
//! * Windows I/O is bridged by two threads per child, because ConPTY's pipes are blocking
//!   anonymous pipes (see `Cargo.toml`, `continuous_reason`). Unix starts no thread.
//! * Not verified by a unit test: this workspace tests terminals end to end only, against an
//!   installed app. The Windows half was written without a Windows host and needs the device
//!   run listed in the changelog.

use rightkit_child_process::{OwnedChild, OwnedCommand, ProcessHandle, TerminationOptions};
use std::{
    ffi::OsStr,
    io,
    path::Path,
    pin::Pin,
    process::ExitStatus,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "windows.rs"]
mod imp;
#[cfg(not(any(unix, windows)))]
compile_error!("rightkit-pty supports Unix and Windows only");

/// The most one `poll_write` accepts, and the chunk size of [`PtyWriter::write_input`].
pub const INPUT_CHUNK: usize = 8 * 1024;

/// Terminal dimensions in character cells and, where the host uses them, in pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
    /// Width of the terminal in pixels; 0 when unknown. Unix only (ConPTY has no pixel size).
    pub pixel_width: u16,
    /// Height of the terminal in pixels; 0 when unknown. Unix only.
    pub pixel_height: u16,
}

impl PtySize {
    /// `rows` by `cols` cells with no pixel size.
    pub const fn new(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    pub const fn with_pixels(mut self, pixel_width: u16, pixel_height: u16) -> Self {
        self.pixel_width = pixel_width;
        self.pixel_height = pixel_height;
        self
    }

    fn validate(&self) -> io::Result<()> {
        if self.rows == 0 || self.cols == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a terminal needs at least one row and one column",
            ));
        }
        Ok(())
    }
}

impl Default for PtySize {
    fn default() -> Self {
        Self::new(24, 80)
    }
}

/// What to run on a pseudo-terminal. Build it, then [`spawn`](Self::spawn).
#[derive(Debug)]
pub struct PtyCommand {
    command: OwnedCommand,
    size: PtySize,
}

impl PtyCommand {
    /// Runs `program` (searched on `PATH` like any command).
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            command: OwnedCommand::new(program),
            size: PtySize::default(),
        }
    }

    /// Runs an argv that is already complete: element 0 is the program, the rest its arguments.
    /// For a sandbox wrapper that has put its own prefix in front (see the module
    /// documentation). An empty argv is `InvalidInput`.
    pub fn from_argv<I, S>(argv: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut argv = argv.into_iter();
        let program = argv.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "an empty argv names no program",
            )
        })?;
        let mut this = Self::new(program);
        this.args(argv);
        Ok(this)
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.command.command_mut().arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.command_mut().args(args);
        self
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.command.command_mut().env(key, value);
        self
    }

    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.command.command_mut().env_remove(key);
        self
    }

    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.command.command_mut().current_dir(dir);
        self
    }

    /// The initial terminal size (default 24 rows by 80 columns).
    pub fn size(&mut self, size: PtySize) -> &mut Self {
        self.size = size;
        self
    }

    /// The underlying owned command, for the options it offers (Job limits, priority,
    /// environment allowlist). Do not set its stdio or its console flags: the terminal replaces
    /// them. See the module documentation.
    pub fn owned_mut(&mut self) -> &mut OwnedCommand {
        &mut self.command
    }

    /// Unix: runs `hook` in the forked child as the very last step before `exec` (after
    /// `setsid`, the controlling-terminal ioctl, priority and the descriptor sweep). Windows
    /// ignores it.
    ///
    /// # Safety
    ///
    /// As for `std::os::unix::process::CommandExt::pre_exec`: the hook runs in a forked child
    /// of a possibly multi-threaded process and may call only async-signal-safe functions.
    #[cfg(unix)]
    pub unsafe fn pre_exec_last<F>(&mut self, hook: F) -> &mut Self
    where
        F: FnMut() -> io::Result<()> + Send + Sync + 'static,
    {
        // SAFETY: the caller upholds the contract documented above, which is the callee's.
        unsafe { self.command.pre_exec_last(hook) };
        self
    }

    /// Opens the terminal and starts the program on it. Call it inside a tokio runtime.
    pub fn spawn(mut self) -> io::Result<PtyChild> {
        self.size.validate()?;
        tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let has_term = self
            .command
            .command_mut()
            .get_envs()
            .any(|(key, _)| key == "TERM");
        if !has_term {
            self.command.command_mut().env("TERM", "xterm-256color");
        }
        let (child, terminal) = imp::spawn(self.command, self.size)?;
        Ok(PtyChild {
            child: Some(child),
            terminal,
            size: self.size,
            reader_taken: false,
            writer_taken: false,
        })
    }
}

/// A running PTY child. Dropping it stops its tree like an [`OwnedChild`] and closes the
/// terminal.
#[derive(Debug)]
pub struct PtyChild {
    child: Option<OwnedChild>,
    terminal: imp::Terminal,
    size: PtySize,
    reader_taken: bool,
    writer_taken: bool,
}

impl PtyChild {
    fn owned(&self) -> &OwnedChild {
        self.child
            .as_ref()
            .expect("the child is present outside an in-flight cancel")
    }

    /// The child's process id (on Unix also its session id).
    pub fn id(&self) -> u32 {
        self.owned().id()
    }

    /// The size last set (the initial size until [`resize`](Self::resize) succeeds).
    pub fn size(&self) -> PtySize {
        self.size
    }

    /// A shared handle to the child's process (exit observation, resource usage).
    pub fn process_handle(&self) -> io::Result<ProcessHandle> {
        self.owned().process_handle()
    }

    /// The terminal's output (what the program printed, with its escape sequences) as an
    /// `AsyncRead`. Taken once; `None` afterwards.
    pub fn take_reader(&mut self) -> io::Result<Option<PtyReader>> {
        if self.reader_taken {
            return Ok(None);
        }
        let reader = PtyReader {
            inner: self.terminal.reader()?,
        };
        self.reader_taken = true;
        Ok(Some(reader))
    }

    /// The terminal's input as an `AsyncWrite` with bounded writes and EOF. Taken once.
    pub fn take_writer(&mut self) -> io::Result<Option<PtyWriter>> {
        if self.writer_taken {
            return Ok(None);
        }
        let writer = PtyWriter {
            inner: self.terminal.writer()?,
            eof: self.terminal.eof_sequence(),
        };
        self.writer_taken = true;
        Ok(Some(writer))
    }

    /// Changes the terminal size. The program is told (`SIGWINCH` on Unix, the console buffer
    /// resize on Windows).
    pub fn resize(&mut self, size: PtySize) -> io::Result<()> {
        size.validate()?;
        self.terminal.resize(size)?;
        self.size = size;
        Ok(())
    }

    /// Non-blocking: the exit status when the child has exited.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self
            .child
            .as_mut()
            .expect("the child is present outside an in-flight cancel")
            .try_wait()?;
        if status.is_some() {
            self.terminal.child_exited();
        }
        Ok(status)
    }

    /// Waits for the child to exit and returns its status. Cancel-safe: dropping the future
    /// leaves the child running and waitable. (The exit status of a Unix child killed by a
    /// signal carries the signal: `ExitStatusExt::signal`.)
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let handle = self.process_handle()?;
        let status = tokio::task::spawn_blocking(move || handle.wait())
            .await
            .map_err(io::Error::other)??;
        self.terminal.child_exited();
        Ok(status)
    }

    /// The staged kill of the whole session: Unix `SIGTERM` to the child's group, up to
    /// `options.grace` for it to leave, then `SIGKILL`, then the same for every other process
    /// group of the session (a job the shell started and moved to its own group); Windows the
    /// job is terminated with `options.windows_exit_code`. Returns the leader's status once it
    /// is reaped. Runs on the blocking pool; if the future is dropped before it finishes the
    /// kill still completes there.
    pub async fn cancel(&mut self, options: TerminationOptions) -> io::Result<ExitStatus> {
        let mut child = self
            .child
            .take()
            .expect("the child is present outside an in-flight cancel");
        let pid = child.id();
        let (child, result) = tokio::task::spawn_blocking(move || {
            let result = child.terminate_tree_with_options(options);
            (child, result)
        })
        .await
        .map_err(io::Error::other)?;
        self.child = Some(child);
        let status = result?;
        self.terminal.child_exited();
        #[cfg(unix)]
        if let Some(sid) = std::num::NonZeroU32::new(pid) {
            // Other groups of the session survive a group signal to the leader's own.
            rightkit_child_process::tokio_support::terminate_session(sid, options).await?;
        }
        #[cfg(not(unix))]
        let _ = pid;
        Ok(status)
    }

    /// Gives up the wrapper: the owned child and the streams not yet taken. The terminal stays
    /// open as long as a stream is alive; with none left it closes (the child gets a hangup on
    /// Unix).
    pub fn into_owned(mut self) -> OwnedChild {
        self.child
            .take()
            .expect("the child is present outside an in-flight cancel")
    }
}

/// The terminal's output, as a tokio `AsyncRead`. End of file once every process holding the
/// terminal has closed it (and, on Windows, the pseudoconsole is closed).
#[derive(Debug)]
pub struct PtyReader {
    inner: imp::RawReader,
}

impl AsyncRead for PtyReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

/// The terminal's input, as a tokio `AsyncWrite`. Dropping it closes the input side where the
/// platform has one (Windows) and otherwise changes nothing: Unix keeps the terminal open for
/// as long as the child or the reader lives.
#[derive(Debug)]
pub struct PtyWriter {
    inner: imp::RawWriter,
    eof: Vec<u8>,
}

impl PtyWriter {
    /// Writes all of `data` in chunks of at most [`INPUT_CHUNK`], waiting for the terminal to
    /// accept each before the next, and returns when the last is accepted. Back-pressure comes
    /// from the terminal: a program that does not read its input holds this call.
    pub async fn write_input(&mut self, data: &[u8]) -> io::Result<()> {
        for chunk in data.chunks(INPUT_CHUNK) {
            self.write_all(chunk).await?;
        }
        self.flush().await
    }

    /// Ends the input as the terminal's canonical mode does (see the module documentation).
    /// In canonical mode the end-of-file character acts only at the start of a line; after a
    /// partial line the program needs it twice, so a caller that does not know whether the
    /// last line was finished writes a newline first.
    pub async fn send_eof(&mut self) -> io::Result<()> {
        let eof = self.eof.clone();
        self.write_all(&eof).await?;
        self.flush().await
    }
}

impl AsyncWrite for PtyWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let bounded = &buf[..buf.len().min(INPUT_CHUNK)];
        Pin::new(&mut self.inner).poll_write(cx, bounded)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
