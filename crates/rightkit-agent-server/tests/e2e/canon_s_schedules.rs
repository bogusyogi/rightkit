//! S:N5 workflow-trigger misfire bounds through the public workflow API.

use crate::support::e2e_orch::*;
use serde_json::{json, Value};

fn enable_workflows(home: &std::path::Path, _workspace: &std::path::Path) {
    append_config(home, "[features]\nworkflows-enabled = true\n");
}

fn definition(workspace: &std::path::Path) -> Value {
    let mut d: Value =
        serde_json::from_str(include_str!("../fixtures/orch/budget-watch.workflow.json")).unwrap();
    d["workspace"] = json!({"root": slash_path(workspace), "trust": "trusted"});
    d
}

#[test]
fn cron_trigger_accepts_only_named_bounded_misfire_policies() {
    let env = Env::start(
        Provider::start(|_, _| Reply::text("schedule answer")),
        &[],
        enable_workflows,
    );
    let (status, workflow) = env.post(
        "/workflows",
        json!({"name":"S-N5", "definition":definition(&env.workspace)}),
    );
    assert_eq!(status, 201, "create workflow: {workflow}");
    let workflow_id = workflow["id"].as_str().expect("workflow id");
    let version_id = workflow["versionId"].as_str().expect("workflow version");
    let (status, validation) = env.post(
        &format!("/workflows/{workflow_id}/versions/{version_id}/validate"),
        json!({}),
    );
    assert_eq!(status, 200, "validate workflow: {validation}");
    let (status, published) = env.post(
        &format!("/workflows/{workflow_id}/versions/{version_id}/publish"),
        json!({}),
    );
    assert!(
        (200..300).contains(&status),
        "publish workflow: {published}"
    );

    let body = |policy: &str| {
        json!({
            "kind":"cron", "enabled":true, "cron_expression":"0 3 1 1 *",
            "timezone":"UTC", "version_pin":version_id,
            "overlap_policy":"forbid", "misfire_policy":policy,
            "concurrency_cap":1, "dst_policy":"skip_gap"
        })
    };
    let (status, rejected) = env.post(
        &format!("/workflows/{workflow_id}/triggers"),
        body("unbounded_catchup"),
    );
    assert_eq!(
        status, 400,
        "unknown misfire policy must fail closed: {rejected}"
    );
    for policy in ["skip", "coalesce", "bounded_catchup"] {
        let (status, trigger) =
            env.post(&format!("/workflows/{workflow_id}/triggers"), body(policy));
        assert_eq!(status, 201, "valid policy {policy}: {trigger}");
        // Workflow trigger DTOs use snake_case (CreateTriggerRequest and
        // WorkflowTriggerRow); bot schedule DTOs separately use camelCase.
        assert_eq!(trigger["misfire_policy"], policy, "{trigger}");
    }
}
