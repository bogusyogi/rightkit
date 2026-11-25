//! The schedule service: what a host does with schedule records between the owner's card and the runner's claim.
//!
//! Neutral logic over a [`Connection`] the host opens (where the file lives, who may call which method, what is logged and what a
//! claim fires all stay with the host):
//!
//! * **Nothing is scheduled until the owner confirms the card.** [`add`] stores an unconfirmed schedule and returns its next
//!   [`SHOWN`] run times and a digest of them; [`confirm`] confirms exactly what was shown (a digest that differs, or a first
//!   time already past, is refused `schedule_changed`). The runner is asked only for confirmed schedules ([`claim_confirmed`]).
//! * **Sync rows.** [`apply_synced_row`] lands a row that arrived through the owner's own encrypted sync; call it from nowhere else.
//!
//! Errors are [`ServiceError`]: bad input, a refusal with a stable code, or a storage failure the host logs and hides.

use std::fmt::Display;

use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::model::{ScheduleSpec, MIN_INTERVAL_SECONDS};
use crate::profile::MisfirePolicy;
use crate::rows::{
    delete_row, get_row, list_rows, misfire_of, misfire_word, upsert_row, ScheduleRow,
};
use crate::runner::{claim_due, RunnerSchedule, TickOutcome};
use crate::time::next_due_epoch_minute_in_time_zone;
use crate::timezone::normalize_time_zone;

/// Most schedules a host holds.
pub const MAX_SCHEDULES: i64 = 50;
/// Longest label kept, in characters.
pub const LABEL_MAX_CHARS: usize = 80;
/// How far ahead a cron time is looked for (a week and a day: a weekly schedule and a clock change both fit).
pub const SEARCH_MINUTES: i64 = 8 * 24 * 60;
/// How many next run times the owner is shown.
pub const SHOWN: usize = 3;

/// Why a service call did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    /// The caller's parameters are wrong; the text says how.
    BadInput(String),
    /// Refused, with a stable code a caller can match.
    Refused { code: &'static str, message: String },
    /// The records could not be read or written; the host logs the text and shows a plain refusal.
    Storage(String),
}

pub type ServiceResult<T> = Result<T, ServiceError>;

fn refuse(code: &'static str, message: impl Into<String>) -> ServiceError {
    ServiceError::Refused {
        code,
        message: message.into(),
    }
}

fn bad(message: impl Into<String>) -> ServiceError {
    ServiceError::BadInput(message.into())
}

fn storage(error: impl Display) -> ServiceError {
    ServiceError::Storage(error.to_string())
}

/// A schedule id: 1 to 64 characters, ASCII letters and digits, `-` and `_`.
pub fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// The `id` parameter of a call, as a plain id.
pub fn id_param(params: &Value) -> ServiceResult<String> {
    match params.get("id").and_then(Value::as_str) {
        Some(id) if plain_id(id) => Ok(id.to_string()),
        Some(_) => Err(bad("param `id` is not a plain id")),
        None => Err(bad("missing param `id`")),
    }
}

/// The 16-character digest of what the owner was shown: the id, the spec text and the run times.
pub fn digest_of(id: &str, spec: &str, shown: &[i64]) -> String {
    let text = format!("schedule:{id}:{spec}:{shown:?}");
    let full = rightkit_fs::digest::sha256_bytes(text.as_bytes());
    full.chars().take(16).collect()
}

/// The next [`SHOWN`] run times (Unix seconds) after `now_ms`, or why they cannot be worked out.
pub fn next_runs(spec: &ScheduleSpec, now_ms: i64) -> Result<Vec<i64>, String> {
    let now = now_ms / 1000;
    match spec {
        ScheduleSpec::Cron {
            expression,
            time_zone,
        } => {
            let mut after = now.div_euclid(60);
            let mut found = Vec::new();
            for _ in 0..SHOWN {
                match next_due_epoch_minute_in_time_zone(
                    expression,
                    time_zone,
                    after,
                    SEARCH_MINUTES,
                )? {
                    Some(minute) => {
                        found.push(minute * 60);
                        after = minute;
                    }
                    None => break,
                }
            }
            Ok(found)
        }
        ScheduleSpec::Interval {
            every_seconds,
            start_unix_ms,
        } => {
            let every =
                i64::try_from(*every_seconds).map_err(|_| "interval too long".to_string())?;
            let start = start_unix_ms.map_or(now, |ms| ms / 1000);
            let first = if start > now {
                start
            } else {
                start + ((now - start) / every + 1) * every
            };
            Ok((0..SHOWN as i64).map(|k| first + k * every).collect())
        }
        ScheduleSpec::Once { at_unix_ms } => {
            let at = at_unix_ms / 1000;
            Ok(if at > now { vec![at] } else { Vec::new() })
        }
        ScheduleSpec::Monitor { .. } => Err("monitor schedules are not offered".to_string()),
    }
}

