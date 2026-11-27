#[cfg(unix)]
#[path = "parent_death.rs"]
pub mod parent_death;

static ANCHOR_CHILDREN_TO_PARENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Chooses whether children launched through [`NativeCommandSandboxAdapter`], the shell tool,
/// `process_launch::LocalProcessLaunchAdapter` die with this process. The Linux PTY transport
/// does not anchor its child and is not covered.
/// `false` (the default) leaves them as they always were: a child that is still running when the
/// host exits is not signalled. `true` makes every such child die with the host: on Linux the
/// kernel SIGKILLs it when the host process exits (`PR_SET_PDEATHSIG`, with every spawn made from
/// one process-lifetime thread) and its whole process group is tied to the host by a watchdog; on
/// macOS the watchdog alone ties the group (see `parent_death`); other platforms are unchanged.
/// A child that cannot be tied is killed and its launch fails. Call once at start-up; returns
/// `false` (and changes nothing) when a choice was already made.
pub fn set_anchor_children_to_parent(anchor: bool) -> bool {
    ANCHOR_CHILDREN_TO_PARENT.set(anchor).is_ok()
}

/// Whether children are anchored to this process (see [`set_anchor_children_to_parent`]).
pub fn anchor_children_to_parent() -> bool {
    ANCHOR_CHILDREN_TO_PARENT.get().copied().unwrap_or(false)
}

include!("host_options.rs");
include!("tools.rs");
include!("execution_helpers.rs");
include!("result_sandbox.rs");
include!("terminal_stream.rs");
include!("change_diff.rs");
