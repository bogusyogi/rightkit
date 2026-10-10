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
        Self {
            shortcuts,
            message: message.to_string(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::parse_chord;

    fn chord(text: &str) -> Chord {
        parse_chord(text).unwrap()
    }

    #[test]
    fn bind_succeeds_for_distinct_chords() {
        let mut reg = ChordRegistry::new();
        assert!(reg.bind(&chord("Ctrl+Shift+K"), "palette").is_ok());
        assert!(reg.bind(&chord("Ctrl+Shift+J"), "search").is_ok());
        assert_eq!(reg.owner_of(&chord("Ctrl+Shift+K")), Some("palette"));
    }

    #[test]
    fn bind_reports_conflict_with_holder_and_requester() {
        let mut reg = ChordRegistry::new();
        reg.bind(&chord("Ctrl+Space"), "dictation").unwrap();
        let err = reg.bind(&chord("ctrl+space"), "launcher").unwrap_err();
        assert_eq!(
            err,
            RegistryConflict {
                chord: "Ctrl+Space".into(),
                held_by: "dictation".into(),
                requested_by: "launcher".into(),
            }
        );
        assert_eq!(reg.owner_of(&chord("Ctrl+Space")), Some("dictation"));
    }

    #[test]
    fn conflict_detection_ignores_modifier_order_and_aliases() {
        let mut reg = ChordRegistry::new();
        reg.bind(&chord("Shift+Cmd+K"), "a").unwrap();
        assert!(reg.bind(&chord("Meta+Shift+K"), "b").is_err());
    }

    #[test]
    fn rebinding_same_owner_is_idempotent() {
        let mut reg = ChordRegistry::new();
        reg.bind(&chord("Alt+F4"), "close").unwrap();
        assert!(reg.bind(&chord("Alt+F4"), "close").is_ok());
    }

    #[test]
    fn release_frees_the_chord_for_another_owner() {
        let mut reg = ChordRegistry::new();
        reg.bind(&chord("Ctrl+1"), "one").unwrap();
        reg.release(&chord("Ctrl+1"));
        assert_eq!(reg.owner_of(&chord("Ctrl+1")), None);
        assert!(reg.bind(&chord("Ctrl+1"), "two").is_ok());
    }

    #[test]
    fn registration_error_names_the_failed_required_shortcut() {
        let c = HotkeyConflict::from_registration_error(
            "register Ctrl+Space failed: already taken",
            &["Ctrl+Space", "Alt+Space"],
            "in use",
        );
        assert_eq!(c.shortcuts, vec!["Ctrl+Space".to_string()]);
        assert_eq!(c.message, "in use");
    }

    #[test]
    fn unrecognized_registration_error_lists_all_required() {
        let c = HotkeyConflict::from_registration_error(
            "register F13 failed: nope",
            &["Ctrl+Space", "Alt+Space"],
            "in use",
        );
        assert_eq!(
            c.shortcuts,
            vec!["Ctrl+Space".to_string(), "Alt+Space".to_string()]
        );
        let c = HotkeyConflict::from_registration_error("garbage", &["A"], "m");
        assert_eq!(c.shortcuts, vec!["A".to_string()]);
    }
}