/// The spec a caller described: `daily {time: "HH:MM", zone}`, `interval {everySeconds}` or `once {atUnix}`.
pub fn spec_of(params: &Value) -> ServiceResult<ScheduleSpec> {
    match params.get("kind").and_then(Value::as_str) {
        Some("daily") => {
            let time = params.get("time").and_then(Value::as_str).unwrap_or("");
            let (hour, minute) = time
                .split_once(':')
                .and_then(|(h, m)| h.parse::<u8>().ok().zip(m.parse::<u8>().ok()))
                .filter(|(h, m)| *h < 24 && *m < 60)
                .ok_or_else(|| bad("`time` is HH:MM"))?;
            let zone = params.get("zone").and_then(Value::as_str).unwrap_or("UTC");
            let zone = normalize_time_zone(zone).map_err(|_| {
                refuse(
                    "unsupported_zone",
                    "that time zone is not one RightKit's tables know",
                )
            })?;
            Ok(ScheduleSpec::Cron {
                expression: format!("{minute} {hour} * * *"),
                time_zone: zone,
            })
        }
        Some("interval") => {
            let every = params
                .get("everySeconds")
                .and_then(Value::as_u64)
                .filter(|s| (MIN_INTERVAL_SECONDS..=30 * 86_400).contains(s))
                .ok_or_else(|| bad("`everySeconds` is 60 to 30 days"))?;
            Ok(ScheduleSpec::Interval {
                every_seconds: every,
                start_unix_ms: None,
            })
        }
        Some("once") => {
            let at = params
                .get("atUnix")
                .and_then(Value::as_i64)
                .ok_or_else(|| bad("`atUnix` is Unix seconds"))?;
            Ok(ScheduleSpec::Once {
                at_unix_ms: at.saturating_mul(1000),
            })
        }
        _ => Err(bad("`kind` is daily, interval or once")),
    }
}

/// One schedule as the JSON a surface shows.
pub fn row_value(
    id: &str,
    label: &str,
    spec: &str,
    misfire: &str,
    shown: &str,
    digest: &str,
    confirmed: Option<i64>,
) -> Value {
    json!({
        "id": id,
        "label": label,
        "spec": serde_json::from_str::<Value>(spec).unwrap_or(Value::Null),
        "misfire": misfire,
        "nextRuns": serde_json::from_str::<Value>(shown).unwrap_or(Value::Null),
        "digest": digest,
        "confirmed": confirmed.is_some(),
        "confirmedAt": confirmed.map(|ms| ms / 1000),
    })
}

