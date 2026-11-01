//! Clipboard history: classification of copied text ([`detect`]), the on-disk store with
//! retention and pinboards ([`model`]), search ([`search`]) and encryption at rest ([`seal`]).
//! The application supplies its data folder, limits, translation and logging through [`host`].

pub mod detect;
pub mod host;
pub mod json;
pub mod model;
pub mod seal;
pub mod search;

pub use detect::{Detected, detect};
