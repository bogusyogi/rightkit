pub(crate) fn skills_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    insert_skill_openapi_core_components(&mut schemas);
    insert_skill_openapi_governance_components(&mut schemas);

    schemas
}

fn skills_schema_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

pub(crate) fn skills_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/skills/install") => Some(install_skill_request_schema()),
        ("post", "/skills/provenance-review") => Some(skill_provenance_review_request_schema()),
        ("put", "/skills/{slug}") => Some(skill_save_request_schema()),
        ("post", "/skills/proposals") => Some(create_skill_proposal_request_schema()),
        ("post", "/skills/proposals/{id}/quarantine") => {
            Some(skill_proposal_quarantine_request_schema())
        }
        ("post", "/skills/proposals/{id}/benchmark") => Some(skill_benchmark_request_schema()),
        ("post", "/skills/proposals/{id}/run-benchmark") => {
            Some(skill_proposal_benchmark_run_request_schema())
        }
        ("post", "/skills/training-runs") => Some(skill_training_run_create_request_schema()),
        ("post", "/skills/training-runs/{id}/resume") => {
            Some(skill_training_run_resume_request_schema())
        }
        ("post", "/skills/training-runs/{id}/execute") => {
            Some(skill_training_run_execute_request_schema())
        }
        ("post", "/skills/training-runs/{id}/propose-candidate") => {
            Some(skill_training_run_propose_candidate_request_schema())
        }
        ("post", "/skills/training-runs/{id}/cancel") => {
            Some(skill_training_run_cancel_request_schema())
        }
        ("post", "/skills/{slug}/quarantine")
        | ("post", "/skills/{slug}/activate")
        | ("post", "/skills/{slug}/retire") => Some(skill_lifecycle_request_schema()),
        _ => None,
    }
}

pub(crate) fn skills_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/skills") => Some(skill_list_response_schema()),
        ("post", "/skills/install") => Some(install_skill_response_schema()),
        ("post", "/skills/provenance-review") => Some(skill_provenance_review_response_schema()),
        ("get", "/skills/{slug}") => Some(skill_read_response_schema()),
        ("put", "/skills/{slug}") => Some(skill_save_response_schema()),
        ("delete", "/skills/{slug}") => Some(skill_delete_response_schema()),
        ("get", "/skills/proposals") => Some(skill_governance_response_schema()),
        ("post", "/skills/proposals") => Some(create_skill_proposal_response_schema()),
        ("post", "/skills/proposals/{id}/quarantine") => {
            Some(skill_proposal_quarantine_response_schema())
        }
        ("post", "/skills/proposals/{id}/benchmark") => Some(skill_benchmark_response_schema()),
        ("post", "/skills/proposals/{id}/run-benchmark") => {
            Some(skill_proposal_benchmark_run_response_schema())
        }
        ("get", "/skills/training-runs") => Some(skill_training_runs_response_schema()),
        ("post", "/skills/training-runs") => Some(skill_training_run_create_response_schema()),
        ("get", "/skills/training-runs/{id}") => Some(skill_training_run_response_schema()),
        ("post", "/skills/training-runs/{id}/resume") => {
            Some(skill_training_run_resume_response_schema())
        }
        ("post", "/skills/training-runs/{id}/execute") => {
            Some(skill_training_run_execute_response_schema())
        }
        ("post", "/skills/training-runs/{id}/propose-candidate") => {
            Some(skill_training_run_propose_candidate_response_schema())
        }
        ("post", "/skills/training-runs/{id}/cancel") => {
            Some(skill_training_run_cancel_response_schema())
        }
        ("get", "/skills/training-runs/{id}/history") => {
            Some(skill_training_run_history_response_schema())
        }
        ("get", "/skills/training-runs/{id}/rejected-edits") => {
            Some(skill_training_run_rejected_edits_response_schema())
        }
        ("post", "/skills/{slug}/quarantine")
        | ("post", "/skills/{slug}/activate")
        | ("post", "/skills/{slug}/retire") => Some(skill_lifecycle_response_schema()),
        _ => None,
    }
}

pub(crate) fn skill_list_response_schema() -> Value {
    skills_schema_ref("SkillListResponse")
}

pub(crate) fn install_skill_request_schema() -> Value {
    skills_schema_ref("InstallSkillRequest")
}

pub(crate) fn install_skill_response_schema() -> Value {
    skills_schema_ref("InstallSkillResponse")
}

