//! Visible subagent windows — the one sanctioned exception to this product's
//! "zero visible console windows by default" rule.
//!
//! Every other spawn path in this workspace (`process_tree.rs`,
//! `cli_presence.rs`, `daemon/launch.rs`, ...) sets `CREATE_NO_WINDOW`
//! (`0x0800_0000`) so a subagent's child processes never flash a console at
//! the operator. That default is correct and stays untouched here. This
//! module exists only for the opposite, explicitly-requested case: an
//! operator who wants to *watch* one subagent work, live, in a real
//! terminal window.
//!
//! # Design
//!
//! When an operator opts in (`SubagentSpawnOptions::visible = true`), this
//! module builds a `CREATE_NEW_CONSOLE` (`0x0000_0010`) child process — a
//! genuine new console, the opposite flag from every other spawn path in
//! this crate — running `host app attach <session-id>`. `host app attach`
//! is the existing, already-shipped entrypoint
//! (`cli_definitions_command.rs::Command::Attach`, dispatched in
//! `main_runtime_entry.rs` to `daemon_attach` in
//! `main_runtime_daemon_streams.rs`) that resumes a session and drives the
//! real `host_tui` event loop against it. Reusing it means this module
//! never has to build a second, simplified renderer: the visible window
//! shows the exact same transcript view a human `host app attach` would.
//!
//! # Platform
//!
//! `CREATE_NEW_CONSOLE` is a Windows `CreateProcess` creation flag with no
//! portable equivalent — POSIX has no "detached window" concept, and
//! reliably launching a new terminal emulator cross-platform (which
//! emulator? which shell integration?) is not something this workspace can
//! guarantee. Per the product rule, failing to honor an explicit request
//! must never silently degrade to an invisible spawn, so non-Windows
//! platforms return [`VisibleWindowError::Unsupported`] instead of falling
//! back.
//!
//! # Lifecycle
//!
//! The visible console's process handle is returned to the caller
//! ([`spawn_visible_window`]), which is expected to hold it for exactly as
//! long as the underlying subagent work is in flight and then call
//! [`VisibleWindowHandle::close`] (or simply drop it after killing) once
//! that work reaches a terminal state. `host app attach` itself exits on
//! EOF/disconnect from the daemon side when the attached session ends, but
//! that is a courtesy, not the enforced mechanism — the owning caller
//! killing the child on subagent completion is what actually guarantees no
//! orphaned console outlives its subagent (see `spawn_subagent.rs`'s
//! `execute()`, which holds this handle across the `spawner.spawn().await`
//! and kills it in every branch of that `match`, success or error).

use std::process::Child;

/// `CREATE_NEW_CONSOLE` — the Windows `CreateProcess` flag that opens a real,
/// visible console for the child. Deliberately the mirror image of the
/// `CREATE_NO_WINDOW` (`0x0800_0000`) flag used everywhere else in this
/// crate; never mix the two up.
pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// The command this module builds to show a subagent's live transcript in a
/// visible window. Kept as a plain data type (rather than an already-spawned
/// `std::process::Command`) so the construction logic — which binary, which
/// args, which flag — can be unit-tested without ever creating a real
/// process or a real console window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleWindowCommand {
    /// The `host app` executable to launch. Always the current process's
    /// own executable — the visible window runs the same build that is
    /// hosting the subagent, so its TUI renderer is guaranteed compatible.
    pub program: String,
    /// Always `["attach", session_id]` — see module docs for why `attach`
    /// (not a bespoke log-follower) is reused here.
    pub args: Vec<String>,
    /// The Windows creation flags this command must be spawned with.
    /// `None` on non-Windows targets, where this command is never actually
    /// spawned (see [`spawn_visible_window`]).
    pub creation_flags: Option<u32>,
}

impl VisibleWindowCommand {
    /// Build the `host app attach <session_id>` command for a visible
    /// window. `host_exe` is the path to the `host app` binary to
    /// launch (normally `std::env::current_exe()`); accepting it as a
    /// parameter, rather than reading `current_exe()` inside this function,
    /// keeps this constructor pure and directly testable.
    pub fn attach(host_exe: &str, session_id: &str) -> Self {
        Self {
            program: host_exe.to_string(),
            args: vec!["attach".to_string(), session_id.to_string()],
            creation_flags: Some(CREATE_NEW_CONSOLE),
        }
    }
}

