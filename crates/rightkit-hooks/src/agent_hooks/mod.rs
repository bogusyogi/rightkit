// RIGHTKIT-SERIOUS: host-side hook runner (spawn command hooks, chain, matchers, fail-closed PreToolUse) has no published rightkit-hooks equivalent; rightkit-hooks 0.2.0 only parses Claude/Codex hook payloads inside a hook process (report S-HOOKS-RUNNER)
//! `host_hooks` — lifecycle hook bus and runtime hook registration helpers.

pub mod bus;
pub mod chain;
pub mod registry;

pub use bus::{
    HookBus, HookEvent, HookOutcome, HookRevocationFilter, HookSubscription, HookUrlSink,
    PreToolUseOutcome, PreToolUseReport, ProcessLaunchAdapter,
};
pub use chain::{ChainReport, ConfiguredHooks, HookDecision, HookKind, HookResult, HookSpec};
pub use registry::{dry_run_hook_with_adapter, HookDryRunReport, HookRegistry, IdentifiedHook};

/// Environment-variable prefix of the variables hooks read and set (`<prefix>_HOOKS_JSON`,
/// `<prefix>_HOOK_EVENT`, `<prefix>_HOOK_REASON`, ...) until the host calls [`set_env_prefix`].
pub const DEFAULT_ENV_PREFIX: &str = "RIGHTKIT";
/// Workspace state folder holding the project hook file until the host calls
/// [`set_state_dir_name`].
pub const DEFAULT_STATE_DIR_NAME: &str = ".rightkit";

static ENV_PREFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static STATE_DIR_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the environment-variable prefix, upper case without the trailing `_` (for example
/// `MYAPP`). Call once at start-up; returns `false` when one was already set.
pub fn set_env_prefix(prefix: &str) -> bool {
    ENV_PREFIX.set(prefix.to_string()).is_ok()
}

/// The environment-variable prefix in force.
pub fn env_prefix() -> &'static str {
    ENV_PREFIX
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_ENV_PREFIX)
}

/// Sets the workspace state folder name with its leading dot (for example `.myapp`). Call once at
/// start-up; returns `false` when one was already set.
pub fn set_state_dir_name(name: &str) -> bool {
    STATE_DIR_NAME.set(name.to_string()).is_ok()
}

/// The workspace state folder name in force.
pub fn state_dir_name() -> &'static str {
    STATE_DIR_NAME
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_STATE_DIR_NAME)
}

/// `name` (written with the default `RIGHTKIT_` prefix) under the host's prefix. A name that
/// already carries the host's prefix is kept as it is, so a host that passes its own names (for
/// example `CODERIGHT_HOOK_ROLE` in an extra variable) does not get them prefixed twice
/// (`CODERIGHTCODERIGHT_HOOK_ROLE`). The renamed strings are interned once each, so a call
/// allocates nothing after the first.
pub(crate) fn host_env_name(name: &'static str) -> &'static str {
    let prefix = env_prefix();
    if prefix == DEFAULT_ENV_PREFIX
        || name
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('_'))
    {
        return name;
    }
    static NAMES: std::sync::Mutex<Option<std::collections::HashMap<&'static str, &'static str>>> =
        std::sync::Mutex::new(None);
    let mut guard = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(Default::default)
        .entry(name)
        .or_insert_with(|| {
            let rest = name.strip_prefix("RIGHTKIT").unwrap_or(name);
            Box::leak(format!("{prefix}{rest}").into_boxed_str())
        })
}

/// Read the JSON-form hook configuration variable (`RIGHTKIT_HOOKS_JSON`).
///
/// Hook commands arrive from the environment only in desktop and daemon
/// builds (`host-env`). A build without that feature (the iOS mobile runtime)
/// reads no environment variable here and runs no environment-selected command.
#[cfg(feature = "agent_hooks-host-env")]
pub(crate) fn hook_env(name: &'static str) -> Option<String> {
    std::env::var(host_env_name(name))
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(not(feature = "agent_hooks-host-env"))]
pub(crate) fn hook_env(_name: &'static str) -> Option<String> {
    None
}

/// Fire-and-forget hook runs still in flight (a `SessionEnd` hook, which may not block the turn).
/// A short-lived process (an in-process one-shot run) waits for them before it exits: every child
/// the engine starts can die with the process, so a hook still running at exit would be killed
/// before it did its work.
static PENDING: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>> =
    std::sync::Mutex::new(Vec::new());

/// Remember a fire-and-forget hook task so [`wait_pending`] can wait for it. Tasks that already
/// finished are dropped from the list on the way.
pub fn track_pending(task: tokio::task::JoinHandle<()>) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    pending.retain(|task| !task.is_finished());
    pending.push(task);
}

/// Wait up to `limit` for the tracked hook tasks to finish. A hook's own timeout bounds each run;
/// `limit` bounds the whole wait so a stuck hook cannot hold the process open. A task that ended
/// abnormally, and a wait that ran out of time, are reported on standard error.
pub async fn wait_pending(limit: std::time::Duration) {
    let tasks: Vec<_> = std::mem::take(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()));
    if tasks.is_empty() {
        return;
    }
    let drained = tokio::time::timeout(limit, async {
        for task in tasks {
            if let Err(error) = task.await {
                eprintln!("hook task ended abnormally: {error}");
            }
        }
    })
    .await;
    if drained.is_err() {
        eprintln!("hook tasks still running after {limit:?}");
    }
}

/// Turns this process's engine started and ended. Every turn that starts records one end on
/// every path out of it; a turn that runs in another process (a daemon) counts nothing here.
static SESSION_STARTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SESSION_ENDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SESSION_END_NOTIFY: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Called by the engine when a turn starts.
pub fn note_session_start() {
    SESSION_STARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Called by the engine once a turn's session end is recorded (after any `SessionEnd` hook task
/// was tracked with [`track_pending`]).
pub fn note_session_end() {
    SESSION_ENDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    SESSION_END_NOTIFY.notify_waiters();
}

/// Wait up to `limit` until every turn this process's engine started has recorded its end. The
/// host can report a turn finished a moment before the engine records the end; this closes that
/// gap. Returns at once when no turn ran in this process. A wait that ran out of time is reported
/// on standard error.
pub async fn wait_session_ends(limit: std::time::Duration) {
    let settled = || {
        SESSION_ENDS.load(std::sync::atomic::Ordering::SeqCst)
            >= SESSION_STARTS.load(std::sync::atomic::Ordering::SeqCst)
    };
    let waited = tokio::time::timeout(limit, async {
        loop {
            let notified = SESSION_END_NOTIFY.notified();
            if settled() {
                return;
            }
            notified.await;
        }
    })
    .await;
    if waited.is_err() {
        eprintln!("a turn's session end was not recorded within {limit:?}");
    }
}
