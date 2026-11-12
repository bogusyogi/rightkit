use rightkit_agent_engine_protocol::{ScheduleRunEvent, ScheduleRunState};
use rightkit_workflow::control_plane::ids::{Prefix, StableId};
use rightkit_workflow::control_plane::trigger::{CausalReceipt, CausalReceiptKind};
use rightkit_workflow::control_plane::waits::TaskIsolationV1;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::execution::{sanitize_id, schedule_run_event};
use super::model::{ScheduleEntry, ScheduleLifecycleState, ScheduleRuntimeMetadata, ScheduleSpec};
use super::time::cron_matches_in_time_zone;

pub(super) fn schedules_path(root: &Path) -> PathBuf {
    root.join(crate::host_identity::home_dir())
        .join("schedules.json")
}

pub(super) fn runs_path(root: &Path) -> PathBuf {
    root.join(crate::host_identity::home_dir())
        .join("schedule-runs.json")
}

pub(super) fn load_schedules(root: &Path) -> Result<HashMap<String, ScheduleEntry>, String> {
    let path = schedules_path(root);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read schedules '{}': {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid schedules '{}': {e}", path.display()))
}

pub(super) fn load_runtime_metadata(
    root: &Path,
    entry: &ScheduleEntry,
) -> Result<ScheduleRuntimeMetadata, String> {
    let path = schedules_path(root);
    if path.exists() {
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("invalid schedules '{}': {e}", path.display()))?;
        if let Some(raw) = value.get(&entry.id) {
            let mut metadata = ScheduleRuntimeMetadata {
                profile: raw
                    .get("botProfile")
                    .or_else(|| raw.get("profile"))
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok()),
                trigger: raw
                    .get("trigger")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok()),
                lifecycle: raw
                    .get("lifecycle")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_else(ScheduleLifecycleState::conservative),
                ..Default::default()
            };
            if metadata.trigger.is_none() {
                metadata.trigger = Some(ScheduleSpec::from_legacy(
                    &entry.cron_expr,
                    &entry.time_zone,
                )?);
            }
            return Ok(metadata);
        }
    }
    let schedule = ScheduleSpec::from_legacy(&entry.cron_expr, &entry.time_zone)?;
    Ok(ScheduleRuntimeMetadata {
        trigger: Some(schedule),
        ..ScheduleRuntimeMetadata::default()
    })
}

pub(super) fn load_run_state(root: &Path) -> Result<ScheduleRunState, String> {
    let path = runs_path(root);
    if !path.exists() {
        return Ok(ScheduleRunState::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read schedule runs '{}': {e}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|e| format!("invalid schedule runs '{}': {e}", path.display()))
}

pub(super) fn save_run_state(root: &Path, state: &ScheduleRunState) -> Result<(), String> {
    let path = runs_path(root);
    let text = serde_json::to_string_pretty(state).map_err(|e| format!("serialize runs: {e}"))?;
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |f| std::io::Write::write_all(f, text.as_bytes()),
    )
    .map_err(|e| format!("failed to write '{}': {e}", path.display()))
}

pub(super) fn append_run_event(root: &Path, event: ScheduleRunEvent) -> Result<(), String> {
    let mut state = load_run_state(root)?;
    state.history.push(event);
    if state.history.len() > 500 {
        let keep_from = state.history.len().saturating_sub(500);
        state.history.drain(0..keep_from);
    }
    save_run_state(root, &state)
}

pub(super) fn release_queued_occurrence(root: &Path, schedule_id: &str) -> Result<bool, String> {
    let mut state = load_run_state(root)?;
    let Some(event) = state
        .history
        .iter_mut()
        .find(|event| event.schedule_id == schedule_id && event.status == "queued")
    else {
        return Ok(false);
    };
    event.status = "dequeued".into();
    event.message = "queue-one occurrence released after prior terminal result".into();
    state.last_claimed_minute.remove(schedule_id);
    save_run_state(root, &state)?;
    Ok(true)
}

