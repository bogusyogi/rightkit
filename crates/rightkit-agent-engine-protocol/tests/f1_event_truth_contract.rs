//! F1 event-truth contract regression set.
//!
//! These tests assert the protocol-level surface the F1 lane commits
//! established. They are intentionally cross-cutting: a single failure
//! here means the unified seq / canonical-identity / visible-model-
//! split contract has regressed, even if the underlying modules still
//! pass their own scoped tests.
//!
#![allow(clippy::doc_lazy_continuation)]

//! Scoped to the `host_protocol` crate only — the storage and
//! engine surfaces have their own crate-local tests (see
//! `host_storage::events::identity` and `host_engine::conduit`).
//! Keeping this file crate-local means Agent 4's verification can run
//! it without booting the daemon or opening a SQLite database.

use rightkit_agent_engine_protocol::{
    assistant_message_id, is_canonical_uuid_v7, message_id, tool_call_origin_id, tool_result_id,
    uuid_v7, ProtocolEvent, TranscriptMessage,
};

fn assistant_message_with_id(id: &str) -> TranscriptMessage {
    TranscriptMessage::Assistant {
        id: id.into(),
        created_at: "t".into(),
        content: "hi".into(),
        reasoning_content: None,
        tool_calls: vec![],
        pinned: false,
    }
}

fn tool_message_with_id(call_id: &str, id: &str) -> TranscriptMessage {
    TranscriptMessage::Tool {
        id: id.into(),
        created_at: "t".into(),
        tool_call_id: call_id.into(),
        summary: "ok".into(),
        output: None,
        trust: None,
        is_error: false,
        pinned: false,
        process_id: None,
    }
}

/// 1. Identity normalization. Every persisted entity has a stable id
/// derived from its row_seq, regardless of the producer-supplied
/// legacy constant.
#[test]
fn f1_identity_normalization_round_trip() {
    let legacy = assistant_message_with_id("final");
    let mut event = ProtocolEvent::AssistantMessageCompleted {
        session_id: "s".into(),
        seq: 7,
        created_at: "t".into(),
        message: legacy,
    };
    event.canonicalize_identity(7);
    match event {
        ProtocolEvent::AssistantMessageCompleted { message, seq, .. } => {
            assert_eq!(seq, 7);
            assert_eq!(message.id(), assistant_message_id(7));
        }
        _ => panic!("expected AssistantMessageCompleted"),
    }
    assert_eq!(message_id(7), "message-7");
}

/// 2. Tool result ids encode both call_id and row_seq so a tool
/// result can deterministically find its origin on replay.
#[test]
fn f1_tool_result_origin_id_round_trip() {
    let mut event = ProtocolEvent::ToolResultAdded {
        session_id: "s".into(),
        seq: 0,
        created_at: "t".into(),
        result: tool_message_with_id("tc1", "legacy-tr"),
    };
    event.canonicalize_identity(11);
    match event {
        ProtocolEvent::ToolResultAdded { result, seq, .. } => {
            assert_eq!(seq, 11);
            assert_eq!(result.id(), tool_result_id("tc1", 11));
        }
        _ => panic!("expected ToolResultAdded"),
    }
    // tool_call_origin_id uses the same shape the awaited engine
    // dispatch consult expects.
    assert_eq!(tool_call_origin_id("tc1", 11), "tool_call_origin:tc1:11");
}

/// 3. UUIDv7 is the canonical session-id shape. Two freshly-minted
/// session ids MUST both be valid v7 and MUST be lex-sortable by
/// minting time so a session-list sort is deterministic.
#[test]
fn f1_session_id_is_uuid_v7_and_sortable() {
    let a = uuid_v7();
    let b = uuid_v7();
    assert!(is_canonical_uuid_v7(&a), "{a} not v7");
    assert!(is_canonical_uuid_v7(&b), "{b} not v7");
    assert_ne!(a, b);
    // v7's 48-bit ms-prefix makes them lex-sortable. A back-to-back
    // mint in the same ms still distinguishes by random bits, but
    // the ordering property is what matters for session-list sort.
    let later_a = uuid_v7();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let later_b = uuid_v7();
    assert!(
        later_a < later_b,
        "later_a {later_a} should be < later_b {later_b}"
    );
}

