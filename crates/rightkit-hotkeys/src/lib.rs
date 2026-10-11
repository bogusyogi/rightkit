//! `rightkit-hotkeys`: one hotkey foundation for every Right Suite app.
//!
//! * [`chord`] — parse/format chords and single-key push-to-talk aliases (platform display).
//! * [`conflict`] — conflict reporting for registration failures and an in-process registry.
//! * [`hold`] — press/release pairing for press-only APIs; [`register_status`] — typed registration failures.
//! * [`ptt`] — pure push-to-talk policy and the start/end handoff state machine.
//! * [`decision`] — pure predicates shared by the platform hooks (fn chord gate, middle button).
//! * [`macos`] — `CGEventTap` primitive, fn chord tap, middle-mouse tap, Carbon global hotkeys.
//! * [`windows`] — `WH_KEYBOARD_LL` hook and `RegisterHotKey` registration.
//!
//! Every platform module is `cfg`-gated; the pure modules compile and test everywhere.

pub mod chord;
pub mod conflict;
pub mod decision;
pub mod hold;
pub mod ptt;
pub mod register_status;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

pub use chord::PttAlias;
pub use chord::{
    parse_chord, parse_modifier_token, parse_ptt_key_name, Chord, ChordError, Key, Modifier,
    ParsedModifier, Platform, Side,
};
pub use conflict::{ChordRegistry, HotkeyConflict, RegistryConflict};
pub use hold::{HoldKeys, HoldTracker, HotkeyState, KeyStateSource};
pub use register_status::RegisterFailure;
