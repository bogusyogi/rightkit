include!("openapi_components_learning.rs");
include!("openapi_components_skillopt.rs");
include!("openapi_components_observability.rs");

pub(crate) fn runtime_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    insert_runtime_learning_openapi_components(&mut schemas);
    insert_runtime_skillopt_openapi_components(&mut schemas);
    insert_runtime_observability_openapi_components(&mut schemas);
    schemas
}
fn runtime_schema_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

fn adapt_manifest_request_schema(adjudicate: bool) -> Value {
    let mut properties = json!({
        "manifest": {"type":"null"},
        "manifestValue": {"type":"null"},
    });
    let mut required = Vec::new();
    if adjudicate {
        properties["decisions"] = json!({"type":"string"});
        properties["validatedAt"] = json!({"type":"string"});
        required.extend(["decisions", "validatedAt"]);
    } else {
        properties["dryRun"] = json!({"type":"boolean","default":false});
    }
    let branches = [("manifest", "string"), ("manifestValue", "object")]
        .into_iter()
        .map(|(field, kind)| {
            let mut properties = properties.clone();
            properties[field] = json!({"type":kind});
            let mut required = required.clone();
            required.push(field);
            json!({"type":"object","required":required,"properties":properties})
        })
        .collect::<Vec<_>>();
    json!({"oneOf":branches})
}

pub(crate) fn runtime_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/runtime/context/adapt/review-taste") => Some(
            json!({"type":"object","required":["input","installationId","createdAt"],"properties":{"input":{"type":"string"},"installationId":{"type":"string"},"createdAt":{"type":"string"},"canonicalPoolSha256":{"type":["string","null"]}}}),
        ),
        ("post", "/runtime/context/adapt/adjudicate-taste") => {
            Some(adapt_manifest_request_schema(true))
        }
        ("post", "/runtime/context/adapt/apply-insights") => Some(
            json!({"type":"object","required":["input","installationId"],"properties":{"input":{"type":"string"},"installationId":{"type":"string"},"dryRun":{"type":"boolean","default":false}}}),
        ),
        ("post", "/runtime/context/adapt/insights") => Some(
            json!({"type":"object","additionalProperties":false,"properties":{"sessionIds":{"type":"array","maxItems":128,"items":{"type":"string","minLength":1}},"workspaceRoot":{"type":["string","null"]},"scope":{"type":"string","minLength":1,"default":"global"},"minRecurrence":{"type":"integer","minimum":1,"maximum":100,"default":2}}}),
        ),
        ("post", "/runtime/context/adapt/apply-taste") => {
            Some(adapt_manifest_request_schema(false))
        }
        ("post", "/runtime/context/adapt/recall") => Some(
            json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"k":{"type":"integer","minimum":0,"default":6},"scope":{"type":"string","default":"global"},"dimensions":{"type":"array","items":{"type":"string"}}}}),
        ),
        ("post", "/runtime/skillopt/runs") => Some(create_skillopt_run_request_schema()),
        ("post", "/runtime/skillopt/runs/{id}/resume") => {
            Some(resume_skillopt_run_request_schema())
        }
        ("post", "/runtime/skillopt/runs/{id}/execute") => {
            Some(execute_skillopt_epoch_request_schema())
        }
        ("post", "/runtime/skillopt/runs/{id}/propose-candidate") => {
            Some(propose_skillopt_candidate_request_schema())
        }
        ("post", "/runtime/skillopt/runs/{id}/cancel") => {
            Some(cancel_skillopt_run_request_schema())
        }
        ("post", "/runtime/harness-radar/proposals/{id}/promote") => {
            Some(promote_harness_radar_proposal_request_schema())
        }
        ("post", "/runtime/harness-radar/schedule") => {
            Some(schedule_harness_radar_refresh_request_schema())
        }
        _ => None,
    }
}

