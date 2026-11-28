//! `GET /sessions/{id}/transcript/steps`: what each tool step was and how long it took.
//!
//! The native engine stores a tool-free final assistant message, and a tool result row holds only
//! the call id, so the transcript rows alone cannot say what a step was. The stored events can:
//! `toolCallStarted` carries the call (name and arguments) and `toolResultAdded` the result, each
//! with a time. This route folds those events, so it reads only the engine's own store and works
//! the same for a live session, one loaded from storage and one reached through a remote engine.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use rightkit_agent_engine_protocol::tool_step::{
    is_shell_tool, step_command, step_duration_ms, step_exit_code, tool_step_kind, tool_step_label,
};
use rightkit_agent_engine_protocol::{ProtocolEvent, TranscriptMessage};
use rightkit_agent_engine_storage::EventRow;
use serde_json::{json, Map, Value};

use crate::api::DaemonState;

struct Step {
    turn_index: usize,
    tool_call_id: String,
    tool_name: String,
    label: String,
    kind: &'static str,
    started_at: String,
    ended_at: Option<String>,
    duration_ms: Option<i64>,
    is_error: bool,
    command: Option<String>,
    exit_code: Option<i32>,
    /// What a test command's own summary line said, when the step ran one and printed one.
    tests: Option<super::test_summary::TestSummary>,
}

/// Fold stored events (in seq order) into one step per tool call, in the order the calls started.
/// A user turn is counted for every stored user message; a call belongs to the turn it started in.
fn fold_steps(events: &[EventRow]) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let mut turn_index = 0usize;
    let mut user_turns = 0usize;
    for row in events {
        let Ok(mut event) = serde_json::from_str::<ProtocolEvent>(&row.payload_json) else {
            continue;
        };
        if event.session_id() != row.session_id || !event.is_durable_fact() {
            continue;
        }
        event.canonicalize_identity(row.seq);
        match event {
            ProtocolEvent::UserMessageSubmitted { .. } => {
                turn_index = user_turns;
                user_turns += 1;
            }
            ProtocolEvent::ToolCallStarted {
                created_at, call, ..
            } => {
                if steps.iter().any(|step| step.tool_call_id == call.id) {
                    continue;
                }
                steps.push(Step {
                    turn_index,
                    label: tool_step_label(&call.name, &call.arguments_json),
                    kind: tool_step_kind(&call.name),
                    command: step_command(&call.name, &call.arguments_json),
                    tool_call_id: call.id,
                    tool_name: call.name,
                    started_at: created_at,
                    ended_at: None,
                    duration_ms: None,
                    is_error: false,
                    exit_code: None,
                    tests: None,
                });
            }
            ProtocolEvent::ToolResultAdded {
                result:
                    TranscriptMessage::Tool {
                        tool_call_id,
                        created_at,
                        summary,
                        is_error,
                        ..
                    },
                ..
            } => {
                if let Some(step) = steps
                    .iter_mut()
                    .rev()
                    .find(|step| step.tool_call_id == tool_call_id && step.ended_at.is_none())
                {
                    step.duration_ms = step_duration_ms(&step.started_at, &created_at);
                    step.is_error = is_error;
                    if is_shell_tool(&step.tool_name) {
                        step.exit_code = step_exit_code(&step.tool_name, &summary, is_error);
                        if let Some(command) = step.command.as_deref() {
                            step.tests = super::test_summary::parse_test_summary(command, &summary);
                        }
                    }
                    step.ended_at = Some(created_at);
                }
            }
            _ => {}
        }
    }
    steps
}

fn step_json(step: &Step) -> Value {
    let mut row = Map::new();
    row.insert("toolCallId".into(), json!(step.tool_call_id));
    row.insert("toolName".into(), json!(step.tool_name));
    row.insert("label".into(), json!(step.label));
    row.insert("kind".into(), json!(step.kind));
    row.insert("turnIndex".into(), json!(step.turn_index));
    row.insert("startedAt".into(), json!(step.started_at));
    row.insert("endedAt".into(), json!(step.ended_at));
    row.insert("durationMs".into(), json!(step.duration_ms));
    row.insert("isError".into(), json!(step.is_error));
    if let Some(command) = &step.command {
        row.insert("command".into(), json!(command));
    }
    if let Some(code) = step.exit_code {
        row.insert("exitCode".into(), json!(code));
    }
    if let Some(tests) = &step.tests {
        row.insert("tests".into(), json!(tests));
    }
    Value::Object(row)
}

