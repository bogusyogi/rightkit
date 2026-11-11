//! The agent engine, as one crate.
//!
//! One engine and one task contract:
//!
//! * [`core`]: the turn loop (`rightkit-agent-engine-core`): tool dispatch with the permission
//!   gate and approval flow, compaction, goals, guardian and worker agents.
//! * [`protocol`]: the events, envelopes, tool calls and approval records the loop sends
//!   (`rightkit-agent-engine-protocol`). A tool call that waits for approval carries the
//!   approvals core's binding; an effect whose result was lost maps to
//!   [`TaskStatus::Uncertain`].
//! * [`storage`]: the SQLite session journal, transcripts, tasks and goals
//!   (`rightkit-agent-engine-storage`).
//! * [`contract`]: the task contract (`rightkit-agent-task-contract`): task envelope, its ten
//!   statuses and legal transitions, principals, intent provenance, effect classes, device
//!   references, approval binding, voice hand-off. Its modules and records are also re-exported
//!   at this crate's root, where they were before 0.2.0.
//!
//! The other `rightkit-agent-engine-*` crates (history, tasks, attention, runtime middleware and
//! resources, the API route crates, the mobile runtime and the command line) are used directly.

pub use rightkit_agent_engine_core as core;
pub use rightkit_agent_engine_protocol as protocol;
pub use rightkit_agent_engine_storage as storage;
pub use rightkit_agent_task_contract as contract;

pub use rightkit_agent_task_contract::*;