pub(crate) fn runtime_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/runtime/context/status") => Some(
            json!({"type":"object","required":["schemaVersion","state","contextDegraded","degradedReason"],"properties":{"schemaVersion":{"type":"string","enum":[crate::host_identity::schema("context-status.v1")]},"state":{"type":"string"},"contextDegraded":{"type":"boolean"},"degradedReason":{"type":["string","null"]},"reason":{"type":"string"},"source":{"type":"string"},"snapshot":{"type":"object"}}}),
        ),
        // Adapt payloads are SDK-owned JSON objects, forwarded without reshaping.
        ("get", "/runtime/context/adapt/status")
        | ("post", "/runtime/context/adapt/review-taste")
        | ("post", "/runtime/context/adapt/adjudicate-taste")
        | ("post", "/runtime/context/adapt/apply-insights")
        | ("post", "/runtime/context/adapt/insights")
        | ("post", "/runtime/context/adapt/apply-taste")
        | ("post", "/runtime/context/adapt/recall") => Some(
            json!({"type":"object","additionalProperties":true,"description":"Authenticated Adapt SDK response; payload shape is owned by Membrane."}),
        ),
        ("get", "/runtime/learning") => Some(runtime_learning_response_schema()),
        ("post", "/runtime/learning/{id}/approve") | ("post", "/runtime/learning/{id}/reject") => {
            Some(runtime_learning_decision_response_schema())
        }
        ("get", "/runtime/skillopt/runs") => Some(skillopt_runs_response_schema()),
        ("post", "/runtime/skillopt/runs") => Some(create_skillopt_run_response_schema()),
        ("get", "/runtime/skillopt/runs/{id}") => Some(skillopt_run_response_schema()),
        ("post", "/runtime/skillopt/runs/{id}/resume") => {
            Some(resume_skillopt_run_response_schema())
        }
        ("post", "/runtime/skillopt/runs/{id}/execute") => {
            Some(execute_skillopt_epoch_response_schema())
        }
        ("post", "/runtime/skillopt/runs/{id}/propose-candidate") => {
            Some(propose_skillopt_candidate_response_schema())
        }
        ("post", "/runtime/skillopt/runs/{id}/cancel") => {
            Some(cancel_skillopt_run_response_schema())
        }
        ("get", "/runtime/skillopt/runs/{id}/history") => {
            Some(skillopt_run_history_response_schema())
        }
        ("get", "/runtime/skillopt/runs/{id}/rejected-edits") => {
            Some(skillopt_rejected_edits_response_schema())
        }
        ("get", "/runtime/harness-radar") => Some(harness_radar_response_schema()),
        ("post", "/runtime/harness-radar/proposals/{id}/promote") => {
            Some(promote_harness_radar_proposal_response_schema())
        }
        ("post", "/runtime/harness-radar/schedule") => {
            Some(schedule_harness_radar_refresh_response_schema())
        }
        ("get", "/runtime/benchmarks") => Some(runtime_benchmarks_response_schema()),
        ("get", "/runtime/metrics") => Some(runtime_metrics_response_schema()),
        ("get", "/runtime/audit") => Some(runtime_audit_response_schema()),
        ("get", "/runtime/taste") => Some(runtime_taste_response_schema()),
        ("get", "/runtime/proposals") => Some(runtime_taste_proposals_response_schema()),
        ("get", "/runtime/rollouts") => Some(runtime_rollouts_response_schema()),
        ("get", "/runtime/finetuning") => Some(runtime_finetuning_response_schema()),
        ("get", "/runtime/middleware") => Some(runtime_middleware_list_response_schema()),
        ("put", "/runtime/middleware/{id}/enabled") => {
            Some(runtime_middleware_update_response_schema())
        }
        ("get", "/runtime/spans") => Some(runtime_spans_response_schema()),
        ("get", "/runtime/spans/{id}") => Some(runtime_span_response_schema()),
        ("get", "/runtime/observations") => Some(runtime_observations_response_schema()),
        _ => None,
    }
}

pub(crate) fn runtime_learning_response_schema() -> Value {
    runtime_schema_ref("RuntimeLearningResponse")
}

pub(crate) fn runtime_learning_decision_response_schema() -> Value {
    runtime_schema_ref("RuntimeLearningDecisionResponse")
}

pub(crate) fn create_skillopt_run_request_schema() -> Value {
    runtime_schema_ref("CreateSkillOptRunRequest")
}

pub(crate) fn resume_skillopt_run_request_schema() -> Value {
    runtime_schema_ref("ResumeSkillOptRunRequest")
}

pub(crate) fn execute_skillopt_epoch_request_schema() -> Value {
    runtime_schema_ref("ExecuteSkillOptEpochRequest")
}

