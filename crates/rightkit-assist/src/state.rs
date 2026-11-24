//! Storage: everything the loop keeps, as one plain value behind a trait.
//!
//! [`AssistState`] is serde-serialisable and holds signals, facts, the owner's sender lists, standing
//! instructions and occurrences. A host stores it however it likes by implementing [`AssistStore`]: load the
//! state, run the closure, write the state back, all under one lock. [`MemoryAssistStore`] keeps it in memory.

use crate::facts::Fact;
use crate::instruction::{
    self, FireOutcome, Occurrence, OccurrenceState, OwnerApproval, StandingInstruction,
};
use crate::signal::{NewSignal, Signal};
use crate::sort::SenderLists;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistState {
    /// Keyed by signal id.
    #[serde(default)]
    pub signals: BTreeMap<String, Signal>,
    /// Keyed by fact id.
    #[serde(default)]
    pub facts: BTreeMap<String, Fact>,
    #[serde(default)]
    pub sender_lists: SenderLists,
    /// Keyed by instruction id.
    #[serde(default)]
    pub instructions: BTreeMap<String, StandingInstruction>,
    /// Keyed by occurrence key.
    #[serde(default)]
    pub occurrences: BTreeMap<String, Occurrence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    pub signal_id: String,
    /// False when the same (source, account, message id) was already admitted: nothing changed.
    pub created: bool,
}

impl AssistState {
    /// Admit one signal. A second admission of the same (source, account, message id) changes nothing.
    pub fn admit(&mut self, new: &NewSignal) -> Admission {
        let signal = Signal::admit(new);
        let id = signal.id.clone();
        if self
            .signals
            .values()
            .any(|s| s.dedupe_key == signal.dedupe_key)
        {
            let existing = self
                .signals
                .values()
                .find(|s| s.dedupe_key == signal.dedupe_key)
                .map(|s| s.id.clone())
                .unwrap_or(id);
            return Admission {
                signal_id: existing,
                created: false,
            };
        }
        self.signals.insert(id.clone(), signal);
        Admission {
            signal_id: id,
            created: true,
        }
    }

    /// Insert a fact. False when the same fact (same signal, same body) is already there.
    pub fn insert_fact(&mut self, fact: Fact) -> bool {
        if self.facts.contains_key(&fact.id) {
            return false;
        }
        self.facts.insert(fact.id.clone(), fact);
        true
    }

    /// Drop the facts one signal produced, for a replay. Returns how many.
    pub fn remove_facts_of(&mut self, signal_id: &str) -> usize {
        let before = self.facts.len();
        self.facts.retain(|_, f| f.signal_id != signal_id);
        before - self.facts.len()
    }

    pub fn facts_of(&self, signal_id: &str) -> Vec<&Fact> {
        self.facts
            .values()
            .filter(|f| f.signal_id == signal_id)
            .collect()
    }

    pub fn all_facts(&self) -> Vec<Fact> {
        self.facts.values().cloned().collect()
    }

    /// Add or replace an instruction as the owner wrote it (see [`StandingInstruction::propose`]).
    pub fn put_instruction(&mut self, instruction: StandingInstruction) {
        self.instructions
            .insert(instruction.id.clone(), instruction);
    }

    /// Record the owner's approval and switch the instruction on. Owner path only.
    pub fn approve_instruction(&mut self, id: &str, approval: OwnerApproval) -> Result<(), String> {
        let instruction = self
            .instructions
            .get_mut(id)
            .ok_or_else(|| "no such instruction".to_string())?;
        instruction.approve(approval)?;
        instruction.enabled = true;
        Ok(())
    }

    pub fn fire_instruction(&mut self, id: &str, trigger_key: &str, now: i64) -> FireOutcome {
        match self.instructions.get(id) {
            Some(instruction) => {
                instruction::fire(instruction, trigger_key, now, &mut self.occurrences)
            }
            None => FireOutcome::Inert,
        }
    }

    /// The host reports an occurrence finished; the next queued one, if any, becomes ready and is returned.
    pub fn finish_occurrence(&mut self, key: &str, outcome: OccurrenceState) -> Option<String> {
        instruction::finish(&mut self.occurrences, key, outcome)
    }
}

/// Storage behind one closure. The closure sees the whole state under the store's lock.
pub trait AssistStore {
    fn update<R>(&self, f: impl FnOnce(&mut AssistState) -> R) -> Result<R, String>;

    fn snapshot(&self) -> Result<AssistState, String> {
        self.update(|state| state.clone())
    }
}

/// The state kept in memory, for tests of a host and for embedding.
#[derive(Default)]
pub struct MemoryAssistStore {
    state: Mutex<AssistState>,
}

impl AssistStore for MemoryAssistStore {
    fn update<R>(&self, f: impl FnOnce(&mut AssistState) -> R) -> Result<R, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&mut state))
    }
}
