//! The bookkeeping around `RegisterHotKey`, behind a seam so it is tested without real input.
//!
//! [`Registrations`] owns everything the Windows hotkey loop decides for itself: which id a
//! chord gets, which modifier flags it is registered with, which chord an id belongs to, and
//! what happens to that record when the OS refuses or has already forgotten a registration.
//! The two OS calls go through [`HotkeyApi`]. On Windows that is `RegisterHotKey` and
//! `UnregisterHotKey`; a test passes a fake that records the calls and returns the error codes
//! Windows would.
//!
//! The module is compiled on every platform, so its tests run wherever the crate is tested.

use crate::chord::{Chord, Modifier};
use crate::hold::HoldKeys;

/// The `fsModifiers` bits of `RegisterHotKey`.
pub mod modifier_bits {
    /// `MOD_ALT`.
    pub const ALT: u32 = 0x0001;
    /// `MOD_CONTROL`.
    pub const CONTROL: u32 = 0x0002;
    /// `MOD_SHIFT`.
    pub const SHIFT: u32 = 0x0004;
    /// `MOD_WIN`.
    pub const WIN: u32 = 0x0008;
    /// `MOD_NOREPEAT`: holding the chord posts one `WM_HOTKEY`, not one per keyboard repeat.
    pub const NOREPEAT: u32 = 0x4000;
}

/// `ERROR_HOTKEY_NOT_REGISTERED`: the OS has no registration under this id.
pub const ERROR_HOTKEY_NOT_REGISTERED: u32 = 1419;

/// The two OS calls a registration makes. An `Err` carries the `GetLastError` code, read
/// immediately after the failing call.
pub trait HotkeyApi {
    /// Register `vk` with `modifiers` (see [`modifier_bits`]) under `id` for the calling thread.
    fn register(&mut self, id: i32, modifiers: u32, vk: u32) -> Result<(), u32>;
    /// Remove the registration made under `id` by the calling thread.
    fn unregister(&mut self, id: i32) -> Result<(), u32>;
}

/// The `fsModifiers` value `chord` is registered with. Without `repeat` the registration is
/// `MOD_NOREPEAT`.
pub fn modifier_flags(chord: &Chord, repeat: bool) -> u32 {
    let mut flags = if repeat { 0 } else { modifier_bits::NOREPEAT };
    for modifier in &chord.modifiers {
        flags |= match modifier {
            Modifier::Ctrl => modifier_bits::CONTROL,
            Modifier::Alt => modifier_bits::ALT,
            Modifier::Shift => modifier_bits::SHIFT,
            _ => modifier_bits::WIN,
        };
    }
    flags
}

/// The virtual keys that must stay down for `chord` to count as held.
pub fn hold_keys(chord: &Chord) -> HoldKeys {
    const VK_SHIFT: u16 = 0x10;
    const VK_CONTROL: u16 = 0x11;
    const VK_MENU: u16 = 0x12;
    const VK_LWIN: u16 = 0x5B;
    const VK_RWIN: u16 = 0x5C;
    let modifier_groups = chord
        .modifiers
        .iter()
        .map(|modifier| match modifier {
            Modifier::Ctrl => vec![VK_CONTROL],
            Modifier::Alt => vec![VK_MENU],
            Modifier::Shift => vec![VK_SHIFT],
            _ => vec![VK_LWIN, VK_RWIN],
        })
        .collect();
    HoldKeys {
        key: chord.key.windows_vk(),
        modifier_groups,
    }
}

/// Why [`Registrations::unregister`] removed nothing it can vouch for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnregisterFailure {
    /// No record for the chord here, or the OS no longer had the registration
    /// ([`ERROR_HOTKEY_NOT_REGISTERED`]). In the second case the stale record is dropped.
    NotRegistered,
    /// The OS refused with this code. The record is kept, because the hotkey is still live.
    Os(u32),
}

#[derive(Debug)]
struct Entry {
    /// The canonical spelling of the chord, which is what a later unregister is matched on.
    chord: String,
    id: i32,
    hold: HoldKeys,
}

