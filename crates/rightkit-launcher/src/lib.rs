//! The portable command launcher core: calculator and unit conversion ([`calc`]), user-made
//! items and snippets ([`config`]), word lookup ([`dictionary`]), currency and coin rates
//! ([`rates`]), notes ([`notes`]), custom shell commands ([`commands`]), the file index
//! ([`files`]) and bounded JSON reading ([`json`]). The application supplies translation, HTTPS
//! and logging through [`host`]. The Windows panel that draws these is `rightkit-launcher-win`.

pub mod calc;
pub mod commands;
pub mod config;
pub mod dictionary;
pub mod files;
pub mod host;
pub mod json;
pub mod notes;
pub mod rates;
