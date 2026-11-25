//! The Google write tools for an agent: a Gmail draft, label changes, archive, a send and a
//! calendar entry. Tier 3. The app registers the tool names, gates who may call and its switches,
//! then calls [`draft`], [`label`], [`archive`], [`send`] and [`calendar_add`] with its [`Home`], a
//! [`WritePolicy`] and the approval [`Gate`].
//!
//! * **Draft, label and archive act alone** (they can be undone). A draft is created and never
//!   sent. A label call changes labels on up to 50 messages; it cannot touch `TRASH`, `SPAM`,
//!   `SENT` or `DRAFT` (moving to Trash is a hard-to-undo step: it asks first, and is not a tool
//!   here). Archive removes `INBOX`.
//! * **A send asks, every time, for that exact message.** The approval is bound to a digest of what
//!   would be sent (the recipients, subject, text, thread and reply header after cleaning,
//!   `Composed::bind`), so changing a word asks again; the owner is shown the recipients, the
//!   subject and the whole text. Sends are limited an hour and a day, counted from a log
//!   (`send_limits`), checked before an approval is asked for and counted before the message
//!   leaves, so an uncertain answer still counts.
//! * **"Sent" only with Gmail's message id.** Gmail's answer without an id, and any answer that may
//!   mean the message left (`gmail::is_uncertain_send`), is `send_uncertain`: the run is told to
//!   check Sent and not send again, and nothing retries it.
//! * **No header injection.** Recipients are bare addresses, the subject takes no control
//!   character, the text takes only line breaks and tabs; a reply takes its thread and Message-Id
//!   from Gmail, never from the caller. The message has no `From`: Gmail sets the signed-in account.
//! * **No token.** The client reads and refreshes the grant inside the sealed store, and a failure
//!   is a fixed code and text.

use super::{Refusal, Result};
use rightkit_google_apis::calendar::{self, EventInput};
use rightkit_google_apis::compose::{self, Composed};
use rightkit_google_apis::drafts;
use rightkit_google_apis::gmail::{self, FullSyncApi};
use rightkit_google_apis::send_limits::{self, SendLimitError, SendLimits};
use rightkit_google_identity::{Failure, Home};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const WRITE_DEADLINE: Duration = Duration::from_secs(60);
const IDS_MAX: usize = 50;
const ADDRESSES_MAX: usize = 20;
const SUBJECT_MAX: usize = 300;
const BODY_MAX: usize = 20_000;
/// Labels no tool here may add or remove.
const FORBIDDEN_LABELS: [&str; 4] = ["TRASH", "SPAM", "SENT", "DRAFT"];

/// What the app decides for the write tools.
#[derive(Clone, Copy, Debug)]
pub struct WritePolicy<'a> {
    /// The tool name a send's approval is bound to and asked under.
    pub send_tool: &'a str,
    /// The tool name a calendar entry's approval is bound to and asked under.
    pub calendar_tool: &'a str,
    /// The send log; `None` when the state folder cannot be resolved (no send goes out).
    pub send_log: Option<&'a Path>,
    pub limits: SendLimits,
    /// The most characters an approval card shows; a longer send summary is refused.
    pub summary_max: usize,
    /// The description a calendar entry carries (names the app).
    pub calendar_description: &'a str,
}

/// The approval gate: `(tool, bind, summary, approval)`. With no approval it files one and answers
/// the refusal that asks; with one it checks it against the bind.
pub type Gate<'a> = &'a dyn Fn(&str, &str, &str, Option<&str>) -> Result<()>;

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

fn refuse(failure: Failure) -> Refusal {
    Refusal::new(failure.code, failure.message)
}

// ---- arguments --------------------------------------------------------------------------------

