//! The Google read tools for an agent: mail by Gmail history cursor, one message, calendars and
//! events. Tier 2, read calls only. The app registers the tool names, gates who may call (its own
//! agent only) and its switches, then calls [`mail_list`], [`mail_read`] and [`calendar_list`]
//! with its [`Home`].
//!
//! * **Mail by Gmail history cursor.** `mail_list` with no `cursor` lists the newest messages of a
//!   label (default `INBOX`) and returns the account's current history id as `cursor`. Passing that
//!   cursor back returns what changed since. A cursor Gmail has forgotten, or more than
//!   `DELTA_MAX` changed messages, forces one reconcile: the newest messages again and a fresh
//!   cursor, with `reason` saying so. A cursor never advances past a message that failed to fetch.
//! * **Outside text is wrapped.** Every string a sender or an invitation's author wrote comes back
//!   as `{"untrusted":true,"source":"email"|"calendar","text":"...","truncated":false}`, the shape
//!   of `rightkit-agent-gateway`'s `content::wrap`. Ids, labels, times and counts are plain. No
//!   HTML body, link or attachment content is returned.
//! * **No token.** Nothing here sees one: the client reads, refreshes and saves it inside the
//!   sealed store ([`Home::client`]), and a failure is a fixed code and text ([`Failure`]).

use super::{Refusal, Result};
use rightkit_google_apis::calendar::{self, PageCap};
use rightkit_google_apis::gmail::{self, FetchedMessage, FullSyncApi};
use rightkit_google_apis::google::{Google, GoogleError};
use rightkit_google_identity::{Failure, Home};
use serde_json::{Value, json};
use std::time::Duration;

const LIST_DEADLINE: Duration = Duration::from_secs(90);
const READ_DEADLINE: Duration = Duration::from_secs(45);
const CALENDAR_DEADLINE: Duration = Duration::from_secs(60);

const MAIL_DEFAULT: usize = 15;
const MAIL_MAX: usize = 25;
/// More changed messages than this since a cursor are not listed one by one: the reply is a
/// reconcile instead (the newest again, a fresh cursor).
const DELTA_MAX: usize = 100;
const IDS_SHOWN: usize = 100;
const BODY_DEFAULT: usize = 8_000;
const BODY_MAX: usize = 12_000;
const ATTACHMENTS_SHOWN: usize = 20;
const EVENTS_DEFAULT: usize = 50;
const EVENTS_MAX: usize = 200;
const EVENT_PAGES: usize = 4;
const CALENDARS_MAX: usize = 100;
const WINDOW_DEFAULT_DAYS: i64 = 7;
const WINDOW_MAX_DAYS: i64 = 93;
/// Characters of outside text one response may carry, over all its fields.
const RESPONSE_CHARS: usize = 24_000;

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

fn refuse(failure: Failure) -> Refusal {
    Refusal::new(failure.code, failure.message)
}

// ---- outside text -------------------------------------------------------------------------

/// A shared allowance of characters for one response; each field of outside text is cleaned,
/// capped by its own limit and by what is left, and wrapped.
struct Untrusted {
    remaining: usize,
}

impl Untrusted {
    fn new() -> Untrusted {
        Untrusted {
            remaining: RESPONSE_CHARS,
        }
    }

    fn take(&mut self, source: &str, text: &str, cap: usize) -> Value {
        let cleaned: String = text.chars().filter(|c| keep(*c)).collect();
        let allowed = cap.min(self.remaining);
        let (kept, cut) = match cleaned.char_indices().nth(allowed) {
            Some((at, _)) => (cleaned[..at].to_string(), true),
            None => (cleaned, false),
        };
        self.remaining -= kept.chars().count();
        json!({"untrusted": true, "source": source, "text": kept, "truncated": cut})
    }
}

/// Text a model reads: line breaks and tabs stay; other control characters, the search-highlight
/// sentinels and the bidirectional overrides go.
fn keep(c: char) -> bool {
    if c == '\n' || c == '\t' {
        return true;
    }
    !(c.is_control()
        || ('\u{202a}'..='\u{202e}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
        || c == '\u{200e}'
        || c == '\u{200f}')
}

// ---- arguments -----------------------------------------------------------------------------

fn int_arg(args: &Value, key: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    match &args[key] {
        Value::Null => Ok(default),
        value => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| invalid(format!("{key} is a whole number from {min} to {max}"))),
    }
}

/// An optional text argument: trimmed, not empty, at most `max` characters, no control
/// characters.
fn text_arg<'a>(args: &'a Value, key: &str, max: usize) -> Result<Option<&'a str>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                Ok(None)
            } else if text.chars().count() > max || text.chars().any(char::is_control) {
                Err(invalid(format!(
                    "{key} is plain text of at most {max} characters"
                )))
            } else {
                Ok(Some(text))
            }
        }
        _ => Err(invalid(format!("{key} is text"))),
    }
}

