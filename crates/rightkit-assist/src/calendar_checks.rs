//! Calendar checks: rules over booking facts and calendar-entry facts, in code.
//!
//! A calendar entry's title and location come from outside (anyone can invite the owner). They are compared by
//! code and never copied into a finding, which is built from dates and times only. An entry from an outsider
//! (provenance `Untrusted`) does not count as covering a trip or matching a booking: it can only add a conflict.

use crate::checks::{date_text, day_after, CheckInput, TRIP_WINDOW_DAYS};
use crate::digest;
use crate::facts::{BookingKind, CalendarEntry, Fact, LocalDateTime, Place, Provenance};
use rightkit_agent_engine_attention::inbox::{
    ApprovalNeed, InboxAction, InboxConfidence, NewInboxItem,
};

/// A hotel stay or a flight that no calendar entry covers.
pub const TRIP_EVENT: &str = "trip.event";
/// A calendar entry about a hotel's place whose dates differ from the booking's.
pub const TRIP_EVENT_DATES: &str = "trip.event_dates";
/// Two timed calendar entries that overlap.
pub const CALENDAR_CONFLICT: &str = "calendar.conflict";

/// A calendar entry that is on the owner's calendar and that he has not declined, with the first and last day
/// it covers.
fn live_events(facts: &[Fact]) -> Vec<(&Fact, &CalendarEntry, i64, i64)> {
    let mut out = Vec::new();
    for fact in facts {
        let Some(entry) = fact.event() else {
            continue;
        };
        if entry.declined {
            continue;
        }
        let Some(first) = entry.start.day_number() else {
            continue;
        };
        let last = entry
            .end
            .as_ref()
            .and_then(|e| e.day_number())
            .unwrap_or(first)
            .max(first);
        out.push((fact, entry, first, last));
    }
    out
}

/// Entries an outsider did not send.
fn trusted_events(facts: &[Fact]) -> Vec<(&Fact, &CalendarEntry, i64, i64)> {
    live_events(facts)
        .into_iter()
        .filter(|(fact, ..)| fact.provenance != Provenance::Untrusted)
        .collect()
}

/// A hotel stay or a flight with both of its days.
fn booking_spans(facts: &[Fact]) -> Vec<(&Fact, BookingKind, i64, i64)> {
    let mut out = Vec::new();
    for fact in facts {
        let Some(booking) = fact.booking() else {
            continue;
        };
        let start = booking.start.as_ref().and_then(|d| d.day_number());
        let end = booking.end.as_ref().and_then(|d| d.day_number());
        if let (Some(first), Some(last)) = (start, end) {
            out.push((fact, booking.kind, first, last.max(first)));
        }
    }
    out
}

/// The loop knows the calendar once it holds a calendar-entry fact. Before that, "no entry" would only mean "not
/// read yet", so the booking-against-calendar checks stay quiet. (A calendar with no entries at all raises
/// nothing for that reason: the loop has no fact to show it was read.)
fn calendar_was_read(facts: &[Fact]) -> bool {
    facts.iter().any(|f| f.event().is_some())
}

fn today_of(now: i64) -> i64 {
    now.div_euclid(86_400)
}

fn kind_text(kind: BookingKind) -> &'static str {
    match kind {
        BookingKind::Hotel => "hotel stay",
        BookingKind::Flight => "flight",
    }
}

