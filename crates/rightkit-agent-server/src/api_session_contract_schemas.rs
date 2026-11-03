use super::*;

pub(crate) fn create_session_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/CreateSessionRequest"})
}

pub(crate) fn fork_session_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ForkSessionRequest"})
}

pub(crate) fn patch_session_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/PatchSessionRequest"})
}

pub(crate) fn bind_session_spec_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/BindSessionSpecRequest"})
}

pub(crate) fn post_message_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/PostMessageRequest"})
}

pub(crate) fn compact_session_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/CompactSessionRequest"})
}

pub(crate) fn resolve_tool_approval_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ResolveToolApprovalRequest"})
}

pub(crate) fn answer_question_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/AnswerQuestionRequest"})
}

pub(crate) fn resolve_plan_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ResolvePlanRequest"})
}

pub(crate) fn steer_session_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SteerSessionRequest"})
}

pub(crate) fn set_goal_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SetGoalRequest"})
}

pub(crate) fn edit_goal_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/EditGoalRequest"})
}

pub(crate) fn orchestrate_session_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/OrchestrateSessionRequest"})
}

pub(crate) fn session_row_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SessionRow"})
}

pub(crate) fn session_rows_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/SessionRow"}})
}

pub(crate) fn delete_session_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/DeleteSessionResponse"})
}

pub(crate) fn session_spec_completion_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SessionSpecCompletionResponse"})
}

pub(crate) fn transcript_messages_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/TranscriptMessage"}})
}

pub(crate) fn untrusted_content_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/UntrustedContentResponse"})
}

pub(crate) fn away_summary_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/AwaySummaryResponse"})
}

pub(crate) fn post_message_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/PostMessageResponse"})
}

pub(crate) fn fork_session_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ForkSessionResponse"})
}

pub(crate) fn event_stream_response_schema() -> Value {
    json!({"type": "string", "description": "Server-sent event stream carrying serialized ProtocolEvent JSON objects."})
}

pub(crate) fn protocol_events_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/ProtocolEvent"}})
}

pub(crate) fn compact_session_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/CompactSessionResponse"})
}

pub(crate) fn resolve_tool_approval_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ResolveToolApprovalResponse"})
}

pub(crate) fn answer_question_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/AnswerQuestionResponse"})
}

pub(crate) fn resolve_plan_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ResolvePlanResponse"})
}

pub(crate) fn resume_session_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ResumeSessionResponse"})
}

pub(crate) fn interrupt_session_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/InterruptSessionResponse"})
}

pub(crate) fn steer_session_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SteerSessionResponse"})
}

pub(crate) fn set_goal_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SetGoalResponse"})
}

pub(crate) fn get_goal_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/GetGoalResponse"})
}

pub(crate) fn clear_goal_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ClearGoalResponse"})
}

pub(crate) fn pause_goal_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/PauseGoalResponse"})
}

pub(crate) fn resume_goal_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ResumeGoalResponse"})
}

pub(crate) fn edit_goal_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/EditGoalResponse"})
}

pub(crate) fn rewind_session_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RewindSessionResponse"})
}

pub(crate) fn orchestrate_session_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/OrchestrateSessionResponse"})
}

pub(crate) fn artifact_rows_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/ArtifactRow"}})
}

pub(crate) fn task_nodes_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/TaskNode"}})
}

pub(crate) fn instruction_status_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/InstructionStatus"})
}
