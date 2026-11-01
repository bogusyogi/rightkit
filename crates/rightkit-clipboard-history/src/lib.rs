//! Clipboard history: classification of copied text ([`detect`]), the on-disk store with
//! retention and pinboards ([`model`]), search ([`search`]) and encryption at rest ([`seal`]). On Windows it also holds the clipboard calls ([`win`]), picture conversion ([`img`]) and the nearby shelf ([`shelf`]).
//! The application supplies its data folder, limits, translation and logging through [`host`].

pub mod detect;
pub mod host;
#[cfg(windows)]
pub mod img;
pub mod json;
pub mod model;
pub mod seal;
pub mod search;

pub use detect::{Detected, detect};
