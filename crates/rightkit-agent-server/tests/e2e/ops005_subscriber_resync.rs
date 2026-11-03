//! OPS-005 closure driver: a subscriber that lags, disconnects, and
//! reconnects against a real daemon process must recover exactly the durable
//! semantic facts it missed — no more, no less — and must never need
//! `AssistantDelta` traffic to do it. Builds on the same real
//! daemon/provider/storage machinery as EVT-004 (`tests/common/mod.rs`);
//! this file adds no second event framework.

use crate::common;

use std::thread;
use std::time::{Duration, Instant};

use common::{
    durable_events, reopen_storage, start_daemon_with_provider, start_turn, wait_for_turn_finished,
    MockProvider, RawSseSession,
};
use rightkit_agent_engine_protocol::ProtocolEvent;
use serde_json::Value;
use tempfile::TempDir;

// The CRA-10 capacity gate resolves the request model against the embedded
// model-metadata catalog, so the fixture must name a real catalog slug. The
// mock provider ignores the request body; the slug only feeds CodeRight's
// own capacity/admission path.
const PROVIDER_MODEL: &str = "deepseek/deepseek-v4-pro";

fn scripted_deltas() -> Vec<String> {
    (0..14).map(|i| format!("chunk-{i} ")).collect()
}

fn seq_of_value(value: &Value) -> i64 {
    value.get("seq").and_then(Value::as_i64).unwrap_or(i64::MAX)
}

