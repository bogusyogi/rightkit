use super::*;

pub(crate) fn session_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/sessions") => Some(create_session_request_schema()),
        ("patch", "/sessions/{id}") => Some(patch_session_request_schema()),
        ("post", "/sessions/{id}/spec") => Some(bind_session_spec_request_schema()),
        ("post", "/sessions/{id}/messages") => Some(post_message_request_schema()),
        ("post", "/sessions/{id}/fork") => Some(fork_session_request_schema()),
        ("post", "/sessions/{id}/clone") => Some(fork_session_request_schema()),
        ("post", "/sessions/{id}/compact") => Some(compact_session_request_schema()),
        ("post", "/sessions/{id}/context-fold") => {
            Some(crate::api_context_folds::fold_request_schema())
        }
        ("post", "/sessions/{id}/tool-approval") => Some(resolve_tool_approval_request_schema()),
        ("post", "/sessions/{id}/answer-question") => Some(answer_question_request_schema()),
        ("post", "/sessions/{id}/resolve-plan") => Some(resolve_plan_request_schema()),
        ("post", "/sessions/{id}/steer") => Some(steer_session_request_schema()),
        ("post", "/sessions/{id}/goal") => Some(set_goal_request_schema()),
        ("post", "/sessions/{id}/goal/edit") => Some(edit_goal_request_schema()),
        ("post", "/sessions/{id}/orchestrate") => Some(orchestrate_session_request_schema()),
        _ => None,
    }
}

pub(crate) fn session_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/sessions") => Some(session_rows_response_schema()),
        ("get", "/sessions/{id}/goal") => Some(get_goal_response_schema()),
        ("post", "/sessions") => Some(session_row_response_schema()),
        ("get", "/sessions/search") => Some(session_rows_response_schema()),
        ("delete", "/sessions/{id}") => Some(delete_session_response_schema()),
        ("patch", "/sessions/{id}") => Some(session_row_response_schema()),
        ("post", "/sessions/{id}/spec") => Some(session_row_response_schema()),
        ("delete", "/sessions/{id}/spec") => Some(session_row_response_schema()),
        ("get", "/sessions/{id}/spec/completion") => {
            Some(session_spec_completion_response_schema())
        }
        ("get", "/sessions/{id}/transcript") => Some(transcript_messages_response_schema()),
        ("get", "/sessions/{id}/untrusted-content") => Some(untrusted_content_response_schema()),
        ("get", "/sessions/{id}/away-summary") => Some(away_summary_response_schema()),
        ("post", "/sessions/{id}/messages") => Some(post_message_response_schema()),
        ("post", "/sessions/{id}/fork") => Some(fork_session_response_schema()),
        ("post", "/sessions/{id}/clone") => Some(json!({
            "type":"object", "required":["id","sourceSessionId","title","workspaceRoot","status","copiedEvents","session"],
            "properties": {"id":{"type":"string"},"sourceSessionId":{"type":"string"},"title":{"type":"string"},"workspaceRoot":{"type":"string"},"status":{"type":"string"},"copiedEvents":{"type":"integer","minimum":0},"session":{"type":"object"}}
        })),
        ("get", "/sessions/{id}/export") => Some(json!({
            "type":"object","required":["sessionId","format","content"],
            "properties":{"sessionId":{"type":"string"},"format":{"type":"string","enum":["markdown","html"]},"content":{"type":"string"}}
        })),
        ("get", "/sessions/{id}/rewind/points") => Some(json!({
            "type":"array","items":{"type":"object","required":["text","nthFromEnd","createdAt"],
                "properties":{"text":{"type":"string"},"nthFromEnd":{"type":"integer","minimum":0},"createdAt":{"type":"string"}}}
        })),
        ("get", "/sessions/{id}/events") => Some(event_stream_response_schema()),
        ("get", "/sessions/{id}/events/history") => Some(protocol_events_response_schema()),
        ("get", "/sessions/{id}/handoff") => {
            Some(crate::api_session_handoff::session_handoff_response_schema())
        }
        ("post", "/sessions/{id}/compact") => Some(compact_session_response_schema()),
        ("post", "/sessions/{id}/context-fold") => {
            Some(crate::api_context_folds::fold_response_schema())
        }
        ("get", "/sessions/{id}/context-folds/{archive_ref}") => {
            Some(crate::api_context_folds::fold_archive_response_schema())
        }
        ("post", "/sessions/{id}/tool-approval") => Some(resolve_tool_approval_response_schema()),
        ("post", "/sessions/{id}/answer-question") => Some(answer_question_response_schema()),
        ("post", "/sessions/{id}/resolve-plan") => Some(resolve_plan_response_schema()),
        ("post", "/sessions/{id}/resume") => Some(resume_session_response_schema()),
        ("post", "/sessions/{id}/interrupt") => Some(interrupt_session_response_schema()),
        ("post", "/sessions/{id}/steer") => Some(steer_session_response_schema()),
        ("post", "/sessions/{id}/goal") => Some(set_goal_response_schema()),
        ("delete", "/sessions/{id}/goal") => Some(clear_goal_response_schema()),
        ("post", "/sessions/{id}/goal/pause") => Some(pause_goal_response_schema()),
        ("post", "/sessions/{id}/goal/resume") => Some(resume_goal_response_schema()),
        ("post", "/sessions/{id}/goal/edit") => Some(edit_goal_response_schema()),
        ("post", "/sessions/{id}/rewind") => Some(rewind_session_response_schema()),
        ("post", "/sessions/{id}/orchestrate") => Some(orchestrate_session_response_schema()),
        ("get", "/sessions/{id}/runtime/artifacts") => Some(artifact_rows_response_schema()),
        ("get", "/sessions/{id}/runtime/taskgraph") => Some(task_nodes_response_schema()),
        ("get", "/instructions") => Some(instruction_status_response_schema()),
        ("get", "/tasks") => Some(task_nodes_response_schema()),
        _ => None,
    }
}
