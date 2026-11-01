//! Clipboard history logic that needs no operating system: [`detect`] classifies copied text and
//! recognises copies that are secrets.

pub mod detect;

pub use detect::{Detected, detect};
