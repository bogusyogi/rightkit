//! Conflict reporting.
//!
//! * [`HotkeyConflict`]: structured result of a failed OS registration, naming which required
//!   shortcut failed (donor: HeardRight `hotkey_conflict`, generalized: the app supplies the
//!   shortcuts it needs and the message).
//! * [`ChordRegistry`]: in-process registry that refuses a chord already bound to another
//!   action, so two features never fight over one chord before the OS is even asked.

use std::collections::BTreeMap;

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
        Self { shortcuts, message: message.to_string() }
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
