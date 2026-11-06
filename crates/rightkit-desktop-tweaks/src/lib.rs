//! Desktop tweaks for Windows: the global keyboard layer (`keys`) with Mac-style editing
//! shortcuts, screenshot hot keys, a hook hub for other features, and key injection. Windows
//! only; the crate is empty elsewhere. The application supplies its services through
//! `services`.

#[cfg(windows)]
pub mod keys;
#[cfg(windows)]
pub mod services;