/// Store causal execution evidence in existing schedule-run history.  The
/// legacy event envelope stays unchanged, while newer readers can replay the
/// validated trigger -> run -> child task -> result chain from its message.
pub(super) fn causal_message(message: &str, chain: &[CausalReceipt]) -> String {
    serde_json::json!({"message": message, "causalReceipts": chain}).to_string()
}

pub(super) fn causal_message_with_isolation(
    message: &str,
    chain: &[CausalReceipt],
    isolation: Option<&TaskIsolationV1>,
) -> String {
    let mut value = serde_json::json!({
        "message": message,
        "causalReceipts": chain,
    });
    if let Some(isolation) = isolation {
        value["taskIsolation"] = serde_json::to_value(isolation).unwrap_or_default();
    }
    value.to_string()
}

/// Deterministic, prefix-bound evidence id (`wf_ev_*`) for one causal hop.
/// The occurrence discriminator keeps ids unique per fired occurrence while
/// the kind keeps them stable across replay.
pub(super) fn causal_receipt_id(kind: &str, occurrence: &str) -> String {
    StableId::new(Prefix::Evidence, format!("{kind}:{occurrence}"))
        .as_str()
        .to_string()
}

/// Hop 0 — the trigger fire. Trigger identity stays the stable schedule id;
/// the receipt id and timestamp are captured at this hop.
pub(super) fn trigger_receipt(
    trigger_id: &str,
    occurrence: &str,
    at_unix_ms: u128,
) -> CausalReceipt {
    CausalReceipt {
        id: causal_receipt_id("trigger", occurrence),
        sequence: 0,
        kind: CausalReceiptKind::Trigger {
            trigger_id: trigger_id.to_string(),
        },
        created_at_unix_ms: at_unix_ms.min(i64::MAX as u128) as i64,
    }
}

/// Next hop — a run admitted by the previous receipt.
pub(super) fn run_receipt(
    parent: &CausalReceipt,
    run_id: &str,
    occurrence: &str,
    at_unix_ms: u128,
) -> CausalReceipt {
    CausalReceipt {
        id: causal_receipt_id("run", occurrence),
        sequence: parent.sequence + 1,
        kind: CausalReceiptKind::Run {
            run_id: run_id.to_string(),
            trigger_receipt_id: parent.id.clone(),
        },
        created_at_unix_ms: at_unix_ms.min(i64::MAX as u128) as i64,
    }
}

/// Next hop — a child task executed by the previous receipt (the agent
/// session itself, or a post-run verifier session).
pub(super) fn child_task_receipt(
    parent: &CausalReceipt,
    task_id: &str,
    occurrence: &str,
    at_unix_ms: u128,
) -> CausalReceipt {
    CausalReceipt {
        id: causal_receipt_id("child", occurrence),
        sequence: parent.sequence + 1,
        kind: CausalReceiptKind::ChildTask {
            task_id: task_id.to_string(),
            run_receipt_id: parent.id.clone(),
        },
        created_at_unix_ms: at_unix_ms.min(i64::MAX as u128) as i64,
    }
}

/// Final hop — the gated terminal outcome (run result or verifier verdict).
pub(super) fn result_receipt(
    parent: &CausalReceipt,
    result_id: &str,
    occurrence: &str,
    at_unix_ms: u128,
) -> CausalReceipt {
    CausalReceipt {
        id: causal_receipt_id("result", occurrence),
        sequence: parent.sequence + 1,
        kind: CausalReceiptKind::Result {
            result_id: result_id.to_string(),
            task_receipt_id: parent.id.clone(),
        },
        created_at_unix_ms: at_unix_ms.min(i64::MAX as u128) as i64,
    }
}

