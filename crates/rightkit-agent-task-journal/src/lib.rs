//! The durable agent task journal over `rightkit_job_queue::JobStore`: acceptance, owner epoch
//! fencing, restart recovery, the lifecycle event outbox, device-link request replay and the lane
//! workers. The product (kinds, executors, logging, the event bus, finished-task effects) is behind
//! [`JournalHost`]; the task types are `rightkit-agent-task-contract`'s.
//!
//! Modules
//! * `claude_cli`: where the Claude Code CLI is, whether a preset still turns tools off, the widened
//!   `PATH`, the stale-scratch sweep.
//! * `host`: [`JournalHost`], [`KindFacts`], [`Lane`], [`FinishedTask`], [`LogLevel`].
//! * `journal`: [`Journal`], [`RunCtl`], [`RunState`], the task types ([`TaskInput`], [`TaskError`], [`Executor`]),
//!   and `worker` ([`JournalService`]).
//! * `link`: the device-link replay row types and SQL.
//! * `schema`: the storage shape, the transition hook, the metadata table and the version check.

pub mod claude_cli;
mod host;
mod journal;
mod link;
mod schema;

pub use host::{FinishedTask, JournalHost, KindFacts, Lane, LogLevel};
pub use journal::{
    Executor, Journal, JournalService, RunCtl, RunState, TaskError, TaskInput, MAX_ATTEMPTS, TOPICS,
};
pub use link::{
    LinkEntry, LinkOutcome, LinkRecord, LinkSubmit, DEFAULT_LINK_PRUNE_MARGIN_MS, UNAVAILABLE_CODE,
};
pub use schema::{now_ms, parse_status, pulse_name, store_name, topic_for, JOURNAL_SCHEMA_VERSION};
