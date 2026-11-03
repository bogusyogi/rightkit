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
