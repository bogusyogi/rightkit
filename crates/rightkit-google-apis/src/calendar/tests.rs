use super::*;
use serde_json::from_str;

fn make_event_json(extra: &str) -> String {
    // Only prepend the comma when there's an extra field, else the empty
    // case leaves a trailing comma (invalid JSON).
    let tail = if extra.trim().is_empty() {
        String::new()
    } else {
        format!(",\n                {extra}")
    };
    format!(
        r#"{{
                "id": "evt123",
                "summary": "Team Standup",
                "description": "Daily sync",
                "location": "Room 1",
                "htmlLink": "https://calendar.google.com/event?eid=xxx",
                "start": {{ "dateTime": "2026-05-23T09:00:00+05:30", "timeZone": "Asia/Kolkata" }},
                "end":   {{ "dateTime": "2026-05-23T09:30:00+05:30", "timeZone": "Asia/Kolkata" }}{tail}
            }}"#
    )
}

// --- meet_link extraction -------------------------------------------------

#[test]
fn meet_link_extracted_from_video_entry_point() {
    let json = make_event_json(
        r#"
            "conferenceData": {
                "entryPoints": [
                    { "entryPointType": "phone", "uri": "tel:+1234567890" },
                    { "entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij" }
                ]
            }
            "#,
    );
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "primary");
    assert_eq!(ev.meet_link, "https://meet.google.com/abc-defg-hij");
}

#[test]
fn meet_link_empty_when_no_conference_data() {
    let json = make_event_json("");
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "primary");
    assert_eq!(ev.meet_link, "");
}

#[test]
fn meet_link_empty_when_only_phone_entry_point() {
    let json = make_event_json(
        r#"
            "conferenceData": {
                "entryPoints": [
                    { "entryPointType": "phone", "uri": "tel:+1" }
                ]
            }
            "#,
    );
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "primary");
    assert_eq!(ev.meet_link, "");
}

// --- my_response ---------------------------------------------------------

#[test]
fn my_response_from_self_attendee() {
    let json = make_event_json(
        r#"
            "attendees": [
                { "email": "alice@example.com", "responseStatus": "accepted", "self": false },
                { "email": "me@example.com",    "responseStatus": "tentative", "self": true }
            ]
            "#,
    );
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "primary");
    assert_eq!(ev.my_response, "tentative");
}

#[test]
fn my_response_empty_when_not_attendee() {
    let json = make_event_json(
        r#"
            "attendees": [
                { "email": "alice@example.com", "responseStatus": "accepted", "self": false }
            ]
            "#,
    );
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "primary");
    assert_eq!(ev.my_response, "");
}

// --- all_day detection ---------------------------------------------------

#[test]
fn all_day_event_uses_date_field() {
    let json = r#"{
            "id": "allday1",
            "summary": "Holiday",
            "start": { "date": "2026-12-25" },
            "end":   { "date": "2026-12-26" }
        }"#;
    let raw: RawEvent = from_str(json).unwrap();
    let ev = parse_event(raw, "primary");
    assert!(ev.all_day);
    assert_eq!(ev.start, "2026-12-25");
    assert_eq!(ev.end, "2026-12-26");
}

#[test]
fn timed_event_is_not_all_day() {
    let json = make_event_json("");
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "primary");
    assert!(!ev.all_day);
}

// --- is_recurring --------------------------------------------------------

#[test]
fn is_recurring_true_when_recurring_event_id_present() {
    let json = make_event_json(r#""recurringEventId": "base_event_id""#);
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "cal_work");
    assert!(ev.is_recurring);
    assert_eq!(ev.calendar_id, "cal_work");
}

#[test]
fn is_recurring_false_for_standalone_event() {
    let json = make_event_json("");
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "primary");
    assert!(!ev.is_recurring);
}

// --- attendees list ------------------------------------------------------

#[test]
fn attendees_list_mapped_correctly() {
    let json = make_event_json(
        r#"
            "attendees": [
                { "email": "a@x.com", "responseStatus": "accepted",  "self": false },
                { "email": "b@x.com", "responseStatus": "declined",  "self": false },
                { "email": "me@x.com","responseStatus": "needsAction","self": true  }
            ]
            "#,
    );
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "primary");
    assert_eq!(ev.attendees.len(), 3);
    assert_eq!(ev.attendees[1].response_status, "declined");
    assert_eq!(ev.my_response, "needsAction");
}

// --- organizer -----------------------------------------------------------

