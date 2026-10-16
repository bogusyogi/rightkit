//! Hide the console window of a child you build yourself (`std::process::Command`, and
//! `tokio::process::Command` with the `tokio` feature), without going through
//! [`OwnedCommand`](crate::OwnedCommand).

/// The Windows `CREATE_NO_WINDOW` creation flag, exposed on every platform so a caller that
/// composes its own flags can OR it in.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_value_is_the_windows_constant() {
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    }

    #[test]
    fn a_std_command_chains_and_still_runs() {
        let mut command = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "exit 0"]);
            c
        } else {
            std::process::Command::new("true")
        };
        let status = command.no_window().status().expect("run");
        assert!(status.success());
    }
}
