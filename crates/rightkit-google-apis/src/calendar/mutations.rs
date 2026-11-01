use super::*;

pub async fn create_event(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    calendar_id: &str,
    ev: EventInput,
) -> Result<CalEvent, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url =
        format!("{BASE}/calendars/{calendar_id}/events?sendUpdates=all&conferenceDataVersion=1");
    let body = event_body(&ev, ev.add_meet);
    let raw: RawEvent = g.post_json(&url, &body).await.map_err(String::from)?;
    Ok(parse_event(raw, calendar_id))
}

/// Update an existing event via PATCH (merge semantics).
pub async fn update_event(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    calendar_id: &str,
    event_id: &str,
    ev: EventInput,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!(
        "{BASE}/calendars/{calendar_id}/events/{event_id}?sendUpdates=all&conferenceDataVersion=1"
    );
    let body = event_body(&ev, ev.add_meet);
    // PATCH returns the updated event; we discard it (frontend reloads).
    let _: Value = g.patch_json(&url, &body).await.map_err(String::from)?;
    Ok(())
}

/// Delete an event. Sends update notifications to attendees.
pub async fn delete_event(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    calendar_id: &str,
    event_id: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    // delete_no_content takes only a url, so append the query param inline.
    let url = format!("{BASE}/calendars/{calendar_id}/events/{event_id}?sendUpdates=all");
    g.delete_no_content(&url).await.map_err(String::from)
}

/// Rewrite an RRULE line to end at `until`, dropping any existing UNTIL/COUNT (a
/// rule can carry only one of them). Other parts (FREQ, BYDAY, INTERVAL…) stay.
pub fn rrule_with_until(rule: &str, until: &str) -> String {
    let body = rule.strip_prefix("RRULE:").unwrap_or(rule);
    let kept: Vec<&str> = body
        .split(';')
        .filter(|p| {
            if p.is_empty() {
                return false;
            }
            let key = p.split('=').next().unwrap_or("").to_ascii_uppercase();
            key != "UNTIL" && key != "COUNT"
        })
        .collect();
    format!("RRULE:{};UNTIL={}", kept.join(";"), until)
}

/// The UNTIL value (just before `occurrence_start`) for truncating a series:
/// timed → UTC basic `YYYYMMDDTHHMMSSZ` one second earlier; all-day → `YYYYMMDD`
/// of the day before.
pub fn until_before(occurrence_start: &str) -> Result<String, String> {
    use rightkit_time::DateTime;
    // A UTC instant as RFC 3339 with `-` and `:` dropped: `YYYYMMDDTHHMMSSZ`.
    let basic = |secs: i64| {
        DateTime::from_unix(secs, 0).map(|t| t.format_rfc3339().replace(['-', ':'], ""))
    };
    if occurrence_start.contains('T') {
        let bad = || format!("bad occurrence start '{occurrence_start}'");
        let dt = DateTime::parse_rfc3339(occurrence_start).ok_or_else(bad)?;
        basic(dt.unix_seconds() - 1).ok_or_else(bad)
    } else {
        // The owner has no date-only parser: read the date as midnight UTC (a strict
        // `YYYY-MM-DD`), step back one day, keep the date part of the basic form.
        let midnight = DateTime::parse_rfc3339(&format!("{occurrence_start}T00:00:00Z"))
            .filter(|_| occurrence_start.len() == 10)
            .ok_or_else(|| format!("bad occurrence date '{occurrence_start}'"))?;
        basic(midnight.unix_seconds() - 86_400)
            .and_then(|s| s.get(..8).map(str::to_string))
            .ok_or_else(|| format!("bad occurrence date '{occurrence_start}'"))
    }
}