/// One roll-up per user turn that ran at least one tool, in turn order.
fn turn_rollups(steps: &[Step]) -> Vec<Value> {
    let mut turns: Vec<usize> = steps.iter().map(|step| step.turn_index).collect();
    turns.dedup();
    turns
        .into_iter()
        .map(|turn| {
            let own: Vec<&Step> = steps
                .iter()
                .filter(|step| step.turn_index == turn)
                .collect();
            let mut kinds: std::collections::BTreeMap<&str, u64> = Default::default();
            for step in &own {
                *kinds.entry(step.kind).or_default() += 1;
            }
            json!({
                "turnIndex": turn,
                "steps": own.len(),
                "failed": own.iter().filter(|step| step.is_error).count(),
                "durationMs": own.iter().filter_map(|step| step.duration_ms).sum::<i64>(),
                "kinds": kinds,
                // The last test run of the turn, as its own summary line reported it; `null`
                // when the turn ran no test command or none printed a summary.
                "tests": own.iter().rev().find_map(|step| step.tests.as_ref()),
            })
        })
        .collect()
}

pub(crate) fn build_steps_response(session_id: &str, events: &[EventRow]) -> Value {
    let steps = fold_steps(events);
    json!({
        "sessionId": session_id,
        "steps": steps.iter().map(step_json).collect::<Vec<_>>(),
        "turns": turn_rollups(&steps),
    })
}

pub(crate) async fn get_transcript_steps(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let live = state.get_session(&id).is_some();
    let Some(storage) = state.storage.as_ref() else {
        // No store: a live session has no stored calls to report, anything else is unknown.
        return if live {
            Ok(Json(build_steps_response(&id, &[])))
        } else {
            Err(StatusCode::NOT_FOUND)
        };
    };
    let storage = storage.clone();
    let id2 = id.clone();
    let events = tokio::task::spawn_blocking(move || storage.blocking_lock().all_events(&id2))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if events.is_empty() && !live {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(build_steps_response(&id, &events)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seq: i64, payload: Value) -> EventRow {
        EventRow {
            session_id: "s1".into(),
            seq,
            kind: String::new(),
            payload_json: payload.to_string(),
            created_at: String::new(),
        }
    }

    fn started(seq: i64, at: &str, id: &str, name: &str, args: &str) -> EventRow {
        row(
            seq,
            json!({"$kind":"toolCallStarted","sessionId":"s1","seq":seq,"createdAt":at,
                   "call":{"id":id,"name":name,"argumentsJson":args}}),
        )
    }

    fn result(seq: i64, at: &str, id: &str, summary: &str, is_error: bool) -> EventRow {
        row(
            seq,
            json!({"$kind":"toolResultAdded","sessionId":"s1","seq":seq,"createdAt":at,
                   "result":{"$kind":"tool","id":format!("tool-{seq}"),"createdAt":at,
                             "toolCallId":id,"summary":summary,"isError":is_error,"pinned":false}}),
        )
    }

    #[test]
    fn steps_carry_name_label_times_and_exit_code() {
        let events = vec![
            started(
                1,
                "2026-11-12T10:00:00Z",
                "c1",
                "read_file",
                r#"{"path":"a.txt"}"#,
            ),
            result(2, "2026-11-12T10:00:01Z", "c1", "ok", false),
            started(
                3,
                "2026-11-12T10:00:02Z",
                "c2",
                "shell",
                r#"{"command":"false"}"#,
            ),
            result(4, "2026-11-12T10:00:04Z", "c2", "[exit 1]\n", true),
        ];
        let response = build_steps_response("s1", &events);
        let steps = response["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["toolName"], "read_file");
        assert_eq!(steps[0]["label"], "Read a.txt");
        assert_eq!(steps[0]["durationMs"], 1000);
        assert_eq!(steps[1]["exitCode"], 1);
        assert_eq!(steps[1]["isError"], true);
        assert_eq!(steps[1]["command"], "false");
        let turn = &response["turns"][0];
        assert_eq!(turn["steps"], 2);
        assert_eq!(turn["failed"], 1);
        assert_eq!(turn["durationMs"], 3000);
        assert_eq!(turn["kinds"]["read"], 1);
        assert_eq!(turn["kinds"]["run"], 1);
    }

    #[test]
    fn a_call_without_a_result_has_no_end() {
        let events = vec![started(
            1,
            "2026-11-12T10:00:00Z",
            "c1",
            "search",
            r#"{"pattern":"x"}"#,
        )];
        let response = build_steps_response("s1", &events);
        assert_eq!(response["steps"][0]["endedAt"], Value::Null);
        assert_eq!(response["steps"][0]["durationMs"], Value::Null);
        assert_eq!(response["steps"][0]["label"], "Searched x");
    }
}