fn id_list(args: &Value, key: &str, required: bool) -> Result<Vec<String>> {
    let list = match &args[key] {
        Value::Null if !required => return Ok(Vec::new()),
        Value::Array(items) => items,
        _ => return Err(invalid(format!("{key} is a list of ids"))),
    };
    if list.is_empty() && required || list.len() > IDS_MAX {
        return Err(invalid(format!("{key} is 1 to {IDS_MAX} ids")));
    }
    list.iter()
        .map(|item| match item.as_str() {
            Some(id) if compose::plain_id(id) => Ok(id.to_string()),
            _ => Err(invalid(format!(
                "{key} holds only ids (letters, digits, _ and -)"
            ))),
        })
        .collect()
}

fn addresses(args: &Value, key: &str, required: bool) -> Result<Vec<String>> {
    let text = match &args[key] {
        Value::Null if !required => return Ok(Vec::new()),
        Value::String(text) => text.as_str(),
        _ => return Err(invalid(format!("{key} is addresses separated by commas"))),
    };
    let found: Vec<String> = text
        .split(',')
        .map(|a| a.trim().to_ascii_lowercase())
        .filter(|a| !a.is_empty())
        .collect();
    if found.is_empty() && required || found.len() > ADDRESSES_MAX {
        return Err(invalid(format!("{key} is 1 to {ADDRESSES_MAX} addresses")));
    }
    if found.iter().any(|a| !compose::valid_address(a)) {
        return Err(invalid(format!(
            "{key} holds only bare addresses like name@example.com"
        )));
    }
    Ok(found)
}

fn subject(args: &Value) -> Result<String> {
    let text = args["subject"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("subject is needed"))?;
    if text.chars().count() > SUBJECT_MAX || text.chars().any(char::is_control) {
        return Err(invalid(format!(
            "subject is plain text on one line of at most {SUBJECT_MAX} characters"
        )));
    }
    Ok(text.to_string())
}

