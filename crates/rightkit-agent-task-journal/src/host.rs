//! What the journal asks of the product that embeds it.

use rightkit_agent_task_contract::TaskEnvelope;
use rightkit_job_queue::{JobError, JobResult};
use serde_json::Value;

/// Severity of a [`JournalHost::log`] line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Which tasks a worker claims. The kind sets are disjoint; a kind the host does not name runs on
/// `General`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// Every kind no other lane claims.
    General,
    /// One long-running read-only agent kind at a time.
    Agent,
    /// Long acts that may wait for the owner, one at a time.
    Act,
    /// Tasks bound to another computer: they wait for it and follow its run, and the lane also parks and
    /// wakes `waiting-device` tasks, so its idle tick is short.
    Relay,
}

/// The facts about one task kind that recovery, the lanes and the events ask of it.
#[derive(Clone, Copy, Debug)]
pub struct KindFacts {
    pub name: &'static str,
    /// Running it twice is the same as once: a lost run is retried (bounded by `MAX_ATTEMPTS`).
    pub read_only: bool,
    /// A lost run cannot have left an effect outside the journal. Such a kind that is not
    /// read-only is never retried; the lost run is settled `failed` (`interrupted`) at once.
    pub effect_free: bool,
    /// The kind keeps an audit that is its receipt query (`JournalHost::lost_run_is_clean`), and every
    /// effect passes a broker that dies with the engine, so recovery settles a lost run from it and a
    /// cancel of an `uncertain` run is final.
    pub audited: bool,
    /// A running task of this kind writes heartbeats (`RunCtl::beat`) and the reaper ends one that stopped.
    pub beats: bool,
    /// The task is bound to another computer: parked `waiting-device` while it is not reachable.
    pub needs_device: bool,
    pub lane: Lane,
    /// The route word the kind's lifecycle events carry, if it has one. A word, never content.
    pub route: Option<&'static str>,
}

/// A task that just ended `succeeded` or `failed` (a cancelled or `uncertain` one never reaches the host).
pub struct FinishedTask<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub params: &'a Value,
    /// The envelope stored at ACCEPTANCE; `None` only if the stored record no longer parses.
    pub envelope: Option<&'a TaskEnvelope>,
    /// `true`: `succeeded`; `false`: `failed`.
    pub succeeded: bool,
}

/// The product behind the journal. Methods are called from the workers and the callers' threads, some
/// while no journal lock is held (`task_finished`) and some under the journal's gate (`kind`, `kinds`,
/// `check_submit`, `lost_run_is_clean`, `device_task_expired`, `device_ready`): none may call back into the journal.
pub trait JournalHost: Send + Sync {
    /// One log line (the product's hub log).
    fn log(&self, level: LogLevel, event: &str, fields: Value);

    /// Publishes a lifecycle event (`task.*`) on the product's event bus. Delivery is at-least-once.
    fn emit(&self, topic: &str, payload: Value);

    /// The facts of one kind; `None` for a kind this engine does not run.
    fn kind(&self, name: &str) -> Option<KindFacts>;

    /// Every kind, in registry order (the `task.journal` view).
    fn kinds(&self) -> Vec<KindFacts>;

    /// Checks that belong to one kind, run at submit after retries and admission and before ACCEPTANCE.
    /// `local_device` is this computer's device id when the engine knows it.
    fn check_submit(
        &self,
        kind: &str,
        params: &Value,
        envelope: &TaskEnvelope,
        local_device: Option<&str>,
    ) -> JobResult<()>;

    /// Whether a lost run of this task is known to have changed nothing outside the journal: the kind is
    /// effect-free and not read-only, or its receipt query says so. `false` leaves an `uncertain` run for the owner.
    fn lost_run_is_clean(&self, kind: &str, task_id: &str) -> bool;

    /// Most tasks of `kind` one origin (the envelope's origin subject) may have queued or running; `None`: no limit.
    fn queue_limit(&self, _kind: &str) -> Option<usize> {
        None
    }

    /// The refusal when [`queue_limit`](Self::queue_limit) is reached.
    fn queue_full(&self, kind: &str) -> JobError {
        JobError::new("queue_full", format!("too many {kind} tasks are queued"))
    }

    /// A `needs_device` task whose own time bound passed while it waited fails and is never run late.
    fn device_task_expired(&self, _params: &Value) -> bool {
        false
    }

    /// Whether the computer a `needs_device` task is bound to is reachable.
    fn device_ready(&self, _device_id: &str) -> bool {
        false
    }

    /// A task ended `succeeded` or `failed` and its status is committed. Runs after the gate is released and
    /// never changes the task.
    fn task_finished(&self, _task: &FinishedTask<'_>) {}

    /// A running task of a kind that `beats` whose last heartbeat is older than this is ended by the reaper.
    fn stall_after_ms(&self) -> i64 {
        150_000
    }

    /// Link request rows live until their expiry plus this margin (the clock skew the authorizer tolerates).
    fn link_prune_margin_ms(&self) -> u64 {
        crate::link::DEFAULT_LINK_PRUNE_MARGIN_MS
    }

    /// Prefix of the worker thread names (`<prefix>-worker`, `<prefix>-agent-worker`, ...).
    fn thread_prefix(&self) -> &str {
        "journal"
    }
}
