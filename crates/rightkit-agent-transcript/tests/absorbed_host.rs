//! Absorbed-stream rows from an agent server hosted by an app other than CodeRight: the
//! exporter writes `<app>_event_v1` rows naming the app (`rightkit-agent-server`
//! `native_transcript_exporter.rs`), and they normalize for that session's host.

use rightkit_agent_transcript::adapters::{host_event_type, iter_events_for_host};
use rightkit_agent_transcript::{parse_absorbed_rows, parse_absorbed_rows_for_host};
use serde_json::{json, Value};

/// One exported row as the agent server's exporter writes it for `host`.
fn row(host: &str, session: &str, seq: u64, kind: &str, text: &str) -> (u64, Value) {
    let event_id = format!("evt-v1:{session}:{seq}");
    let value = json!({
        "type": format!("{host}_event_v1"),
        "host": host,
        "sessionId": session,
        "sourceSeq": seq,
        "eventId": event_id,
        "occurredAt": "2026-11-12T10:00:00Z",
        "scopeId": "scope",
        "provenance": {
            "source": host,
            "producer": format!("{host}-daemon"),
            "sessionId": session,
            "sourceSeq": seq,
            "eventId": event_id,
        },
        "event": {
            "kind": kind,
            "role": if kind == "user_message" { "user" } else { "assistant" },
            "timestamp": "2026-11-12T10:00:00Z",
            "text": text,
            "sessionId": session,
        },
    });
    (seq, value)
}

#[test]
fn a_mailright_stream_normalizes_for_its_own_host() {
    let rows = vec![
        row(
            "mailright",
            "s1",
            1,
            "user_message",
            "Draft a reply to the invoice thread",
        ),
        row(
            "mailright",
            "s1",
            2,
            "assistant_message",
            "Here is a draft reply.",
        ),
    ];
    let events = parse_absorbed_rows_for_host("mailright", "s1", "digest-1", &rows);
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(event.host, "mailright");
        assert_eq!(event.session_id, "s1");
        assert_eq!(event.parser_digest, "digest-1");
    }
    assert_eq!(events[0].kind, "user_message");
    assert_eq!(events[0].row_index, 1);
    assert_eq!(events[1].kind, "assistant_message");
}

#[test]
fn rows_of_another_host_yield_nothing() {
    let rows = vec![row("mailright", "s1", 1, "user_message", "Draft a reply")];
    // The CodeRight reader, and a session of a third host, drop MailRight's rows.
    assert!(parse_absorbed_rows("s1", "d", &rows).is_empty());
    assert!(parse_absorbed_rows_for_host("postright", "s1", "d", &rows).is_empty());
    // A row that names one host in its type and another in `host` is not read.
    let mut forged = row("mailright", "s1", 1, "user_message", "Draft a reply").1;
    forged["host"] = json!("postright");
    assert!(iter_events_for_host("mailright", &forged).is_empty());
}

#[test]
fn coderight_rows_still_read_as_before() {
    let rows = vec![row(
        "coderight",
        "s2",
        1,
        "user_message",
        "Fix the failing test",
    )];
    let old = parse_absorbed_rows("s2", "d", &rows);
    let new = parse_absorbed_rows_for_host("coderight", "s2", "d", &rows);
    assert_eq!(old.len(), 1);
    assert_eq!(old, new);
    assert_eq!(old[0].host, "coderight");
}

#[test]
fn only_plain_host_names_make_a_row_type() {
    assert_eq!(
        host_event_type("mailright").as_deref(),
        Some("mailright_event_v1")
    );
    assert_eq!(host_event_type(""), None);
    assert_eq!(host_event_type("Mail Right"), None);
}
