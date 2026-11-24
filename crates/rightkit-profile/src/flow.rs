//! The propose/confirm flow's decisions and the sync landing rules, as plain functions over [`ProfileState`].
//! The host keeps its own dispatch, switches, approval wiring and wording; nothing here reads a file or a clock.
//! The open points of the Nucleus contract answered here (review days, which areas a task kind receives) are
//! provisional and live in this file only.

use crate::area::Area;
use crate::entry::{entry_id, Entry, Source, State};
use crate::interview::{QuestionTemplate, Reason};
use crate::profile::{proposal_id, ProfileState, SetOutcome};
use crate::sync_row::entry_json;
use rightkit_time::civil_from_days;
use serde_json::{json, Value};

/// Questions asked at once when the caller names no limit.
pub const DEFAULT_QUESTIONS: usize = 3;
pub const MAX_QUESTIONS: usize = 10;

/// Days from a confirmation to the entry's review date: twelve months for an ordinary area, six for health and money,
/// fourteen days for what the owner is doing now.
pub fn review_days(area: Area) -> i64 {
    match area {
        Area::Now => 14,
        Area::Health | Area::Money => 183,
        _ => 365,
    }
}

/// Whole days since the Unix epoch for a Unix-millisecond time.
pub fn now_days(now: i64) -> i64 {
    now.div_euclid(86_400_000)
}

