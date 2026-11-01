#[path = "openapi_components/core.rs"]
mod specs_openapi_core;
#[path = "openapi_components/workflow.rs"]
mod specs_openapi_workflow;

pub(crate) fn specs_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    specs_openapi_core::insert(&mut schemas);
    specs_openapi_workflow::insert(&mut schemas);
    schemas
}

pub(crate) fn specs_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method.to_ascii_lowercase().as_str(), path) {
        ("post", "/specs") => Some(create_spec_request_schema()),
        ("patch", "/specs/{id}") => Some(patch_spec_request_schema()),
        ("post", "/specs/{id}/clarify") | ("post", "/specs/{id}/analyze") => {
            Some(spec_empty_request_schema())
        }
        ("post", "/specs/{id}/plan") => Some(plan_spec_request_schema()),
        ("post", "/specs/{id}/tasks") => Some(add_spec_tasks_request_schema()),
        ("post", "/specs/{id}/constitution/decisions") => {
            Some(constitution_decision_request_schema())
        }
        ("post", "/specs/{id}/checklist") => Some(checklist_spec_request_schema()),
        ("post", "/specs/{id}/implement") => Some(implement_spec_request_schema()),
        _ => None,
    }
}

pub(crate) fn specs_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method.to_ascii_lowercase().as_str(), path) {
        ("get", "/specs") => Some(list_specs_response_schema()),
        ("post", "/specs") => Some(create_spec_response_schema()),
        ("get", "/specs/{id}") => Some(get_spec_response_schema()),
        ("patch", "/specs/{id}") => Some(patch_spec_response_schema()),
        ("post", "/specs/{id}/clarify") => Some(clarify_spec_response_schema()),
        ("post", "/specs/{id}/plan") => Some(plan_spec_response_schema()),
        ("post", "/specs/{id}/tasks") => Some(add_spec_tasks_response_schema()),
        ("get", "/specs/{id}/constitution/decisions") => {
            Some(list_constitution_decisions_response_schema())
        }
        ("post", "/specs/{id}/constitution/decisions") => {
            Some(record_constitution_decision_response_schema())
        }
        ("post", "/specs/{id}/checklist") => Some(checklist_spec_response_schema()),
        ("post", "/specs/{id}/analyze") => Some(analyze_spec_response_schema()),
        ("post", "/specs/{id}/implement") => Some(implement_spec_response_schema()),
        ("get", "/specs/{id}/trace") => Some(get_spec_trace_response_schema()),
        _ => None,
    }
}

pub(crate) fn list_specs_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/FeatureSpec"}})
}

pub(crate) fn create_spec_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/CreateSpecRequest"})
}

pub(crate) fn create_spec_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/FeatureSpec"})
}

pub(crate) fn get_spec_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SpecEnvelope"})
}

pub(crate) fn patch_spec_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/PatchSpecRequest"})
}

pub(crate) fn patch_spec_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/FeatureSpec"})
}

pub(crate) fn spec_empty_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SpecEmptyRequest"})
}

pub(crate) fn clarify_spec_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SpecClarifyResponse"})
}

pub(crate) fn plan_spec_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/PlanSpecRequest"})
}

pub(crate) fn plan_spec_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SpecPlanResponse"})
}

pub(crate) fn add_spec_tasks_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/AddSpecTasksRequest"})
}

pub(crate) fn add_spec_tasks_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/FeatureTask"}})
}

pub(crate) fn list_constitution_decisions_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/ConstitutionDecision"}})
}

pub(crate) fn constitution_decision_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ConstitutionDecisionRequest"})
}

pub(crate) fn record_constitution_decision_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ConstitutionDecision"})
}

pub(crate) fn checklist_spec_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ChecklistSpecRequest"})
}

pub(crate) fn checklist_spec_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SpecChecklistResponse"})
}

pub(crate) fn analyze_spec_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SpecAnalysisRun"})
}

pub(crate) fn implement_spec_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ImplementSpecRequest"})
}

pub(crate) fn implement_spec_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SpecImplementResponse"})
}

pub(crate) fn get_spec_trace_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SpecTrace"})
}