pub(crate) fn propose_skillopt_candidate_request_schema() -> Value {
    runtime_schema_ref("ProposeSkillOptCandidateRequest")
}

pub(crate) fn cancel_skillopt_run_request_schema() -> Value {
    runtime_schema_ref("CancelSkillOptRunRequest")
}

pub(crate) fn skillopt_runs_response_schema() -> Value {
    runtime_schema_ref("SkillOptRunsResponse")
}

pub(crate) fn create_skillopt_run_response_schema() -> Value {
    runtime_schema_ref("CreateSkillOptRunResponse")
}

pub(crate) fn skillopt_run_response_schema() -> Value {
    runtime_schema_ref("SkillOptRunResponse")
}

pub(crate) fn resume_skillopt_run_response_schema() -> Value {
    runtime_schema_ref("ResumeSkillOptRunResponse")
}

pub(crate) fn execute_skillopt_epoch_response_schema() -> Value {
    runtime_schema_ref("ExecuteSkillOptEpochResponse")
}

pub(crate) fn propose_skillopt_candidate_response_schema() -> Value {
    runtime_schema_ref("ProposeSkillOptCandidateResponse")
}

pub(crate) fn cancel_skillopt_run_response_schema() -> Value {
    runtime_schema_ref("CancelSkillOptRunResponse")
}

pub(crate) fn skillopt_run_history_response_schema() -> Value {
    runtime_schema_ref("SkillOptRunHistoryResponse")
}

pub(crate) fn skillopt_rejected_edits_response_schema() -> Value {
    runtime_schema_ref("SkillOptRejectedEditsResponse")
}

pub(crate) fn harness_radar_response_schema() -> Value {
    runtime_schema_ref("HarnessRadarResponse")
}

pub(crate) fn promote_harness_radar_proposal_response_schema() -> Value {
    runtime_schema_ref("PromoteHarnessRadarProposalResponse")
}

pub(crate) fn promote_harness_radar_proposal_request_schema() -> Value {
    runtime_schema_ref("PromoteHarnessRadarProposalRequest")
}

pub(crate) fn schedule_harness_radar_refresh_request_schema() -> Value {
    runtime_schema_ref("ScheduleHarnessRadarRefreshRequest")
}

pub(crate) fn schedule_harness_radar_refresh_response_schema() -> Value {
    runtime_schema_ref("ScheduleHarnessRadarRefreshResponse")
}

pub(crate) fn runtime_benchmarks_response_schema() -> Value {
    runtime_schema_ref("RuntimeBenchmarksResponse")
}

pub(crate) fn runtime_metrics_response_schema() -> Value {
    runtime_schema_ref("RuntimeMetricsResponse")
}

pub(crate) fn runtime_audit_response_schema() -> Value {
    runtime_schema_ref("RuntimeAuditResponse")
}

pub(crate) fn runtime_taste_response_schema() -> Value {
    runtime_schema_ref("RuntimeTasteResponse")
}

pub(crate) fn runtime_taste_proposals_response_schema() -> Value {
    runtime_schema_ref("RuntimeTasteProposalsResponse")
}

pub(crate) fn runtime_rollouts_response_schema() -> Value {
    runtime_schema_ref("RuntimeRolloutsResponse")
}

pub(crate) fn runtime_finetuning_response_schema() -> Value {
    runtime_schema_ref("RuntimeFinetuningResponse")
}

pub(crate) fn runtime_middleware_list_response_schema() -> Value {
    runtime_schema_ref("RuntimeMiddlewareListResponse")
}

pub(crate) fn set_runtime_middleware_enabled_request_schema() -> Value {
    runtime_schema_ref("SetRuntimeMiddlewareEnabledRequest")
}

pub(crate) fn runtime_middleware_update_response_schema() -> Value {
    runtime_schema_ref("RuntimeMiddlewareUpdateResponse")
}

pub(crate) fn runtime_spans_response_schema() -> Value {
    runtime_schema_ref("RuntimeSpansResponse")
}

pub(crate) fn runtime_span_response_schema() -> Value {
    runtime_schema_ref("RuntimeSpanResponse")
}

pub(crate) fn runtime_observations_response_schema() -> Value {
    runtime_schema_ref("RuntimeObservationsResponse")
}
