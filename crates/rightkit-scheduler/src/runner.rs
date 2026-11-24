//! The schedule runner as a library: decides which schedule minutes are due and claims each one
//! exactly once in SQLite. It runs nothing itself; the host fires the work for every [`Claim`]
//! returned.
//!
//! * A claim is keyed on `(schedule id, UTC epoch minute)` with a primary key, taken in one
//!   `BEGIN IMMEDIATE` transaction together with the schedule's cursor, so two runners (or a
//!   restart) cannot both win a minute.
//! * `MisfirePolicy` decides what a late tick does with minutes that came due while the runner
//!   was not ticking: `Skip` drops them, `Coalesce` runs once for all of them, `CatchUp` runs each.
//! * Cron times are matched on the zone's wall clock. A wall time that falls in a clock gap
//!   (spring forward) runs once, at the first minute after the gap; a wall time that occurs twice
//!   (fall back) runs once, at its first occurrence.
//! * Nothing is scheduled until the host confirms it: a schedule with `confirmed == false` is
//!   never claimed and its cursor never moves.
//!
//! Zone offsets come from `rightkit_time::zones::offset_at`, whose tables are limited (see the
//! README). A zone or year outside them is an error for that schedule, never a guess.

use std::fmt;

use rightkit_db::rusqlite::{params, Connection, OptionalExtension};

use crate::model::ScheduleSpec;
use crate::profile::MisfirePolicy;
use crate::time::cron_matches_local_minute;
use crate::timezone::{normalize_time_zone, time_zone_offset_minutes_at};

/// A minute this close to the tick (the tick's own minute and the one before it) is on time. An
/// older due minute is a misfire and goes through the schedule's [`MisfirePolicy`].
pub const ON_TIME_GRACE_MINUTES: i64 = 1;

/// The longest gap one tick looks back across. A runner that was down longer resumes from
/// `now - MAX_CATCHUP_MINUTES`; the older minutes are dropped.
pub const MAX_CATCHUP_MINUTES: i64 = 7 * 24 * 60;

/// How far back to look for the first occurrence of a repeated wall minute (a DST fold is at most
/// two hours).
const FOLD_LOOKBACK_MINUTES: i64 = 180;

/// A schedule as the runner needs it.
#[derive(Clone, Debug)]
pub struct RunnerSchedule {
    pub id: String,
    pub spec: ScheduleSpec,
    pub misfire: MisfirePolicy,
    /// The host has confirmed this schedule. The runner claims nothing for an unconfirmed one.
    pub confirmed: bool,
}

/// One minute of one schedule that this runner won and the host should now run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub schedule_id: String,
    /// UTC minutes since the epoch.
    pub minute: i64,
}

/// What one tick over several schedules produced. A schedule that fails (bad expression, a zone
/// outside the offset tables) is listed in `errors` and rolled back; the others still run.
#[derive(Debug, Default)]
pub struct TickOutcome {
    pub claims: Vec<Claim>,
    pub errors: Vec<(String, RunnerError)>,
}

#[derive(Debug)]
pub enum RunnerError {
    Db(rightkit_db::DbError),
    Sqlite(rightkit_db::rusqlite::Error),
    /// The schedule itself is not valid or cannot be evaluated.
    Spec(String),
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Db(e) => write!(f, "scheduler database: {e}"),
            Self::Sqlite(e) => write!(f, "scheduler database: {e}"),
            Self::Spec(e) => write!(f, "schedule: {e}"),
        }
    }
}

impl std::error::Error for RunnerError {}

impl From<rightkit_db::DbError> for RunnerError {
    fn from(e: rightkit_db::DbError) -> Self {
        Self::Db(e)
    }
}