/// `{"schedules": [...]}`, oldest first.
pub fn list(conn: &Connection) -> ServiceResult<Value> {
    let mut statement = conn
        .prepare("SELECT id, label, spec, misfire, shown, digest, confirmed_ms FROM schedules ORDER BY created_ms")
        .map_err(storage)?;
    let rows = statement
        .query_map([], |row| {
            Ok(row_value(
                &row.get::<_, String>(0)?,
                &row.get::<_, String>(1)?,
                &row.get::<_, String>(2)?,
                &row.get::<_, String>(3)?,
                &row.get::<_, String>(4)?,
                &row.get::<_, String>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<Value>, _>>()
        .map_err(storage)?;
    Ok(json!({ "schedules": rows }))
}

/// Adds an unconfirmed schedule. `open` is called only after the parameters are checked and the run times worked out, so a bad
/// request is refused before the host's storage is touched.
pub fn add(
    open: impl FnOnce() -> Result<Connection, String>,
    params: &Value,
    now_ms: i64,
) -> ServiceResult<Value> {
    let id = id_param(params)?;
    let label: String = params
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(LABEL_MAX_CHARS)
        .collect();
    let misfire = match params.get("misfire").and_then(Value::as_str) {
        None => MisfirePolicy::Skip,
        Some(word) => {
            misfire_of(word).ok_or_else(|| bad("`misfire` is skip, coalesce or catch_up"))?
        }
    };
    let spec = spec_of(params)?;
    let shown = next_runs(&spec, now_ms).map_err(|why| refuse("unsupported_zone", why))?;
    if shown.is_empty() {
        return Err(refuse(
            "never_runs",
            "that schedule has no run time ahead of it",
        ));
    }
    let spec_text = serde_json::to_string(&spec).map_err(storage)?;
    let digest = digest_of(&id, &spec_text, &shown);
    let shown_text = serde_json::to_string(&shown).map_err(storage)?;
    let conn = open().map_err(storage)?;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schedules", [], |row| row.get(0))
        .map_err(storage)?;
    if count >= MAX_SCHEDULES {
        return Err(refuse(
            "too_many_schedules",
            "remove a schedule before adding another",
        ));
    }
    // A new schedule is never confirmed here, and a schedule that exists is not replaced: remove it first.
    if get_row(&conn, &id).map_err(storage)?.is_some() {
        return Err(refuse(
            "exists",
            "a schedule with that id exists; remove it first",
        ));
    }
    upsert_row(
        &conn,
        &ScheduleRow {
            id: id.clone(),
            label: label.clone(),
            spec: spec.clone(),
            misfire,
            shown: shown.clone(),
            digest: digest.clone(),
            created_ms: now_ms,
            confirmed_ms: None,
            updated_ms: now_ms,
        },
    )
    .map_err(storage)?;
    Ok(row_value(
        &id,
        &label,
        &spec_text,
        misfire_word(misfire),
        &shown_text,
        &digest,
        None,
    ))
}

/// Confirms the schedule `params.id` when `params.digest` is what was shown. Returns the reply and whether this call made the
/// confirmation (false when it was already confirmed).
pub fn confirm(conn: &Connection, params: &Value, now_ms: i64) -> ServiceResult<(Value, bool)> {
    let id = id_param(params)?;
    let given = params.get("digest").and_then(Value::as_str).unwrap_or("");
    let found: Option<(String, String, String, Option<i64>)> = conn
        .query_row(
            "SELECT spec, shown, digest, confirmed_ms FROM schedules WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(storage)?;
    let Some((spec_text, shown_text, digest, confirmed)) = found else {
        return Err(refuse("not_found", "no schedule has that id"));
    };
    if confirmed.is_some() {
        return Ok((
            json!({ "id": id, "confirmed": true, "changed": false }),
            false,
        ));
    }
    let shown: Vec<i64> = serde_json::from_str(&shown_text).map_err(storage)?;
    let first_still_ahead = shown.first().is_some_and(|t| *t > now_ms / 1000);
    if given != digest || digest_of(&id, &spec_text, &shown) != digest || !first_still_ahead {
        return Err(refuse(
            "schedule_changed",
            "what you were shown is not what would run now; add the schedule again and look at its dates",
        ));
    }
    if let Some(mut row) = get_row(conn, &id).map_err(storage)? {
        row.confirmed_ms = Some(now_ms);
        row.updated_ms = now_ms;
        upsert_row(conn, &row).map_err(storage)?;
    }
    Ok((
        json!({ "id": id, "confirmed": true, "changed": true }),
        true,
    ))
}

/// Removes the schedule `params.id` with the runner's cursor and claims for it.
pub fn remove(conn: &Connection, params: &Value) -> ServiceResult<Value> {
    let id = id_param(params)?;
    let removed = delete_row(conn, &id).map_err(storage)?;
    Ok(json!({ "id": id, "removed": removed }))
}

/// Lands one row that arrived through the owner's own encrypted sync, when it is newer than the one held. A row this host cannot
/// hold (the cap, a spec it refuses) is skipped, not an error. Reach this from the sync engine only: a row carries the owner's
/// confirmation.
pub fn apply_synced_row(conn: &Connection, row: &ScheduleRow) -> Result<(), String> {
    match get_row(conn, &row.id).map_err(|e| e.to_string())? {
        Some(local) if local.updated_ms >= row.updated_ms => return Ok(()),
        Some(_) => {}
        None => {
            let held = list_rows(conn).map_err(|e| e.to_string())?.len();
            if held as i64 >= MAX_SCHEDULES {
                return Ok(());
            }
        }
    }
    // A row the library refuses (an invalid spec) is skipped, never retried forever.
    let _ = upsert_row(conn, row);
    Ok(())
}

/// Asks the runner for the due minutes of every confirmed schedule. `None` when the confirmed list cannot be read (nothing is
/// claimed this tick). A stored schedule whose spec or misfire policy is unreadable is left out.
pub fn claim_confirmed(conn: &mut Connection, now_ms: i64) -> Option<TickOutcome> {
    let rows: Vec<(String, String, String)> = {
        let Ok(mut statement) =
            conn.prepare("SELECT id, spec, misfire FROM schedules WHERE confirmed_ms IS NOT NULL")
        else {
            return None;
        };
        let Ok(mapped) = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        else {
            return None;
        };
        let collected: Vec<(String, String, String)> = mapped.flatten().collect();
        collected
    };
    let schedules: Vec<RunnerSchedule> = rows
        .iter()
        .filter_map(|(id, spec, misfire)| {
            Some(RunnerSchedule {
                id: id.clone(),
                spec: serde_json::from_str(spec).ok()?,
                misfire: misfire_of(misfire)?,
                confirmed: true,
            })
        })
        .collect();
    Some(claim_due(conn, &schedules, now_ms))
}