/// The `YYYY-MM-DD` date `days` after `now` (Unix milliseconds).
pub fn date_after(now: i64, days: i64) -> String {
    let (y, m, d) = civil_from_days(now_days(now) + days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The review date of an entry confirmed or stated at `now`.
pub fn review_date(area: Area, now: i64) -> String {
    date_after(now, review_days(area))
}

/// The areas beyond the identity card that a kind of task receives. An unknown kind gets the card alone.
pub fn task_areas(kind: &str) -> Vec<Area> {
    let mut areas: Vec<Area> = Area::CARD.to_vec();
    let more: &[Area] = match kind {
        "travel" | "booking" => &[Area::HomeTravel, Area::People],
        "mail" | "reply" => &[Area::Work, Area::People, Area::Routines],
        "writing" => &[Area::Work, Area::People, Area::Writing],
        "money" => &[Area::Money],
        "health" => &[Area::Health],
        _ => &[],
    };
    for area in more {
        if !areas.contains(area) {
            areas.push(*area);
        }
    }
    areas
}

/// The source a proposal may name: repository, mail or memory. The owner states values themself, so no other word maps.
pub fn proposal_source(word: &str) -> Option<Source> {
    match word {
        "repository" => Some(Source::Repository),
        "mail" => Some(Source::Mail),
        "memory" => Some(Source::Memory),
        _ => None,
    }
}

/// The outcome of setting or confirming an entry, as the word the host answers with.
pub fn set_outcome_name(outcome: SetOutcome) -> &'static str {
    match outcome {
        SetOutcome::Created => "created",
        SetOutcome::Changed => "changed",
        SetOutcome::Reconfirmed => "reconfirmed",
    }
}

/// How many entries are stale.
pub fn stale_count(state: &ProfileState) -> usize {
    state
        .entries
        .values()
        .filter(|e| e.state == State::Stale)
        .count()
}

/// The limit of a questions call: the caller's number clamped to 1..=`MAX_QUESTIONS`, else `DEFAULT_QUESTIONS`.
pub fn question_limit(requested: Option<u64>) -> usize {
    requested.map_or(DEFAULT_QUESTIONS, |n| (n as usize).clamp(1, MAX_QUESTIONS))
}

/// The due questions as JSON rows: area, key, prompt, reason, the current value and the waiting proposals.
pub fn questions_json(
    state: &ProfileState,
    templates: &[QuestionTemplate],
    limit: usize,
) -> Vec<Value> {
    state
        .questions(templates, limit)
        .into_iter()
        .map(|q| {
            let current = state.entry(q.area, &q.key).map(|e| e.value.clone());
            let proposals: Vec<Value> = state
                .proposals_for(q.area, &q.key)
                .into_iter()
                .map(|p| json!({ "proposalId": proposal_id(&p.id, &p.value), "value": p.value, "source": p.source }))
                .collect();
            json!({
                "area": q.area.slug(),
                "key": q.key,
                "prompt": q.prompt,
                "reason": match q.reason { Reason::Unanswered => "unanswered", Reason::Stale => "stale" },
                "current": current,
                "proposals": proposals,
            })
        })
        .collect()
}

/// Every entry as JSON.
pub fn entries_json(state: &ProfileState) -> Vec<Value> {
    state.entries.values().map(entry_json).collect()
}

/// Every proposal as JSON, each with its `proposalId`.
pub fn proposals_json(state: &ProfileState) -> Vec<Value> {
    state
        .proposals
        .iter()
        .map(|(id, entry)| {
            let mut row = entry_json(entry);
            row["proposalId"] = json!(id);
            row
        })
        .collect()
}

/// Whether a remote row may land as it is: the id names its area and key, a confirmed or stale entry carries the owner's
/// confirmation, a proposal names a source other than the owner and carries none.
pub fn valid_remote(kind: &str, key: &str, entry: &Entry) -> bool {
    let id_ok = entry.id == entry_id(entry.area, &entry.key);
    let shape_ok = match kind {
        "entry" => {
            key == entry.id && entry.state != State::Proposed && entry.confirmation.is_some()
        }
        "proposal" => {
            entry.state == State::Proposed
                && entry.source != Source::Owner
                && entry.confirmation.is_none()
                && key == proposal_id(&entry.id, &entry.value)
        }
        _ => false,
    };
    id_ok && shape_ok && entry.sensitivity >= entry.area.floor()
}

/// Lands rows that came from another of the owner's computers. A remote entry replaces the local one only when it is
/// newer; a confirmed entry must carry its owner confirmation and a proposal must not, or the row is skipped. Never
/// confirms anything that was not confirmed where it came from. Returns whether `state` changed.
pub fn apply_remote(state: &mut ProfileState, rows: &[Vec<u8>]) -> bool {
    let mut changed = false;
    for body in rows {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            continue;
        };
        let (Some(kind), Some(key)) = (value["kind"].as_str(), value["key"].as_str()) else {
            continue;
        };
        let Ok(entry) = serde_json::from_value::<Entry>(value["entry"].clone()) else {
            continue;
        };
        if !valid_remote(kind, key, &entry) {
            continue;
        }
        match kind {
            "entry" => {
                let newer = state
                    .entries
                    .get(key)
                    .is_none_or(|local| entry.updated_at > local.updated_at);
                if newer {
                    // A confirmed entry that landed answers any proposal waiting for the same value.
                    state
                        .proposals
                        .retain(|_, p| !(p.id == entry.id && p.value == entry.value));
                    state.entries.insert(key.to_owned(), entry);
                    changed = true;
                }
            }
            _ => {
                let settled = state
                    .entries
                    .get(&entry.id)
                    .is_some_and(|e| e.value == entry.value);
                if !settled && !state.proposals.contains_key(key) {
                    state.proposals.insert(key.to_owned(), entry);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Removes the entry or proposal whose sync item id is `target`, where `derive(kind, key)` is how the sync layer names a
/// row. Returns whether anything was removed.
pub fn remove_by_target(
    state: &mut ProfileState,
    derive: &dyn Fn(&str, &str) -> String,
    target: &str,
) -> bool {
    let before = state.entries.len() + state.proposals.len();
    state
        .entries
        .retain(|key, _| derive("entry", key) != target);
    state
        .proposals
        .retain(|key, _| derive("proposal", key) != target);
    state.entries.len() + state.proposals.len() != before
}