/// `trip.event`: a booking, still ahead, that no calendar entry of the owner's covers. A flight within
/// [`TRIP_WINDOW_DAYS`] of a hotel stay is part of that trip and is not reported on its own.
pub fn trip_event(input: &CheckInput) -> Vec<NewInboxItem> {
    if !calendar_was_read(input.facts) {
        return Vec::new();
    }
    let today = today_of(input.now);
    let events = trusted_events(input.facts);
    let spans = booking_spans(input.facts);
    let mut items = Vec::new();
    for (fact, kind, first, last) in &spans {
        if *last < today {
            continue;
        }
        let part_of_a_stay = *kind == BookingKind::Flight
            && spans.iter().any(|(_, other, from, to)| {
                *other == BookingKind::Hotel
                    && *first <= to + TRIP_WINDOW_DAYS
                    && *last >= from - TRIP_WINDOW_DAYS
            });
        if part_of_a_stay {
            continue;
        }
        if events
            .iter()
            .any(|(_, _, from, to)| from <= last && to >= first)
        {
            continue;
        }
        let key = digest::short(&[TRIP_EVENT, &fact.id]);
        items.push(NewInboxItem {
            id: format!("assist-{key}"),
            finding: format!(
                "The {} from {} to {} has no calendar entry.",
                kind_text(*kind),
                date_text(*first),
                date_text(*last)
            ),
            rule: "A booking that is still ahead has no calendar entry on its dates (trip.event)"
                .to_string(),
            check_id: TRIP_EVENT.to_string(),
            fact_ids: vec![fact.id.clone()],
            evidence_ids: vec![fact.source_message_id.clone(), fact.id.clone()],
            proposed_actions: vec![InboxAction {
                id: "add_calendar_event".to_string(),
                label: "Add it to the calendar".to_string(),
                approval: ApprovalNeed::Signed,
                done: false,
            }],
            confidence: Some(InboxConfidence::Medium),
            missing_information: vec![
                "Whether the trip is on another calendar".to_string(),
                "Whether the owner is going on this booking".to_string(),
            ],
            urgent: false,
            expires_at: Some(day_after(*last).max(input.now)),
        });
    }
    items
}

/// Whether `location` (a calendar entry's own text) names `place`: the place's city or name, three characters
/// or more, appears in it without regard to case.
fn place_matches(location: &str, place: &Place) -> bool {
    let location = location.to_lowercase();
    [&place.city, &place.name]
        .into_iter()
        .flatten()
        .map(|p| p.trim().to_lowercase())
        .any(|p| p.chars().count() >= 3 && location.contains(&p))
}

/// `trip.event_dates`: a calendar entry whose location names a hotel's place, whose first or last day is one to
/// [`TRIP_WINDOW_DAYS`] days from the hotel's check-in or check-out, while no entry there matches both. One
/// finding per (hotel, entry) pair; the entry's own text is used only to match the place.
pub fn trip_event_dates(input: &CheckInput) -> Vec<NewInboxItem> {
    if !calendar_was_read(input.facts) {
        return Vec::new();
    }
    let today = today_of(input.now);
    let events = trusted_events(input.facts);
    let mut items = Vec::new();
    for hotel in input.facts {
        let Some(booking) = hotel.booking() else {
            continue;
        };
        let (Some(place), BookingKind::Hotel) = (&booking.place, booking.kind) else {
            continue;
        };
        let (Some(checkin), Some(checkout)) = (
            booking.start.as_ref().and_then(|d| d.day_number()),
            booking.end.as_ref().and_then(|d| d.day_number()),
        ) else {
            continue;
        };
        if checkout < today {
            continue;
        }
        let there: Vec<_> = events
            .iter()
            .filter(|(_, entry, ..)| {
                entry
                    .place
                    .as_deref()
                    .is_some_and(|location| place_matches(location, place))
            })
            .collect();
        if there
            .iter()
            .any(|(_, _, from, to)| *from == checkin && *to == checkout)
        {
            continue;
        }
        for (event, _, from, to) in there {
            let off = (1..=TRIP_WINDOW_DAYS).contains(&(from - checkin).abs())
                || (1..=TRIP_WINDOW_DAYS).contains(&(to - checkout).abs());
            if !off {
                continue;
            }
            let mut fact_ids = vec![hotel.id.clone(), event.id.clone()];
            fact_ids.sort();
            let key = digest::short(&[TRIP_EVENT_DATES, &fact_ids.join(",")]);
            items.push(NewInboxItem {
                id: format!("assist-{key}"),
                finding: format!(
                    "A calendar entry at the hotel's place runs {} to {} and the hotel booking runs {} to {}.",
                    date_text(*from),
                    date_text(*to),
                    date_text(checkin),
                    date_text(checkout)
                ),
                rule: "A calendar entry at a hotel's place has other dates than the booking (trip.event_dates)"
                    .to_string(),
                check_id: TRIP_EVENT_DATES.to_string(),
                fact_ids,
                evidence_ids: vec![
                    hotel.source_message_id.clone(),
                    event.source_message_id.clone(),
                ],
                proposed_actions: vec![InboxAction {
                    id: "review_booking_dates".to_string(),
                    label: "Review the booking and the calendar entry".to_string(),
                    approval: ApprovalNeed::None,
                    done: false,
                }],
                confidence: Some(InboxConfidence::Medium),
                missing_information: vec![
                    "Whether the calendar entry is about this stay".to_string(),
                    "Whether the booking or the entry has the right dates".to_string(),
                ],
                urgent: false,
                expires_at: Some(day_after((*to).max(checkout)).max(input.now)),
            });
        }
    }
    items
}

