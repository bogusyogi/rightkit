//! Single instance without Tauri (rightkit#956): arbitrate which process is primary, forward a second
//! launch's argv and working directory to it, and keep answering the legacy `tauri-plugin-single-instance`
//! socket or window so an old and a new build never both run.

pub mod instance;
mod legacy_hold;
mod legacy_instance;
pub mod single_instance;