/// The chords one hotkey loop has registered, and the ids it gave them.
#[derive(Debug)]
pub struct Registrations {
    next_id: i32,
    entries: Vec<Entry>,
}

impl Default for Registrations {
    fn default() -> Self {
        Self::new()
    }
}

impl Registrations {
    /// No registrations; the first id handed out is 1.
    pub fn new() -> Self {
        Self {
            next_id: 1,
            entries: Vec::new(),
        }
    }

    /// Register `chord` and return its id. On a refusal the OS error code is returned, nothing
    /// is recorded and the id is not used up.
    ///
    /// A chord already registered here is offered to the OS again under a new id, and the OS
    /// answers with a conflict, exactly as it does for a chord another program owns.
    pub fn register(
        &mut self,
        api: &mut dyn HotkeyApi,
        chord: &Chord,
        repeat: bool,
    ) -> Result<i32, u32> {
        let id = self.next_id;
        api.register(
            id,
            modifier_flags(chord, repeat),
            u32::from(chord.key.windows_vk()),
        )?;
        self.next_id += 1;
        self.entries.push(Entry {
            chord: chord.canonical(),
            id,
            hold: hold_keys(chord),
        });
        Ok(id)
    }

    /// The id `chord` is registered under. The match is on the canonical spelling, so modifier
    /// order and aliases do not matter.
    pub fn id_of(&self, chord: &Chord) -> Option<i32> {
        let name = chord.canonical();
        self.entries
            .iter()
            .find(|entry| entry.chord == name)
            .map(|entry| entry.id)
    }

