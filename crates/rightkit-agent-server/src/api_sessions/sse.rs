use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, KeepAliveStream, Sse},
        IntoResponse, Json, Response,
    },
};
use std::convert::Infallible;

use rightkit_agent_engine_core::conduit::{seq_of, ConduitReceive, ConduitReplay};
use rightkit_agent_engine_protocol::ProtocolEvent;

use crate::api::{kind_matches, now_iso, AfterQuery, DaemonState};

/// Terminal signal that the client's cursor fell outside contiguous conduit
/// coverage. Reuses the existing `streamResyncRequired` kind so client-side
/// terminal detection recognises it unchanged; the driver must resync from
/// durable session state before reconnecting.
fn resync_event(
    session_id: &str,
    after_seq: i64,
    oldest_available_seq: i64,
    high_water: i64,
) -> ProtocolEvent {
    ProtocolEvent::StreamResyncRequired {
        session_id: session_id.to_string(),
        // One past the highest seq the client can ever have observed: the
        // cursor names a gap, not an event the conduit allocated.
        seq: high_water.saturating_add(1),
        created_at: now_iso(),
        after_seq,
        oldest_available_seq,
        high_water_mark: high_water,
    }
}

/// Translate a replay outcome into the terminal resync signal, or `None`
/// when contiguous replay exists and no signal is owed.
fn replay_resync_event(session_id: &str, replay: &ConduitReplay) -> Option<ProtocolEvent> {
    match replay {
        ConduitReplay::Events(_) => None,
        ConduitReplay::ResyncRequired {
            after_seq,
            oldest_available_seq,
            high_water_mark,
        } => Some(resync_event(
            session_id,
            *after_seq,
            *oldest_available_seq,
            *high_water_mark,
        )),
    }
}

pub(crate) async fn get_events_sse(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(q): Query<AfterQuery>,
) -> Result<
    Sse<KeepAliveStream<tokio_stream::wrappers::ReceiverStream<Result<Event, Infallible>>>>,
    Response,
