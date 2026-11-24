//! The noticing loop (RK-2): admit one signal per event, sort it in code, read structured bookings in code,
//! reach a model only for what code could not place (a call with no tools, typed output), keep typed facts
//! with provenance, run checks after each fact, and raise findings for the attention Inbox.
//!
//! Pure logic and storage traits. No network, no model calls, no clock reads: the host supplies the mail,
//! the model's answer and the time. Outside text never becomes an instruction: the only things that leave
//! this crate from a mail are typed facts and findings built from typed values.
//!
//! The package name lives in `Cargo.toml` (and the `[lib]` name beside it) and nowhere else; [`NAME`] reads it
//! back, and no source file spells it.

pub mod booking;
pub mod calendar;
pub mod calendar_checks;
pub mod checks;
pub mod digest;
pub mod facts;
pub mod instruction;
pub mod markup;
pub mod pipeline;
pub mod signal;
pub mod sort;
pub mod state;
pub mod text;
pub mod triage;

/// The package name, from `Cargo.toml`.
pub const NAME: &str = env!("CARGO_PKG_NAME");
