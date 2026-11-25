//! The profile as one plain value, and the only ways it changes.
//!
//! [`ProfileState`] holds the entries (confirmed or stale) and the proposals waiting for a yes. A proposal
//! can come from a repository, a mail or a memory and is only ever stored beside the entries; the way into
//! `entries` is [`ProfileState::set_by_owner`] or [`ProfileState::confirm`], and both take an
//! [`OwnerConfirmation`]. A changed value keeps the old one as history.

use crate::area::Area;
use crate::digest::digest;
use crate::entry::{
    check_date, check_key, check_reference, clean_value, entry_id, Entry, OwnerConfirmation,
    Sensitivity, Source, State, Superseded, MAX_HISTORY,
};
use crate::ProfileError;
use rightkit_time::Date;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileState {
    /// Keyed by entry id (`<area slug>/<key>`).
    #[serde(default)]
    pub entries: BTreeMap<String, Entry>,
    /// Keyed by proposal id (see [`proposal_id`]); each value is an entry in state `Proposed`.
    #[serde(default)]
    pub proposals: BTreeMap<String, Entry>,
}

/// What the owner says directly (an interview answer, an edit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInput {
    pub area: Area,
    pub key: String,
    pub value: String,
    pub true_from: Option<String>,
    pub review_by: Option<String>,
    pub sensitivity: Sensitivity,
}

