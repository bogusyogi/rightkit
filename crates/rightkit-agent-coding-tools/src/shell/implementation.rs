#[cfg(target_os = "linux")]
#[path = "parent_death.rs"]
pub(crate) mod parent_death;

static ANCHOR_CHILDREN_TO_PARENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Chooses whether children launched through [`NativeCommandSandboxAdapter`] and the shell tool
/// die with this process. `false` (the default) leaves them as they always were: a child that is
/// still running when the host exits is not signalled. `true` makes the kernel SIGKILL each such
/// child when the host process exits (Linux `PR_SET_PDEATHSIG`, with every spawn made from one
/// process-lifetime thread; other platforms are unchanged, so this is a no-op there). Call once at
/// start-up; returns `false` (and changes nothing) when a choice was already made.
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
