//! The profile (RK-8): the confirmed picture of the owner, as entries in areas.
//!
//! An entry is a value, who said it, the date it was true from, a state (confirmed, proposed or stale) and a
//! sensitivity; a changed value keeps the old one as history. Repositories, mails and memories can only
//! propose; the owner alone confirms, and every path that confirms takes an [`OwnerConfirmation`] the host
//! builds from the owner's own words. The crate also decides which questions are due, builds the card a run
//! receives (confirmed entries only, health and money behind their switches) and writes and reads the neutral
//! export that carries a profile across any change of layout.
//!
//! Pure logic and plain values. No network, no model calls, no clock reads, no files: the host supplies the
//! time and the storage. The on-disk form of the area files is an open point of the Nucleus contract and is
//! not here (see the README), so nothing in this crate touches the disk.
//!
//! The package name lives in `Cargo.toml` and nowhere else; [`NAME`] reads it back, and no source file spells it.

pub mod area;
pub mod card;
mod digest;
pub mod entry;
pub mod interview;
pub mod neutral;
pub mod profile;

pub use area::Area;
pub use card::{Card, CardEntry, Switches};
pub use entry::{
    entry_id, parse_date, Entry, OwnerConfirmation, Sensitivity, Source, State, Superseded,
    MAX_HISTORY, MAX_KEY_BYTES, MAX_REFERENCE_BYTES, MAX_VALUE_BYTES,
};
pub use interview::{Question, QuestionTemplate, Reason};
pub use neutral::{export, import, ImportReport, Rejection, NEUTRAL_FORMAT};
pub use profile::{proposal_id, EntryInput, ProfileState, Proposal, ProposeOutcome, SetOutcome};

use std::fmt;

/// The package name, from `Cargo.toml`.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Why a change to the profile was refused. No variant carries a value from an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileError {
    /// A key is 1 to 64 characters of `a-z 0-9 . _ -`.
    BadKey,
    /// A value is empty, too long, or holds a control character other than a newline or a tab.
    BadValue,
    /// A reference is too long, holds a control character, or (for a confirmation) is empty.
    BadReference,
    /// The named date is not a real `YYYY-MM-DD` date.
    BadDate(&'static str),
    /// A proposal named the owner as its source. The owner states values with `set_by_owner`.
    OwnerSourceProposed,
    NoSuchProposal,
    /// Writing the neutral export failed.
    Encode,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::BadKey => f.write_str("key must be 1 to 64 characters of a-z 0-9 . _ -"),
            ProfileError::BadValue => {
                f.write_str("value is empty, too long, or has a control character")
            }
            ProfileError::BadReference => {
                f.write_str("reference is empty, too long, or has a control character")
            }
            ProfileError::BadDate(what) => write!(f, "{what} is not a YYYY-MM-DD date"),
            ProfileError::OwnerSourceProposed => {
                f.write_str("a proposal cannot name the owner as its source")
            }
            ProfileError::NoSuchProposal => f.write_str("no such proposal"),
            ProfileError::Encode => f.write_str("could not write the export"),
        }
    }
}

impl std::error::Error for ProfileError {}