> {
    let live = state
        .get_session(&id)
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;

    // Subscribe FIRST so any event that arrives between this call and
    // our history replay is captured by the live subscription. Without
    // this ordering an SSE reconnect with a `lastSeq` cursor loses every
    // event the conductor publishes while the history query runs.
    let mut sub = live.conduit.subscribe();
    let session_id = live.id.clone();
    let replay = live.conduit.replay_after(q.after);

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);

    tokio::spawn(async move {
        // Replay buffered events first. Anything with seq <= high_water
        // is already in the live subscription's tail — replay_after
        // returns events strictly greater than `q.after`, so there is no
        // overlap by construction (q.after < high_water when the client
        // is reconnecting with a real cursor; q.after >= high_water when
        // the client is up to date and `history` is empty).
        let history = match replay {
            ConduitReplay::Events(events) => events,
            // The cursor predates contiguous ring coverage: a silent empty
            // replay would drop every event in the gap, so name the gap and
            // close the stream instead.
            resync @ ConduitReplay::ResyncRequired { .. } => {
                let event = replay_resync_event(&session_id, &resync)
                    .expect("ResyncRequired replay always produces a resync signal");
                let data = serde_json::to_string(&event).unwrap_or_default();
                let _ = tx.send(Ok(Event::default().data(data))).await;
                return;
            }
        };
        // CR-02/CR-03: the highest seq actually handed to this subscriber
        // so far. Starts at the client's request cursor and only advances
        // past a replayed/live event once it has genuinely been accounted
        // for (sent, or filtered by kind — either way the client has
        // "seen" that seq pass by and must never be replayed again).
        //
        // This single counter does double duty:
        //   - CR-02: the live broadcast receiver was subscribed *before*
        //     replay ran (so a same-race-window publish is never lost),
        //     which means it may still be holding a queued copy of an
        //     event replay already delivered from the ring. Any live event
        //     with seq <= last_delivered_seq is that duplicate and is
        //     dropped rather than sent twice.
        //   - CR-03: a lag resync must be derived from the last seq this
        //     subscriber actually observed, not from the original request
        //     cursor `q.after` — after many live events have already
        //     flowed, deriving from `q.after` would force an unnecessarily
        //     old (and, combined with CR-02's filter, duplicate-prone)
        //     catch-up point.
        let mut last_delivered_seq = q.after;
        for event in history {
            let seq = seq_of(&event);
            let data = serde_json::to_string(&event).unwrap_or_default();
            if !kind_matches(&data, &q.kind) {
                last_delivered_seq = last_delivered_seq.max(seq);
                continue;
            }
            if tx.send(Ok(Event::default().data(data))).await.is_err() {
                return;
            }
            last_delivered_seq = last_delivered_seq.max(seq);
        }
        // Then stream live events as they arrive.
        loop {
            let event = match sub.recv().await {
                ConduitReceive::Event(event) => event,
                // The client's cursor is stale: name the gap with the same
                // terminal resync signal so it can resync from durable
                // history instead of inferring lag from a bare close.
                ConduitReceive::Lagged { .. } => {
                    let event = match live.conduit.replay_after(last_delivered_seq) {
                        ConduitReplay::ResyncRequired {
                            after_seq,
                            oldest_available_seq,
                            high_water_mark,
                        } => resync_event(
                            &session_id,
                            after_seq,
                            oldest_available_seq,
                            high_water_mark,
                        ),
                        // The ring still covers the cursor, but the live
                        // broadcast dropped events, so contiguous delivery
                        // is broken either way: point the driver at the
                        // ring's oldest retained seq and close.
                        ConduitReplay::Events(events) => {
                            let high_water = live.conduit.current_high_water();
                            let oldest_available_seq = events
                                .first()
                                .map(seq_of)
                                .unwrap_or_else(|| high_water.saturating_add(1));
                            resync_event(
                                &session_id,
                                last_delivered_seq,
                                oldest_available_seq,
                                high_water,
                            )
                        }
                    };
                    let data = serde_json::to_string(&event).unwrap_or_default();
                    let _ = tx.send(Ok(Event::default().data(data))).await;
                    return;
                }
                ConduitReceive::Closed => return,
            };
            let seq = seq_of(&event);
            // EVT-002: live projections are anchored to (not numbered in) the
            // durable seq space and are never replayed, so they neither dedup
            // against nor advance the durable cursor.
            let durable = event.is_durable_fact();
            // CR-02: drop a live event replay already delivered (see the
            // comment on `last_delivered_seq` above).
            if durable && seq <= last_delivered_seq {
                continue;
            }
            let data = serde_json::to_string(&event).unwrap_or_default();
            if !kind_matches(&data, &q.kind) {
                if durable {
                    last_delivered_seq = last_delivered_seq.max(seq);
                }
                continue;
            }
            if tx.send(Ok(Event::default().data(data))).await.is_err() {
                return;
            }
            if durable {
                last_delivered_seq = last_delivered_seq.max(seq);
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
/// Rows returned when a scrollback request names no explicit limit.
///
/// Sized to comfortably fill a tall terminal in one round trip without
/// pulling an unbounded slice of a long transcript into memory.
const DEFAULT_SCROLLBACK_PAGE: usize = 200;

/// Failure detail returned when durable catch-up cannot be trusted. Kept
/// distinct from a legitimate empty page: an empty `Vec` from storage means
/// "no events in range," while this means "we don't know what's in range."
/// CR-05: silently flattening a storage/decode fault to an empty response
/// let a client resume live streaming while missing durable events with no
/// signal at all — that is the exact failure mode this type exists to
/// prevent.
fn history_catch_up_error(
    session_id: &str,
    detail: String,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": "history_catch_up_failed",
            "sessionId": session_id,
            "detail": detail,
        })),
    )
}

