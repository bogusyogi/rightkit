//! EVT-004 closure driver: a real daemon process, a real file-backed SQLite
//! database, and a real session driven against a deterministic local
//! OpenAI-compatible SSE provider double. Proves that a subscriber that
//! disconnects mid-stream and reconnects after durable terminal settlement
//! reconstructs the exact same transcript and terminal status as an
//! uninterrupted run — using only durable facts, never `AssistantDelta`
//! traffic — and that tail-first hydration plus backward paging reconstructs
//! the durable log's exact ordering.
//!
//! `rehydrate.rs`'s `qualification_evt004_disconnect_reconnect_pages_to_same_terminal_truth`
//! (hand-built `EventRow`s over `Storage::open_in_memory()`) stays as a fine
//! unit regression for the fold/paging logic in isolation; it is not this
//! closure driver because it never runs a real daemon process, a real
//! provider turn, or a real subscriber connect/disconnect over HTTP.

use crate::common;

use std::time::Duration;

use common::{
    durable_events, reopen_storage, start_daemon_with_provider, start_turn, wait_for_turn_finished,
    MockProvider,
};
use rightkit_agent_engine_protocol::ProtocolEvent;
use tempfile::TempDir;

// The CRA-10 capacity gate resolves the request model against the embedded
// model-metadata catalog, so the fixture must name a real catalog slug. The
// mock provider ignores the request body; the slug only feeds CodeRight's
// own capacity/admission path.
const PROVIDER_MODEL: &str = "deepseek/deepseek-v4-pro";
const SCRIPTED_DELTAS: &[&str] = &["Hello", ", ", "durable", " ", "world", "."];

fn scripted_deltas() -> Vec<String> {
    SCRIPTED_DELTAS.iter().map(|s| s.to_string()).collect()
}

