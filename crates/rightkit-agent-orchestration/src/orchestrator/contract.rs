use crate::conductor::{WorkerOutcome, WorkerResult, WorkerTask};
use crate::handoff_brief::{HandoffArchiveConfig, HandoffBriefBuilder, SkeletonizeConfig};
use crate::registry::{AgentDescriptor, AgentRegistry};
use serde_json::{json, Value};

pub(super) fn task_payload(goal: &str, node: &rightkit_agent_engine_protocol::TaskNode) -> String {
    let mut p = String::new();
    if !goal.is_empty() {
        p.push_str(&format!("Overall goal: {goal}\n\n"));
    }
    p.push_str(&format!("Your task: {}", node.title));
    if !node.done_when.is_empty() {
        p.push_str(&format!("\nDone when: {}", node.done_when));
    }
    p
}

/// Build a compacted handoff brief for a worker task, using only the context
/// the worker actually needs (its dependencies' outputs, not the full transcript).
pub(super) fn handoff_brief_payload(
    goal: &str,
    node: &rightkit_agent_engine_protocol::TaskNode,
    agent: Option<&AgentDescriptor>,
    completed_results: &[WorkerResult],
    archive_config: Option<&HandoffArchiveConfig>,
) -> String {
    let mut builder = HandoffBriefBuilder::new(goal, node, completed_results);
    if let Some(agent) = agent {
        builder = builder.with_agent(agent);
    }
    if let Some(config) = archive_config {
        builder = builder.with_archive(config.clone());
    }
    if let Some(config) = SkeletonizeConfig::for_handoff(node, completed_results) {
        builder = builder.with_skeletonization(config);
    }
    let brief = builder.build();
    format!("{brief}")
}

pub(super) fn worker_task_contract_value(task: &WorkerTask) -> Value {
    json!({
        "task_id": task.task_id,
        "agent": task.agent,
        "payload": task.payload,
        "agent_version": task.agent_version,
    })
}

pub(super) fn worker_result_contract_value(result: &WorkerResult) -> Value {
    json!({
        "task_id": result.task_id,
        "agent": result.agent,
        "success": result.success,
        "output": result.output,
        "input_tokens": result.input_tokens,
        "mop_fired": result.mop_fired,
        "agent_version": result.agent_version,
        "rollback_step": result.rollback_step,
    })
}

pub(super) fn validate_contract(schema: &Value, value: &Value) -> Result<(), String> {
    match schema {
        Value::Bool(true) => Ok(()),
        Value::Bool(false) => Err("boolean schema false rejects all values".into()),
        Value::Object(schema_obj) => {
            if let Some(type_schema) = schema_obj.get("type") {
                validate_type(type_schema, value)?;
            }

            if let Some(required) = schema_obj.get("required") {
                let required = required
                    .as_array()
                    .ok_or_else(|| "schema required must be an array".to_string())?;
                let value_obj = value
                    .as_object()
                    .ok_or_else(|| "required fields need an object value".to_string())?;
                for field in required {
                    let field = field
                        .as_str()
                        .ok_or_else(|| "schema required entries must be strings".to_string())?;
                    if !value_obj.contains_key(field) {
                        return Err(format!("missing required field `{field}`"));
                    }
                }
            }

            if let Some(properties) = schema_obj.get("properties") {
                let properties = properties
                    .as_object()
                    .ok_or_else(|| "schema properties must be an object".to_string())?;
                let value_obj = value
                    .as_object()
                    .ok_or_else(|| "properties need an object value".to_string())?;
                for (field, field_schema) in properties {
                    if let Some(field_value) = value_obj.get(field) {
                        validate_contract(field_schema, field_value)
                            .map_err(|err| format!("{field}: {err}"))?;
                    }
                }
            }

            if schema_obj.get("additionalProperties") == Some(&Value::Bool(false)) {
                let properties = schema_obj
                    .get("properties")
                    .and_then(Value::as_object)
                    .ok_or_else(|| "additionalProperties=false requires properties".to_string())?;
                let value_obj = value
                    .as_object()
                    .ok_or_else(|| "additionalProperties needs an object value".to_string())?;
                for key in value_obj.keys() {
                    if !properties.contains_key(key) {
                        return Err(format!("unexpected field `{key}`"));
                    }
                }
            }

            Ok(())
        }
        _ => Err("schema must be a boolean or object".into()),
    }
}

pub(super) fn validate_type(type_schema: &Value, value: &Value) -> Result<(), String> {
    let matches_named_type = |name: &str| match name {
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "object" => value.is_object(),
        "string" => value.is_string(),
        _ => false,
    };

    match type_schema {
        Value::String(name) => {
            if matches_named_type(name) {
                Ok(())
            } else {
                Err(format!("expected type `{name}`"))
            }
        }
        Value::Array(names) => {
            let mut expected = Vec::new();
            for name in names {
                let name = name
                    .as_str()
                    .ok_or_else(|| "schema type array entries must be strings".to_string())?;
                expected.push(name.to_string());
                if matches_named_type(name) {
                    return Ok(());
                }
            }
            Err(format!("expected one of [{}]", expected.join(", ")))
        }
        _ => Err("schema type must be a string or string array".into()),
    }
}

pub(super) fn contract_failure_result(task: &WorkerTask, message: String) -> WorkerResult {
    WorkerResult {
        task_id: task.task_id.clone(),
        agent: task.agent.clone(),
        success: false,
        output: message,
        input_tokens: 0,
        mop_fired: false,
        failure_kind: Some(crate::failure::FailureKind::Unknown),
        agent_version: task.agent_version.clone(),
        rollback_step: None,
        outcome: WorkerOutcome::Complete,
    }
}

pub(super) fn validate_output_contract(
    registry: &AgentRegistry,
    mut result: WorkerResult,
) -> WorkerResult {
    let Some(agent) = registry.get(&result.agent) else {
        return result;
    };
    let Some(schema) = &agent.output_schema else {
        return result;
    };
    let value = worker_result_contract_value(&result);
    if let Err(err) = validate_contract(schema, &value) {
        result.success = false;
        result.output = format!("output schema validation failed: {err}");
        result.failure_kind = Some(crate::failure::FailureKind::Unknown);
    }
    result
}
