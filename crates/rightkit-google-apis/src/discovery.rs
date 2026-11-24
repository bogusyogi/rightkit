//! Mail discovery for a noticing loop: which messages are new since a Gmail history cursor, one
//! message read whole, and the events of a calendar window. Plain values in, plain values out: no
//! library type, error text or token crosses to the caller except as a [`Fault`], which the caller
//! maps to its own failure codes.
//!
//! * **Discovery** ([`discover`]) never lists more than it must. With no cursor it only takes the
//!   account's current history id as a baseline ([`Discovery::Baseline`]): mail that arrived
//!   before the loop was switched on is not read. With a cursor it asks Gmail what changed
//!   ([`Discovery::Delta`]). A cursor Gmail has forgotten, or more than [`DELTA_MAX`] changed
//!   messages, forces one reconcile ([`Discovery::Reconcile`]): the newest messages of the label
//!   again and a fresh cursor, the baseline taken before the listing so mail that arrives meanwhile
//!   is replayed by the next delta. A cursor never advances past a message that failed to fetch.
//! * **Not before.** Gmail reports a changed message for any label change (read, archived), so a
//!   delta also names old mail the owner touched. Only messages Gmail received at or after
//!   `not_before_ms` are returned.
//! * **Reading** ([`fetch`]) returns the headers a loop sorts on, the plain and HTML bodies and
//!   nothing else: no attachment content.
//!
//! The functions are async and take the shared client; the caller owns the runtime and deadlines.

use crate::calendar::{self, PageCap};
use crate::gmail::{self, FetchedMessage, FullSyncApi};
use crate::google::{Google, GoogleError};

/// More changed messages than this since a cursor are not read one by one: the pass reconciles instead.
pub const DELTA_MAX: usize = 100;
/// Most ids one discovery returns.
pub const IDS_MAX: usize = 100;
/// Pages of events one read follows.
pub const EVENT_PAGES: usize = 4;

/// Why a read did not complete. The text a library error carries can hold a response body, so the
/// caller classifies it and drops it.
#[derive(Debug)]
pub enum Fault {
    /// A typed library failure.
    Google(GoogleError),
    /// A library failure that only arrives as text (`gmail::run_delta_sync`).
    Text(String),
    /// Gmail no longer has that message (HTTP 404).
    NotFound,
    /// A time could not be written as RFC 3339.
    BadTime,
}

/// One message as a loop needs it. Every text field was written by the sender (outside text).
#[derive(Clone, Debug)]
pub struct RawMail {
    pub id: String,
    pub from_name: String,
    pub from_addr: String,
    pub to: String,
    pub cc: String,
    pub reply_to: Option<String>,
    pub list_unsubscribe: Option<String>,
    pub auth_results: Option<String>,
    pub subject: String,
    /// Gmail's receive time, Unix milliseconds.
    pub internal_ms: i64,
    pub plain: Option<String>,
    pub html: Option<String>,
}

/// What a discovery found. `cursor` is the history id to keep for the next one.
#[derive(Clone, Debug)]
pub enum Discovery {
    /// First pass: nothing is read, the cursor is the baseline.
    Baseline { cursor: String },
    /// What changed since the cursor, oldest first as Gmail lists it.
    Delta {
        cursor: String,
        ids: Vec<String>,
        /// The cursor moved. False when a message failed to fetch, so the next pass tries it again.
        advanced: bool,
    },
    /// The cursor was too old or too much changed: the newest messages again.
    Reconcile {
        cursor: String,
        ids: Vec<String>,
        reason: &'static str,
    },
}

fn cap(mut ids: Vec<String>) -> Vec<String> {
    ids.truncate(IDS_MAX);
    ids
}

