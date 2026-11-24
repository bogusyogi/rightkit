//! Standing instructions and occurrences.
//!
//! An instruction is the owner's rule: a trigger, a condition, an action, a revision. It is inert until the
//! owner approves it, and an approval covers one revision: any edit bumps the revision and voids it. Nothing in
//! this crate creates an approval from a mail, a fact or a model's output; the host builds an
//! [`OwnerApproval`] only from the owner's own typed or spoken words, a setting, or an approval prompt.
//! Built-in instructions ship switched off and unapproved.
//!
//! A firing is an occurrence. Its key is a digest of the instruction, its revision and the trigger, so the same
//! firing twice is one occurrence. A firing while another occurrence of the same instruction is still open is
//! queued once or skipped, never overlapped. The host runs each occurrence as a `rightkit-job-queue` task.

use crate::digest;
use crate::facts::{FactBody, Provenance};
use crate::signal::{Signal, Source};
use crate::sort::Category;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    /// A mail signal, optionally of one category and from one sender pattern (an address or `@domain`).
    Mail {
        #[serde(default)]
        category: Option<Category>,
        #[serde(default)]
        sender: Option<String>,
    },
    /// A schedule from `rightkit-scheduler`, by its id. The trigger key carries the minute.
    Schedule { schedule_id: String },
    /// A check that produced a finding.
    Check { check_id: String },
}

/// The fact kinds a condition can require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    Booking,
    Party,
    Commitment,
    Money,
    Event,
}

