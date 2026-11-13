//! Installed-app management: inventory with sizes, icons and usage, updates (Sparkle, Homebrew,
//! winget and the Microsoft Store), safe uninstall with leftovers moved to the Trash, and
//! quitting or force-quitting apps by bundle with system and self protection.
//!
//! The host app names itself with [`set_host_identity`] (it is then never listed for removal or
//! quit), its state folder with [`set_state_directory`] and its helper approval with
//! [`set_privileged_helper_enabled`] (or [`set_privileged_helper_probe`] when the state can change
//! while the process runs).

#[cfg(unix)]
pub mod app_manager;
pub mod apps;
#[cfg(windows)]
pub mod apps_windows;
mod host;
#[cfg(unix)]
pub mod process_control;
pub mod subprocess;

pub use host::{
    HostIdentity, set_host_identity, set_privileged_helper_enabled, set_privileged_helper_probe,
    set_state_directory,
};