/// EVT-004: does this session have a durable row or retained events?
///
/// Distinguishes "paged past the beginning of a real session's history" from
/// "this session never existed", which `events_before` alone cannot do - it
/// returns zero rows for both.
async fn session_has_durable_history(state: &DaemonState, id: &str) -> Result<bool, String> {
    let Some(storage) = state.storage.clone() else {
        return Ok(false);
    };
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        let exists = store
            .get_session(&id)
            .map_err(|error| error.to_string())?
            .is_some();
        let has_events = store.latest_seq(&id).map_err(|error| error.to_string())? > 0;
        Ok(exists || has_events)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Hard ceiling for one durable replay page.
const MAX_REPLAY_PAGE: usize = 1000;

/// Clamp a requested replay page size: unset or zero takes the default,
/// anything above the ceiling is capped.
fn clamp_replay_limit(limit: Option<usize>) -> usize {
    match limit {
        None | Some(0) => DEFAULT_SCROLLBACK_PAGE,
        Some(n) => n.min(MAX_REPLAY_PAGE),
    }
}

/// The client may stop paging from storage and attach live once the ring
/// replays contiguously from the page cursor; until then storage must keep
/// supplying the gap. Storage exhaustion also ends paging.
fn replay_page_complete(storage_complete: bool, ring_contiguous: bool) -> bool {
    storage_complete || ring_contiguous
}

/// `GET /sessions/{id}/events/replay?after=N&limit=M`: durable paged replay
/// for a client whose cursor fell off the conduit ring (the
/// `streamResyncRequired` path). Returns `{events, next, complete, ...}`;
/// the client repeats with `after=next` until `complete`, then reconnects to
/// `/events?after=next` and continues live. Additive: `/events` and
/// `/events/history` are unchanged.
pub(crate) async fn get_events_replay_page(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(q): Query<AfterQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let limit = clamp_replay_limit(q.limit);
    let after = q.after.max(0);
    let Some(storage) = state.storage.clone() else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "session_not_found", "sessionId": id })),
        ));
    };
    let id2 = id.clone();
    let page = tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        let page = store
            .events_page_after(&id2, after, limit)
            .map_err(|error| error.to_string())?;
        let latest = store.latest_seq(&id2).map_err(|error| error.to_string())?;
        Ok::<_, String>((page, latest))
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner)
    .map_err(|detail| history_catch_up_error(&id, detail))?;
    let (page, durable_latest) = page;

    if page.rows.is_empty()
        && durable_latest == 0
        && !session_has_durable_history(&state, &id)
            .await
            .map_err(|detail| history_catch_up_error(&id, detail))?
    {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "session_not_found", "sessionId": id })),
        ));
    }

    let mut events = Vec::with_capacity(page.rows.len());
    for row in &page.rows {
        match serde_json::from_str::<ProtocolEvent>(&row.payload_json) {
            Ok(event) => events.push(event),
            Err(error) => {
                return Err(history_catch_up_error(
                    &id,
                    format!("stored event payload failed to decode: {error}"),
                ));
            }
        }
    }

    let (ring_contiguous, high_water) = match state.get_session(&id) {
        Some(live) => (
            matches!(
                live.conduit.replay_after(page.next),
                ConduitReplay::Events(_)
            ),
            live.conduit.current_high_water().max(durable_latest),
        ),
        None => (false, durable_latest),
    };
    let complete = replay_page_complete(page.complete, ring_contiguous);
    Ok(Json(serde_json::json!({
        "sessionId": id,
        "after": after,
        "events": events,
        "next": page.next,
        "complete": complete,
        "highWaterMark": high_water,
    })))
}