    /// What must stay down for the chord registered under `id` to count as held.
    pub fn hold_keys(&self, id: i32) -> Option<&HoldKeys> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| &entry.hold)
    }

    /// Remove the registration made for `chord` and return the id it had.
    pub fn unregister(
        &mut self,
        api: &mut dyn HotkeyApi,
        chord: &Chord,
    ) -> Result<i32, UnregisterFailure> {
        let name = chord.canonical();
        let position = self
            .entries
            .iter()
            .position(|entry| entry.chord == name)
            .ok_or(UnregisterFailure::NotRegistered)?;
        let id = self.entries[position].id;
        match api.unregister(id) {
            Ok(()) => {
                self.entries.remove(position);
                Ok(id)
            }
            Err(ERROR_HOTKEY_NOT_REGISTERED) => {
                self.entries.remove(position);
                Err(UnregisterFailure::NotRegistered)
            }
            Err(code) => Err(UnregisterFailure::Os(code)),
        }
    }

    /// Unregister everything, ignoring refusals. For shutdown.
    pub fn clear(&mut self, api: &mut dyn HotkeyApi) {
        for entry in self.entries.drain(..) {
            let _ = api.unregister(entry.id);
        }
    }

    /// How many chords are registered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::parse_chord;

    /// `ERROR_HOTKEY_ALREADY_REGISTERED`.
    const CONFLICT: u32 = 1409;
    /// `ERROR_ACCESS_DENIED`.
    const DENIED: u32 = 5;

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Register { id: i32, modifiers: u32, vk: u32 },
        Unregister { id: i32 },
    }

    /// A stand-in for the OS hotkey table. Like Windows, it refuses a chord that is already
    /// registered (whatever the id, and whether or not `MOD_NOREPEAT` differs) and an id it
    /// does not know.
    #[derive(Default)]
    struct FakeOs {
        calls: Vec<Call>,
        /// Live registrations: id, modifiers without `MOD_NOREPEAT`, virtual key.
        live: Vec<(i32, u32, u32)>,
        /// When set, every unregister fails with this code and removes nothing.
        unregister_error: Option<u32>,
    }

    impl FakeOs {
        /// Another program owns `text`.
        fn taken_elsewhere(&mut self, text: &str) {
            let chord = chord(text);
            self.live.push((
                -1,
                modifier_flags(&chord, true),
                u32::from(chord.key.windows_vk()),
            ));
        }

        /// The OS drops the registration behind our back.
        fn forget(&mut self, id: i32) {
            self.live.retain(|(live, _, _)| *live != id);
        }

        fn live_ids(&self) -> Vec<i32> {
            self.live.iter().map(|(id, _, _)| *id).collect()
        }
    }

    impl HotkeyApi for FakeOs {
        fn register(&mut self, id: i32, modifiers: u32, vk: u32) -> Result<(), u32> {
            self.calls.push(Call::Register { id, modifiers, vk });
            let keys = modifiers & !modifier_bits::NOREPEAT;
            if self.live.iter().any(|(_, m, v)| *m == keys && *v == vk) {
                return Err(CONFLICT);
            }
            self.live.push((id, keys, vk));
            Ok(())
        }

        fn unregister(&mut self, id: i32) -> Result<(), u32> {
            self.calls.push(Call::Unregister { id });
            if let Some(code) = self.unregister_error {
                return Err(code);
            }
            let before = self.live.len();
            self.live.retain(|(live, _, _)| *live != id);
            if self.live.len() == before {
                Err(ERROR_HOTKEY_NOT_REGISTERED)
            } else {
                Ok(())
            }
        }
    }

    fn chord(text: &str) -> Chord {
        parse_chord(text).unwrap()
    }

    #[test]
    fn the_flags_carry_each_modifier_and_no_repeat_unless_asked() {
        use modifier_bits::{ALT, CONTROL, NOREPEAT, SHIFT, WIN};
        assert_eq!(modifier_flags(&chord("f9"), false), NOREPEAT);
        assert_eq!(modifier_flags(&chord("f9"), true), 0);
        assert_eq!(
            modifier_flags(&chord("ctrl+alt+shift+win+k"), false),
            CONTROL | ALT | SHIFT | WIN | NOREPEAT
        );
        assert_eq!(
            modifier_flags(&chord("ctrl+shift+k"), true),
            CONTROL | SHIFT
        );
    }

    #[test]
    fn a_registration_passes_the_id_the_flags_and_the_virtual_key() {
        let mut os = FakeOs::default();
        let mut registrations = Registrations::new();
        let id = registrations
            .register(&mut os, &chord("alt+space"), false)
            .unwrap();
        assert_eq!(id, 1);
        assert_eq!(
            os.calls,
            vec![Call::Register {
                id: 1,
                modifiers: modifier_bits::ALT | modifier_bits::NOREPEAT,
                vk: 0x20,
            }]
        );
    }

    #[test]
    fn ids_count_up_and_a_refused_registration_does_not_use_one() {
        let mut os = FakeOs::default();
        os.taken_elsewhere("ctrl+shift+k");
        let mut registrations = Registrations::new();
        assert_eq!(registrations.register(&mut os, &chord("f9"), false), Ok(1));
        assert_eq!(
            registrations.register(&mut os, &chord("ctrl+shift+k"), false),
            Err(CONFLICT)
        );
        assert_eq!(registrations.id_of(&chord("ctrl+shift+k")), None);
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations.register(&mut os, &chord("f10"), false), Ok(2));
    }

    #[test]
    fn a_chord_registered_twice_here_is_a_conflict_and_keeps_the_first_id() {
        let mut os = FakeOs::default();
        let mut registrations = Registrations::new();
        assert_eq!(
            registrations.register(&mut os, &chord("alt+space"), false),
            Ok(1)
        );
        // The repeating form is the same chord to the OS.
        assert_eq!(
            registrations.register(&mut os, &chord("Space + Alt"), true),
            Err(CONFLICT)
        );
        assert_eq!(registrations.id_of(&chord("alt+space")), Some(1));
        assert_eq!(registrations.len(), 1);
    }

    #[test]
    fn a_chord_is_found_whatever_the_modifier_order_or_spelling() {
        let mut os = FakeOs::default();
        let mut registrations = Registrations::new();
        let id = registrations
            .register(&mut os, &chord("ctrl+shift+k"), false)
            .unwrap();
        assert_eq!(registrations.id_of(&chord("Shift + Ctrl + K")), Some(id));
        assert_eq!(
            registrations.unregister(&mut os, &chord("shift+ctrl+k")),
            Ok(id)
        );
        assert!(registrations.is_empty());
        assert!(os.live.is_empty());
    }

    #[test]
    fn the_hold_keys_of_an_id_are_those_of_its_chord() {
        let mut os = FakeOs::default();
        let mut registrations = Registrations::new();
        let id = registrations
            .register(&mut os, &chord("ctrl+win+space"), false)
            .unwrap();
        let keys = registrations.hold_keys(id).unwrap();
        assert_eq!(keys.key, 0x20);
        assert!(keys.modifier_groups.contains(&vec![0x11]));
        assert!(keys.modifier_groups.contains(&vec![0x5B, 0x5C]));
        assert_eq!(registrations.hold_keys(id + 1), None);
    }

    #[test]
    fn unregistering_an_unknown_chord_does_not_reach_the_os() {
        let mut os = FakeOs::default();
        let mut registrations = Registrations::new();
        assert_eq!(
            registrations.unregister(&mut os, &chord("f9")),
            Err(UnregisterFailure::NotRegistered)
        );
        assert!(os.calls.is_empty());
    }

    #[test]
    fn a_registration_the_os_has_forgotten_is_dropped_and_can_be_made_again() {
        let mut os = FakeOs::default();
        let mut registrations = Registrations::new();
        let id = registrations
            .register(&mut os, &chord("f9"), false)
            .unwrap();
        os.forget(id);
        assert_eq!(
            registrations.unregister(&mut os, &chord("f9")),
            Err(UnregisterFailure::NotRegistered)
        );
        assert_eq!(registrations.id_of(&chord("f9")), None);
        // A fresh registration gets a fresh id: ids are never reused.
        assert_eq!(
            registrations.register(&mut os, &chord("f9"), false),
            Ok(id + 1)
        );
    }

    #[test]
    fn a_refused_unregister_keeps_the_record() {
        let mut os = FakeOs::default();
        let mut registrations = Registrations::new();
        let id = registrations
            .register(&mut os, &chord("f9"), false)
            .unwrap();
        os.unregister_error = Some(DENIED);
        assert_eq!(
            registrations.unregister(&mut os, &chord("f9")),
            Err(UnregisterFailure::Os(DENIED))
        );
        assert_eq!(registrations.id_of(&chord("f9")), Some(id));
        os.unregister_error = None;
        assert_eq!(registrations.unregister(&mut os, &chord("f9")), Ok(id));
    }

    #[test]
    fn a_shortcut_is_changed_by_unregistering_the_old_chord_and_registering_the_new() {
        let mut os = FakeOs::default();
        let mut registrations = Registrations::new();
        let old = registrations
            .register(&mut os, &chord("ctrl+shift+k"), false)
            .unwrap();
        assert_eq!(
            registrations.unregister(&mut os, &chord("ctrl+shift+k")),
            Ok(old)
        );
        let new = registrations
            .register(&mut os, &chord("ctrl+shift+j"), false)
            .unwrap();
        assert_ne!(old, new);
        assert_eq!(os.live_ids(), vec![new]);
        // The old chord is free again.
        assert!(registrations
            .register(&mut os, &chord("ctrl+shift+k"), false)
            .is_ok());
    }

    #[test]
    fn clear_unregisters_everything_even_when_the_os_refuses() {
        let mut os = FakeOs::default();
        let mut registrations = Registrations::new();
        let first = registrations
            .register(&mut os, &chord("f9"), false)
            .unwrap();
        let second = registrations
            .register(&mut os, &chord("f10"), false)
            .unwrap();
        os.calls.clear();
        os.unregister_error = Some(DENIED);
        registrations.clear(&mut os);
        assert_eq!(
            os.calls,
            vec![
                Call::Unregister { id: first },
                Call::Unregister { id: second }
            ]
        );
        assert!(registrations.is_empty());
    }
}
