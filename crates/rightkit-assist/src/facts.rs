//! Facts: typed working rows with provenance and the source message id. A date is a local date plus a zone.

use crate::digest;
use crate::signal::{Signal, Trust};
use rightkit_time::Date;
use serde::{Deserialize, Serialize};

/// A local calendar date, an optional local time and an optional zone. The date and time are as written at
/// the place (a flight's arrival is in the arrival city's time); the zone is `UTC`, a fixed offset such as
/// `+05:30`, or a zone name such as `Asia/Kolkata`. No conversion happens here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalDateTime {
    /// `YYYY-MM-DD`.
    pub date: String,
    /// `HH:MM`.
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
}

fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok()
}

fn valid_zone(zone: &str) -> bool {
    !zone.is_empty()
        && zone.len() <= 40
        && zone
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'+' | b'-' | b':'))
}

impl LocalDateTime {
    /// Validated construction from parts. `None` when the date is not a real calendar date, the time is not
    /// `HH:MM`, or the zone has odd characters.
    pub fn from_parts(date: &str, time: Option<&str>, zone: Option<&str>) -> Option<LocalDateTime> {
        let y = date.get(0..4).and_then(digits)?;
        let m = date.get(5..7).and_then(digits)?;
        let d = date.get(8..10).and_then(digits)?;
        if date.len() != 10 || date.get(4..5) != Some("-") || date.get(7..8) != Some("-") {
            return None;
        }
        Date::new(i64::from(y), m, d)?;
        let time = match time {
            Some(t) => {
                let h = t.get(0..2).and_then(digits)?;
                let mi = t.get(3..5).and_then(digits)?;
                if t.len() != 5 || t.get(2..3) != Some(":") || h > 23 || mi > 59 {
                    return None;
                }
                Some(t.to_string())
            }
            None => None,
        };
        let zone = match zone {
            Some(z) => {
                if !valid_zone(z) {
                    return None;
                }
                Some(z.to_string())
            }
            None => None,
        };
        Some(LocalDateTime {
            date: date.to_string(),
            time,
            zone,
        })
    }

    /// Parse ISO 8601 as schema.org writes it: `2026-12-14`, `2026-12-14T15:00`, `2026-12-14T15:00:00-05:00`,
    /// `...Z`. A trailing offset becomes the zone. Anything else is `None`.
    pub fn parse(raw: &str) -> Option<LocalDateTime> {
        let raw = raw.trim();
        let date = raw.get(0..10)?;
        let rest = raw.get(10..)?;
        if rest.is_empty() {
            return LocalDateTime::from_parts(date, None, None);
        }
        let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
        let hhmm = rest.get(0..5)?;
        let mut tail = rest.get(5..)?;
        if let Some(t) = tail.strip_prefix(':') {
            // Seconds: two digits.
            digits(t.get(0..2)?)?;
            tail = t.get(2..)?;
            if let Some(frac) = tail.strip_prefix('.') {
                let n = frac.bytes().take_while(|b| b.is_ascii_digit()).count();
                tail = frac.get(n..)?;
            }
        }
        let zone: Option<String> = if tail.is_empty() {
            None
        } else if tail == "Z" || tail == "z" {
            Some("UTC".to_string())
        } else if tail.starts_with('+') || tail.starts_with('-') {
            let sign = &tail[..1];
            let body = &tail[1..];
            let (h, m) = if let Some((h, m)) = body.split_once(':') {
                (h, m)
            } else if body.len() == 4 {
                (body.get(0..2)?, body.get(2..4)?)
            } else if body.len() == 2 {
                (body, "00")
            } else {
                return None;
            };
            let (hv, mv) = (digits(h)?, digits(m)?);
            if h.len() != 2 || m.len() != 2 || hv > 23 || mv > 59 {
                return None;
            }
            Some(format!("{sign}{h}:{m}"))
        } else {
            return None;
        };
        LocalDateTime::from_parts(date, Some(hhmm), zone.as_deref())
    }

    pub fn date_value(&self) -> Option<Date> {
        let y = self.date.get(0..4).and_then(digits)?;
        let m = self.date.get(5..7).and_then(digits)?;
        let d = self.date.get(8..10).and_then(digits)?;
        Date::new(i64::from(y), m, d)
    }