pub(crate) async fn get_events_history(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(q): Query<AfterQuery>,
) -> Result<Json<Vec<ProtocolEvent>>, (StatusCode, Json<serde_json::Value>)> {
    // Durable ledger is canonical across daemon restart. A rehydrated live
    // session may exist with an empty conduit, so preferring it here loses
    // history precisely when clients reconnect after a restart.
    let stored = if let Some(storage) = state.storage.clone() {
        let id2 = id.clone();
        let before = q.before;
        let limit = q.limit;
        let after = q.after;
        let query: Result<Vec<_>, String> = tokio::task::spawn_blocking(move || {
            let store = storage.blocking_lock();
            match (before, limit) {
                // Scrollback: a bounded page of the events immediately before
                // the client's lowest retained seq. This is what lets a TUI
                // hold a small window in memory and page backwards on demand
                // instead of keeping an entire transcript resident.
                (Some(before_seq), limit) => store
                    .events_before(&id2, before_seq, limit.unwrap_or(DEFAULT_SCROLLBACK_PAGE))
                    .map_err(|error| error.to_string()),
                // Forward paging with an explicit bound.
                (None, Some(limit)) => store
                    .events_after(&id2, after)
                    .map(|mut rows| {
                        rows.truncate(limit);
                        rows
                    })
                    .map_err(|error| error.to_string()),
                // Unbounded, as before. Kept so existing cursor-only clients
                // are unaffected by the addition of paging.
                (None, None) => store
                    .events_after(&id2, after)
                    .map_err(|error| error.to_string()),
            }
        })
        .await
        // A panicked/cancelled blocking task is exactly the same kind of
        // "we don't know what's in range" failure as a storage error below.
        .map_err(|join_error| join_error.to_string())
        .and_then(|inner| inner);

        let rows = query.map_err(|detail| history_catch_up_error(&id, detail))?;
        let mut decoded = Vec::with_capacity(rows.len());
        for row in &rows {
            match serde_json::from_str::<ProtocolEvent>(&row.payload_json) {
                Ok(event) => decoded.push(event),
                // A row that fails to decode is a storage-side corruption
                // fault, not "no event here" — CR-05 requires this to
                // surface as a terminal continuity error too, rather than
                // being silently dropped from the page.
                Err(error) => {
                    return Err(history_catch_up_error(
                        &id,
                        format!("stored event payload failed to decode: {error}"),
                    ));
                }
            }
        }
        decoded
    } else {
        Vec::new()
    };
    let durable_exists = if stored.is_empty() {
        session_has_durable_history(&state, &id)
            .await
            .map_err(|detail| history_catch_up_error(&id, detail))?
    } else {
        true
    };
    let events = if !stored.is_empty() {
        stored
    } else if q.before.is_some() && durable_exists {
        // EVT-004: a backward page that came back empty means the client has
        // paged to the beginning of durable history. That is the normal
        // termination condition for scrollback, NOT a missing session —
        // returning 404 made "no more history" indistinguishable from "no
        // such session", so a reconnecting client could not terminate
        // backward paging cleanly.
        //
        // The durable-history check is required: `events_before` runs an
        // unconditional `WHERE session_id = ?` with no existence check, so an
        // unknown session also yields zero rows. Without it, paging an id
        // that never existed would answer `200 []` instead of 404.
        Vec::new()
    } else if let Some(live) = state.get_session(&id) {
        if let Some(before) = q.before {
            // A live-only session still obeys the backwards cursor & page bound.
            // The conduit is bounded; retain the newest qualifying rows in order.
            let mut page: Vec<_> = live
                .conduit
                .events_after(0)
                .into_iter()
                .filter(|event| seq_of(event) < before)
                .collect();
            let limit = q.limit.unwrap_or(DEFAULT_SCROLLBACK_PAGE);
            let discard = page.len().saturating_sub(limit);
            page.drain(..discard);
            page
        } else {
            live.conduit.events_after(q.after)
        }
    } else if durable_exists {
        Vec::new()
    } else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "session_not_found", "sessionId": id })),
        ));
    };

    let filtered = events
        .into_iter()
        .filter(|event| {
            serde_json::to_string(event)
                .ok()
                .map(|data| kind_matches(&data, &q.kind))
                .unwrap_or(false)
        })
        .collect();
    Ok(Json(filtered))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resync_event_serializes_as_terminal_stream_resync_kind() {
        let event = resync_event("s1", 3, 8, 12);
        let data = serde_json::to_value(&event).unwrap();
        // `$kind` is what client-side terminal detection reads; reusing the
        // existing kind keeps `streamResyncRequired` terminal unchanged.
        assert_eq!(data["$kind"], "streamResyncRequired");
        assert_eq!(data["afterSeq"], 3);
        assert_eq!(data["oldestAvailableSeq"], 8);
        assert_eq!(data["highWaterMark"], 12);
        // The cursor names a gap: seq sits one past anything observable.
        assert_eq!(data["seq"], 13);
        assert_eq!(data["sessionId"], "s1");
    }

    #[test]
    fn replay_resync_event_is_none_when_contiguous_replay_exists() {
        let replay = ConduitReplay::Events(Vec::new());
        assert!(replay_resync_event("s1", &replay).is_none());
    }

    #[test]
    fn replay_resync_event_carries_all_gap_fields() {
        let replay = ConduitReplay::ResyncRequired {
            after_seq: 1,
            oldest_available_seq: 3,
            high_water_mark: 5,
        };
        let event = replay_resync_event("s1", &replay).expect("resync signal owed");
        match event {
            ProtocolEvent::StreamResyncRequired {
                session_id,
                after_seq,
                oldest_available_seq,
                high_water_mark,
                ..
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(after_seq, 1);
                assert_eq!(oldest_available_seq, 3);
                assert_eq!(high_water_mark, 5);
            }
            _ => panic!("expected StreamResyncRequired"),
        }
    }

    // CR-06 / CR-02: reproduce the exact replay/live race the handler is
    // built to survive — subscribe, THEN publish, THEN replay — and prove
    // the `last_delivered_seq` dedup rule used in `get_events_sse`'s task
    // (seq <= last_delivered_seq on the live side is dropped) yields
    // exactly one delivery of the raced event instead of two. This drives
    // `rightkit_agent_engine_core::conduit::Conduit` directly rather than the full
    // axum handler, since the handler's dedup decision is a plain seq
    // comparison with no framework dependency.
    #[tokio::test]
    async fn replay_live_race_delivers_raced_event_exactly_once() {
        use rightkit_agent_engine_core::conduit::Conduit;

        fn ev(seq: i64) -> ProtocolEvent {
            ProtocolEvent::UserMessageSubmitted {
                session_id: "s1".into(),
                seq,
                created_at: "t".into(),
                text: format!("e{seq}"),
                nonce: None,
                trust: None,
                attachments: Vec::new(),
            }
        }

        let conduit = Conduit::new(16);
        conduit.publish(ev(1)); // durable seq 1, already acknowledged by client
        let after = 1_i64;

        // Mirrors `get_events_sse`: subscribe BEFORE running the replay
        // query, so a publish landing in the race window is never lost.
        let mut sub = conduit.subscribe();
        // Race window: a new event lands after subscribing but before the
        // replay query executes below.
        conduit.publish(ev(2));
        let replay = conduit.replay_after(after);

        let history = match replay {
            ConduitReplay::Events(events) => events,
            ConduitReplay::ResyncRequired { .. } => panic!("contiguous replay expected"),
        };
        let mut delivered = Vec::new();
        let mut last_delivered_seq = after;
        for event in &history {
            delivered.push(seq_of(event));
            last_delivered_seq = last_delivered_seq.max(seq_of(event));
        }
        assert_eq!(delivered, vec![2], "replay must recover the raced event");

        // The live subscriber, having been created before the publish,
        // also has event 2 queued. The dedup rule must drop it.
        match sub.recv().await {
            ConduitReceive::Event(event) => {
                let seq = seq_of(&event);
                if seq <= last_delivered_seq {
                    // Correctly dropped as a duplicate — do not deliver.
                } else {
                    delivered.push(seq);
                }
            }
            other => panic!("expected the raced live event, got {other:?}"),
        }

        assert_eq!(
            delivered,
            vec![2],
            "the raced event must be delivered exactly once, not once from replay and once live"
        );
    }

    // CR-03: a lag resync must be derived from the last seq actually
    // delivered to the subscriber, not from the stale original request
    // cursor — otherwise catch-up re-walks events the subscriber already
    // received live, compounding with CR-02's dedup in the wrong direction
    // (a resync naming an already-delivered seq is at least redundant, and
    // naming a seq indistinguishable from "never delivered" invites either
    // silent gaps or unnecessary re-delivery).
    #[test]
    fn lag_resync_uses_last_delivered_seq_not_original_request_cursor() {
        let original_after = 1_i64;
        // Client received live events up through seq 40 after the initial
        // request cursor of 1.
        let last_delivered_seq = 40_i64;
        let event = resync_event("s1", last_delivered_seq, 41, 40);
        match event {
            ProtocolEvent::StreamResyncRequired { after_seq, .. } => {
                assert_eq!(
                    after_seq, last_delivered_seq,
                    "resync must name the last-delivered cursor"
                );
                assert_ne!(
                    after_seq, original_after,
                    "resync must not regress to the stale original request cursor"
                );
            }
            _ => panic!("expected StreamResyncRequired"),
        }
    }

    // CR-05: a storage/decode failure during durable catch-up must surface
    // as a structured non-success error, not an empty `Vec` indistinguishable
    // from "no events in range".
    #[test]
    fn replay_limit_clamps_default_and_ceiling() {
        assert_eq!(clamp_replay_limit(None), DEFAULT_SCROLLBACK_PAGE);
        assert_eq!(clamp_replay_limit(Some(0)), DEFAULT_SCROLLBACK_PAGE);
        assert_eq!(clamp_replay_limit(Some(7)), 7);
        assert_eq!(clamp_replay_limit(Some(usize::MAX)), MAX_REPLAY_PAGE);
    }

    #[test]
    fn replay_page_completes_on_storage_exhaustion_or_ring_contiguity() {
        assert!(!replay_page_complete(false, false));
        assert!(replay_page_complete(true, false));
        assert!(replay_page_complete(false, true));
    }

    /// A cursor evicted from the ring resyncs; paging from storage to the
    /// ring's oldest seq then makes the ring contiguous so live can resume.
    #[test]
    fn evicted_cursor_becomes_ring_contiguous_after_paging_forward() {
        use rightkit_agent_engine_core::conduit::Conduit;
        let conduit = Conduit::new(16);
        for seq in 1..=3 {
            conduit.publish(ProtocolEvent::UserMessageSubmitted {
                session_id: "s1".into(),
                seq,
                created_at: "t".into(),
                text: format!("e{seq}"),
                nonce: None,
                trust: None,
                attachments: Vec::new(),
            });
        }
        // Simulate eviction of seq 1..=2 from a fresh ring holding only 3.
        let late = Conduit::new(16);
        late.publish(conduit.events_after(2).remove(0));
        assert!(matches!(
            late.replay_after(0),
            ConduitReplay::ResyncRequired { .. }
        ));
        assert!(matches!(late.replay_after(2), ConduitReplay::Events(_)));
    }

    #[test]
    fn history_catch_up_error_is_structured_and_non_success() {
        let (status, body) = history_catch_up_error("s1", "disk read failed".to_string());
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let value = body.0;
        assert_eq!(value["error"], "history_catch_up_failed");
        assert_eq!(value["sessionId"], "s1");
        assert_eq!(value["detail"], "disk read failed");
    }
}