#[test]
fn organizer_email_extracted() {
    let json = make_event_json(r#""organizer": { "email": "boss@corp.com" }"#);
    let raw: RawEvent = from_str(&json).unwrap();
    let ev = parse_event(raw, "primary");
    assert_eq!(ev.organizer, "boss@corp.com");
}

// --- event_body helper ---------------------------------------------------

#[test]
fn event_body_timed_has_date_time_blocks() {
    let ev = EventInput {
        summary: "Sync".into(),
        description: String::new(),
        location: String::new(),
        start_rfc3339: "2026-05-23T10:00:00+05:30".into(),
        end_rfc3339: "2026-05-23T11:00:00+05:30".into(),
        all_day: false,
        time_zone: "Asia/Kolkata".into(),
        attendees: vec![],
        reminder_minutes: None,
        add_meet: false,
        recurrence: None,
    };
    let body = event_body(&ev, false);
    assert!(body["start"]["dateTime"].is_string());
    assert!(body["start"].get("date").is_none());
}

#[test]
fn event_body_all_day_uses_date_keys() {
    let ev = EventInput {
        summary: "Day Off".into(),
        description: String::new(),
        location: String::new(),
        start_rfc3339: "2026-05-25".into(),
        end_rfc3339: "2026-05-26".into(),
        all_day: true,
        time_zone: "UTC".into(),
        attendees: vec![],
        reminder_minutes: None,
        add_meet: false,
        recurrence: None,
    };
    let body = event_body(&ev, false);
    assert!(body["start"]["date"].is_string());
    assert!(body["start"].get("dateTime").is_none());
}

#[test]
fn event_body_meet_creates_conference_data() {
    let ev = EventInput {
        summary: "Meeting".into(),
        description: String::new(),
        location: String::new(),
        start_rfc3339: "2026-05-23T14:00:00Z".into(),
        end_rfc3339: "2026-05-23T15:00:00Z".into(),
        all_day: false,
        time_zone: "UTC".into(),
        attendees: vec!["guest@example.com".into()],
        reminder_minutes: Some(10),
        add_meet: true,
        recurrence: None,
    };
    let body = event_body(&ev, true);
    let cd = &body["conferenceData"];
    assert!(cd.is_object(), "conferenceData missing");
    let req_id = cd["createRequest"]["requestId"].as_str().unwrap_or("");
    assert_eq!(
        req_id.len(),
        16,
        "requestId should be 16 hex chars, got {req_id}"
    );
    let reminder_mins = body["reminders"]["overrides"][0]["minutes"]
        .as_i64()
        .unwrap_or(0);
    assert_eq!(reminder_mins, 10);
}

#[test]
fn event_body_no_meet_has_no_conference_data() {
    let ev = EventInput {
        summary: "Quick call".into(),
        description: String::new(),
        location: String::new(),
        start_rfc3339: "2026-05-23T14:00:00Z".into(),
        end_rfc3339: "2026-05-23T15:00:00Z".into(),
        all_day: false,
        time_zone: "UTC".into(),
        attendees: vec![],
        reminder_minutes: None,
        add_meet: false,
        recurrence: None,
    };
    let body = event_body(&ev, false);
    assert!(
        body.get("conferenceData").is_none(),
        "conferenceData should be absent"
    );
}

#[test]
fn event_body_recurrence_included_as_array() {
    let ev = EventInput {
        summary: "Weekly".into(),
        description: String::new(),
        location: String::new(),
        start_rfc3339: "2026-05-23T10:00:00Z".into(),
        end_rfc3339: "2026-05-23T11:00:00Z".into(),
        all_day: false,
        time_zone: "UTC".into(),
        attendees: vec![],
        reminder_minutes: None,
        add_meet: false,
        recurrence: Some("RRULE:FREQ=WEEKLY;COUNT=10".into()),
    };
    let body = event_body(&ev, false);
    let rrule = body["recurrence"][0].as_str().unwrap_or("");
    assert_eq!(rrule, "RRULE:FREQ=WEEKLY;COUNT=10");
}

// --- series truncation (delete this + following) -------------------------
#[test]
fn rrule_with_until_replaces_count_and_until() {
    // COUNT dropped, UNTIL set.
    assert_eq!(
        rrule_with_until("RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=10", "20260520T235959Z"),
        "RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20260520T235959Z"
    );
    // existing UNTIL replaced, FREQ kept, accepts no RRULE: prefix.
    assert_eq!(
        rrule_with_until("FREQ=DAILY;UNTIL=20990101T000000Z", "20260520T235959Z"),
        "RRULE:FREQ=DAILY;UNTIL=20260520T235959Z"
    );
}

#[test]
fn until_before_timed_and_all_day() {
    // timed: one second earlier, UTC basic.
    assert_eq!(
        until_before("2026-05-21T10:00:00Z").unwrap(),
        "20260521T095959Z"
    );
    // all-day: the day before, date basic.
    assert_eq!(until_before("2026-05-21").unwrap(), "20260520");
    // Offsets are normalised to UTC; a month boundary steps back correctly.
    assert_eq!(
        until_before("2026-05-21T10:00:00+05:30").unwrap(),
        "20260521T042959Z"
    );
    assert_eq!(until_before("2026-03-01").unwrap(), "20260228");
}

#[test]
fn until_before_rejects_malformed_starts() {
    for bad in [
        "",
        "2026-13-01",
        "2026-5-21",
        "2026-05-21x",
        "2026-05-21T25:00:00Z",
    ] {
        assert!(until_before(bad).is_err(), "{bad:?} must be rejected");
    }
}

// --- random_hex_id -------------------------------------------------------

#[test]
fn random_hex_id_is_16_lowercase_hex_chars() {
    let id = random_hex_id();
    assert_eq!(id.len(), 16, "expected 16 chars, got {id}");
    assert!(
        id.chars().all(|c| c.is_ascii_hexdigit()),
        "non-hex char in {id}"
    );
}

#[test]
fn random_hex_id_varies() {
    let a = random_hex_id();
    let b = random_hex_id();
    // Statistically safe: 2^64 collision probability ≈ 0.
    assert_ne!(a, b, "two random IDs should differ");
}