/// What a repository, a mail or a memory suggests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub area: Area,
    pub key: String,
    pub value: String,
    /// Never [`Source::Owner`].
    pub source: Source,
    /// A short reference to the source, never its text.
    pub source_ref: String,
    pub true_from: Option<String>,
    pub sensitivity: Sensitivity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposeOutcome {
    /// Stored. `made_stale` is true when it disagrees with a confirmed entry, which then turns stale and
    /// comes back as a question; nothing about the entry's value changes.
    Stored {
        proposal_id: String,
        made_stale: bool,
    },
    /// The entry already holds this value.
    AlreadyKnown,
    /// The same value is already waiting as a proposal.
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOutcome {
    Created,
    /// The value differed; the old one went to history.
    Changed,
    /// The value was the same; the entry is confirmed again and its dates refreshed.
    Reconfirmed,
}

/// The id of a proposal: a digest of the entry it targets and the value, so the same suggestion twice is one.
pub fn proposal_id(entry_id: &str, value: &str) -> String {
    digest(&["profile.proposal", entry_id, value])
}

struct Incoming {
    area: Area,
    key: String,
    value: String,
    source: Source,
    source_ref: String,
    true_from: Option<String>,
    review_by: Option<String>,
    sensitivity: Sensitivity,
}

impl ProfileState {
    pub fn entry(&self, area: Area, key: &str) -> Option<&Entry> {
        self.entries.get(&entry_id(area, key))
    }

    /// The proposals waiting for one entry.
    pub fn proposals_for(&self, area: Area, key: &str) -> Vec<&Entry> {
        let id = entry_id(area, key);
        self.proposals.values().filter(|p| p.id == id).collect()
    }

    /// Store a suggestion. It never changes an entry's value and never confirms anything.
    pub fn propose(
        &mut self,
        proposal: Proposal,
        now: i64,
    ) -> Result<ProposeOutcome, ProfileError> {
        if proposal.source == Source::Owner {
            return Err(ProfileError::OwnerSourceProposed);
        }
        check_key(&proposal.key)?;
        let value = clean_value(&proposal.value)?;
        check_reference(&proposal.source_ref)?;
        check_date(proposal.true_from.as_deref(), "true_from")?;
        let id = entry_id(proposal.area, &proposal.key);
        if self.entries.get(&id).is_some_and(|e| e.value == value) {
            return Ok(ProposeOutcome::AlreadyKnown);
        }
        let pid = proposal_id(&id, &value);
        if self.proposals.contains_key(&pid) {
            return Ok(ProposeOutcome::Duplicate);
        }
        let made_stale = match self.entries.get_mut(&id) {
            Some(existing) if existing.state == State::Confirmed => {
                existing.state = State::Stale;
                existing.updated_at = now;
                true
            }
            _ => false,
        };
        self.proposals.insert(
            pid.clone(),
            Entry {
                id,
                area: proposal.area,
                key: proposal.key,
                value,
                source: proposal.source,
                source_ref: proposal.source_ref,
                true_from: proposal.true_from,
                state: State::Proposed,
                sensitivity: proposal.sensitivity.max(proposal.area.floor()),
                review_by: None,
                created_at: now,
                updated_at: now,
                history: Vec::new(),
                confirmation: None,
            },
        );
        Ok(ProposeOutcome::Stored {
            proposal_id: pid,
            made_stale,
        })
    }

    /// The owner states a value: an interview answer or an edit. The entry becomes confirmed.
    pub fn set_by_owner(
        &mut self,
        input: EntryInput,
        owner: &OwnerConfirmation,
        now: i64,
    ) -> Result<SetOutcome, ProfileError> {
        owner.check()?;
        check_key(&input.key)?;
        let value = clean_value(&input.value)?;
        check_date(input.true_from.as_deref(), "true_from")?;
        check_date(input.review_by.as_deref(), "review_by")?;
        Ok(self.install(
            Incoming {
                area: input.area,
                key: input.key,
                value,
                source: Source::Owner,
                source_ref: String::new(),
                true_from: input.true_from,
                review_by: input.review_by,
                sensitivity: input.sensitivity,
            },
            owner,
            now,
        ))
    }

    /// The owner says yes to a proposal. It becomes the entry's confirmed value; a different earlier value
    /// goes to history.
    pub fn confirm(
        &mut self,
        proposal_id: &str,
        owner: &OwnerConfirmation,
        review_by: Option<String>,
        now: i64,
    ) -> Result<SetOutcome, ProfileError> {
        owner.check()?;
        check_date(review_by.as_deref(), "review_by")?;
        let proposal = self
            .proposals
            .remove(proposal_id)
            .ok_or(ProfileError::NoSuchProposal)?;
        Ok(self.install(
            Incoming {
                area: proposal.area,
                key: proposal.key,
                value: proposal.value,
                source: proposal.source,
                source_ref: proposal.source_ref,
                true_from: proposal.true_from,
                review_by,
                sensitivity: proposal.sensitivity,
            },
            owner,
            now,
        ))
    }

    /// The owner says no to a proposal; it is dropped. Whether a rejected proposal should be remembered so
    /// it is not suggested again is an open point; today it can be proposed again. An entry that turned
    /// stale because of it stays stale until the owner confirms it again.
    pub fn reject(
        &mut self,
        proposal_id: &str,
        owner: &OwnerConfirmation,
    ) -> Result<(), ProfileError> {
        owner.check()?;
        self.proposals
            .remove(proposal_id)
            .map(|_| ())
            .ok_or(ProfileError::NoSuchProposal)
    }

    /// Turn every confirmed entry whose review date is `today` or earlier into a stale one. Returns their ids.
    pub fn mark_stale(&mut self, today: Date, now: i64) -> Vec<String> {
        let mut marked = Vec::new();
        for entry in self.entries.values_mut() {
            if entry.state == State::Confirmed && entry.is_due(today) {
                entry.state = State::Stale;
                entry.updated_at = now;
                marked.push(entry.id.clone());
            }
        }
        marked
    }

    fn install(&mut self, incoming: Incoming, owner: &OwnerConfirmation, now: i64) -> SetOutcome {
        let id = entry_id(incoming.area, &incoming.key);
        let sensitivity = incoming.sensitivity.max(incoming.area.floor());
        let value = incoming.value.clone();
        let outcome = match self.entries.get_mut(&id) {
            Some(entry) => {
                let changed = entry.value != incoming.value;
                if changed {
                    entry.history.push(Superseded {
                        value: entry.value.clone(),
                        true_from: entry.true_from.clone(),
                        source: entry.source,
                        source_ref: entry.source_ref.clone(),
                        replaced_at: now,
                    });
                    if entry.history.len() > MAX_HISTORY {
                        let extra = entry.history.len() - MAX_HISTORY;
                        entry.history.drain(..extra);
                    }
                    entry.value = incoming.value;
                    entry.source = incoming.source;
                    entry.source_ref = incoming.source_ref;
                    entry.true_from = incoming.true_from;
                } else if incoming.true_from.is_some() {
                    entry.true_from = incoming.true_from;
                }
                entry.review_by = incoming.review_by;
                entry.sensitivity = sensitivity;
                entry.state = State::Confirmed;
                entry.updated_at = now;
                entry.confirmation = Some(owner.clone());
                if changed {
                    SetOutcome::Changed
                } else {
                    SetOutcome::Reconfirmed
                }
            }
            None => {
                self.entries.insert(
                    id.clone(),
                    Entry {
                        id: id.clone(),
                        area: incoming.area,
                        key: incoming.key,
                        value: incoming.value,
                        source: incoming.source,
                        source_ref: incoming.source_ref,
                        true_from: incoming.true_from,
                        state: State::Confirmed,
                        sensitivity,
                        review_by: incoming.review_by,
                        created_at: now,
                        updated_at: now,
                        history: Vec::new(),
                        confirmation: Some(owner.clone()),
                    },
                );
                SetOutcome::Created
            }
        };
        self.proposals
            .retain(|_, p| !(p.id == id && p.value == value));
        outcome
    }
}