/// The text: line breaks and tabs, no other control character.
fn body(args: &Value) -> Result<String> {
    let text = args["body"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid("body is needed"))?;
    if text.chars().count() > BODY_MAX
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
    {
        return Err(invalid(format!(
            "body is text of at most {BODY_MAX} characters"
        )));
    }
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

// ---- composing --------------------------------------------------------------------------------

fn compose_message(home: &Home, args: &Value) -> Result<Composed> {
    let to = addresses(args, "to", true)?;
    let (thread_id, in_reply_to) = match &args["replyToId"] {
        Value::Null => (None, None),
        Value::String(id) if compose::plain_id(id) => {
            let id = id.clone();
            let g = home.client().map_err(refuse)?;
            let message = home
                .run(WRITE_DEADLINE, async move {
                    g.message(&id).await.map_err(|e| home.from_google(&e))
                })
                .map_err(refuse)?;
            let header = compose::reply_header(message.rfc822_message_id);
            (Some(message.thread_id).filter(|t| !t.is_empty()), header)
        }
        _ => return Err(invalid("replyToId is a Gmail message id")),
    };
    Ok(Composed {
        to,
        cc: addresses(args, "cc", false)?,
        bcc: addresses(args, "bcc", false)?,
        subject: subject(args)?,
        body: body(args)?,
        thread_id,
        in_reply_to,
    })
}

// ---- the tools --------------------------------------------------------------------------------

/// A draft, created and never sent.
pub fn draft(home: &Home, args: &Value) -> Result<Value> {
    let composed = compose_message(home, args)?;
    let g = home.client().map_err(refuse)?;
    let raw = composed.raw();
    let thread = composed.thread_id.clone();
    let (draft_id, message_id) = home
        .run(WRITE_DEADLINE, async move {
            drafts::create_draft_with(&g, &raw, thread.as_deref())
                .await
                .map_err(|text| home.from_text(&text))
        })
        .map_err(refuse)?;
    Ok(json!({"drafted": true, "draftId": draft_id, "messageId": message_id, "sent": false}))
}

fn change_labels(
    home: &Home,
    ids: Vec<String>,
    add: Vec<String>,
    remove: Vec<String>,
) -> Result<Value> {
    if add.is_empty() && remove.is_empty() {
        return Err(invalid("name at least one label to add or remove"));
    }
    if add
        .iter()
        .chain(&remove)
        .any(|l| FORBIDDEN_LABELS.contains(&l.to_ascii_uppercase().as_str()))
    {
        return Err(Refusal::new(
            "label_not_allowed",
            "TRASH, SPAM, SENT and DRAFT cannot be added or removed here: moving mail to Trash asks the owner first and is not a tool",
        ));
    }
    let g = home.client().map_err(refuse)?;
    let changed = ids.len();
    home.run(WRITE_DEADLINE, async move {
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
        gmail::modify_labels_with(&g, &ids, &add, &remove)
            .await
            .map_err(|text| home.from_text(&text))
    })
    .map_err(refuse)?;
    Ok(json!({"changed": changed}))
}

/// Add and remove labels on up to 50 messages.
pub fn label(home: &Home, args: &Value) -> Result<Value> {
    change_labels(
        home,
        id_list(args, "ids", true)?,
        id_list(args, "add", false)?,
        id_list(args, "remove", false)?,
    )
}

/// Remove `INBOX` from up to 50 messages.
pub fn archive(home: &Home, args: &Value) -> Result<Value> {
    change_labels(
        home,
        id_list(args, "ids", true)?,
        Vec::new(),
        vec!["INBOX".to_string()],
    )
}

// ---- the send limits ----------------------------------------------------------------------------

fn check_limits(policy: &WritePolicy) -> Result<()> {
    send_limits::check(policy.send_log, policy.limits).map_err(|error| match error {
        SendLimitError::Reached => Refusal::new(
            "send_limit",
            format!(
                "the limit of {} sends an hour and {} a day is reached; nothing was sent",
                policy.limits.per_hour, policy.limits.per_day
            ),
        ),
        _ => Refusal::new(
            "send_log_unavailable",
            "the record of recent sends could not be read, so nothing was sent",
        ),
    })
}

/// Counts a send before it leaves.
fn count_send(policy: &WritePolicy) -> Result<()> {
    send_limits::count(policy.send_log).map_err(|error| match error {
        SendLimitError::Unreadable => Refusal::new(
            "send_log_unavailable",
            "the record of recent sends could not be read, so nothing was sent",
        ),
        _ => Refusal::new(
            "send_log_unavailable",
            "the record of recent sends could not be written, so nothing was sent",
        ),
    })
}

// ---- send ---------------------------------------------------------------------------------------

/// Send one message after the owner approved exactly it.
pub fn send(home: &Home, args: &Value, policy: &WritePolicy, gate: Gate) -> Result<Value> {
    let composed = compose_message(home, args)?;
    check_limits(policy)?;
    let bind = composed.bind(policy.send_tool);
    let approval = args["approval"].as_str();
    // What the owner approves is what is shown, all of it: a message whose whole text does not fit the approval card is refused,
    // so the agent shortens or splits it, and nothing the owner could not read is ever sent under their approval.
    let summary = composed.summary(policy.send_tool);
    if summary.chars().count() > policy.summary_max {
        return Err(Refusal::new(
            "message_too_long_to_approve",
            "the whole message does not fit the approval card (about 1,200 characters of text): make it shorter or send it in parts; nothing was sent",
        ));
    }
    gate(policy.send_tool, &bind, &summary, approval)?;
    count_send(policy)?;
    let g = home.client().map_err(refuse)?;
    let raw = composed.raw();
    let thread = composed.thread_id.clone();
    let sent = home
        .run(WRITE_DEADLINE, async move {
            Ok::<_, Failure>(gmail::send_message_with(&g, &raw, thread.as_deref()).await)
        })
        .map_err(refuse)?;
    match sent {
        Ok(reference) if !reference.id.is_empty() => {
            Ok(json!({"sent": true, "messageId": reference.id, "threadId": reference.thread_id}))
        }
        Ok(_) => Err(uncertain()),
        Err(text) if gmail::is_uncertain_send(&text) => Err(uncertain()),
        Err(text) => Err(refuse(home.from_text(&text))),
    }
}

// ---- calendar_add (the owner's own calendar entry, approved as exactly what it is) ----------------

/// One line of plain text of at most `max` characters, or the reason it is not.
fn line(args: &Value, key: &str, max: usize, required: bool) -> Result<String> {
    match &args[key] {
        Value::Null if !required => Ok(String::new()),
        Value::String(text)
            if (!required || !text.trim().is_empty())
                && text.chars().count() <= max
                && !text.chars().any(char::is_control) =>
        {
            Ok(text.trim().to_string())
        }
        _ => Err(invalid(format!(
            "{key} is one line of plain text of at most {max} characters"
        ))),
    }
}

/// Add one entry to the owner's primary calendar after the owner approved exactly it.
pub fn calendar_add(home: &Home, args: &Value, policy: &WritePolicy, gate: Gate) -> Result<Value> {
    let summary = line(args, "summary", 120, true)?;
    let location = line(args, "location", 120, false)?;
    let all_day = args["allDay"].as_bool() == Some(true);
    let start = line(args, "start", 40, true)?;
    let end = line(args, "end", 40, true)?;
    if all_day {
        if !compose::date_only(&start) || !compose::date_only(&end) || end <= start {
            return Err(invalid(
                "an all-day entry has start and end as YYYY-MM-DD, the end the day after the last day",
            ));
        }
    } else {
        let at =
            |text: &str| rightkit_time::DateTime::parse_rfc3339(text).map(|d| d.unix_seconds());
        match (at(&start), at(&end)) {
            (Some(from), Some(to)) if to > from => {}
            _ => {
                return Err(invalid(
                    "start and end are RFC 3339 date-times and the end is after the start",
                ));
            }
        }
    }
    let zone = match line(args, "timeZone", 64, false)? {
        z if z.is_empty() => "UTC".to_string(),
        z if z
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '+' | '-' | ':')) =>
        {
            z
        }
        _ => return Err(invalid("timeZone is a zone name like Europe/London or UTC")),
    };
    let payload = json!({
        "summary": summary, "start": start, "end": end, "allDay": all_day,
        "location": location, "timeZone": zone,
    });
    let bind = compose::bind_digest(policy.calendar_tool, &payload);
    let fingerprint: String = bind.chars().take(8).collect();
    let place = if location.is_empty() {
        String::new()
    } else {
        format!(" at {location}")
    };
    let text = format!(
        "add this entry to your Google Calendar: \"{summary}\" from {start} to {end}{} ({zone}){place}. No guests, no invitations. Entry fingerprint {fingerprint}: the approval is for exactly this entry, and any change asks again.",
        if all_day {
            " (all day; the end is the day after the last day)"
        } else {
            ""
        },
    );
    gate(
        policy.calendar_tool,
        &bind,
        &text,
        args["approval"].as_str(),
    )?;
    let g = home.client().map_err(refuse)?;
    let entry = EventInput {
        summary,
        description: policy.calendar_description.to_string(),
        location,
        start_rfc3339: start,
        end_rfc3339: end,
        all_day,
        time_zone: zone,
        attendees: Vec::new(),
        reminder_minutes: None,
        add_meet: false,
        recurrence: None,
    };
    let created = home
        .run(WRITE_DEADLINE, async move {
            calendar::create_event_with(&g, "primary", entry)
                .await
                .map_err(|text| home.from_text(&text))
        })
        .map_err(|failure| {
            Refusal::new(
                failure.code,
                format!(
                    "{} The entry may exist: check the calendar before trying again.",
                    failure.message
                ),
            )
        })?;
    Ok(json!({"added": true, "eventId": created.id}))
}

fn uncertain() -> Refusal {
    Refusal::new(
        "send_uncertain",
        "it is not known whether the message left: check the Sent folder, do not send it again, and tell the owner",
    )
}
