#![cfg(windows)]
//! A sandboxed ConPTY child (Windows), ported from CodeRight
//! `engine/crates/tools/src/managed_pty_windows.rs`, whose raw winapi code is replaced by
//! [`rightkit_pty::ConPty`] for the console and `windows_appcontainer::ConfinedChild` for the
//! process.
//!
//! The child runs in an AppContainer inside its own kill-on-close Job Object, attached to the
//! pseudoconsole by one `CreateProcessW` (see `windows_appcontainer::spawn_pty`). ConPTY has no
//! pixel size: a [`rightkit_pty::PtySize`] that carries pixels is accepted and the pixels are
//! ignored. Every method here is a thin forwarder; no unsafe code lives in this file.

use crate::windows_appcontainer::ConfinedChild;
use rightkit_child_process::{ReapOutcome, TerminationOptions};
use std::io;
use std::process::ExitStatus;
use std::time::Duration;

/// How long `cancel` waits for the terminated Job Object to empty.
const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// The console and the confined process. Field order is the teardown order: `conpty` first,
/// `process` second.
pub(crate) struct WindowsPtyChild {
    conpty: rightkit_pty::ConPty,
    process: Option<ConfinedChild>,
}

impl WindowsPtyChild {
    pub(crate) fn new(conpty: rightkit_pty::ConPty, process: ConfinedChild) -> Self {
        Self {
            conpty,
            process: Some(process),
        }
    }

    pub(crate) fn id(&self) -> u32 {
        self.process.as_ref().map_or(0, ConfinedChild::id)
    }

    pub(crate) fn take_reader(&mut self) -> io::Result<Option<rightkit_pty::PtyReader>> {
        self.conpty.take_reader()
    }

    pub(crate) fn take_writer(&mut self) -> io::Result<Option<rightkit_pty::PtyWriter>> {
        self.conpty.take_writer()
    }

    /// ConPTY has no pixel size, so the pixels in `size` are accepted and ignored.
    pub(crate) fn resize(&mut self, size: rightkit_pty::PtySize) -> io::Result<()> {
        self.conpty.resize(size)
    }

    /// The last size set; `pixel_width` and `pixel_height` are always 0 because the Windows side
    /// ignores pixels.
    pub(crate) fn winsize(&self) -> io::Result<rightkit_pty::PtySize> {
        let size = self.conpty.size();
        Ok(rightkit_pty::PtySize::new(size.rows, size.cols))
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let process = self
            .process
            .as_mut()
            .ok_or_else(|| io::Error::other("the child is being cancelled"))?;
        let status = process.try_wait()?;
        if status.is_some() {
            // Closes the pseudoconsole so the output stream ends after the last byte.
            self.conpty.child_exited();
        }
        Ok(status)
    }

    /// Polls, so it is cancel-safe and no raw handle escapes into a detached task.
    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Terminates the whole Job Object and waits up to 5 s for it to empty. The exit code
    /// `options.windows_exit_code` does not apply to the confined job (`ConfinedChild`
    /// terminates its Job with its own code) and `options.grace` is ignored on Windows.
    pub(crate) async fn cancel(&mut self, _options: TerminationOptions) -> io::Result<ExitStatus> {
        let Some(mut child) = self.process.take() else {
            return Err(io::Error::other("the child is already being cancelled"));
        };
        let joined = tokio::task::spawn_blocking(move || {
            let result = child.terminate_tree_bounded(CANCEL_WAIT);
            (child, result)
        })
        .await;
        let (child, result) = joined.map_err(io::Error::other)?;
        self.process = Some(child);
        match result? {
            ReapOutcome::Exited(status) => {
                self.conpty.child_exited();
                Ok(status)
            }
            ReapOutcome::TimedOut => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the confined tree did not exit within 5 s of its Job Object being terminated",
            )),
        }
    }
}

impl Drop for WindowsPtyChild {
    fn drop(&mut self) {
        // The Job is terminated first so descendants die before the console closes; then the
        // fields drop in order: `conpty` (ClosePseudoConsole on its own thread), then `process`
        // (waits up to 1 s, closes handles and releases the profile leases last, so the
        // workspace ACL grants outlive the child).
        if let Some(child) = &self.process {
            let _ = child.kill_tree();
        }
    }
}
