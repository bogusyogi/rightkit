//! Workspace schedule runner for `schedule_cron` records.
//!
//! The tool writes durable records into `.coderight/schedules.json`; this daemon
//! runner claims due records, prevents duplicate fires per minute, and submits
//! the scheduled prompt through the normal session/message path.

mod delivery;
mod execution;
mod lifecycle;
mod model;
mod persistence;
pub(crate) mod time;

#[allow(unused_imports)]
pub(crate) use execution::{
    fire_schedule_by_id, script_only_execution_backend, ScriptOnlyExecutionBackend,
};
#[allow(unused_imports)]
pub use lifecycle::has_enabled_schedules_with_future_occurrence;
pub use lifecycle::spawn_schedule_runner;
pub(crate) use lifecycle::sync_workflow_schedule_compat;
pub(crate) use lifecycle::{
    disable_workflow_schedule_compat, ensure_workflow_schedule_mapping,
    workflow_schedule_cutover_enabled,
};
#[allow(unused_imports)]
pub use model::{DeliveryTarget, ScheduleEntry, ScheduleSpec};
#[allow(unused_imports)]
pub use persistence::claim_due_schedules;
pub use time::{current_epoch_minute, next_due_epoch_minute_in_time_zone, normalize_time_zone};
