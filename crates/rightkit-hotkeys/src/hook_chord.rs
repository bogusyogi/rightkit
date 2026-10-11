//! Chord matching over raw key events: the fallback for chords the system hotkey API refuses.
//!
//! `RegisterHotKey` fails with a conflict when Windows or another program already owns a chord
//! (Alt+Space is the usual case). An app that still wants the chord can opt into watching every
//! key through a low-level keyboard hook and matching the chord itself. This module is that
//! matcher. It is pure: it is fed `(virtual key, down, injected)` events and answers with the
//! chord events to deliver and whether to swallow the key, so tests drive it without real
//! input. The Windows glue is `windows::HookHotkeys`.
//!
//! Rules, chosen to match what `RegisterHotKey` does for a registered chord:
//! * A chord fires when its key goes down while **exactly** its modifiers are down (Ctrl+K does
//!   not fire while Ctrl+Shift is held). Left and right variants of a modifier are the same.
//! * The key-down and key-up of a matched key are swallowed, so the focused app never sees
//!   them. Modifier keys are never swallowed.
//! * A held chord reports `Pressed` once (auto-repeat is swallowed silently) and `Released`
//!   once: when its key goes up, or when one of its modifiers goes up first.
//! * Injected events (`SendInput`) pass through and change nothing.

use crate::chord::{Chord, Modifier};
use crate::hold::HotkeyState;

const CTRL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;
const WIN: u8 = 8;

/// The modifier group a Windows virtual key belongs to, with a bit unique to the physical key
/// so that releasing one of two held Shift keys does not count as releasing Shift.
fn modifier_of(vk: u32) -> Option<(u8, u8)> {
    Some(match vk {
        0x11 => (CTRL, 0x01),  // VK_CONTROL
        0xA2 => (CTRL, 0x02),  // VK_LCONTROL
        0xA3 => (CTRL, 0x04),  // VK_RCONTROL
        0x12 => (ALT, 0x01),   // VK_MENU
        0xA4 => (ALT, 0x02),   // VK_LMENU
        0xA5 => (ALT, 0x04),   // VK_RMENU
        0x10 => (SHIFT, 0x01), // VK_SHIFT
        0xA0 => (SHIFT, 0x02), // VK_LSHIFT
        0xA1 => (SHIFT, 0x04), // VK_RSHIFT
        0x5B => (WIN, 0x02),   // VK_LWIN
        0x5C => (WIN, 0x04),   // VK_RWIN
        _ => return None,
    })
}

fn mask_of(chord: &Chord) -> u8 {
    chord.modifiers.iter().fold(0, |mask, m| {
        mask | match m {
            Modifier::Ctrl => CTRL,
            Modifier::Alt => ALT,
            Modifier::Shift => SHIFT,
            _ => WIN,
        }
    })
}

#[derive(Debug)]
struct Bound {
    id: i32,
    name: String,
    vk: u32,
    mask: u8,
    held: bool,
}

/// What the hook does with one key event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookOutcome {
    /// Chord events to deliver, in order. Usually empty or one; a modifier going up can end
    /// more than one held chord.
    pub events: Vec<(i32, HotkeyState)>,
    /// Keep the key event from the rest of the system.
    pub swallow: bool,
}

/// A chord is already bound in this matcher under the returned id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlreadyBound(pub i32);

/// The set of chords matched through the hook, and the modifier state seen so far.
#[derive(Debug, Default)]
pub struct HookChords {
    bound: Vec<Bound>,
    next_id: i32,
    /// Per group (index 0 Ctrl, 1 Alt, 2 Shift, 3 Win): bits of the physical keys down.
    down: [u8; 4],
}

fn group_index(group: u8) -> usize {
    match group {
        CTRL => 0,
        ALT => 1,
        SHIFT => 2,
        _ => 3,
    }
}

impl HookChords {
    pub fn new() -> Self {
        Self {
            bound: Vec::new(),
            next_id: 1,
            down: [0; 4],
        }
    }

    /// True when no chord is bound (the hook has nothing to match).
    pub fn is_empty(&self) -> bool {
        self.bound.is_empty()
    }

