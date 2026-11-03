//! Platform backends. Each exposes the same functions: `capabilities`, `permission`,
//! `request_permission`, `activation_registered`, `post`, `close`, `close_all`,
//! `delivered`, `set_badge`.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub(crate) use mac::*;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub(crate) use win::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::*;

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod unsupported;
#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub(crate) use unsupported::*;