impl FactKind {
    pub fn of(body: &FactBody) -> FactKind {
        match body {
            FactBody::Booking(_) => FactKind::Booking,
            FactBody::Party(_) => FactKind::Party,
            FactBody::Commitment(_) => FactKind::Commitment,
            FactBody::Money(_) => FactKind::Money,
            FactBody::Event(_) => FactKind::Event,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    /// Every fact the signal produced must be at least this trusted. `None` means any.
    #[serde(default)]
    pub provenance_at_least: Option<Provenance>,
    /// The signal must have produced a fact of this kind.
    #[serde(default)]
    pub requires_fact: Option<FactKind>,
}

/// What a firing asks for. It names a recipe and typed parameters the owner wrote; it carries no text from
/// a mail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Raise an Inbox item with the owner's own wording.
    Notify { text: String },
    /// Start one bounded run of a named recipe (`rightkit-agent-recipes`) with the owner's parameters.
    RunRecipe {
        recipe: String,
        #[serde(default)]
        params: BTreeMap<String, String>,
    },
}

/// How the owner's approval arrived. The host sets this from its own channels; never from outside text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSource {
    OwnerWords,
    Setting,
    ApprovalPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerApproval {
    pub source: ApprovalSource,
    /// The revision approved.
    pub revision: u32,
    pub approved_at: i64,
    /// A host reference to the proof (a chat turn id, an approval id).
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandingInstruction {
    pub id: String,
    pub revision: u32,
    pub trigger: Trigger,
    #[serde(default)]
    pub condition: Condition,
    pub action: Action,
    /// A built-in instruction ships with the product, switched off.
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub approval: Option<OwnerApproval>,
}

impl StandingInstruction {
    /// A new instruction: revision 1, switched off, unapproved.
    pub fn propose(
        id: &str,
        trigger: Trigger,
        condition: Condition,
        action: Action,
        builtin: bool,
    ) -> Self {
        StandingInstruction {
            id: id.to_string(),
            revision: 1,
            trigger,
            condition,
            action,
            builtin,
            enabled: false,
            approval: None,
        }
    }

    /// Inert until the owner approved this revision and switched it on.
    pub fn is_active(&self) -> bool {
        self.enabled
            && self
                .approval
                .as_ref()
                .map(|a| a.revision == self.revision)
                .unwrap_or(false)
    }

    /// Record the owner's approval of the current revision. An approval for another revision is refused.
    pub fn approve(&mut self, approval: OwnerApproval) -> Result<(), String> {
        if approval.revision != self.revision {
            return Err("the approval is for a different revision".to_string());
        }
        self.approval = Some(approval);
        Ok(())
    }

    /// Change what the instruction does. The revision goes up and the approval is void until the owner
    /// approves again.
    pub fn revise(&mut self, trigger: Trigger, condition: Condition, action: Action) {
        self.revision = self.revision.saturating_add(1);
        self.trigger = trigger;
        self.condition = condition;
        self.action = action;
        self.approval = None;
    }

    /// Whether this instruction's trigger matches a mail signal, given the sender's address.
    pub fn matches_mail(&self, signal: &Signal, sender_address: &str) -> bool {
        if signal.source != Source::Mail {
            return false;
        }
        match &self.trigger {
            Trigger::Mail { category, sender } => {
                if let Some(want) = category {
                    if signal.category != Some(*want) {
                        return false;
                    }
                }
                if let Some(pattern) = sender {
                    let pattern = pattern.trim().to_ascii_lowercase();
                    let address = sender_address.trim().to_ascii_lowercase();
                    let hit = if let Some(domain) = pattern.strip_prefix('@') {
                        address
                            .rsplit_once('@')
                            .map(|(_, d)| d == domain || d.ends_with(&format!(".{domain}")))
                            .unwrap_or(false)
                    } else {
                        address == pattern
                    };
                    if !hit {
                        return false;
                    }
                }
                true
            }
            _ => false,
        }
    }
}

/// The occurrence key: a digest of the instruction, its revision and the trigger.
pub fn occurrence_key(instruction_id: &str, revision: u32, trigger_key: &str) -> String {
    digest::digest(&[
        "occurrence",
        instruction_id,
        &revision.to_string(),
        trigger_key,
    ])
}

/// The trigger key for a mail signal.
pub fn mail_trigger_key(signal: &Signal) -> String {
    format!("mail:{}", signal.dedupe_key)
}

/// The trigger key for a schedule firing: the schedule and the minute, so a minute fires at most once.
pub fn schedule_trigger_key(schedule_id: &str, unix_seconds: i64) -> String {
    format!("schedule:{}@{}", schedule_id, unix_seconds.div_euclid(60))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceState {
    /// Waiting behind a running occurrence of the same instruction. At most one waits.
    Queued,
    /// Ready for the host to run.
    Ready,
    Running,
    Done,
    /// The effect may or may not have happened and nothing proved which.
    Uncertain,
    Skipped,
}

impl OccurrenceState {
    /// Still holding the instruction's lane.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            OccurrenceState::Queued | OccurrenceState::Ready | OccurrenceState::Running
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    pub key: String,
    pub instruction_id: String,
    pub revision: u32,
    pub trigger_key: String,
    pub created_at: i64,
    pub state: OccurrenceState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FireOutcome {
    /// New occurrence, ready to run.
    Ready(Occurrence),
    /// New occurrence, waiting behind the one that runs.
    Queued(Occurrence),
    /// The same firing was already recorded.
    Duplicate(String),
    /// A run is open and one is already waiting: this firing is dropped.
    Skipped(String),
    /// The instruction is not approved, not switched on, or its approval is for an older revision.
    Inert,
}

/// Fire an instruction on a trigger. Pure over the occurrence map; the caller persists it.
pub fn fire(
    instruction: &StandingInstruction,
    trigger_key: &str,
    now: i64,
    occurrences: &mut BTreeMap<String, Occurrence>,
) -> FireOutcome {
    if !instruction.is_active() {
        return FireOutcome::Inert;
    }
    let key = occurrence_key(&instruction.id, instruction.revision, trigger_key);
    if occurrences.contains_key(&key) {
        return FireOutcome::Duplicate(key);
    }
    let open: Vec<&Occurrence> = occurrences
        .values()
        .filter(|o| o.instruction_id == instruction.id && o.state.is_open())
        .collect();
    let running = open
        .iter()
        .any(|o| matches!(o.state, OccurrenceState::Running | OccurrenceState::Ready));
    let waiting = open.iter().any(|o| o.state == OccurrenceState::Queued);
    let state = if running {
        if waiting {
            return FireOutcome::Skipped(key);
        }
        OccurrenceState::Queued
    } else {
        OccurrenceState::Ready
    };
    let occurrence = Occurrence {
        key: key.clone(),
        instruction_id: instruction.id.clone(),
        revision: instruction.revision,
        trigger_key: trigger_key.to_string(),
        created_at: now,
        state,
    };
    occurrences.insert(key, occurrence.clone());
    if state == OccurrenceState::Queued {
        FireOutcome::Queued(occurrence)
    } else {
        FireOutcome::Ready(occurrence)
    }
}

/// When an occurrence finishes, the host marks it, and the queued one (if any) becomes ready.
pub fn finish(
    occurrences: &mut BTreeMap<String, Occurrence>,
    key: &str,
    outcome: OccurrenceState,
) -> Option<String> {
    let instruction_id = {
        let occurrence = occurrences.get_mut(key)?;
        if outcome.is_open() {
            return None;
        }
        occurrence.state = outcome;
        occurrence.instruction_id.clone()
    };
    let still_running = occurrences.values().any(|o| {
        o.instruction_id == instruction_id
            && matches!(o.state, OccurrenceState::Running | OccurrenceState::Ready)
    });
    if still_running {
        return None;
    }
    let next = occurrences
        .values()
        .filter(|o| o.instruction_id == instruction_id && o.state == OccurrenceState::Queued)
        .min_by_key(|o| o.created_at)
        .map(|o| o.key.clone())?;
    if let Some(o) = occurrences.get_mut(&next) {
        o.state = OccurrenceState::Ready;
    }
    Some(next)
}