    /// Days since 1970-01-01 of the local date, for comparing dates.
    pub fn day_number(&self) -> Option<i64> {
        self.date_value().map(|d| d.days_since_epoch())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookingKind {
    Hotel,
    Flight,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    /// An airport code for a flight end.
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
}

impl Place {
    pub fn is_empty(&self) -> bool {
        self.code.is_none() && self.name.is_none() && self.city.is_none()
    }
}

/// A hotel stay or a flight. A hotel's `start` is check-in and `end` check-out; a flight's `start` is its
/// departure and `end` its arrival, each in the local time of that airport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Booking {
    pub kind: BookingKind,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub start: Option<LocalDateTime>,
    #[serde(default)]
    pub end: Option<LocalDateTime>,
    /// A hotel's place.
    #[serde(default)]
    pub place: Option<Place>,
    /// A flight's departure airport.
    #[serde(default)]
    pub from: Option<Place>,
    /// A flight's arrival airport.
    #[serde(default)]
    pub to: Option<Place>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartyRole {
    Customer,
    Vendor,
    Colleague,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Party {
    /// Display text from outside. Never used as an instruction or a tool argument.
    pub name: String,
    pub role: PartyRole,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentKind {
    ReplyNeeded,
    Deliver,
    Attend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Commitment {
    pub kind: CommitmentKind,
    #[serde(default)]
    pub due: Option<LocalDateTime>,
    /// Display text from outside. Never used as an instruction or a tool argument.
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoneyDirection {
    OwedByOwner,
    OwedToOwner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Money {
    /// Minor units (cents, paise).
    pub amount_minor: i64,
    /// Three uppercase ASCII letters.
    pub currency: String,
    pub direction: MoneyDirection,
    #[serde(default)]
    pub due: Option<LocalDateTime>,
    /// Display text from outside. Never used as an instruction or a tool argument.
    pub label: String,
}

/// A calendar entry, read by code from the calendar's own fields. `start` and `end` are local dates, with a
/// time and a zone for a timed entry; an all-day entry has dates only, and its `end` is the last day it covers
/// (Google's exclusive end date is already turned back one day).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEntry {
    /// Display text from outside (anyone can invite the owner). Never used as an instruction or a tool
    /// argument, and never copied into a finding.
    pub title: String,
    /// The location text, same rule as the title. Checks only compare it with the places of bookings.
    #[serde(default)]
    pub place: Option<String>,
    pub start: LocalDateTime,
    #[serde(default)]
    pub end: Option<LocalDateTime>,
    pub all_day: bool,
    /// The owner declined it: it is on the calendar but he is not going.
    pub declined: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum FactBody {
    Booking(Booking),
    Party(Party),
    Commitment(Commitment),
    Money(Money),
    Event(CalendarEntry),
}

/// Where a fact came from: `Owner` (the owner said it), `KnownSender` (a sender on the owner's lists, authenticated)
/// or `Untrusted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Owner,
    KnownSender,
    Untrusted,
}

/// What produced the fact's content: code reading structured data, or a model's checked answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Code,
    Model,
}

pub fn provenance_of(signal: &Signal) -> Provenance {
    if signal.trust == Trust::Owner {
        Provenance::Owner
    } else if signal.sender_known {
        Provenance::KnownSender
    } else {
        Provenance::Untrusted
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    pub id: String,
    pub signal_id: String,
    pub source_message_id: String,
    pub provenance: Provenance,
    pub origin: Origin,
    pub created_at: i64,
    pub body: FactBody,
}

impl Fact {
    /// The id is a digest of the signal and the body, so the same fact from the same signal is the same row:
    /// a replay or a repeated poll inserts nothing new.
    pub fn new(signal: &Signal, origin: Origin, body: FactBody, now: i64) -> Fact {
        let canonical = serde_json::to_string(&body).unwrap_or_default();
        let id = format!("fact-{}", digest::short(&["fact", &signal.id, &canonical]));
        Fact {
            id,
            signal_id: signal.id.clone(),
            source_message_id: signal.message_id.clone(),
            provenance: provenance_of(signal),
            origin,
            created_at: now,
            body,
        }
    }

    pub fn booking(&self) -> Option<&Booking> {
        match &self.body {
            FactBody::Booking(b) => Some(b),
            _ => None,
        }
    }

    pub fn event(&self) -> Option<&CalendarEntry> {
        match &self.body {
            FactBody::Event(e) => Some(e),
            _ => None,
        }
    }
}