pub(crate) fn skill_provenance_review_request_schema() -> Value {
    skills_schema_ref("SkillProvenanceReviewRequest")
}

pub(crate) fn skill_provenance_review_response_schema() -> Value {
    skills_schema_ref("SkillProvenanceReviewResponse")
}

pub(crate) fn skill_read_response_schema() -> Value {
    skills_schema_ref("SkillReadResponse")
}

pub(crate) fn skill_save_request_schema() -> Value {
    skills_schema_ref("SkillSaveRequest")
}

pub(crate) fn skill_save_response_schema() -> Value {
    skills_schema_ref("SkillSaveResponse")
}

pub(crate) fn skill_delete_response_schema() -> Value {
    skills_schema_ref("SkillDeleteResponse")
}

pub(crate) fn skill_governance_response_schema() -> Value {
    skills_schema_ref("SkillGovernanceResponse")
}

pub(crate) fn create_skill_proposal_request_schema() -> Value {
    skills_schema_ref("CreateSkillProposalRequest")
}

pub(crate) fn create_skill_proposal_response_schema() -> Value {
    skills_schema_ref("CreateSkillProposalResponse")
}

pub(crate) fn skill_proposal_quarantine_request_schema() -> Value {
    skills_schema_ref("SkillProposalQuarantineRequest")
}

pub(crate) fn skill_proposal_quarantine_response_schema() -> Value {
    skills_schema_ref("SkillProposalQuarantineResponse")
}

pub(crate) fn skill_benchmark_request_schema() -> Value {
    skills_schema_ref("SkillBenchmarkRequest")
}

pub(crate) fn skill_benchmark_response_schema() -> Value {
    skills_schema_ref("SkillBenchmarkResponse")
}

pub(crate) fn skill_proposal_benchmark_run_request_schema() -> Value {
    skills_schema_ref("SkillProposalBenchmarkRunRequest")
}

pub(crate) fn skill_proposal_benchmark_run_response_schema() -> Value {
    skills_schema_ref("SkillProposalBenchmarkRunResponse")
}

pub(crate) fn skill_lifecycle_request_schema() -> Value {
    skills_schema_ref("SkillLifecycleRequest")
}

pub(crate) fn skill_lifecycle_response_schema() -> Value {
    skills_schema_ref("SkillLifecycleResponse")
}

pub(crate) fn skill_training_run_create_request_schema() -> Value {
    crate::api_runtime::create_skillopt_run_request_schema()
}

pub(crate) fn skill_training_run_resume_request_schema() -> Value {
    crate::api_runtime::resume_skillopt_run_request_schema()
}

pub(crate) fn skill_training_run_execute_request_schema() -> Value {
    crate::api_runtime::execute_skillopt_epoch_request_schema()
}

pub(crate) fn skill_training_run_propose_candidate_request_schema() -> Value {
    crate::api_runtime::propose_skillopt_candidate_request_schema()
}

pub(crate) fn skill_training_run_cancel_request_schema() -> Value {
    crate::api_runtime::cancel_skillopt_run_request_schema()
}

pub(crate) fn skill_training_runs_response_schema() -> Value {
    crate::api_runtime::skillopt_runs_response_schema()
}

pub(crate) fn skill_training_run_create_response_schema() -> Value {
    crate::api_runtime::create_skillopt_run_response_schema()
}

pub(crate) fn skill_training_run_response_schema() -> Value {
    crate::api_runtime::skillopt_run_response_schema()
}

pub(crate) fn skill_training_run_resume_response_schema() -> Value {
    crate::api_runtime::resume_skillopt_run_response_schema()
}

pub(crate) fn skill_training_run_execute_response_schema() -> Value {
    crate::api_runtime::execute_skillopt_epoch_response_schema()
}

pub(crate) fn skill_training_run_propose_candidate_response_schema() -> Value {
    crate::api_runtime::propose_skillopt_candidate_response_schema()
}

pub(crate) fn skill_training_run_cancel_response_schema() -> Value {
    crate::api_runtime::cancel_skillopt_run_response_schema()
}

pub(crate) fn skill_training_run_history_response_schema() -> Value {
    crate::api_runtime::skillopt_run_history_response_schema()
}

pub(crate) fn skill_training_run_rejected_edits_response_schema() -> Value {
    crate::api_runtime::skillopt_rejected_edits_response_schema()
}
