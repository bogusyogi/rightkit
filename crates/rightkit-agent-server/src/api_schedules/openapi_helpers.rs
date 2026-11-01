pub(crate) fn schedule_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method.to_ascii_lowercase().as_str(), path) {
        ("post", "/schedules") => Some(create_schedule_request_schema()),
        ("patch", "/schedules/{id}") => Some(patch_schedule_request_schema()),
        ("put", "/schedules/{id}/time-zone") => Some(update_schedule_timezone_request_schema()),
        ("post", "/schedules/{id}/run")
        | ("post", "/schedules/{id}/pause")
        | ("post", "/schedules/{id}/resume") => Some(schedule_action_request_schema()),
        ("post", "/bots") | ("patch", "/bots/{id}") => Some(bot_profile_schema()),
        ("post", "/bots/import") => Some(authority_free_bot_package_schema()),
        _ => None,
    }
}

pub(crate) fn schedule_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method.to_ascii_lowercase().as_str(), path) {
        ("get", "/schedules") => Some(list_schedules_response_schema()),
        ("get", "/schedules/audit") => Some(schedule_admin_audit_response_schema()),
        ("post", "/schedules") => Some(create_schedule_response_schema()),
        ("patch", "/schedules/{id}") => Some(patch_schedule_response_schema()),
        ("delete", "/schedules/{id}") => Some(delete_schedule_response_schema()),
        ("get", "/schedules/{id}/runs") => Some(schedule_runs_response_schema()),
        ("put", "/schedules/{id}/time-zone") => Some(update_schedule_timezone_response_schema()),
        ("post", "/schedules/{id}/run") => Some(run_schedule_now_response_schema()),
        ("post", "/schedules/{id}/pause") => Some(pause_schedule_response_schema()),
        ("post", "/schedules/{id}/resume") => Some(resume_schedule_response_schema()),
        ("get", "/bots") => Some(list_bots_response_schema()),
        ("post", "/bots")
        | ("post", "/bots/import")
        | ("get", "/bots/{id}")
        | ("patch", "/bots/{id}") => Some(bot_profile_schema()),
        ("post", "/bots/{id}/run") => Some(run_bot_response_schema()),
        ("get", "/bots/{id}/export") => Some(authority_free_bot_package_schema()),
        _ => None,
    }
}

pub(crate) fn list_schedules_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ListSchedulesResponse"})
}

pub(crate) fn schedule_admin_audit_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleAdminAuditResponse"})
}

pub(crate) fn create_schedule_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/CreateScheduleRequest"})
}

pub(crate) fn create_schedule_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleMutationResponse"})
}

pub(crate) fn patch_schedule_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/PatchScheduleRequest"})
}

pub(crate) fn patch_schedule_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleMutationResponse"})
}

pub(crate) fn delete_schedule_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/DeleteScheduleResponse"})
}

pub(crate) fn schedule_runs_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleRunsResponse"})
}

pub(crate) fn update_schedule_timezone_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/UpdateScheduleTimeZoneRequest"})
}

pub(crate) fn schedule_action_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleActionRequest"})
}

pub(crate) fn update_schedule_timezone_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleMutationResponse"})
}

pub(crate) fn run_schedule_now_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RunScheduleNowResponse"})
}

pub(crate) fn pause_schedule_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleMutationResponse"})
}

pub(crate) fn resume_schedule_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleMutationResponse"})
}

pub(crate) fn bot_profile_schema() -> Value {
    json!({"$ref": "#/components/schemas/BotProfileV1"})
}

pub(crate) fn list_bots_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ListBotsResponse"})
}

pub(crate) fn authority_free_bot_package_schema() -> Value {
    json!({"$ref": "#/components/schemas/AuthorityFreeBotPackage"})
}

pub(crate) fn run_bot_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RunBotResponse"})
}
