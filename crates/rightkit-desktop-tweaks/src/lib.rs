//! Desktop tweaks for Windows: Mac-style editing shortcuts (`keys`: the left Alt acts as Ctrl
//! for the editing keys, standing down in terminals and full-screen programs), as one handler on
//! the shared hook hub of `rightkit-hotkeys` (`rightkit_hotkeys::windows::hub`), with tagged key
//! injection (`keys::send_input`, `keys::paste`). Windows only; the crate is empty elsewhere. The
//! application supplies logging through `services`. The Mac tweaks (Finder, window actions) are
//! the Swift side.

#[cfg(windows)]
pub mod keys;
#[cfg(windows)]
pub mod services;
