//! Calendar signals: one signal per calendar event, read by code, never by a model.
//!
//! A calendar event is structured data, so there is no triage call: [`extract`] turns the event's own fields
//! into a typed [`CalendarEntry`] fact and the checks run over it (a booking with no entry, entries that
//! overlap, an entry whose dates disagree with a booking). The host reads the owner's calendar with
//! `rightkit-google-apis` (`calendar::list_events` or `list_events_with`) and maps each `CalEvent` to an
//! [`InboundEvent`]; this crate does not depend on that client.
//!
//! Trust: an entry the owner made (the organizer is the owner's own account) is the owner's; an invitation
//! from anyone else is outside text. A title or location from outside is display text only: it is cleaned and
//! capped, compared with booking places by code, and never copied into a finding or used as an instruction.

use crate::facts::CalendarEntry;
use crate::facts::LocalDateTime;
use crate::signal::{NewSignal, Source, Trust};
use crate::text;
use serde::{Deserialize, Serialize};

/// Longest kept title, in characters.
pub const MAX_TITLE_CHARS: usize = 120;
/// Longest kept location, in characters.
pub const MAX_PLACE_CHARS: usize = 120;

/// One calendar event as the poller read it. Field meanings are those of `rightkit-google-apis`'s `CalEvent`
/// (`start` and `end` are an RFC 3339 date-time, or a `YYYY-MM-DD` date for an all-day event whose `end` is the
/// day after its last day), plus the two facts the poller adds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundEvent {
    /// The event id (an instance id for a recurring event), unique within a calendar.
    pub id: String,
    /// The calendar the event is on. Part of the signal's message id, since event ids repeat across calendars.
    pub calendar_id: String,
    pub summary: String,
    #[serde(default)]
    pub location: String,
    pub start: String,
    pub end: String,
    pub all_day: bool,
    /// The owner's own answer (`accepted`, `declined`, `tentative`, `needsAction`), or empty when he is not an
    /// attendee (an entry he made himself).
    #[serde(default)]
    pub my_response: String,
    /// The event is cancelled (Google `status: cancelled`). It stays on the calendar list as a stub.
    #[serde(default)]
    pub cancelled: bool,
    /// The organizer is the owner's own account.
    pub organizer_is_self: bool,
}

/// The signal for one event: source `calendar`, message id `<calendar id>/<event id>`, trust `Owner` when the
/// owner organizes it and `Outside` otherwise.
pub fn new_signal(account: &str, event: &InboundEvent, seen_at: i64) -> NewSignal {
    NewSignal {
        source: Source::Calendar,
        account: account.to_string(),
        message_id: format!("{}/{}", event.calendar_id, event.id),
        seen_at,
        trust: if event.organizer_is_self {
            Trust::Owner
        } else {
            Trust::Outside
        },
    }
}

/// The date `n` days before `date` (`YYYY-MM-DD`), for Google's exclusive all-day end.
fn day_before(date: &LocalDateTime) -> Option<LocalDateTime> {
    let day = date.day_number()?.checked_sub(1)?;
    let (y, m, d) = rightkit_time::civil_from_days(day);
    LocalDateTime::from_parts(&format!("{y:04}-{m:02}-{d:02}"), None, None)
}

/// The typed entry for `event`, or `None` for a cancelled event or one whose start is not a date or date-time.
/// An end that cannot be read leaves `end` empty. Declined entries are kept, marked `declined`.
pub fn extract(event: &InboundEvent) -> Option<CalendarEntry> {
    if event.cancelled {
        return None;
    }
    let start = LocalDateTime::parse(&event.start)?;
    let end = LocalDateTime::parse(&event.end);
    let end = if event.all_day {
        // An all-day end is the day after the last day; an end that is not after the start is one day.
        match end.as_ref().and_then(day_before) {
            Some(last) if last.day_number() >= start.day_number() => Some(last),
            _ => Some(LocalDateTime {
                date: start.date.clone(),
                time: None,
                zone: None,
            }),
        }
    } else {
        end
    };
    let place = text::clean(&event.location, MAX_PLACE_CHARS);
    Some(CalendarEntry {
        title: text::clean(&event.summary, MAX_TITLE_CHARS),
        place: (!place.is_empty()).then_some(place),
        start,
        end,
        all_day: event.all_day,
        declined: event.my_response == "declined",
    })
}