/// Truncate a recurring series so `occurrence_start` and everything after it are
/// removed — i.e. "delete this and following events". Reads the master's RRULE,
/// sets its UNTIL to just before this occurrence, and PATCHes the master.
/// Non-RRULE recurrence lines (EXDATE/RDATE) are preserved.
pub async fn truncate_series(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    calendar_id: &str,
    recurring_event_id: &str,
    occurrence_start: &str,
) -> Result<(), String> {
    #[derive(Deserialize)]
    struct MasterRec {
        #[serde(default)]
        recurrence: Vec<String>,
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let get_url = format!("{BASE}/calendars/{calendar_id}/events/{recurring_event_id}");
    let master: MasterRec = g.get_json(&get_url, &[]).await.map_err(String::from)?;
    let rrule = master
        .recurrence
        .iter()
        .find(|r| r.to_ascii_uppercase().starts_with("RRULE"))
        .ok_or("master event has no RRULE to truncate")?;
    let until = until_before(occurrence_start)?;
    let mut new_rec: Vec<String> = master
        .recurrence
        .iter()
        .filter(|r| !r.to_ascii_uppercase().starts_with("RRULE"))
        .cloned()
        .collect();
    new_rec.push(rrule_with_until(rrule, &until));
    let patch_url =
        format!("{BASE}/calendars/{calendar_id}/events/{recurring_event_id}?sendUpdates=all");
    let body = json!({ "recurrence": new_rec });
    let _: Value = g
        .patch_json(&patch_url, &body)
        .await
        .map_err(String::from)?;
    Ok(())
}

/// Set the authenticated user's RSVP response on an event.
/// `response` must be one of: accepted | declined | tentative.
pub async fn rsvp_event(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    calendar_id: &str,
    event_id: &str,
    self_email: &str,
    response: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    // 1. Fetch the current event to get the full attendee list.
    let get_url = format!("{BASE}/calendars/{calendar_id}/events/{event_id}");
    let raw: RawEvent = g.get_json(&get_url, &[]).await.map_err(String::from)?;

    // 2. Build an updated attendees array.
    let self_lower = self_email.to_lowercase();
    let updated: Vec<Value> = raw
        .attendees
        .into_iter()
        .map(|a| {
            if a.email.to_lowercase() == self_lower {
                json!({
                    "email": a.email,
                    "responseStatus": response,
                    "self": true,
                })
            } else {
                json!({
                    "email": a.email,
                    "responseStatus": a.response_status,
                })
            }
        })
        .collect();

    // 3. PATCH only the attendees field.
    let patch_url = format!("{BASE}/calendars/{calendar_id}/events/{event_id}?sendUpdates=all");
    let patch_body = json!({ "attendees": updated });
    let _: Value = g
        .patch_json(&patch_url, &patch_body)
        .await
        .map_err(String::from)?;
    Ok(())
}

/// One busy interval (RFC3339 start/end) for a calendar/attendee.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BusySlot {
    pub start: String,
    pub end: String,
}

/// Free/busy result for one queried calendar id (an email or calendar id).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FreeBusyResult {
    pub calendar_id: String,
    pub busy: Vec<BusySlot>,
}

/// Query free/busy for a set of calendars/attendees over a window
/// (`POST calendar/v3/freeBusy`). Used to flag conflicts when adding guests.
pub async fn free_busy(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    calendar_ids: Vec<String>,
    time_min: &str,
    time_max: &str,
) -> Result<Vec<FreeBusyResult>, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let items: Vec<Value> = calendar_ids.iter().map(|id| json!({ "id": id })).collect();
    let body = json!({ "timeMin": time_min, "timeMax": time_max, "items": items });
    let url = format!("{BASE}/freeBusy");
    let resp: Value = g
        .post_json::<Value, Value>(&url, &body)
        .await
        .map_err(String::from)?;

    let mut out = Vec::new();
    if let Some(cals) = resp.get("calendars").and_then(|c| c.as_object()) {
        for (id, v) in cals {
            let busy = v
                .get("busy")
                .and_then(|b| b.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|slot| {
                            Some(BusySlot {
                                start: slot.get("start")?.as_str()?.to_string(),
                                end: slot.get("end")?.as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(FreeBusyResult {
                calendar_id: id.clone(),
                busy,
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Offline parse tests
// ---------------------------------------------------------------------------
