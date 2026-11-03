//! Command launcher logic: calculator and unit conversion ([`calc`]), user-made items and
//! snippets ([`config`]), word lookup ([`dictionary`]), currency and coin rates ([`rates`]),
//! notes ([`notes`]), custom shell commands ([`commands`]), the file index ([`files`]) and, on
//! Windows, Store apps. The application supplies translation, HTTPS and logging through [`host`].

pub mod calc;
#[cfg(windows)]
pub mod chat;
pub mod commands;
pub mod config;
pub mod dictionary;
pub mod files;
pub mod host;
pub mod json;
pub mod notes;
#[cfg(windows)]
pub mod panel;
pub mod rates;
pub mod response_layout;
#[cfg(windows)]
pub mod snippets;
#[cfg(windows)]
pub mod spot_takeover;
#[cfg(windows)]
pub mod store;