/// Why a visible subagent window could not be opened. Every variant is a
/// hard failure the caller must surface — never a signal to fall back to an
/// invisible spawn (that would silently violate the operator's explicit
/// request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisibleWindowError {
    /// The current platform cannot open a visible console window at all.
    /// Only non-Windows targets return this today.
    Unsupported { platform: String },
    /// The current executable path could not be resolved
    /// (`std::env::current_exe()` failed), so no `program` could be built.
    ExecutableUnresolvable { message: String },
    /// `CreateProcess`/`Command::spawn` itself failed (permissions, resource
    /// limits, antivirus interception, etc). Carries the OS error text.
    SpawnFailed { message: String },
}

impl std::fmt::Display for VisibleWindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VisibleWindowError::Unsupported { platform } => write!(
                f,
                "visible subagent windows are not supported on {platform}: opening a new \
                 terminal emulator cannot be done reliably cross-platform, so this request \
                 fails closed rather than spawning invisibly"
            ),
            VisibleWindowError::ExecutableUnresolvable { message } => {
                write!(
                    f,
                    "could not resolve the rightkit-agent executable path: {message}"
                )
            }
            VisibleWindowError::SpawnFailed { message } => {
                write!(f, "failed to open visible subagent window: {message}")
            }
        }
    }
}

impl std::error::Error for VisibleWindowError {}

/// A live visible-window child process, tied to one subagent's session.
/// The owning caller must call [`VisibleWindowHandle::close`] once the
/// subagent's own work reaches a terminal state (completed, failed, or
/// cancelled) so the console does not outlive its subagent.
pub struct VisibleWindowHandle {
    session_id: String,
    child: Child,
}

impl VisibleWindowHandle {
    /// The subagent session id this window is attached to.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The OS process id of the visible console child, for diagnostics.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Terminate the visible window. Called once the subagent it was
    /// watching reaches a terminal state, so no console is ever left
    /// running after its subagent has finished — the enforced half of the
    /// "no orphan consoles" contract (`host app attach` exiting on its own
    /// when the daemon session ends is a courtesy on top of this, not a
    /// substitute for it).
    pub fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Open a visible window showing `session_id`'s live transcript via
/// `host app attach`. Windows-only; see [`VisibleWindowError::Unsupported`]
/// for every other target. Never falls back to an invisible spawn on
/// failure — every error path here is a hard `Err`.
#[cfg(windows)]
pub fn spawn_visible_window(session_id: &str) -> Result<VisibleWindowHandle, VisibleWindowError> {
    use std::os::windows::process::CommandExt as _;

    let exe =
        std::env::current_exe().map_err(|error| VisibleWindowError::ExecutableUnresolvable {
            message: error.to_string(),
        })?;
    let command_spec = VisibleWindowCommand::attach(&exe.to_string_lossy(), session_id);

    // Sanctioned exception: operator-requested visible window (CREATE_NEW_CONSOLE).
    #[allow(clippy::disallowed_methods)]
    let mut command = std::process::Command::new(&command_spec.program);
    command.args(&command_spec.args);
    // `expect`: `VisibleWindowCommand::attach` always sets `creation_flags`;
    // this branch only exists under `cfg(windows)`.
    command.creation_flags(
        command_spec
            .creation_flags
            .expect("attach() always sets creation_flags"),
    );

    let child = command
        .spawn()
        .map_err(|error| VisibleWindowError::SpawnFailed {
            message: error.to_string(),
        })?;

    Ok(VisibleWindowHandle {
        session_id: session_id.to_string(),
        child,
    })
}

/// Non-Windows targets cannot reliably open a new visible terminal window
/// (no portable `CreateProcess`-style "new console" primitive exists), so
/// this fails closed with a typed [`VisibleWindowError::Unsupported`]
/// rather than silently spawning the subagent invisibly.
#[cfg(not(windows))]
pub fn spawn_visible_window(_session_id: &str) -> Result<VisibleWindowHandle, VisibleWindowError> {
    Err(VisibleWindowError::Unsupported {
        platform: std::env::consts::OS.to_string(),
    })
}
