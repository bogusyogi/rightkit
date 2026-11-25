//! Clipboard history: classification of copied text ([`detect()`](fn@detect)), the on-disk store with
//! retention and pinboards (`model`), search (`search`) and encryption at rest (`seal`). On Windows it also holds the clipboard calls (`win`), the capture window (`capture`), the paste engine (`paste`), OLE drag and drop (`drag`), picture conversion (`img`) and the nearby shelf (`shelf`).
//! The default `history` feature exposes the store and host integration. Disable default features
//! for portable text detection without history or platform dependencies.

#[cfg(all(feature = "history", windows))]
pub mod burst;
#[cfg(all(feature = "history", windows))]
pub mod capture;
pub mod detect;
#[cfg(feature = "history")]
pub mod host;
#[cfg(all(feature = "history", windows))]
pub mod img;
/// Bounded JSON reading: the launcher core's one copy (`rightkit_launcher::json`), re-exported
/// under its old path.
#[cfg(feature = "history")]
pub use rightkit_launcher::json;
#[cfg(all(feature = "history", windows))]
pub mod drag;
#[cfg(feature = "history")]
pub mod model;
#[cfg(all(feature = "history", windows))]
pub mod paste;
#[cfg(feature = "history")]
pub mod seal;
#[cfg(feature = "history")]
pub mod search;
#[cfg(all(feature = "history", windows))]
pub mod shelf;
#[cfg(all(feature = "history", windows))]
pub mod win;

pub use detect::{Detected, detect};
