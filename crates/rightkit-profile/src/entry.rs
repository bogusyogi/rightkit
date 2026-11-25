//! One entry: a value, who said it, the date it was true from, a state and a sensitivity, with the
//! earlier values it replaced kept as history.

use crate::area::Area;
use crate::ProfileError;
use rightkit_time::Date;
use serde::{Deserialize, Serialize};

/// Longest value, in bytes.
pub const MAX_VALUE_BYTES: usize = 4000;
/// Longest key, in bytes.
pub const MAX_KEY_BYTES: usize = 64;
/// Longest source reference, in bytes.
pub const MAX_REFERENCE_BYTES: usize = 128;
/// Most earlier values one entry keeps.
pub const MAX_HISTORY: usize = 200;

/// Who said it. Only the owner confirms; the other three can only propose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Owner,
    Repository,
    Mail,
    Memory,
}

/// Confirmed, proposed or stale. Whether a fourth state, retired, exists is an open point of the Nucleus
/// contract; history covers what it would, so none is modelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Confirmed,
    Proposed,
    Stale,
}

/// Which switch an entry sits behind. The scale beyond health and money is an open point of the Nucleus
/// contract, so the enum is not exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Sensitivity {
    Ordinary,
    Health,
    Money,
}

/// The owner's yes. The host builds one only from the owner's own typed or spoken words or an approval
/// prompt the owner answered; nothing in this crate makes one from a repository, a mail, a memory or a
/// model's output. `reference` names the proof (a chat turn id, an approval id, a paired device id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerConfirmation {
    pub reference: String,
    /// Unix milliseconds.
    pub confirmed_at: i64,
}

/// A value an entry held before it changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Superseded {
    pub value: String,
    #[serde(default)]
    pub true_from: Option<String>,
    pub source: Source,
    #[serde(default)]
    pub source_ref: String,
    /// Unix milliseconds when it was replaced.
    pub replaced_at: i64,
}

/// An entry, or a proposal for one (`state` is `Proposed` for a proposal; `id` then names the entry it
/// would become).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// `<area slug>/<key>`; see [`entry_id`].
    pub id: String,
    pub area: Area,
    pub key: String,
    pub value: String,
    pub source: Source,
    /// A short reference to the source (a memory id, a sender, a repository name), never its text.
    #[serde(default)]
    pub source_ref: String,
    /// `YYYY-MM-DD`: when the value became true.
    #[serde(default)]
    pub true_from: Option<String>,
    pub state: State,
    pub sensitivity: Sensitivity,
    /// `YYYY-MM-DD`: when the entry next needs review. Whether this is a stored field or derived from the
    /// area is an open point; a host that derives it passes the derived date in.
    #[serde(default)]
    pub review_by: Option<String>,
    /// Unix milliseconds; kept when the value changes.
    pub created_at: i64,
    /// Unix milliseconds; the version a synced copy is ordered by.
    pub updated_at: i64,
    #[serde(default)]
    pub history: Vec<Superseded>,
    /// Present on every confirmed entry.
    #[serde(default)]
    pub confirmation: Option<OwnerConfirmation>,
}

/// The id of the entry for `key` in `area`.
pub fn entry_id(area: Area, key: &str) -> String {
    format!("{}/{}", area.slug(), key)
}

/// `YYYY-MM-DD` as a real calendar date.
pub fn parse_date(raw: &str) -> Option<Date> {
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = raw.get(range)?;
        if part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let year = digits(0..4)?;
    Date::new(i64::from(year), digits(5..7)?, digits(8..10)?)
}

pub(crate) fn check_date(raw: Option<&str>, what: &'static str) -> Result<(), ProfileError> {
    match raw {
        Some(text) if parse_date(text).is_none() => Err(ProfileError::BadDate(what)),
        _ => Ok(()),
    }
}

/// A key is 1 to [`MAX_KEY_BYTES`] bytes of `a-z 0-9 . _ -`, so it can sit in a file name and an id.
pub(crate) fn check_key(key: &str) -> Result<(), ProfileError> {
    let valid = !key.is_empty()
        && key.len() <= MAX_KEY_BYTES
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(ProfileError::BadKey)
    }
}

/// A value, trimmed: not empty, within [`MAX_VALUE_BYTES`], with no control character but a newline or a tab.
pub(crate) fn clean_value(value: &str) -> Result<String, ProfileError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_VALUE_BYTES
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ProfileError::BadValue);
    }
    Ok(value.to_owned())
}

/// A source reference or a confirmation reference: within [`MAX_REFERENCE_BYTES`], no control character.
pub(crate) fn check_reference(reference: &str) -> Result<(), ProfileError> {
    if reference.len() > MAX_REFERENCE_BYTES || reference.chars().any(char::is_control) {
        return Err(ProfileError::BadReference);
    }
    Ok(())
}

impl OwnerConfirmation {
    pub(crate) fn check(&self) -> Result<(), ProfileError> {
        if self.reference.is_empty() {
            return Err(ProfileError::BadReference);
        }
        check_reference(&self.reference)
    }
}

impl Entry {
    /// Whether this entry is past `today` for review. An entry with no review date is never due.
    pub fn is_due(&self, today: Date) -> bool {
        self.review_by
            .as_deref()
            .and_then(parse_date)
            .is_some_and(|due| due <= today)
    }

    /// The label the switches see: the writer's, raised to the area's floor.
    pub fn effective_sensitivity(&self) -> Sensitivity {
        self.sensitivity.max(self.area.floor())
    }
}