/// Which messages of `label` are new. `cursor` is what the last `Discovery` returned (none the
/// first time), `not_before_ms` the oldest receive time worth reading, `reconcile_max` how many of
/// the newest a reconcile lists.
pub async fn discover(
    g: Google,
    cursor: Option<String>,
    label: String,
    not_before_ms: i64,
    reconcile_max: usize,
) -> Result<Discovery, Fault> {
    let mut reason = "cursor_too_old";
    if let Some(start) = cursor.as_deref() {
        let delta = gmail::run_delta_sync(&g, start)
            .await
            .map_err(Fault::Text)?;
        if !delta.stale {
            let matching: Vec<&FetchedMessage> = delta
                .changed
                .iter()
                .filter(|m| m.label_ids.contains(&label))
                .collect();
            if matching.len() <= DELTA_MAX {
                let ids: Vec<String> = matching
                    .iter()
                    .filter(|m| m.internal_date >= not_before_ms)
                    .map(|m| m.id.clone())
                    .collect();
                let moved = delta.new_history_id.clone();
                return Ok(Discovery::Delta {
                    advanced: moved.is_some(),
                    cursor: moved.unwrap_or_else(|| start.to_string()),
                    ids: cap(ids),
                });
            }
            reason = "too_many_changes";
        }
    }
    // The baseline is taken BEFORE the listing, so mail that arrives while the fetch runs is replayed by
    // the next delta.
    let baseline = g.profile_history_id().await.map_err(Fault::Google)?;
    if cursor.is_none() {
        return Ok(Discovery::Baseline { cursor: baseline });
    }
    let listed = g
        .label_ids(&label, u32::try_from(reconcile_max).unwrap_or(1))
        .await
        .map_err(Fault::Google)?;
    let mut ids: Vec<String> = Vec::with_capacity(listed.len());
    for id in &listed {
        match g.message(id).await {
            Ok(m) if m.internal_date >= not_before_ms => ids.push(m.id),
            Ok(_) => {}
            Err(error @ GoogleError::Auth(_)) => return Err(Fault::Google(error)),
            // A message that cannot be read now is found again by the next reconcile or delta.
            Err(_) => {}
        }
    }
    Ok(Discovery::Reconcile {
        cursor: baseline,
        ids: cap(ids),
        reason,
    })
}

/// One message, whole. A message Gmail no longer has is [`Fault::NotFound`].
pub async fn fetch(g: Google, id: String) -> Result<RawMail, Fault> {
    let m = g.message(&id).await.map_err(|e| match e {
        GoogleError::Http { status: 404, .. } => Fault::NotFound,
        other => Fault::Google(other),
    })?;
    Ok(RawMail {
        id: m.id,
        from_name: m.from_name,
        from_addr: m.from_addr,
        to: m.to_addrs,
        cc: m.cc_addrs,
        reply_to: m.reply_to,
        list_unsubscribe: m.list_unsubscribe,
        auth_results: m.auth_results,
        subject: m.subject,
        internal_ms: m.internal_date,
        plain: m.body_plain,
        html: m.body_html,
    })
}

/// One calendar event as a loop needs it. Every text field was written by the event's author
/// (outside text unless the owner made the event).
#[derive(Clone, Debug)]
pub struct RawEvent {
    pub id: String,
    pub calendar_id: String,
    pub summary: String,
    pub location: String,
    /// An RFC 3339 date-time, or `YYYY-MM-DD` for an all-day event (whose `end` is the day after its last day).
    pub start: String,
    pub end: String,
    pub all_day: bool,
    /// The owner's own answer (`accepted`, `declined`, ...), or empty when he is not an attendee.
    pub my_response: String,
    pub organizer: String,
    pub attendee_count: usize,
}

/// The two ends of a window (Unix seconds) written as RFC 3339, or [`Fault::BadTime`]. Do this
/// before the client is built, so a bad time fails first.
pub fn window(from_secs: i64, to_secs: i64) -> Result<(String, String), Fault> {
    let rfc = |secs: i64| {
        rightkit_time::DateTime::from_unix(secs, 0)
            .map(|d| d.format_rfc3339())
            .ok_or(Fault::BadTime)
    };
    Ok((rfc(from_secs)?, rfc(to_secs)?))
}

/// The events of `calendar_id` between `min` and `max` (RFC 3339, from [`window`]), at most
/// `limit`, with recurring events as their instances. Read only. Cancelled events are not listed by
/// Google's default query, so a deleted event is simply not here.
pub async fn events(
    g: Google,
    calendar_id: String,
    min: String,
    max: String,
    limit: usize,
) -> Result<Vec<RawEvent>, Fault> {
    let paged = calendar::list_events_with(
        &g,
        &calendar_id,
        &min,
        &max,
        PageCap::new(EVENT_PAGES, limit),
    )
    .await
    .map_err(Fault::Google)?;
    Ok(paged
        .items
        .into_iter()
        .map(|e| RawEvent {
            id: e.id,
            calendar_id: e.calendar_id,
            summary: e.summary,
            location: e.location,
            start: e.start,
            end: e.end,
            all_day: e.all_day,
            my_response: e.my_response,
            organizer: e.organizer,
            attendee_count: e.attendees.len(),
        })
        .collect())
}