/// A Gmail id or label id: letters, digits, `_` and `-`, at most 64.
fn id_arg<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    let text = text_arg(args, key, 64)?;
    match text {
        Some(t)
            if !t
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
        {
            Err(invalid(format!(
                "{key} is letters, digits, _ and - only (a Gmail id)"
            )))
        }
        other => Ok(other),
    }
}

// ---- mail ----------------------------------------------------------------------------------

pub fn mail_list(home: &Home, args: &Value) -> Result<Value> {
    let limit = int_arg(args, "limit", MAIL_DEFAULT, 1, MAIL_MAX)?;
    let label = id_arg(args, "label")?.unwrap_or("INBOX").to_string();
    let cursor = match text_arg(args, "cursor", 20)? {
        Some(c) if c.chars().all(|ch| ch.is_ascii_digit()) => Some(c.to_string()),
        Some(_) => return Err(invalid("cursor is the number a previous call returned")),
        None => None,
    };
    let g = home.client().map_err(refuse)?;
    home.run(LIST_DEADLINE, list_mail(home, g, cursor, label, limit))
        .map_err(refuse)
}

async fn list_mail(
    home: &Home,
    g: Google,
    cursor: Option<String>,
    label: String,
    limit: usize,
) -> std::result::Result<Value, Failure> {
    let mut reason = None;
    if let Some(start) = cursor.as_deref() {
        let delta = gmail::run_delta_sync(&g, start)
            .await
            .map_err(|text| home.from_text(&text))?;
        if delta.stale {
            reason = Some("cursor_too_old");
        } else {
            let matching: Vec<&FetchedMessage> = delta
                .changed
                .iter()
                .filter(|m| m.label_ids.contains(&label))
                .collect();
            if matching.len() <= DELTA_MAX {
                let mut words = Untrusted::new();
                let messages: Vec<Value> = matching
                    .iter()
                    .map(|m| message_summary(m, &mut words))
                    .collect();
                // A fetch that failed keeps the cursor where it was, so the next call tries again.
                let next = delta
                    .new_history_id
                    .clone()
                    .unwrap_or_else(|| start.to_string());
                return Ok(json!({
                    "mode": "delta",
                    "cursor": next,
                    "advanced": delta.new_history_id.is_some(),
                    "messages": messages,
                    "deletedIds": delta.deleted.iter().take(IDS_SHOWN).collect::<Vec<_>>(),
                    "deletedCount": delta.deleted.len(),
                    "failedFetches": delta.failed,
                }));
            }
            reason = Some("too_many_changes");
        }
    }
    // One reconcile. The baseline cursor is taken BEFORE the listing, so mail that arrives
    // while the fetch runs is replayed by the next delta.
    let baseline = g
        .profile_history_id()
        .await
        .map_err(|e| home.from_google(&e))?;
    let ids = g
        .label_ids(&label, u32::try_from(limit).unwrap_or(1))
        .await
        .map_err(|e| home.from_google(&e))?;
    let mut words = Untrusted::new();
    let mut messages = Vec::with_capacity(ids.len());
    let mut skipped = 0usize;
    for id in &ids {
        match g.message(id).await {
            Ok(m) => messages.push(message_summary(&m, &mut words)),
            Err(error @ GoogleError::Auth(_)) => return Err(home.from_google(&error)),
            Err(_) => skipped += 1,
        }
    }
    Ok(json!({
        "mode": if cursor.is_none() { "inbox" } else { "reconcile" },
        "reason": reason,
        "cursor": baseline,
        "advanced": true,
        "messages": messages,
        "skippedFetches": skipped,
    }))
}

fn message_summary(m: &FetchedMessage, words: &mut Untrusted) -> Value {
    let from = if m.from_name.is_empty() {
        m.from_addr.clone()
    } else {
        format!("{} <{}>", m.from_name, m.from_addr)
    };
    json!({
        "id": m.id,
        "threadId": m.thread_id,
        "internalDateMs": m.internal_date,
        "unread": m.unread,
        "labelIds": m.label_ids,
        "hasAttachments": m.has_attachments,
        "from": words.take("email", &from, 200),
        "subject": words.take("email", &m.subject, 300),
        "snippet": words.take("email", &m.snippet, 300),
    })
}

