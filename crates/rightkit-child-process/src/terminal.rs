use std::{
    io,
    process::{Command, ExitStatus},
};

/// Run a foreground terminal pass-through & wait without an execution deadline.
///
/// Preserves arguments, environment, working directory & stdio. Unconfigured stdio inherits
/// the parent's streams, as for `Command::status`. On Unix, descriptors above stderr are
/// marked close-on-exec, including descriptors allowed by an earlier sanitizing hook.
/// No process group, session, console-window flag, containment job or capture is installed.
/// In particular, Unix children stay in the caller's foreground process group unless the
/// caller itself configured otherwise. This is an intentionally uncontained terminal operation,
/// distinct from [`crate::OwnedCommand`]; it does not change owned-child defaults.
/// Returns the original standard-library launch/wait result, without runner provenance.
pub fn status_inherited_terminal(command: &mut Command) -> io::Result<ExitStatus> {
    #[cfg(unix)]
    crate::SanitizeDescriptors::sanitize_descriptors(command, &[]);
    command.status()
}
