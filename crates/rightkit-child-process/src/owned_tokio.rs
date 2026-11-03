//! `OwnedCommand::spawn_tokio`: a contained child whose stdio are tokio streams (rightkit#555).
//!
//! The spawn is the ordinary [`OwnedCommand`] spawn, so every containment rule holds as it does
//! for [`OwnedCommand::spawn`]: on Windows the child is created suspended, assigned to its Job
//! Object and only then resumed; on Unix it leads its own process group (or session, through
//! [`OwnedCommand::unix_containment`]) and the descriptor sweep applies. What this module adds is
//! the async face: [`AsyncStdin`], [`AsyncStdout`] and [`AsyncStderr`] implement tokio's
//! `AsyncWrite` / `AsyncRead`, [`OwnedTokioChild::wait`] and [`OwnedTokioChild::terminate_tree`]
//! are futures, and dropping the child runs the same staged kill as dropping an [`OwnedChild`].
//!
//! # The Windows pipe decision
//!
//! tokio's own process pipes on Windows are overlapped named pipes, and so are the parent ends of
//! the pipes this crate and `std::process` create (the native spawn builds them with
//! `FILE_FLAG_OVERLAPPED`, see `spawn_windows::child_pipe`). tokio adopts such a pipe with
//! `tokio::process::Child*::from_std`, so every platform uses that one conversion: no bridge
//! thread, no copy, and the suspended-job spawn and handle allowlist stay exactly as they are.
//! Reads and writes are cancel-safe and dropping a stream closes its pipe end (EOF for the child
//! when it is stdin).
//!
//! # Requirements
//!
//! Call [`OwnedCommand::spawn_tokio`] from inside a tokio runtime (it registers the pipes with
//! the runtime's I/O driver). `wait` and `terminate_tree` use the runtime's blocking pool.

use crate::{OwnedChild, OwnedCommand, ProcessHandle, TerminationOptions};
use std::{io, process::ExitStatus};

/// The child's stdin as a tokio `AsyncWrite`. Dropping it closes the pipe (EOF for the child).
pub type AsyncStdin = tokio::process::ChildStdin;
/// The child's stdout as a tokio `AsyncRead`.
pub type AsyncStdout = tokio::process::ChildStdout;
/// The child's stderr as a tokio `AsyncRead`.
pub type AsyncStderr = tokio::process::ChildStderr;

/// An owned, contained child with async stdio. Dropping it kills its tree like an
/// [`OwnedChild`].
#[derive(Debug)]
pub struct OwnedTokioChild {
    child: Option<OwnedChild>,
    pub stdin: Option<AsyncStdin>,
    pub stdout: Option<AsyncStdout>,
    pub stderr: Option<AsyncStderr>,
}

impl OwnedCommand {
    /// [`spawn`](Self::spawn) with all three standard streams piped and handed back as tokio
    /// streams (`stdin` piped, `stdout` and `stderr` piped, whatever `command_mut` set). See the
    /// module documentation for the Windows pipe decision. Call it inside a tokio runtime.
    pub fn spawn_tokio(mut self) -> io::Result<OwnedTokioChild> {
        // `from_std` registers the pipes with the runtime's I/O driver and panics outside one:
        // refuse with an error instead, before anything is started.
        tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        self.pipe_stdio(true);
        let mut child = self.spawn()?;
        match streams(&mut child) {
            Ok((stdin, stdout, stderr)) => Ok(OwnedTokioChild {
                child: Some(child),
                stdin,
                stdout,
                stderr,
            }),
            Err(error) => {
                // No stream could be wrapped: the child must not be left running unobserved.
                let _ = child.terminate_tree_bounded(std::time::Duration::from_secs(2));
                Err(error)
            }
        }
    }
}

type Streams = (Option<AsyncStdin>, Option<AsyncStdout>, Option<AsyncStderr>);

fn streams(child: &mut OwnedChild) -> io::Result<Streams> {
    Ok((
        child
            .take_stdin()
            .map(tokio::process::ChildStdin::from_std)
            .transpose()?,
        child
            .take_stdout()
            .map(tokio::process::ChildStdout::from_std)
            .transpose()?,
        child
            .take_stderr()
            .map(tokio::process::ChildStderr::from_std)
            .transpose()?,
    ))
}

impl OwnedTokioChild {
    fn inner(&self) -> &OwnedChild {
        self.child
            .as_ref()
            .expect("the child is present outside an in-flight terminate_tree")
    }

    pub fn id(&self) -> u32 {
        self.inner().id()
    }

    pub fn take_stdin(&mut self) -> Option<AsyncStdin> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<AsyncStdout> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<AsyncStderr> {
        self.stderr.take()
    }

    /// A shared handle to the child's process (wait, exit observation, usage).
    pub fn process_handle(&self) -> io::Result<ProcessHandle> {
        self.inner().process_handle()
    }

    /// Non-blocking: the exit status when the child has exited.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child
            .as_mut()
            .expect("the child is present outside an in-flight terminate_tree")
            .try_wait()
    }

    /// Waits for the child to exit. Closes the child's stdin first, as `wait` does elsewhere in
    /// the crate (drop your own [`AsyncStdin`] too, or it keeps the pipe open). Cancel-safe:
    /// dropping the future leaves the child running and waitable.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.stdin = None;
        let handle = self.process_handle()?;
        tokio::task::spawn_blocking(move || handle.wait())
            .await
            .map_err(io::Error::other)?
    }

    /// The staged kill of the whole tree: Unix `SIGTERM` to the group, `options.grace` for it to
    /// leave, then `SIGKILL`; Windows the job is terminated with `options.windows_exit_code`.
    /// Returns the leader's status once reaped. Runs on the blocking pool; if the future is
    /// dropped before it finishes, the kill still completes there and the child is dropped.
    pub async fn terminate_tree(&mut self, options: TerminationOptions) -> io::Result<ExitStatus> {
        self.stdin = None;
        let mut child = self
            .child
            .take()
            .expect("the child is present outside an in-flight terminate_tree");
        let (child, result) = tokio::task::spawn_blocking(move || {
            let result = child.terminate_tree_with_options(options);
            (child, result)
        })
        .await
        .map_err(io::Error::other)?;
        self.child = Some(child);
        result
    }

    /// Gives up the async wrapper: the owned child (its std stdio already taken) and the streams
    /// still held.
    pub fn into_parts(
        mut self,
    ) -> (
        OwnedChild,
        Option<AsyncStdin>,
        Option<AsyncStdout>,
        Option<AsyncStderr>,
    ) {
        let child = self
            .child
            .take()
            .expect("the child is present outside an in-flight terminate_tree");
        (
            child,
            self.stdin.take(),
            self.stdout.take(),
            self.stderr.take(),
        )
    }
}
