//! The neutral form: one JSON line per entry or proposal, so a profile survives any change of layout
//! (the contract amendment that exempts profile entries from "a store with another layout is replaced").
//!
//! Export writes every entry with its history, then every proposal. Import is additive: it never wipes and
//! never overwrites. A line whose entry is already present and identical changes nothing; a line whose id
//! exists with a different content is rejected; anything malformed is rejected with a reason and the other
//! lines still land. Import cannot confirm: an entry line must carry the owner's confirmation record, and a
//! proposal line must name a source other than the owner and carry none.

use crate::entry::{
    check_date, check_key, check_reference, clean_value, entry_id, Entry, Source, State,
    MAX_HISTORY,
};
use crate::profile::{proposal_id, ProfileState};
use crate::ProfileError;
use serde::{Deserialize, Serialize};

/// The `format` of every line.
pub const NEUTRAL_FORMAT: &str = "rightkit.profile.neutral.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Entry,
    Proposal,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    format: String,
    kind: Kind,
    entry: Entry,
}

/// A line that was not imported, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// 1-based line number in the input.
    pub line: usize,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub inserted: usize,
    pub already_present: usize,
    pub rejected: Vec<Rejection>,
}

/// Every entry and proposal of `state`, one JSON line each.
pub fn export(state: &ProfileState) -> Result<String, ProfileError> {
    let entries = state.entries.values().map(|e| (Kind::Entry, e));
    let proposals = state.proposals.values().map(|e| (Kind::Proposal, e));
    let mut out = String::new();
    for (kind, entry) in entries.chain(proposals) {
        let line = Line {
            format: NEUTRAL_FORMAT.to_owned(),
            kind,
            entry: entry.clone(),
        };
        let text = serde_json::to_string(&line).map_err(|_| ProfileError::Encode)?;
        out.push_str(&text);
        out.push('\n');
    }
    Ok(out)
}

/// Add the lines of `text` to `state`. See the module notes for what is accepted.
pub fn import(state: &mut ProfileState, text: &str) -> ImportReport {
    let mut report = ImportReport::default();
    for (index, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        let line = index + 1;
        let parsed: Line = match serde_json::from_str(raw) {
            Ok(parsed) => parsed,
            Err(_) => {
                report.rejected.push(Rejection {
                    line,
                    reason: "not a profile record",
                });
                continue;
            }
        };
        if parsed.format != NEUTRAL_FORMAT {
            report.rejected.push(Rejection {
                line,
                reason: "unknown format",
            });
            continue;
        }
        if let Err(reason) = check_line(parsed.kind, &parsed.entry) {
            report.rejected.push(Rejection { line, reason });
            continue;
        }
        let (map, key) = match parsed.kind {
            Kind::Entry => (&mut state.entries, parsed.entry.id.clone()),
            Kind::Proposal => (
                &mut state.proposals,
                proposal_id(&parsed.entry.id, &parsed.entry.value),
            ),
        };
        match map.get(&key) {
            Some(existing) if *existing == parsed.entry => report.already_present += 1,
            Some(_) => report.rejected.push(Rejection {
                line,
                reason: "an item with this id holds different content",
            }),
            None => {
                map.insert(key, parsed.entry);
                report.inserted += 1;
            }
        }
    }
    report
}

fn check_line(kind: Kind, entry: &Entry) -> Result<(), &'static str> {
    if entry.id != entry_id(entry.area, &entry.key) {
        return Err("id does not match area and key");
    }
    if check_key(&entry.key).is_err() {
        return Err("bad key");
    }
    if clean_value(&entry.value).ok().as_deref() != Some(entry.value.as_str()) {
        return Err("bad value");
    }
    if check_reference(&entry.source_ref).is_err() {
        return Err("bad source reference");
    }
    if check_date(entry.true_from.as_deref(), "true_from").is_err()
        || check_date(entry.review_by.as_deref(), "review_by").is_err()
    {
        return Err("bad date");
    }
    if entry.sensitivity < entry.area.floor() {
        return Err("sensitivity below the area's floor");
    }
    if entry.history.len() > MAX_HISTORY {
        return Err("too much history");
    }
    match kind {
        Kind::Entry => {
            if entry.state == State::Proposed {
                return Err("an entry line cannot be proposed");
            }
            match &entry.confirmation {
                Some(confirmation) if confirmation.check().is_ok() => Ok(()),
                _ => Err("an entry needs the owner's confirmation"),
            }
        }
        Kind::Proposal => {
            if entry.state != State::Proposed
                || entry.source == Source::Owner
                || entry.confirmation.is_some()
                || !entry.history.is_empty()
            {
                return Err("not a valid proposal");
            }
            Ok(())
        }
    }
}