/// Require a real delta while provider terminal settlement is gated, then
/// close the subscriber socket before allowing the remaining stream.
fn subscribe_and_disconnect_after_delta(daemon: &common::TestDaemon, session_id: &str) {
    let mut subscriber = common::RawSseSession::connect(daemon, session_id, 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut observed = Vec::new();
    loop {
        let event = subscriber
            .next_event(deadline)
            .unwrap_or_else(|| panic!("subscriber must receive a real delta: {observed:?}"));
        observed.push(event.clone());
        assert_ne!(event["$kind"], "sessionTurnFinished");
        if event["$kind"] == "assistantDelta" {
            break;
        }
    }
    drop(subscriber);
}

#[test]
fn evt004_disconnected_subscriber_reconnects_to_identical_durable_truth() {
    // ---- Reference run: an uninterrupted turn, no subscriber at all. ----
    let reference_provider = MockProvider::start(scripted_deltas());
    let reference_home = TempDir::new().unwrap();
    let reference_workspace = reference_home.path().join("workspace");
    std::fs::create_dir_all(&reference_workspace).unwrap();
    let reference_daemon = start_daemon_with_provider(
        reference_home.path(),
        None,
        &reference_provider.base_url(),
        PROVIDER_MODEL,
    );
    let reference_session = "evt004-reference";
    start_turn(
        &reference_daemon,
        reference_session,
        &reference_workspace,
        "say hello",
    );
    let (reference_status, reference_turn_id) =
        wait_for_turn_finished(&reference_daemon, reference_session);
    assert_eq!(reference_status, "completed");
    assert!(!reference_turn_id.is_empty());
    assert_eq!(
        reference_provider.request_count(),
        1,
        "reference run must make exactly one provider call"
    );

    let reference_db = reference_daemon.database_path();
    drop(reference_daemon); // stop the process; the sqlite file remains
    let reference_storage = reopen_storage(&reference_db);
    let reference_rows = durable_events(&reference_storage, reference_session);
    assert!(
        reference_rows
            .iter()
            .all(|row| common::decode_kind(row).as_deref() != Some("assistantDelta")),
        "EVT-002 live deltas must stay out of durable replay truth"
    );

    // ---- Disconnect/reconnect run: same script, a subscriber disconnects
    // partway through, the turn settles durably with nobody attached, then a
    // reconnect must reconstruct identically from durable facts alone. ----
    let flaky_provider = MockProvider::start_paused_before_terminal(scripted_deltas());
    let flaky_home = TempDir::new().unwrap();
    let flaky_workspace = flaky_home.path().join("workspace");
    std::fs::create_dir_all(&flaky_workspace).unwrap();
    let flaky_daemon = start_daemon_with_provider(
        flaky_home.path(),
        None,
        &flaky_provider.base_url(),
        PROVIDER_MODEL,
    );
    let flaky_session = "evt004-flaky";
    start_turn(&flaky_daemon, flaky_session, &flaky_workspace, "say hello");

    subscribe_and_disconnect_after_delta(&flaky_daemon, flaky_session);
    flaky_provider.resume_stream();

    // Let the turn reach durable terminal settlement with no subscriber
    // attached at all.
    let (flaky_status, flaky_turn_id) = wait_for_turn_finished(&flaky_daemon, flaky_session);
    assert_eq!(flaky_status, "completed");
    assert!(!flaky_turn_id.is_empty());
    let _ = &reference_turn_id;

    let flaky_db = flaky_daemon.database_path();
    drop(flaky_daemon);

    // Reconnect from the durable cursor: independently reopen the SQLite
    // file (never the daemon's own handle) and rebuild using ONLY durable
    // facts.
    let flaky_storage = reopen_storage(&flaky_db);
    let flaky_rows = durable_events(&flaky_storage, flaky_session);

    // The independently reopened durable log must contain the terminal fact.
    assert!(
        flaky_rows
            .iter()
            .any(|row| { common::decode_kind(row).as_deref() == Some("sessionTurnFinished") }),
        "sessionTurnFinished must be present in the durable log"
    );

    // Reconstruction succeeds without any AssistantDelta: filter every delta
    // out before rehydrating and prove the terminal/transcript facts survive.
    let durable_only: Vec<_> = flaky_rows
        .iter()
        .filter(|row| {
            serde_json::from_str::<ProtocolEvent>(&row.payload_json)
                .is_ok_and(|event| event.is_durable_fact())
        })
        .cloned()
        .collect();
    assert!(
        !durable_only
            .iter()
            .any(|row| common::decode_kind(row).as_deref() == Some("assistantDelta")),
        "durable-fact filter must have discarded every AssistantDelta"
    );

    let durable_events_decoded: Vec<ProtocolEvent> = durable_only
        .iter()
        .filter_map(|row| serde_json::from_str::<ProtocolEvent>(&row.payload_json).ok())
        .collect();
    let (flaky_terminal_status, _flaky_terminal_turn_id) =
        common::terminal_status(&durable_events_decoded)
            .expect("a durable terminal turn must reconstruct from durable facts alone");
    assert_eq!(flaky_terminal_status, "completed");

    // Tail-first hydration, then backward paging, must reconstruct the exact
    // durable ordering: fetch the newest page first (as a genuinely
    // reconnecting client would), then walk backwards in small pages until
    // exhausted, and the concatenation must equal the full forward-ordered
    // log exactly.
    let high_water = flaky_storage
        .latest_seq(flaky_session)
        .expect("latest seq for flaky session");
    let mut pages_newest_first: Vec<Vec<rightkit_agent_engine_storage::EventRow>> = Vec::new();
    let mut cursor = high_water + 1;
    loop {
        let page = flaky_storage
            .events_before(flaky_session, cursor, 2)
            .expect("events_before page");
        if page.is_empty() {
            break;
        }
        cursor = page[0].seq;
        pages_newest_first.push(page);
    }
    let mut paged_ascending: Vec<rightkit_agent_engine_storage::EventRow> = Vec::new();
    for page in pages_newest_first.into_iter().rev() {
        paged_ascending.extend(page);
    }
    let all_rows_ascending = flaky_storage
        .all_events(flaky_session)
        .expect("all_events for ordering comparison");
    assert_eq!(
        paged_ascending
            .iter()
            .map(|row| row.seq)
            .collect::<Vec<_>>(),
        all_rows_ascending
            .iter()
            .map(|row| row.seq)
            .collect::<Vec<_>>(),
        "tail-first hydration plus backward paging must reconstruct contiguous, \
         correctly ordered durable seq"
    );
    // The same backward paging must hold through the REAL HTTP endpoint a
    // reconnecting client uses, not only the storage function beneath it.
    // Proving the storage query alone leaves the endpoint's own `before`/
    // `limit` parameter handling and response marshalling unverified, which
    // is exactly where a reconnecting client would break.
    // Restart a daemon on the SAME durable data root - which is exactly what a
    // client reconnecting after the process went away actually talks to - and
    // page backwards through the real endpoint.
    let reconnect_daemon = start_daemon_with_provider(
        flaky_home.path(),
        Some(&flaky_db),
        &flaky_provider.base_url(),
        PROVIDER_MODEL,
    );
    let mut http_pages_newest_first: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut http_cursor: u64 = (high_water + 1) as u64;
    loop {
        let page = common::get_json_or_panic(
            &reconnect_daemon.url(),
            &format!("/sessions/{flaky_session}/events/history?before={http_cursor}&limit=2"),
            reconnect_daemon.token(),
        );
        let rows = page
            .as_array()
            .expect("events/history must return an array")
            .clone();
        if rows.is_empty() {
            break;
        }
        let lowest = rows
            .iter()
            .filter_map(|row| row.get("seq").and_then(serde_json::Value::as_u64))
            .min()
            .expect("every durable event carries a seq");
        assert!(
            lowest < http_cursor,
            "a backward page must strictly precede its cursor, else paging cannot terminate"
        );
        http_cursor = lowest;
        http_pages_newest_first.push(rows);
    }
    let mut http_seqs: Vec<u64> = Vec::new();
    for page in http_pages_newest_first.into_iter().rev() {
        for row in page {
            if let Some(seq) = row.get("seq").and_then(serde_json::Value::as_u64) {
                http_seqs.push(seq);
            }
        }
    }
    let durable_seqs: Vec<u64> = all_rows_ascending
        .iter()
        .map(|row| row.seq as u64)
        .collect();
    assert_eq!(
        http_seqs, durable_seqs,
        "backward paging through GET /sessions/{{id}}/events/history must reconstruct \
         the exact durable ordering held in the independently reopened SQLite"
    );

    assert_eq!(
        paged_ascending, all_rows_ascending,
        "paged reconstruction must equal the full durable log exactly, not just its seqs"
    );

    // Final transcript and terminal status must equal the uninterrupted
    // reference run.
    let reference_events_decoded: Vec<ProtocolEvent> = reference_rows
        .iter()
        .filter_map(|row| serde_json::from_str::<ProtocolEvent>(&row.payload_json).ok())
        .filter(|event| event.is_durable_fact())
        .collect();
    let (reference_terminal_status, _reference_terminal_turn_id) =
        common::terminal_status(&reference_events_decoded).unwrap();
    assert_eq!(
        flaky_terminal_status, reference_terminal_status,
        "reconnected run's terminal status must equal the uninterrupted reference"
    );

    let reference_transcript = common::transcript_signature(&reference_events_decoded);
    let flaky_transcript = common::transcript_signature(&durable_events_decoded);
    assert_eq!(
        reference_transcript, flaky_transcript,
        "reconnected run's final transcript must equal the uninterrupted reference"
    );

    // EVT-002 keeps live deltas outside durable storage; subscriber receipt
    // above proves the run streamed them before disconnect.
    assert!(
        flaky_rows
            .iter()
            .all(|row| common::decode_kind(row).as_deref() != Some("assistantDelta")),
        "live deltas must not be persisted in durable replay truth"
    );
}

/// EVT-004 regression: paging termination must NOT swallow a genuinely
/// unknown session.
///
/// `Storage::events_before` runs an unconditional `WHERE session_id = ?` with
/// no existence check, so an id that never existed yields zero rows exactly
/// like a session paged past the start of its history. An earlier fix for the
/// termination case returned an empty 200 for BOTH, turning "no such session"
/// into a silent success. Both behaviours are pinned here.
#[test]
fn unknown_session_still_404s_even_when_paging_backwards() {
    let provider = common::MockProvider::start(vec!["hi".to_string()]);
    let home = tempfile::tempdir().expect("home");
    let daemon =
        common::start_daemon_with_provider(home.path(), None, &provider.base_url(), PROVIDER_MODEL);

    // A session id that was never created, WITH a before cursor - the exact
    // shape that regressed.
    let response = crate::support::daemon_process::send(
        &rightkit_http::Request::get(format!(
            "{}/sessions/evt004-never-existed/events/history?before=100&limit=2",
            daemon.url()
        ))
        .bearer(daemon.token()),
    )
    .expect("history request");
    assert_eq!(
        response.status, 404,
        "an unknown session must still 404 when a before cursor is supplied, \
         not report an empty page as success"
    );

    // And the same id without a cursor must behave identically.
    let response = crate::support::daemon_process::send(
        &rightkit_http::Request::get(format!(
            "{}/sessions/evt004-never-existed/events/history",
            daemon.url()
        ))
        .bearer(daemon.token()),
    )
    .expect("history request");
    assert_eq!(response.status, 404);
}
