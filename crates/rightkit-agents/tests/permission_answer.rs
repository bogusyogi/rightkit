#![cfg(feature = "claude")]

use rightkit_agents::claude::{
    answer_can_use_tool, parse_can_use_tool, CanUseTool, PermissionAnswer, PermissionDecision,
    NO_ANSWER_MESSAGE,
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

fn request() -> CanUseTool {
    parse_can_use_tool(&serde_json::json!({
        "request_id": "permission-1",
        "request": { "subtype": "can_use_tool", "tool_name": "Read", "input": { "path": "file.txt" } }
    })).unwrap()
}

fn denied() -> PermissionDecision {
    PermissionDecision::Deny {
        message: NO_ANSWER_MESSAGE.into(),
    }
}

#[test]
fn a_host_answer_keeps_original_input_or_explicit_denial() {
    let request = request();
    let (sender, receiver) = mpsc::channel();
    sender.send(PermissionAnswer::Allow).unwrap();
    assert_eq!(
        answer_can_use_tool(&request, Some(receiver), Duration::ZERO),
        PermissionDecision::Allow {
            updated_input: request.input.clone()
        }
    );
    let (sender, receiver) = mpsc::channel();
    sender
        .send(PermissionAnswer::Deny {
            message: Some("host refusal".into()),
        })
        .unwrap();
    assert_eq!(
        answer_can_use_tool(&request, Some(receiver), Duration::ZERO),
        PermissionDecision::Deny {
            message: "host refusal".into()
        }
    );
}

#[test]
fn no_receiver_or_disconnected_host_fails_closed_without_waiting_for_timeout() {
    let request = request();
    assert_eq!(
        answer_can_use_tool(&request, None, Duration::from_secs(10)),
        denied()
    );
    let (sender, receiver) = mpsc::channel();
    drop(sender);
    let started = Instant::now();
    assert_eq!(
        answer_can_use_tool(&request, Some(receiver), Duration::from_secs(10)),
        denied()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn timeout_denies_and_closes_only_this_requests_answer_channel() {
    let (sender, receiver) = mpsc::channel();
    let started = Instant::now();
    assert_eq!(
        answer_can_use_tool(&request(), Some(receiver), Duration::from_millis(20)),
        denied()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        sender.send(PermissionAnswer::Allow).is_err(),
        "late answer must be refused"
    );
    // Producer work is host-owned: no callback or producer thread is spawned here.
    let (sender, receiver) = mpsc::channel();
    sender.send(PermissionAnswer::Allow).unwrap();
    assert!(matches!(
        answer_can_use_tool(&request(), Some(receiver), Duration::ZERO),
        PermissionDecision::Allow { .. }
    ));
}