pub fn mail_read(home: &Home, args: &Value) -> Result<Value> {
    let id = id_arg(args, "id")?
        .ok_or_else(|| invalid("id is needed: a message id from google_mail_list"))?
        .to_string();
    let max_chars = int_arg(args, "maxChars", BODY_DEFAULT, 200, BODY_MAX)?;
    let g = home.client().map_err(refuse)?;
    let message = home
        .run(READ_DEADLINE, async move {
            g.message(&id).await.map_err(|e| home.from_google(&e))
        })
        .map_err(refuse)?;
    let mut words = Untrusted::new();
    let mut out = message_summary(&message, &mut words);
    let fields = [
        ("to", &message.to_addrs, 500),
        ("cc", &message.cc_addrs, 500),
    ];
    for (key, text, cap) in fields {
        out[key] = words.take("email", text, cap);
    }
    if let Some(results) = &message.auth_results {
        out["authenticationResults"] = words.take("email", results, 400);
    }
    // Plain text only: an HTML body is never returned.
    out["body"] = match &message.body_plain {
        Some(text) => words.take("email", text, max_chars),
        None => Value::Null,
    };
    out["hasHtmlOnlyBody"] = json!(message.body_plain.is_none() && message.body_html.is_some());
    let attachments: Vec<Value> = message
        .attachments
        .iter()
        .take(ATTACHMENTS_SHOWN)
        .map(|a| {
            json!({
                "filename": words.take("email", &a.filename, 200),
                "mimeType": words.take("email", &a.mime_type, 100),
                "size": a.size,
            })
        })
        .collect();
    out["attachments"] = json!(attachments);
    out["attachmentCount"] = json!(message.attachments.len());
    Ok(out)
}

// ---- calendar --------------------------------------------------------------------------------

pub fn calendar_list(home: &Home, args: &Value) -> Result<Value> {
    match text_arg(args, "what", 16)? {
        None | Some("events") => events(home, args),
        Some("calendars") => calendars(home),
        Some(_) => Err(invalid("what is events (the default) or calendars")),
    }
}

fn calendars(home: &Home) -> Result<Value> {
    let g = home.client().map_err(refuse)?;
    let paged = home
        .run(CALENDAR_DEADLINE, async move {
            calendar::list_calendars_with(&g, PageCap::new(2, CALENDARS_MAX))
                .await
                .map_err(|e| home.from_google(&e))
        })
        .map_err(refuse)?;
    let mut words = Untrusted::new();
    let items: Vec<Value> = paged
        .items
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "summary": words.take("calendar", &c.summary, 200),
                "primary": c.primary,
                "accessRole": c.access_role,
            })
        })
        .collect();
    Ok(json!({"calendars": items, "truncated": paged.truncated}))
}

/// An RFC 3339 argument as Unix seconds.
fn instant_arg(args: &Value, key: &str) -> Result<Option<i64>> {
    match text_arg(args, key, 40)? {
        None => Ok(None),
        Some(text) => rightkit_time::DateTime::parse_rfc3339(text)
            .map(|d| Some(d.unix_seconds()))
            .ok_or_else(|| {
                invalid(format!(
                    "{key} is an RFC 3339 time, for example 2026-12-01T09:00:00Z"
                ))
            }),
    }
}

fn rfc3339(unix_secs: i64) -> Result<String> {
    rightkit_time::DateTime::from_unix(unix_secs, 0)
        .map(|d| d.format_rfc3339())
        .ok_or_else(|| invalid("that time cannot be written"))
}

fn events(home: &Home, args: &Value) -> Result<Value> {
    let calendar_id = text_arg(args, "calendarId", 200)?
        .unwrap_or("primary")
        .to_string();
    let limit = int_arg(args, "limit", EVENTS_DEFAULT, 1, EVENTS_MAX)?;
    let now = rightkit_time::unix_millis_now() / 1000;
    let min = instant_arg(args, "timeMin")?.unwrap_or(now);
    let max = instant_arg(args, "timeMax")?.unwrap_or(min + WINDOW_DEFAULT_DAYS * 86_400);
    if max <= min || max - min > WINDOW_MAX_DAYS * 86_400 {
        return Err(invalid(format!(
            "timeMax is after timeMin, by at most {WINDOW_MAX_DAYS} days"
        )));
    }
    let (time_min, time_max) = (rfc3339(min)?, rfc3339(max)?);
    let g = home.client().map_err(refuse)?;
    let paged = home
        .run(CALENDAR_DEADLINE, {
            let calendar_id = calendar_id.clone();
            let (time_min, time_max) = (time_min.clone(), time_max.clone());
            async move {
                calendar::list_events_with(
                    &g,
                    &calendar_id,
                    &time_min,
                    &time_max,
                    PageCap::new(EVENT_PAGES, limit),
                )
                .await
                .map_err(|e| home.from_google(&e))
            }
        })
        .map_err(refuse)?;
    let mut words = Untrusted::new();
    let items: Vec<Value> = paged
        .items
        .iter()
        .map(|e| {
            json!({
                "id": e.id,
                "calendarId": e.calendar_id,
                "start": e.start,
                "end": e.end,
                "allDay": e.all_day,
                "isRecurring": e.is_recurring,
                "myResponse": e.my_response,
                "attendeeCount": e.attendees.len(),
                "hasMeetLink": !e.meet_link.is_empty(),
                "summary": words.take("calendar", &e.summary, 300),
                "location": words.take("calendar", &e.location, 300),
                "organizer": words.take("calendar", &e.organizer, 200),
                "description": words.take("calendar", &e.description, 800),
            })
        })
        .collect();
    Ok(json!({
        "timeMin": time_min,
        "timeMax": time_max,
        "events": items,
        "truncated": paged.truncated,
    }))
}