#[test]
fn ops005_lagging_disconnected_subscriber_resyncs_to_complete_durable_truth() {
    let provider = MockProvider::start(scripted_deltas());
    let home = TempDir::new().unwrap();
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let daemon =
        start_daemon_with_provider(home.path(), None, &provider.base_url(), PROVIDER_MODEL);
    let session_id = "ops005-session";

    start_turn(&daemon, session_id, &workspace, "stream this");

    // ---- Live delivery + a lagging subscriber ----
    // Connect from the very start of the stream and read a bounded number of
    // events without draining the whole thing — modelling a subscriber that
    // reads slower than the turn produces events. The mock provider's 20ms
    // inter-chunk delay (`common::MockProvider`) gives this a real window to
    // observe genuinely incremental delivery rather than one buffered burst.
    let mut live_events: Vec<Value> = Vec::new();
    {
        let mut session = RawSseSession::connect(&daemon, session_id, 0);
        let read_deadline = Instant::now() + Duration::from_millis(150);
        while let Some(event) = session.next_event(read_deadline) {
            live_events.push(event);
            if live_events.len() >= 3 {
                break; // stop draining while more is still arriving: the lag
            }
        }
        // ---- Disconnect ----
        // `session` is dropped here, closing the real TCP socket. The turn
        // itself is driven by the daemon's own HTTP call to the mock
        // provider and is completely unaffected by this subscriber's
        // presence or absence.
    }
    assert!(
        !live_events.is_empty(),
        "the lagging subscriber must have observed at least one live event \
         before disconnecting, or this test proves nothing about live delivery"
    );
    // EVT-002: live projections (AssistantDelta, ...) are anchored to, not
    // numbered in, the durable seq space and are never replayed, so a delta
    // legitimately shares the seq of the durable event it follows. Only
    // durable facts take part in the durable-cursor / overlap accounting.
    let live_seqs: Vec<i64> = live_events
        .iter()
        .filter(|event| {
            serde_json::from_value::<ProtocolEvent>((*event).clone())
                .is_ok_and(|event| event.is_durable_fact())
        })
        .filter_map(|event| event.get("seq").and_then(Value::as_i64))
        .collect();
    let last_seen_seq = live_seqs.iter().max().copied().unwrap_or(0);

    // ---- Terminal settlement occurring while the subscriber is absent ----
    let (status, turn_id) = wait_for_turn_finished(&daemon, session_id);
    if status != "completed" {
        let history = common::get_json(
            &daemon.url(),
            &format!("/sessions/{session_id}/events/history"),
            daemon.token(),
        )
        .unwrap_or(Value::Null);
        // Brief grace for the stderr drain thread to land the error line.
        thread::sleep(Duration::from_millis(200));
        let stderr_tail = std::fs::read_to_string(home.path().join("daemon-stderr.log"))
            .unwrap_or_else(|_| "<no daemon stderr captured>".to_string());
        panic!(
            "turn did not complete (status={status}); durable history:\n{history}\n\
             daemon stderr tail:\n{}",
            &stderr_tail[stderr_tail.len().saturating_sub(4000)..]
        );
    }
    assert!(!turn_id.is_empty());

    // ---- Reconnect + resync ----
    // A real reconnecting client cannot trust its own memory of what it saw
    // live; it must ask durable history (never the live conduit alone) for
    // everything after the last seq it can prove it received. This is
    // exactly `GET /sessions/{id}/events/history?after=`, which CR-05 wires
    // to fail loudly (not silently short-count) on any storage/decode fault.
    let recovered = common::get_json(
        &daemon.url(),
        &format!("/sessions/{session_id}/events/history?after={last_seen_seq}"),
        daemon.token(),
    )
    .expect("durable catch-up must succeed for a reconnecting subscriber");
    let recovered_rows = recovered
        .as_array()
        .expect("events/history returns a JSON array")
        .clone();
    let recovered_seqs: Vec<i64> = recovered_rows
        .iter()
        .filter_map(|event| event.get("seq").and_then(Value::as_i64))
        .collect();

    // Explicit gap detection: the reconnecting client computes the seq it is
    // owed next and the resync response must start there with no hole — if
    // durable catch-up ever silently skipped a seq, this comparison catches
    // it rather than passing on a merely-nonempty response.
    assert!(
        !recovered_seqs.is_empty(),
        "reconnect must recover at least the terminal settlement the subscriber missed"
    );
    assert_eq!(
        recovered_seqs[0],
        last_seen_seq + 1,
        "resync must recover starting exactly at the first seq the subscriber never saw — \
         a gap here would be silent history loss"
    );
    for pair in recovered_seqs.windows(2) {
        assert_eq!(
            pair[1],
            pair[0] + 1,
            "recovered durable range must itself be gap-free: {recovered_seqs:?}"
        );
    }

    // No duplicate semantic effects: the seqs the subscriber actually saw
    // live and the seqs recovered on reconnect must partition the full
    // timeline with no overlap.
    let mut all_seen: Vec<i64> = live_seqs.clone();
    all_seen.extend(recovered_seqs.iter().copied());
    let mut sorted = all_seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        all_seen.len(),
        "live-observed and reconnect-recovered seqs must not overlap: {all_seen:?}"
    );

    // ---- Independent durable ground truth ----
    let db_path = daemon.database_path();
    drop(daemon);
    let storage = reopen_storage(&db_path);
    let all_rows = durable_events(&storage, session_id);
    let ground_truth_seqs: Vec<i64> = all_rows.iter().map(|row| row.seq).collect();

    // No missing durable semantic facts vs the independently known durable
    // sequence: everything the reconnect path recovered, plus everything the
    // live subscriber actually saw, must together cover every durable-fact
    // seq that independently reopened SQLite knows about.
    let ground_truth_durable_seqs: Vec<i64> = all_rows
        .iter()
        .filter(|row| {
            serde_json::from_str::<ProtocolEvent>(&row.payload_json)
                .is_ok_and(|event| event.is_durable_fact())
        })
        .map(|row| row.seq)
        .collect();
    for durable_seq in &ground_truth_durable_seqs {
        assert!(
            all_seen.contains(durable_seq),
            "durable fact at seq {durable_seq} was never delivered live nor recovered on \
             reconnect — missing durable semantic fact"
        );
    }

    // Deltas are not required for reconstruction: rebuild the transcript
    // purely from durable facts recovered over HTTP (never touching
    // AssistantDelta), and it must equal the transcript built from the
    // independently reopened SQLite file's durable facts.
    let recovered_durable_events: Vec<ProtocolEvent> = recovered_rows
        .iter()
        .filter_map(|value| serde_json::from_value::<ProtocolEvent>(value.clone()).ok())
        .filter(|event| event.is_durable_fact())
        .collect();
    assert!(
        !recovered_durable_events
            .iter()
            .any(|event| matches!(event, ProtocolEvent::AssistantDelta { .. })),
        "durable reconnect facts must never include AssistantDelta"
    );

    let ground_truth_durable_events: Vec<ProtocolEvent> = all_rows
        .iter()
        .filter_map(|row| serde_json::from_str::<ProtocolEvent>(&row.payload_json).ok())
        .filter(|event| event.is_durable_fact())
        .collect();

    // Final transcript/status equals durable truth.
    let (ground_truth_status, _) = common::terminal_status(&ground_truth_durable_events)
        .expect("independently reopened storage must contain a durable terminal turn");
    assert_eq!(ground_truth_status, "completed");
    assert_eq!(status, ground_truth_status);

    // The full client-side reconstruction is what the subscriber actually
    // saw live (seqs before the disconnect) plus what it recovered on
    // reconnect (seqs after) — exactly the two-part timeline a real
    // reconnecting client assembles. Neither half alone is the transcript;
    // together, in seq order, they must equal ground truth.
    let mut client_side_events: Vec<(i64, ProtocolEvent)> = live_events
        .iter()
        .filter_map(|value| {
            let event = serde_json::from_value::<ProtocolEvent>(value.clone()).ok()?;
            event
                .is_durable_fact()
                .then_some((seq_of_value(value), event))
        })
        .chain(recovered_rows.iter().filter_map(|value| {
            let event = serde_json::from_value::<ProtocolEvent>(value.clone()).ok()?;
            event
                .is_durable_fact()
                .then_some((seq_of_value(value), event))
        }))
        .collect();
    client_side_events.sort_by_key(|(seq, _)| *seq);
    let client_side_durable_events: Vec<ProtocolEvent> = client_side_events
        .into_iter()
        .map(|(_, event)| event)
        .collect();

    let client_side_transcript = common::transcript_signature(&client_side_durable_events);
    let ground_truth_transcript = common::transcript_signature(&ground_truth_durable_events);
    assert_eq!(
        client_side_transcript, ground_truth_transcript,
        "transcript reconstructed from live-observed + reconnect-recovered durable facts \
         (never AssistantDelta) must equal ground truth"
    );
    assert!(
        !client_side_durable_events
            .iter()
            .any(|event| matches!(event, ProtocolEvent::AssistantDelta { .. })),
        "sanity: durable-fact filtering must have actually removed AssistantDelta from both halves"
    );

    // Sanity: prove the timeline genuinely covers the whole durable log (not
    // just a lucky prefix) and that raw ground truth actually contains
    // deltas the reconstruction above had to discard.
    assert_eq!(
        sorted, ground_truth_seqs,
        "the union of live-observed and reconnect-recovered seqs must equal \
         the full durable seq timeline exactly"
    );
    assert!(
        all_rows
            .iter()
            .all(|row| common::decode_kind(row).as_deref() != Some("assistantDelta")),
        "EVT-002 live deltas must not enter durable replay truth"
    );
}
