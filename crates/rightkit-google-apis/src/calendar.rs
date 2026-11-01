mod mutations;
pub use mutations::*;
// Google Calendar v3 REST client. Builds on the shared `google` client for
// auth, retry, and backoff.
//
// Authorised scopes: calendar.events (RW), calendarlist.readonly, freebusy.
// Create/delete whole calendars is NOT in scope (requires `calendar` scope).
//
// Public surface:
//   list_calendars  – GET  calendarList
//   list_events     – GET  calendars/{id}/events (time range, singleEvents)
//   create_event    – POST calendars/{id}/events (Meet + recurrence + reminders)
//   update_event    – PATCH calendars/{id}/events/{eid}
//   delete_event    – DELETE calendars/{id}/events/{eid}?sendUpdates=all
//   rsvp_event      – PATCH self attendee responseStatus

use crate::google::Google;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const BASE: &str = "https://www.googleapis.com/calendar/v3";

// ---------------------------------------------------------------------------
// Public types (Serialize → frontend)
// ---------------------------------------------------------------------------

/// A calendar entry from the calendarList.
#[derive(Debug, Clone, Serialize)]
pub struct CalendarInfo {
    pub id: String,
    pub summary: String,
    pub background_color: String,
    pub primary: bool,
    pub access_role: String,
}

/// A participant in a calendar event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attendee {
    pub email: String,
    pub response_status: String,
}

/// A fully-hydrated calendar event returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct CalEvent {
    pub id: String,
    pub calendar_id: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    /// RFC3339 dateTime or date (YYYY-MM-DD) for all-day events.
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub html_link: String,
    /// Google Meet join URL, extracted from conferenceData.entryPoints.
    pub meet_link: String,
    pub organizer: String,
    pub attendees: Vec<Attendee>,
    /// responseStatus of the "self" attendee, or "" if not present.
    pub my_response: String,
    /// true when the event has a recurringEventId (i.e. is an instance).
    pub is_recurring: bool,
    /// The master series id for a recurring instance ("" if not recurring) —
    /// used to edit/delete "all events" vs just "this event" (the instance id).
    pub recurring_event_id: String,
}

/// Input for create_event / update_event (Deserialize ← frontend).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventInput {
    pub summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub location: String,
    pub start_rfc3339: String,
    pub end_rfc3339: String,
    #[serde(default)]
    pub all_day: bool,
    pub time_zone: String,
    #[serde(default)]
    pub attendees: Vec<String>,
    pub reminder_minutes: Option<i64>,
    #[serde(default)]
    pub add_meet: bool,
    /// A single RRULE line, e.g. "RRULE:FREQ=WEEKLY;COUNT=10"
    pub recurrence: Option<String>,
}

// ---------------------------------------------------------------------------
// Private deserialisation shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawCalendarInfo {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(rename = "backgroundColor", default)]
    background_color: String,
    #[serde(default)]
    primary: Option<bool>,
    #[serde(rename = "accessRole", default)]
    access_role: String,
}

#[derive(Deserialize)]
struct CalendarListResp {
    #[serde(default)]
    items: Vec<RawCalendarInfo>,
}

#[derive(Deserialize)]
struct EventTime {
    #[serde(rename = "dateTime", default)]
    date_time: Option<String>,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Deserialize)]
struct RawAttendee {
    #[serde(default)]
    email: String,
    #[serde(rename = "responseStatus", default)]
    response_status: String,
    #[serde(rename = "self", default)]
    is_self: bool,
}

#[derive(Deserialize)]
struct RawOrganizer {
    #[serde(default)]
    email: String,
}

#[derive(Deserialize)]
struct EntryPoint {
    #[serde(rename = "entryPointType", default)]
    entry_point_type: String,
    #[serde(default)]
    uri: String,
}

#[derive(Deserialize)]
struct ConferenceData {
    #[serde(rename = "entryPoints", default)]
    entry_points: Vec<EntryPoint>,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(default)]
    id: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(rename = "htmlLink", default)]
    html_link: Option<String>,
    start: Option<EventTime>,
    end: Option<EventTime>,
    #[serde(default)]
    organizer: Option<RawOrganizer>,
    #[serde(default)]
    attendees: Vec<RawAttendee>,
    #[serde(rename = "conferenceData", default)]
    conference_data: Option<ConferenceData>,
    #[serde(rename = "recurringEventId", default)]
    recurring_event_id: Option<String>,
}

#[derive(Deserialize)]
struct EventListResp {
    #[serde(default)]
    items: Vec<RawEvent>,
}

// ---------------------------------------------------------------------------
// Helper: parse a RawEvent into CalEvent, attaching a calendar_id
// ---------------------------------------------------------------------------

