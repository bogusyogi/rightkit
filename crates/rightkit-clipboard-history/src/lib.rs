//! Clipboard history: classification of copied text ([`detect()`](fn@detect)), the on-disk store with
//! retention and pinboards ([`model`]), search ([`search`]) and encryption at rest ([`seal`]). On Windows it also holds the clipboard calls (`win`), the capture window (`capture`), the paste engine (`paste`) and the strip (`strip`), picture conversion (`img`) and the nearby shelf (`shelf`).
//! The application supplies its data folder, limits, translation and logging through [`host`].

#[cfg(windows)]
pub mod burst;
#[cfg(windows)]
pub mod capture;
pub mod detect;
pub mod host;
#[cfg(windows)]
pub mod img;
pub mod json;
pub mod model;
#[cfg(windows)]
pub mod paste;
pub mod seal;
pub mod search;
#[cfg(windows)]
pub mod shelf;
#[cfg(windows)]
pub mod strip;
#[cfg(windows)]
pub mod win;

pub use detect::{Detected, detect};