impl From<rightkit_db::rusqlite::Error> for RunnerError {
    fn from(e: rightkit_db::rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

/// Create the runner's two tables if they are missing. Idempotent; the host may call it at every
/// start or fold the statements into its own migrations.
pub fn init_claim_tables(conn: &Connection) -> Result<(), RunnerError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schedule_claims (
             schedule_id   TEXT NOT NULL,
             minute        INTEGER NOT NULL,
             outcome       TEXT NOT NULL CHECK (outcome IN ('claimed', 'skipped', 'coalesced')),
             claimed_at_ms INTEGER NOT NULL,
             PRIMARY KEY (schedule_id, minute)
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS schedule_cursor (
             schedule_id    TEXT NOT NULL PRIMARY KEY,
             through_minute INTEGER NOT NULL
         ) WITHOUT ROWID;",
    )?;
    Ok(())
}

/// Claim every due minute of every schedule at `now_ms` (UTC epoch milliseconds). One
/// transaction per schedule.
pub fn claim_due(conn: &mut Connection, schedules: &[RunnerSchedule], now_ms: i64) -> TickOutcome {
    let mut out = TickOutcome::default();
    for schedule in schedules {
        match claim_schedule(conn, schedule, now_ms) {
            Ok(mut claims) => out.claims.append(&mut claims),
            Err(e) => out.errors.push((schedule.id.clone(), e)),
        }
    }
    out
}

/// Claim the due minutes of one schedule in one transaction: read its cursor, work out the due
/// minutes after it up to `now_ms`, apply the misfire policy, insert the claims (the primary key
/// refuses a minute that is already claimed), move the cursor and commit. An error rolls the whole
/// thing back, cursor included.
///
/// The first tick of a confirmed schedule only sets the cursor to the current minute: nothing
/// before the confirmation is replayed.
pub fn claim_schedule(
    conn: &mut Connection,
    schedule: &RunnerSchedule,
    now_ms: i64,
) -> Result<Vec<Claim>, RunnerError> {
    if !schedule.confirmed {
        return Ok(Vec::new());
    }
    schedule.spec.validate().map_err(RunnerError::Spec)?;
    let now_minute = now_ms.div_euclid(60_000);
    let tx = rightkit_db::write_txn(conn)?;
    let cursor: Option<i64> = tx
        .query_row(
            "SELECT through_minute FROM schedule_cursor WHERE schedule_id = ?1",
            params![schedule.id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(cursor) = cursor else {
        tx.execute(
            "INSERT INTO schedule_cursor (schedule_id, through_minute) VALUES (?1, ?2)",
            params![schedule.id, now_minute],
        )?;
        tx.commit()?;
        return Ok(Vec::new());
    };
    if now_minute <= cursor {
        // The clock did not advance (or went back): nothing new is due, and a backward clock must
        // not move the cursor, or minutes would be claimed twice. Dropping the transaction rolls
        // back its (empty) work.
        return Ok(Vec::new());
    }
    let after = cursor.max(now_minute.saturating_sub(MAX_CATCHUP_MINUTES));
    let due = due_minutes(&schedule.spec, after, now_minute).map_err(RunnerError::Spec)?;
    let on_time_from = now_minute.saturating_sub(ON_TIME_GRACE_MINUTES);
    let missed = due.iter().any(|m| *m < on_time_from);
    let mut claimed = Vec::new();
    let insert = |minute: i64, outcome: &str| -> Result<bool, RunnerError> {
        let changed = tx.execute(
            "INSERT OR IGNORE INTO schedule_claims (schedule_id, minute, outcome, claimed_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![schedule.id, minute, outcome, now_ms],
        )?;
        Ok(changed == 1)
    };
    match schedule.misfire {
        MisfirePolicy::CatchUp => {
            for minute in &due {
                if insert(*minute, "claimed")? {
                    claimed.push(*minute);
                }
            }
        }
        MisfirePolicy::Skip => {
            for minute in &due {
                if *minute >= on_time_from {
                    if insert(*minute, "claimed")? {
                        claimed.push(*minute);
                    }
                } else {
                    insert(*minute, "skipped")?;
                }
            }
        }
        MisfirePolicy::Coalesce => {
            // Any missed minute: one run for everything due, on the latest minute. Otherwise each
            // on-time minute runs as normal.
            let last = due.last().copied();
            for minute in &due {
                if !missed || Some(*minute) == last {
                    if insert(*minute, "claimed")? {
                        claimed.push(*minute);
                    }
                } else {
                    insert(*minute, "coalesced")?;
                }
            }
        }
    }
    tx.execute(
        "UPDATE schedule_cursor SET through_minute = ?2 WHERE schedule_id = ?1",
        params![schedule.id, now_minute],
    )?;
    tx.commit()?;
    Ok(claimed
        .into_iter()
        .map(|minute| Claim {
            schedule_id: schedule.id.clone(),
            minute,
        })
        .collect())
}

/// The UTC minutes in `(after, through]` at which `spec` is due, ascending. A monitor is driven by
/// events, not the clock, so it is never due here.
fn due_minutes(spec: &ScheduleSpec, after: i64, through: i64) -> Result<Vec<i64>, String> {
    match spec {
        ScheduleSpec::Once { at_unix_ms } => {
            let minute = at_unix_ms.div_euclid(60_000);
            Ok(if minute > after && minute <= through {
                vec![minute]
            } else {
                Vec::new()
            })
        }
        ScheduleSpec::Interval {
            every_seconds,
            start_unix_ms,
        } => {
            let start = start_unix_ms.ok_or_else(|| "interval needs a start time".to_string())?;
            let step = (*every_seconds as i64).saturating_mul(1_000);
            if step <= 0 {
                return Err("interval must be positive".to_string());
            }
            let lo = (after + 1).saturating_mul(60_000);
            let mut k = if lo <= start {
                0
            } else {
                (lo - start + step - 1) / step
            };
            let mut out = Vec::new();
            loop {
                let minute = start
                    .saturating_add(k.saturating_mul(step))
                    .div_euclid(60_000);
                if minute > through {
                    break;
                }
                if out.last() != Some(&minute) {
                    out.push(minute);
                }
                k += 1;
            }
            Ok(out)
        }
        ScheduleSpec::Cron {
            expression,
            time_zone,
        } => cron_due(expression, time_zone, after, through),
        ScheduleSpec::Monitor { .. } => Ok(Vec::new()),
    }
}

/// UTC offset in minutes of `zone` (already normalised) at a UTC epoch minute. Fixed `UTC` and
/// `UTC+HH:MM` zones need no table; named zones go through `rightkit_time::zones::offset_at`.
fn offset_minutes(zone: &str, utc_minute: i64) -> Result<i32, String> {
    if zone == "UTC" || zone.starts_with("UTC+") || zone.starts_with("UTC-") {
        return time_zone_offset_minutes_at(zone, utc_minute);
    }
    let seconds = rightkit_time::zones::offset_at(zone, utc_minute.saturating_mul(60))
        .map_err(|(code, message)| format!("{code}: {message}"))?;
    if seconds % 60 != 0 {
        return Err(format!("{zone} has an offset that is not whole minutes"));
    }
    Ok(seconds / 60)
}

/// Whether the wall minute at UTC minute `u` (offset `cur`) already happened once at an earlier
/// UTC minute: the second pass through a fall-back overlap.
fn is_second_occurrence(zone: &str, u: i64, cur: i32) -> Result<bool, String> {
    if offset_minutes(zone, u - FOLD_LOOKBACK_MINUTES)? <= cur {
        return Ok(false);
    }
    let local = u + i64::from(cur);
    for back in 1..=FOLD_LOOKBACK_MINUTES {
        let earlier = u - back;
        if earlier + i64::from(offset_minutes(zone, earlier)?) == local {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cron_due(expr: &str, zone: &str, after: i64, through: i64) -> Result<Vec<i64>, String> {
    let zone = normalize_time_zone(zone)?;
    // A malformed expression fails even when no minute is in range.
    cron_matches_local_minute(expr, 0)?;
    let mut out = Vec::new();
    let mut prev = offset_minutes(&zone, after)?;
    for u in (after + 1)..=through {
        let cur = offset_minutes(&zone, u)?;
        if cur > prev {
            // Clock gap: the wall minutes u+prev .. u+cur-1 do not exist. A cron time in them runs
            // once, now, at the first minute after the gap (together with the wall minute u+cur).
            let hit = ((u + i64::from(prev))..=(u + i64::from(cur)))
                .try_fold(false, |hit, local| {
                    cron_matches_local_minute(expr, local).map(|m| hit || m)
                })?;
            if hit {
                out.push(u);
            }
        } else if !is_second_occurrence(&zone, u, cur)?
            && cron_matches_local_minute(expr, u + i64::from(cur))?
        {
            // Overlap: the second pass through a repeated wall minute is skipped above, so a time
            // that occurs twice runs once, at its first occurrence.
            out.push(u);
        }
        prev = cur;
    }
    Ok(out)
}