fn parse_event(e: RawEvent, calendar_id: &str) -> CalEvent {
    let summary = e
        .summary
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no title)".to_string());

    let (start, all_day) = match e.start {
        Some(t) => {
            if let Some(dt) = t.date_time {
                (dt, false)
            } else if let Some(d) = t.date {
                (d, true)
            } else {
                (String::new(), false)
            }
        }
        None => (String::new(), false),
    };

    let end = match e.end {
        Some(t) => t.date_time.or(t.date).unwrap_or_default(),
        None => String::new(),
    };

    let meet_link = e
        .conference_data
        .and_then(|cd| {
            cd.entry_points
                .into_iter()
                .find(|ep| ep.entry_point_type == "video")
                .map(|ep| ep.uri)
        })
        .unwrap_or_default();

    let organizer = e.organizer.map(|o| o.email).unwrap_or_default();
    let recurring_event_id = e.recurring_event_id.clone().unwrap_or_default();

    let my_response = e
        .attendees
        .iter()
        .find(|a| a.is_self)
        .map(|a| a.response_status.clone())
        .unwrap_or_default();

    let attendees = e
        .attendees
        .into_iter()
        .map(|a| Attendee {
            email: a.email,
            response_status: a.response_status,
        })
        .collect();

    CalEvent {
        id: e.id,
        calendar_id: calendar_id.to_string(),
        summary,
        description: e.description.unwrap_or_default(),
        location: e.location.unwrap_or_default(),
        start,
        end,
        all_day,
        html_link: e.html_link.unwrap_or_default(),
        meet_link,
        organizer,
        attendees,
        my_response,
        is_recurring: !recurring_event_id.is_empty(),
        recurring_event_id,
    }
}

// ---------------------------------------------------------------------------
// Helper: build the JSON body for create / update
// ---------------------------------------------------------------------------

fn event_body(ev: &EventInput, include_meet: bool) -> Value {
    // start / end blocks
    let (start_block, end_block) = if ev.all_day {
        (
            json!({ "date": ev.start_rfc3339 }),
            json!({ "date": ev.end_rfc3339 }),
        )
    } else {
        (
            json!({ "dateTime": ev.start_rfc3339, "timeZone": ev.time_zone }),
            json!({ "dateTime": ev.end_rfc3339,   "timeZone": ev.time_zone }),
        )
    };

    let attendees: Vec<Value> = ev
        .attendees
        .iter()
        .map(|email| json!({ "email": email }))
        .collect();

    let mut body = json!({
        "summary": ev.summary,
        "description": ev.description,
        "location": ev.location,
        "start": start_block,
        "end": end_block,
        "attendees": attendees,
    });

    // reminders
    if let Some(mins) = ev.reminder_minutes {
        body["reminders"] = json!({
            "useDefault": false,
            "overrides": [{ "method": "popup", "minutes": mins }]
        });
    }

    // recurrence
    if let Some(ref rrule) = ev.recurrence {
        body["recurrence"] = json!([rrule]);
    }

    // Meet
    if include_meet {
        let request_id = random_hex_id();
        body["conferenceData"] = json!({
            "createRequest": {
                "requestId": request_id,
                "conferenceSolutionKey": { "type": "hangoutsMeet" }
            }
        });
    }

    body
}

/// Generate a random 16-char hex string for the Meet conferenceData requestId.
fn random_hex_id() -> String {
    let mut buf = [0u8; 8];
    let _ = getrandom::fill(&mut buf);
    rightkit_fs::digest::hex(&buf)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// List all calendars in the authenticated user's calendar list.
pub async fn list_calendars(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vec<CalendarInfo>, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("{BASE}/users/me/calendarList");
    let resp: CalendarListResp = g.get_json(&url, &[]).await.map_err(String::from)?;

    Ok(resp
        .items
        .into_iter()
        .map(|c| CalendarInfo {
            id: c.id,
            summary: c.summary,
            background_color: c.background_color,
            primary: c.primary.unwrap_or(false),
            access_role: c.access_role,
        })
        .collect())
}

/// List events in a calendar within [time_min, time_max] (RFC3339).
/// Expands recurring events (singleEvents=true), ordered by startTime.
pub async fn list_events(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    calendar_id: &str,
    time_min: &str,
    time_max: &str,
) -> Result<Vec<CalEvent>, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("{BASE}/calendars/{calendar_id}/events");
    let query: &[(&str, String)] = &[
        ("timeMin", time_min.to_string()),
        ("timeMax", time_max.to_string()),
        ("singleEvents", "true".to_string()),
        ("orderBy", "startTime".to_string()),
        ("maxResults", "2500".to_string()),
    ];
    let resp: EventListResp = g.get_json(&url, query).await.map_err(String::from)?;

    Ok(resp
        .items
        .into_iter()
        .map(|e| parse_event(e, calendar_id))
        .collect())
}

/// Create a new event. Returns the full hydrated CalEvent (so the meet_link
/// and server-assigned id come back to the caller immediately).
#[cfg(test)]
mod tests;