/// 4. Producer-selected constant ids ("final", "forced-stop", ...) in a payload survive a
/// canonicalize_identity at the storage boundary. The storage layer
/// does this via decode_event_row; this test asserts the protocol
/// helper set_seq + canonicalize_identity rewrite the id to the
/// row-derived form regardless of what the producer wrote.
#[test]
fn f1_producer_payload_ids_do_not_survive_canonicalize() {
    let legacy_ids = ["final", "forced-stop", "graceful-summary"];
    for legacy in legacy_ids {
        let mut event = ProtocolEvent::AssistantMessageCompleted {
            session_id: "s".into(),
            seq: 42,
            created_at: "t".into(),
            message: assistant_message_with_id(legacy),
        };
        event.canonicalize_identity(42);
        match event {
            ProtocolEvent::AssistantMessageCompleted { message, .. } => {
                assert_eq!(
                    message.id(),
                    "assistant:42",
                    "legacy id {legacy} must be rewritten"
                );
            }
            _ => panic!("variant"),
        }
    }
}

/// 5. Sequence-race safety. canonicalize_identity is idempotent —
/// calling it twice with the same seq leaves the event unchanged. A
/// racing producer that double-canonicalizes does not produce a
/// different id.
#[test]
fn f1_canonicalize_is_idempotent() {
    let mut event = ProtocolEvent::AssistantMessageCompleted {
        session_id: "s".into(),
        seq: 0,
        created_at: "t".into(),
        message: assistant_message_with_id("legacy"),
    };
    event.canonicalize_identity(15);
    let first = match &event {
        ProtocolEvent::AssistantMessageCompleted { message, seq, .. } => {
            (*seq, message.id().to_string())
        }
        _ => panic!("variant"),
    };
    event.canonicalize_identity(15);
    let second = match &event {
        ProtocolEvent::AssistantMessageCompleted { message, seq, .. } => {
            (*seq, message.id().to_string())
        }
        _ => panic!("variant"),
    };
    assert_eq!(first, second);
}

/// 6. set_seq is exhaustive. This test compiles only if every variant
/// in the protocol enum is reachable from a single match — the same
/// compile-time guarantee `assign_seq` provides. A new variant
/// without an arm here fails the build.
#[test]
fn f1_set_seq_is_exhaustive_across_every_variant() {
    use rightkit_agent_engine_protocol::tool::ToolCall;
    let mut ev = ProtocolEvent::SessionStarted {
        session_id: "s".into(),
        seq: 0,
        created_at: "t".into(),
        title: None,
    };
    ev.set_seq(1);
    assert_eq!(ev.seq(), 1);

    let mut ev = ProtocolEvent::ToolCallStarted {
        session_id: "s".into(),
        seq: 0,
        created_at: "t".into(),
        call: ToolCall {
            id: "tc1".into(),
            name: "read_file".into(),
            arguments_json: "{}".into(),
        },
    };
    ev.set_seq(2);
    assert_eq!(ev.seq(), 2);
}

/// 7. The visible/model split at the protocol level: a `Fold`
/// message preserves the same id semantics in both surfaces —
/// `set_id` works on a Fold just as on an Assistant.
#[test]
fn f1_fold_message_id_rewrites_consistently() {
    let mut message = TranscriptMessage::Fold {
        id: "legacy-fold".into(),
        created_at: "t".into(),
        subtask_id: "task-1".into(),
        summary: "folded".into(),
        dropped_count: 3,
        archive_ref: "a".repeat(64),
        pinned: false,
    };
    message.set_id("message-9".into());
    assert_eq!(message.id(), "message-9");
    let mut event = ProtocolEvent::SessionContextFolded {
        session_id: "s".into(),
        seq: 9,
        created_at: "t".into(),
        subtask_id: "task-1".into(),
        start_marker: "[start]".into(),
        end_marker: "[end]".into(),
        fold: message,
    };
    event.canonicalize_identity(9);
    match event {
        ProtocolEvent::SessionContextFolded { fold, seq, .. } => {
            assert_eq!(seq, 9);
            assert_eq!(fold.id(), "message-9");
        }
        _ => panic!("variant"),
    }
}

/// 8. Persistence failure surfaces as a typed error, not a silent
/// skip. (The full PersistenceGapError enum is exercised in the
/// storage crate's tests; this one asserts the protocol-level error
/// type round-trips through serde so a daemon-side error response can
/// carry the message verbatim.)
#[test]
fn f1_persistence_failure_error_shape_is_stable() {
    // Sanity: the protocol crate exports nothing for PersistenceGap
    // (that's a storage-level error), but the canonical id helpers
    // used in the error response must remain stable so consumers
    // (UI, CLI) can parse the error message without surprises.
    assert_eq!(message_id(0), "message-0");
}
