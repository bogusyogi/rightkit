//! Checks: rules over facts, in code. They run after each fact insert and return findings in the shape the
//! attention Inbox takes (`NewInboxItem`).
//!
//! A mismatch is a candidate, not a verdict: a flight that lands after midnight, a flexible booking or a
//! layover can all explain unequal dates, so the finding is medium confidence, says what is missing, and
//! asks. Finding text is built only from typed values (dates). Names and labels from a mail never enter it.

use crate::calendar_checks::{calendar_conflict, trip_event, trip_event_dates};
pub use crate::calendar_checks::{CALENDAR_CONFLICT, TRIP_EVENT, TRIP_EVENT_DATES};
use crate::digest;
use crate::facts::{BookingKind, Fact};
use rightkit_agent_engine_attention::inbox::{
    ApprovalNeed, InboxAction, InboxConfidence, NewInboxItem,
};

/// The check id of the first check.
pub const TRIP_DATES: &str = "trip.dates";

/// A flight and a hotel belong to the same trip when the dates they should share are at most this many days
/// apart. Further apart than this they are unrelated bookings.
pub const TRIP_WINDOW_DAYS: i64 = 3;

pub struct CheckInput<'a> {
    pub now: i64,
    /// Every fact the loop holds, not only the new one.
    pub facts: &'a [Fact],
}

pub struct Check {
    pub id: &'static str,
    pub run: fn(&CheckInput) -> Vec<NewInboxItem>,
}

/// The checks, in order. New checks are added here.
pub const CHECKS: &[Check] = &[
    Check {
        id: TRIP_DATES,
        run: trip_dates,
    },
    Check {
        id: TRIP_EVENT,
        run: trip_event,
    },
    Check {
        id: TRIP_EVENT_DATES,
        run: trip_event_dates,
    },
    Check {
        id: CALENDAR_CONFLICT,
        run: calendar_conflict,
    },
];

/// Run every check over the facts. Findings come back in check order, then fact order, without repeats.
pub fn run_all(now: i64, facts: &[Fact]) -> Vec<NewInboxItem> {
    let input = CheckInput { now, facts };
    let mut out: Vec<NewInboxItem> = Vec::new();
    for check in CHECKS {
        for item in (check.run)(&input) {
            if !out.iter().any(|existing| existing.id == item.id) {
                out.push(item);
            }
        }
    }
    out
}

pub(crate) fn day_after(day: i64) -> i64 {
    day.saturating_add(1).saturating_mul(86_400)
}

/// `trip.dates`: a hotel's check-in or check-out differs from the arrival or departure of a flight in the same
/// trip. One finding per (hotel, flight) pair.
pub fn trip_dates(input: &CheckInput) -> Vec<NewInboxItem> {
    let mut hotels: Vec<(&Fact, i64, i64)> = Vec::new();
    let mut flights: Vec<(&Fact, i64, i64)> = Vec::new();
    for fact in input.facts {
        let Some(booking) = fact.booking() else {
            continue;
        };
        let start = booking.start.as_ref().and_then(|d| d.day_number());
        let end = booking.end.as_ref().and_then(|d| d.day_number());
        match (booking.kind, start, end) {
            // Hotel: check-in, check-out.
            (BookingKind::Hotel, Some(checkin), Some(checkout)) => {
                hotels.push((fact, checkin, checkout))
            }
            // Flight: departure date, arrival date.
            (BookingKind::Flight, Some(depart), Some(arrive)) => {
                flights.push((fact, depart, arrive))
            }
            _ => {}
        }
    }

    let mut items = Vec::new();
    for (hotel, checkin, checkout) in &hotels {
        let some_flight_arrives_at_checkin = flights.iter().any(|(_, _, arrive)| arrive == checkin);
        let some_flight_leaves_at_checkout =
            flights.iter().any(|(_, depart, _)| depart == checkout);
        for (flight, depart, arrive) in &flights {
            let arrival_gap = (arrive - checkin).abs();
            let departure_gap = (depart - checkout).abs();
            let arrival_off =
                !some_flight_arrives_at_checkin && (1..=TRIP_WINDOW_DAYS).contains(&arrival_gap);
            let departure_off =
                !some_flight_leaves_at_checkout && (1..=TRIP_WINDOW_DAYS).contains(&departure_gap);
            if !arrival_off && !departure_off {
                continue;
            }
            let mut finding = String::new();
            if arrival_off {
                finding.push_str(&format!(
                    "The hotel check-in is {} and the flight arrives {}.",
                    date_text(*checkin),
                    date_text(*arrive)
                ));
            }
            if departure_off {
                if !finding.is_empty() {
                    finding.push(' ');
                }
                finding.push_str(&format!(
                    "The hotel check-out is {} and the flight departs {}.",
                    date_text(*checkout),
                    date_text(*depart)
                ));
            }
            let mut fact_ids = vec![hotel.id.clone(), flight.id.clone()];
            fact_ids.sort();
            let mut evidence_ids: Vec<String> = Vec::new();
            for id in [
                &hotel.source_message_id,
                &flight.source_message_id,
                &hotel.id,
                &flight.id,
            ] {
                if !evidence_ids.contains(id) {
                    evidence_ids.push(id.clone());
                }
            }
            let key = digest::short(&[TRIP_DATES, &fact_ids.join(",")]);
            items.push(NewInboxItem {
                id: format!("assist-{key}"),
                finding,
                rule: "A hotel's dates differ from the dates of a flight in the same trip (trip.dates)".to_string(),
                check_id: TRIP_DATES.to_string(),
                fact_ids,
                evidence_ids,
                proposed_actions: vec![InboxAction {
                    id: "review_booking_dates".to_string(),
                    label: "Review the two bookings".to_string(),
                    approval: ApprovalNeed::None,
                    done: false,
                }],
                confidence: Some(InboxConfidence::Medium),
                missing_information: vec![
                    "Whether the flight lands after midnight".to_string(),
                    "Whether the hotel stay is flexible".to_string(),
                    "Whether a layover or another booking explains the difference".to_string(),
                ],
                urgent: false,
                expires_at: Some(day_after(*checkout.max(checkin)).max(input.now)),
            });
        }
    }
    items
}

pub(crate) fn date_text(day: i64) -> String {
    let (y, m, d) = rightkit_time::civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}
