//! Conflict reporting.
//!
//! * [`HotkeyConflict`]: structured result of a failed OS registration, naming which required
//!   shortcut failed (donor: HeardRight `hotkey_conflict`, generalized: the app supplies the
//!   shortcuts it needs and the message).
//! * [`ChordRegistry`]: in-process registry that refuses a chord already bound to another
//!   action, so two features never fight over one chord before the OS is even asked.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::chord::Chord;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyConflict {
    pub shortcuts: Vec<String>,
    pub message: String,
}

impl HotkeyConflict {
    /// Interpret an error of the form `"register <shortcut> failed: <reason>"`. If the shortcut
    /// is one of `required` it is named alone; otherwise every required shortcut is listed.
    pub fn from_registration_error(error: &str, required: &[&str], message: &str) -> Self {
        let shortcuts = error
            .strip_prefix("register ")
            .and_then(|rest| rest.split_once(" failed:").map(|(shortcut, _)| shortcut))
            .filter(|shortcut| required.contains(shortcut))
            .map(|shortcut| vec![shortcut.to_string()])
            .unwrap_or_else(|| required.iter().map(|s| (*s).to_string()).collect());
        Self {
            shortcuts,
            message: message.to_string(),
        }
    }
}

/// The current registration conflict of a process, for a `static`: stored when published, read back
/// by a late-loading window, and cleared by publishing `None`.
pub struct ConflictState {
    current: Mutex<Option<HotkeyConflict>>,
}

impl ConflictState {
    /// An empty state (no conflict), usable in a `static`.
    pub const fn new() -> Self {
        Self {
            current: Mutex::new(None),
        }
    }

    /// The conflict published last, if any. A poisoned lock is recovered.
    pub fn snapshot(&self) -> Option<HotkeyConflict> {
        self.current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Store `conflict` as the current state, then call `emit` with it (the host sends its
    /// renderer event there and logs a failed send), then, only when it is a conflict, call
    /// `on_conflict` (the host shows its window there). The lock is not held during the callbacks.
    pub fn publish(
        &self,
        conflict: Option<HotkeyConflict>,
        emit: impl FnOnce(&Option<HotkeyConflict>),
        on_conflict: impl FnOnce(),
    ) {
        *self
            .current
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = conflict.clone();
        emit(&conflict);
        if conflict.is_some() {
            on_conflict();
        }
    }
}

impl Default for ConflictState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryConflict {
    pub chord: String,
    pub held_by: String,
    pub requested_by: String,
}

#[derive(Default)]
pub struct ChordRegistry {
    bound: BTreeMap<String, String>,
}

impl ChordRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `chord` to `owner`. Rebinding the same chord to the same owner is idempotent.
    pub fn bind(&mut self, chord: &Chord, owner: &str) -> Result<(), RegistryConflict> {
        let key = chord.canonical();
        match self.bound.get(&key) {
            Some(existing) if existing != owner => Err(RegistryConflict {
                chord: key,
                held_by: existing.clone(),
                requested_by: owner.to_string(),
            }),
            _ => {
                self.bound.insert(key, owner.to_string());
                Ok(())
            }
        }
    }

    pub fn release(&mut self, chord: &Chord) {
        self.bound.remove(&chord.canonical());
    }

    pub fn owner_of(&self, chord: &Chord) -> Option<&str> {
        self.bound.get(&chord.canonical()).map(String::as_str)
    }
}