pub(super) fn replay_causal_receipts(
    root: &Path,
    schedule_id: &str,
) -> Result<Vec<Vec<CausalReceipt>>, String> {
    let state = load_run_state(root)?;
    let mut chains = Vec::new();
    for event in state
        .history
        .iter()
        .filter(|e| e.schedule_id == schedule_id)
    {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&event.message) else {
            continue;
        };
        let Some(raw) = value.get("causalReceipts") else {
            continue;
        };
        let chain: Vec<CausalReceipt> = serde_json::from_value(raw.clone())
            .map_err(|e| format!("invalid causal receipt chain: {e}"))?;
        CausalReceipt::validate_chain(&chain)?;
        chains.push(chain);
    }
    Ok(chains)
}

pub fn claim_due_schedules(root: &Path, epoch_minute: i64) -> Result<Vec<ScheduleEntry>, String> {
    let schedules = load_schedules(root)?;
    let mut run_state = load_run_state(root)?;
    let mut due = Vec::new();
    let mut changed = false;
    for entry in schedules.values() {
        let metadata = load_runtime_metadata(root, entry)?;
        if entry.paused
            || metadata.lifecycle.paused
            || metadata.profile.as_ref().is_some_and(|p| !p.enabled)
        {
            continue;
        }
        let trigger = metadata.trigger.clone().unwrap_or(ScheduleSpec::Cron {
            expression: entry.cron_expr.clone(),
            time_zone: entry.time_zone.clone(),
        });
        let now_ms = epoch_minute.saturating_mul(60_000);
        let is_once = matches!(&trigger, ScheduleSpec::Once { .. });
        let retry_limit = metadata.lifecycle.retry_limit;
        let failed_total = run_state
            .history
            .iter()
            .filter(|event| event.schedule_id == entry.id && event.status == "failed")
            .count() as u32;
        let mut due_now = match trigger.clone() {
            ScheduleSpec::Once { at_unix_ms } => at_unix_ms.div_euclid(60_000) <= epoch_minute,
            ScheduleSpec::Cron {
                expression,
                time_zone,
            } => cron_matches_in_time_zone(&expression, epoch_minute, &time_zone)?,
            ScheduleSpec::Interval {
                every_seconds,
                start_unix_ms,
            } => {
                let step = (every_seconds as i64).saturating_mul(1_000);
                let start = start_unix_ms.unwrap_or(now_ms);
                step > 0 && now_ms >= start && (now_ms - start) % step < 60_000
            }
            ScheduleSpec::Monitor { .. } => false,
        };
        // Once triggers are consumed permanently after their first claim;
        // later daemon ticks must not replay them.
        if is_once
            && !(failed_total > 0 && failed_total <= retry_limit)
            && run_state.history.iter().any(|event| {
                event.schedule_id == entry.id
                    && matches!(
                        event.status.as_str(),
                        "claimed" | "submitted" | "completed" | "failed"
                    )
            })
        {
            due_now = false;
        }
        let mut misfire_recovered = false;
        if !due_now && metadata.lifecycle.misfire.trim() != "skip" {
            let watermark = run_state.last_claimed_minute.get(&entry.id).copied();
            let missed = match trigger {
                ScheduleSpec::Interval {
                    every_seconds,
                    start_unix_ms,
                } => {
                    let step = (every_seconds as i64).saturating_mul(1_000);
                    let start = start_unix_ms.unwrap_or(now_ms);
                    if step <= 0 {
                        false
                    } else {
                        let candidate = start + ((now_ms - start) / step).saturating_mul(step);
                        now_ms > start
                            && (now_ms - start) / step > 0
                            && watermark.is_none_or(|seen| seen < candidate.div_euclid(60_000))
                    }
                }
                ScheduleSpec::Cron {
                    expression,
                    time_zone,
                } => (1..=60)
                    .find_map(|minutes| {
                        let candidate = epoch_minute - minutes;
                        cron_matches_in_time_zone(&expression, candidate, &time_zone)
                            .unwrap_or(false)
                            .then_some(candidate)
                    })
                    .is_some_and(|candidate| watermark.is_none_or(|seen| seen < candidate)),
                ScheduleSpec::Once { .. } | ScheduleSpec::Monitor { .. } => false,
            };
            if missed {
                // Both coalesce & catch-up are bounded to one recovered claim
                // per daemon tick; catch-up never floods missed occurrences.
                due_now = true;
                misfire_recovered = true;
            }
        }
        if !due_now {
            continue;
        }
        let last = run_state.last_claimed_minute.get(&entry.id).copied();
        let latest = run_state
            .history
            .iter()
            .filter(|event| event.schedule_id == entry.id)
            .rev()
            .find(|_| true);
        let idempotency_seen = metadata
            .lifecycle
            .idempotency_key
            .as_deref()
            .is_some_and(|key| {
                run_state.history.iter().any(|event| {
                    event.schedule_id == entry.id
                        && event.epoch_minute == epoch_minute
                        && serde_json::from_str::<serde_json::Value>(&event.message)
                            .ok()
                            .and_then(|value| {
                                value
                                    .get("idempotencyKey")
                                    .and_then(serde_json::Value::as_str)
                                    .map(|seen| seen == key)
                            })
                            .unwrap_or(false)
                })
            });
        if idempotency_seen {
            continue;
        }
        let failed_attempts = run_state
            .history
            .iter()
            .filter(|event| {
                event.schedule_id == entry.id
                    && event.epoch_minute == epoch_minute
                    && event.status == "failed"
            })
            .count() as u32;
        let retrying = if is_once {
            failed_total > 0 && failed_total <= retry_limit
        } else {
            failed_attempts > 0 && failed_attempts <= retry_limit
        };
        if last == Some(epoch_minute) && !retrying {
            continue;
        }
        let overlap = metadata.lifecycle.overlap.trim().to_ascii_lowercase();
        // A submitted/claimed run remains active across minute boundaries
        // until its terminal event is persisted.
        let active =
            latest.is_some_and(|event| matches!(event.status.as_str(), "submitted" | "queued"));
        let active_count = run_state
            .history
            .iter()
            .filter(|event| event.schedule_id == entry.id)
            .fold(HashMap::<String, String>::new(), |mut active, event| {
                active.insert(event.session_id.clone(), event.status.clone());
                active
            })
            .values()
            .filter(|status| matches!(status.as_str(), "submitted" | "queued"))
            .count() as u32;
        if active && overlap == "skip" {
            run_state
                .last_claimed_minute
                .insert(entry.id.clone(), epoch_minute);
            run_state.history.push(schedule_run_event(
                entry,
                epoch_minute,
                "skipped",
                &format!("schedule-{}", sanitize_id(&entry.id)),
                "overlap policy skipped occurrence",
            ));
            changed = true;
            continue;
        }
        if overlap == "bounded-parallel"
            && active_count
                >= metadata
                    .profile
                    .as_ref()
                    .map(|profile| profile.concurrency_cap)
                    .unwrap_or(1)
        {
            run_state
                .last_claimed_minute
                .insert(entry.id.clone(), epoch_minute);
            run_state.history.push(schedule_run_event(
                entry,
                epoch_minute,
                "skipped",
                &format!("schedule-{}", sanitize_id(&entry.id)),
                "bounded-parallel cap reached",
            ));
            changed = true;
            continue;
        }
        run_state
            .last_claimed_minute
            .insert(entry.id.clone(), epoch_minute);
        let status = if active && overlap == "queue-one" {
            "queued"
        } else {
            "claimed"
        };
        run_state.history.push(schedule_run_event(
            entry,
            epoch_minute,
            status,
            &format!("schedule-{}", sanitize_id(&entry.id)),
            &serde_json::json!({
                "message": if retrying { "claimed bounded retry" } else if misfire_recovered { "claimed bounded misfire recovery" } else { "claimed for execution" },
                "idempotencyKey": metadata.lifecycle.idempotency_key,
            }).to_string(),
        ));
        changed = true;
        if status == "claimed" {
            due.push(entry.clone());
        }
    }
    if changed {
        save_run_state(root, &run_state)?;
    }
    due.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(due)
}
