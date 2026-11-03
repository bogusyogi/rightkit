//! Hide the console window of a child you build yourself (`std::process::Command`, and
//! `tokio::process::Command` with the `tokio` feature), without going through
//! [`OwnedCommand`](crate::OwnedCommand).

/// The Windows `CREATE_NO_WINDOW` creation flag, exposed on every platform so a caller that
/// composes its own flags can OR it in.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The Windows `CREATE_BREAKAWAY_FROM_JOB` creation flag, exposed on every platform so a caller
/// that composes its own flags (for example a deliberately detached replacement process) can OR
/// it in. It asks the new process to leave the creator's job object; the launch fails with
/// access denied when that job forbids breakaway.
pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// A `std::process::Command` for `program` that already has the no-window flag set on Windows,
/// returned by value so it chains: `command_std("git").args(["status"]).output()`. Same effect as
/// `Command::new(program)` followed by [`NoWindow::no_window`]; on Unix the child also inherits
/// no descriptor above stderr (see `SanitizeDescriptors`; pass
/// descriptors with a later `sanitize_descriptors(&[fd])`). The async variant is
/// `tokio_support::command` (feature `tokio`).
pub fn command_std(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.no_window();
    // Unix: the child inherits no descriptor above stderr (Orthic-Labs/rightkit#835).
    #[cfg(unix)]
    crate::SanitizeDescriptors::sanitize_descriptors(&mut command, &[]);
    command
}

/// Suppresses the child's console window on Windows. A no-op elsewhere.
///
/// On Windows this calls `creation_flags(CREATE_NO_WINDOW)`, which *replaces* any creation
/// flags set earlier on the same command (that is how `creation_flags` works for both std and
/// tokio). Call it first and OR any further flags yourself with [`CREATE_NO_WINDOW`], or call
/// `creation_flags(CREATE_NO_WINDOW | other)` directly.
pub trait NoWindow {
    /// Sets the no-window creation flag. Returns `self` for chaining.
    fn no_window(&mut self) -> &mut Self;
}

impl NoWindow for std::process::Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}