    /// Bind `chord` and return its id. Ids are never reused, so an id from before a re-bind
    /// cannot be mistaken for the new binding.
    pub fn bind(&mut self, chord: &Chord) -> Result<i32, AlreadyBound> {
        let name = chord.canonical();
        if let Some(existing) = self.bound.iter().find(|b| b.name == name) {
            return Err(AlreadyBound(existing.id));
        }
        if self.next_id < 1 {
            self.next_id = 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.bound.push(Bound {
            id,
            name,
            vk: u32::from(chord.key.windows_vk()),
            mask: mask_of(chord),
            held: false,
        });
        Ok(id)
    }

    /// Remove the binding for `chord`. Returns its id and whether it was held at that moment
    /// (the caller then delivers `Released` for it), or `None` when it was not bound.
    pub fn unbind(&mut self, chord: &Chord) -> Option<(i32, bool)> {
        let name = chord.canonical();
        let position = self.bound.iter().position(|b| b.name == name)?;
        let bound = self.bound.remove(position);
        Some((bound.id, bound.held))
    }

    /// End every hold (the hook is going away). Returns the ids to deliver `Released` for.
    pub fn release_all(&mut self) -> Vec<i32> {
        self.down = [0; 4];
        self.bound
            .iter_mut()
            .filter(|b| b.held)
            .map(|b| {
                b.held = false;
                b.id
            })
            .collect()
    }

    fn mask_down(&self) -> u8 {
        [CTRL, ALT, SHIFT, WIN]
            .into_iter()
            .filter(|group| self.down[group_index(*group)] != 0)
            .fold(0, |mask, group| mask | group)
    }

    /// Feed one key event.
    pub fn on_key(&mut self, vk: u32, down: bool, injected: bool) -> HookOutcome {
        let mut outcome = HookOutcome::default();
        if injected {
            return outcome;
        }
        if let Some((group, key_bit)) = modifier_of(vk) {
            let slot = &mut self.down[group_index(group)];
            if down {
                *slot |= key_bit;
            } else {
                *slot &= !key_bit;
                if *slot == 0 {
                    // The whole modifier is up: chords that need it are no longer held.
                    for bound in self
                        .bound
                        .iter_mut()
                        .filter(|b| b.held && b.mask & group != 0)
                    {
                        bound.held = false;
                        outcome.events.push((bound.id, HotkeyState::Released));
                    }
                }
            }
            return outcome;
        }
        if down {
            let mask = self.mask_down();
            if let Some(bound) = self
                .bound
                .iter_mut()
                .find(|b| b.vk == vk && (b.held || b.mask == mask))
            {
                if !bound.held {
                    bound.held = true;
                    outcome.events.push((bound.id, HotkeyState::Pressed));
                }
                outcome.swallow = true;
            }
        } else if let Some(bound) = self.bound.iter_mut().find(|b| b.vk == vk && b.held) {
            bound.held = false;
            outcome.events.push((bound.id, HotkeyState::Released));
            outcome.swallow = true;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::parse_chord;

    const VK_LCONTROL: u32 = 0xA2;
    const VK_LMENU: u32 = 0xA4;
    const VK_LSHIFT: u32 = 0xA0;
    const VK_RSHIFT: u32 = 0xA1;
    const VK_SPACE: u32 = 0x20;
    const VK_K: u32 = 0x4B;

    fn chords(texts: &[&str]) -> (HookChords, Vec<i32>) {
        let mut hook = HookChords::new();
        let ids = texts
            .iter()
            .map(|text| hook.bind(&parse_chord(text).unwrap()).unwrap())
            .collect();
        (hook, ids)
    }

    fn pass() -> HookOutcome {
        HookOutcome::default()
    }

    fn fired(id: i32, state: HotkeyState) -> HookOutcome {
        HookOutcome {
            events: vec![(id, state)],
            swallow: true,
        }
    }

    #[test]
    fn a_chord_fires_once_per_hold_and_its_key_is_swallowed() {
        let (mut hook, ids) = chords(&["alt+space"]);
        assert_eq!(hook.on_key(VK_LMENU, true, false), pass());
        assert_eq!(
            hook.on_key(VK_SPACE, true, false),
            fired(ids[0], HotkeyState::Pressed)
        );
        // Auto-repeat: swallowed, no second Pressed.
        assert_eq!(
            hook.on_key(VK_SPACE, true, false),
            HookOutcome {
                events: vec![],
                swallow: true
            }
        );
        assert_eq!(
            hook.on_key(VK_SPACE, false, false),
            fired(ids[0], HotkeyState::Released)
        );
        assert_eq!(hook.on_key(VK_LMENU, false, false), pass());
    }

    #[test]
    fn the_modifiers_must_match_exactly() {
        let (mut hook, _) = chords(&["ctrl+k"]);
        hook.on_key(VK_LCONTROL, true, false);
        hook.on_key(VK_LSHIFT, true, false);
        assert_eq!(hook.on_key(VK_K, true, false), pass());
        assert_eq!(hook.on_key(VK_K, false, false), pass());
        // No modifier at all does not fire either.
        hook.on_key(VK_LSHIFT, false, false);
        hook.on_key(VK_LCONTROL, false, false);
        assert_eq!(hook.on_key(VK_K, true, false), pass());
    }

    #[test]
    fn releasing_a_modifier_first_ends_the_hold_and_the_key_up_is_then_passed() {
        let (mut hook, ids) = chords(&["alt+space"]);
        hook.on_key(VK_LMENU, true, false);
        hook.on_key(VK_SPACE, true, false);
        assert_eq!(
            hook.on_key(VK_LMENU, false, false),
            HookOutcome {
                events: vec![(ids[0], HotkeyState::Released)],
                swallow: false
            }
        );
        // Exactly one Released: the later key-up belongs to nobody.
        assert_eq!(hook.on_key(VK_SPACE, false, false), pass());
    }

    #[test]
    fn one_of_two_held_shift_keys_going_up_keeps_the_modifier_down() {
        let (mut hook, ids) = chords(&["shift+k"]);
        hook.on_key(VK_LSHIFT, true, false);
        hook.on_key(VK_RSHIFT, true, false);
        hook.on_key(VK_K, true, false);
        assert_eq!(hook.on_key(VK_LSHIFT, false, false), pass());
        assert_eq!(
            hook.on_key(VK_RSHIFT, false, false),
            HookOutcome {
                events: vec![(ids[0], HotkeyState::Released)],
                swallow: false
            }
        );
    }

    #[test]
    fn injected_events_pass_and_change_nothing() {
        let (mut hook, ids) = chords(&["alt+space"]);
        assert_eq!(hook.on_key(VK_LMENU, true, true), pass());
        assert_eq!(hook.on_key(VK_SPACE, true, true), pass());
        // The injected Alt was not recorded, so a real Space alone does not fire.
        assert_eq!(hook.on_key(VK_SPACE, true, false), pass());
        hook.on_key(VK_SPACE, false, false);
        hook.on_key(VK_LMENU, true, false);
        assert_eq!(
            hook.on_key(VK_SPACE, true, false),
            fired(ids[0], HotkeyState::Pressed)
        );
    }

    #[test]
    fn binding_twice_is_refused_and_a_rebind_gets_a_new_id() {
        let mut hook = HookChords::new();
        let chord = parse_chord("alt+space").unwrap();
        let first = hook.bind(&chord).unwrap();
        // Modifier order and aliases do not matter: the canonical spelling is the identity.
        assert_eq!(
            hook.bind(&parse_chord("Alt + Space").unwrap()),
            Err(AlreadyBound(first))
        );
        assert_eq!(hook.unbind(&chord), Some((first, false)));
        assert_eq!(hook.unbind(&chord), None);
        assert!(hook.is_empty());
        let second = hook.bind(&chord).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn unbinding_or_dropping_a_held_chord_reports_the_hold() {
        let (mut hook, ids) = chords(&["alt+space", "ctrl+k"]);
        hook.on_key(VK_LMENU, true, false);
        hook.on_key(VK_SPACE, true, false);
        assert_eq!(
            hook.unbind(&parse_chord("alt+space").unwrap()),
            Some((ids[0], true))
        );
        hook.on_key(VK_LMENU, false, false);
        hook.on_key(VK_LCONTROL, true, false);
        hook.on_key(VK_K, true, false);
        assert_eq!(hook.release_all(), vec![ids[1]]);
        assert_eq!(hook.release_all(), Vec::<i32>::new());
    }
}
