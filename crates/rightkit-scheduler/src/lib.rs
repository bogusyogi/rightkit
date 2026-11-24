//! Agent schedule model: bot profiles and schedule lifecycle types, schedule specs (cron, once,
//! interval, monitor) with legacy parsing, due-minute computation in IANA time zones, and time
//! zone normalisation. The daemon runner that claims and fires schedules lives with the host
//! server; this crate holds the portable model it runs on.

pub mod model;
pub mod profile;
pub mod runner;
pub mod time;
pub mod timezone;
#[cfg(feature = "iana-full")]
mod timezone_full;

pub use model::{DeliveryTarget, ScheduleEntry, ScheduleSpec};
pub use profile::{
    BotBudget, BotProfileV1, BrowserPolicy, MisfirePolicy, OverlapPolicy, ScheduleKind,
    ScheduleLifecycleState, VerifierBinding,
};
pub use runner::{
    claim_due, claim_schedule, init_claim_tables, Claim, RunnerError, RunnerSchedule, TickOutcome,
    MAX_CATCHUP_MINUTES, ON_TIME_GRACE_MINUTES,
};
pub use time::{current_epoch_minute, next_due_epoch_minute_in_time_zone};
pub use timezone::{normalize_time_zone, time_zone_offset_minutes_at};