/// Minutes since 1970-01-01 of a timed local date, or `None` for a date without a time.
fn minute_of(dt: &LocalDateTime) -> Option<i64> {
    let day = dt.day_number()?;
    let time = dt.time.as_deref()?;
    let hours: i64 = time.get(0..2)?.parse().ok()?;
    let minutes: i64 = time.get(3..5)?.parse().ok()?;
    Some(day * 1440 + hours * 60 + minutes)
}

fn span_text(begin: i64, end: i64) -> String {
    let (day, minute) = (begin.div_euclid(1440), begin.rem_euclid(1440));
    let (end_day, end_minute) = (end.div_euclid(1440), end.rem_euclid(1440));
    let mut text = format!(
        "{} {:02}:{:02} to ",
        date_text(day),
        minute / 60,
        minute % 60
    );
    if end_day != day {
        text.push_str(&date_text(end_day));
        text.push(' ');
    }
    text.push_str(&format!("{:02}:{:02}", end_minute / 60, end_minute % 60));
    text
}

/// `calendar.conflict`: two timed entries, neither declined, in the same zone, that overlap in time. All-day
/// entries and entries in different zones are not compared: a zone mismatch is unknown, not a conflict.
pub fn calendar_conflict(input: &CheckInput) -> Vec<NewInboxItem> {
    let today = today_of(input.now);
    let mut timed: Vec<(&Fact, &LocalDateTime, i64, i64)> = Vec::new();
    for (fact, entry, _, last) in live_events(input.facts) {
        if entry.all_day || last < today {
            continue;
        }
        let (Some(begin), Some(end)) = (
            minute_of(&entry.start),
            entry.end.as_ref().and_then(minute_of),
        ) else {
            continue;
        };
        if end > begin {
            timed.push((fact, &entry.start, begin, end));
        }
    }
    let mut items = Vec::new();
    for (i, (a, a_start, a_begin, a_end)) in timed.iter().enumerate() {
        for (b, b_start, b_begin, b_end) in timed.iter().skip(i + 1) {
            if a_start.zone != b_start.zone || a_begin >= b_end || b_begin >= a_end {
                continue;
            }
            let mut fact_ids = vec![a.id.clone(), b.id.clone()];
            fact_ids.sort();
            let key = digest::short(&[CALENDAR_CONFLICT, &fact_ids.join(",")]);
            items.push(NewInboxItem {
                id: format!("assist-{key}"),
                finding: format!(
                    "Two calendar entries overlap: {} and {}.",
                    span_text(*a_begin, *a_end),
                    span_text(*b_begin, *b_end)
                ),
                rule: "Two timed calendar entries overlap (calendar.conflict)".to_string(),
                check_id: CALENDAR_CONFLICT.to_string(),
                fact_ids,
                evidence_ids: vec![a.source_message_id.clone(), b.source_message_id.clone()],
                proposed_actions: vec![InboxAction {
                    id: "review_calendar".to_string(),
                    label: "Review the two entries".to_string(),
                    approval: ApprovalNeed::None,
                    done: false,
                }],
                confidence: Some(InboxConfidence::Medium),
                missing_information: vec![
                    "Whether the owner means to attend both".to_string(),
                    "Whether one of them is a placeholder".to_string(),
                ],
                urgent: false,
                expires_at: Some((*a_end.max(b_end) * 60).max(input.now)),
            });
        }
    }
    items
}
