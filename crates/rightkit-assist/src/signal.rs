//! Admission: one signal per event, deduped, moving through fixed states.
//!
//! `new -> sorted -> triaged | no_work | triage_failed`. "Triaged" means the loop settled the signal and
//! has typed facts for it; [`SettledBy`] says whether code alone did that or a model's checked answer did.

use crate::digest;
use crate::sort::Category;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Mail,
    Calendar,
    Schedule,
    Ask,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Mail => "mail",
            Source::Calendar => "calendar",
            Source::Schedule => "schedule",
            Source::Ask => "ask",
        }
    }
}

/// Who the event came from. `Owner` is the owner's own message; everything else is `Outside`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    Owner,
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalState {
    New,
    Sorted,
    Triaged,
    NoWork,
    TriageFailed,
}

impl SignalState {
    /// The only moves there are. A final state never moves again; a replay resets the signal instead.
    pub fn can_move_to(self, next: SignalState) -> bool {
        matches!(
            (self, next),
            (SignalState::New, SignalState::Sorted)
                | (SignalState::Sorted, SignalState::Triaged)
                | (SignalState::Sorted, SignalState::NoWork)
                | (SignalState::Sorted, SignalState::TriageFailed)
        )
    }

    pub fn is_final(self) -> bool {
        matches!(
            self,
            SignalState::Triaged | SignalState::NoWork | SignalState::TriageFailed
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettledBy {
    Code,
    Model,
}

/// What the poller hands over for one event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSignal {
    pub source: Source,
    pub account: String,
    /// The source's own id for the event (a Gmail message id, a calendar event id).
    pub message_id: String,
    /// Unix seconds the poller saw it.
    pub seen_at: i64,
    pub trust: Trust,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    pub id: String,
    pub source: Source,
    pub account: String,
    pub message_id: String,
    pub seen_at: i64,
    pub trust: Trust,
    pub dedupe_key: String,
    pub state: SignalState,
    #[serde(default)]
    pub category: Option<Category>,
    /// The sender is on the owner's lists and its authentication did not fail.
    #[serde(default)]
    pub sender_known: bool,
    #[serde(default)]
    pub settled_by: Option<SettledBy>,
    /// Model answers received so far (the first try and at most one retry).
    #[serde(default)]
    pub triage_attempts: u8,
    /// Why the last answer was rejected: a fixed reason, never the model's or the mail's words.
    #[serde(default)]
    pub last_error: Option<String>,
}

/// The dedupe key: unique on (source, message id). The account is recorded on the signal but is not part of
/// the key, as the plan says.
pub fn dedupe_key(source: Source, message_id: &str) -> String {
    digest::digest(&["signal", source.as_str(), message_id])
}

impl Signal {
    pub fn admit(new: &NewSignal) -> Signal {
        let key = dedupe_key(new.source, &new.message_id);
        let id = format!("sig-{}", key.chars().take(16).collect::<String>());
        Signal {
            id,
            source: new.source,
            account: new.account.clone(),
            message_id: new.message_id.clone(),
            seen_at: new.seen_at,
            trust: new.trust,
            dedupe_key: key,
            state: SignalState::New,
            category: None,
            sender_known: false,
            settled_by: None,
            triage_attempts: 0,
            last_error: None,
        }
    }

    pub fn advance(&mut self, next: SignalState) -> Result<(), String> {
        if !self.state.can_move_to(next) {
            return Err(format!(
                "a signal cannot move from {:?} to {:?}",
                self.state, next
            ));
        }
        self.state = next;
        Ok(())
    }

    /// Back to `new`, for a replay. Identity and admission data stay.
    pub fn reset(&mut self) {
        self.state = SignalState::New;
        self.category = None;
        self.sender_known = false;
        self.settled_by = None;
        self.triage_attempts = 0;
        self.last_error = None;
    }
}
