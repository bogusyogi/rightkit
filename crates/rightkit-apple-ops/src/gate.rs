//! The policy gate shared by native Apple CLI and MCP transports.
//!
//! This module owns request normalisation and effect classification. The operations in this crate
//! stay unaware of host policy; a transport hands every execute request to [`invoke`], which
//! derives the effects the call needs ([`required_effects`], refined by the dry-run plan in
//! [`effects_from_plan`]) and asks the host's [`Gate`] to authorise them before the operation
//! runs. The host decides what a grant is and which evaluator answers; this crate decides only
//! which effects a call has and which filesystem targets it binds.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A coarse effect an Apple operation can have.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Effect {
    /// Writes files.
    FileWrite,
    /// Deletes files.
    FileDelete,
    /// Reaches the network.
    NetworkEgress,
    /// Starts a process.
    ProcessSpawn,
    /// Reads a credential.
    CredentialAccess,
    /// Publishes (App Store submission or release).
    Publish,
    /// Changes something outside this computer or the project.
    ExternalSideEffect,
}

/// Why a gated call did not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// The request itself is malformed or its effects cannot be derived.
    Invalid(String),
    /// The host refused to authorise the effects.
    Denied(String),
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::Invalid(m) | GateError::Denied(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for GateError {}

/// What the host authorises: one derived call.
#[derive(Debug, Clone)]
pub struct Call<'a> {
    /// Operation as requested (aliases are not resolved).
    pub operation: &'a str,
    /// The operation's arguments (always an object).
    pub arguments: &'a Value,
    /// Effects derived from the operation and arguments, sorted and unique.
    pub effects: &'a [Effect],
    /// The adapter's dry-run plan, when the call executes and a plan exists.
    pub plan: Option<&'a Value>,
}

/// The host's authorisation. The caller's request (with its `policyContext`) is passed through
/// untouched; nothing in it is an independent grant.
pub trait Gate: Send + Sync {
    /// Authorise `call`; the returned receipts are attached to the result as `guardReceipts`.
    fn authorize(&self, request: &Value, call: &Call<'_>) -> Result<Vec<Value>, String>;

    /// Synchronous scope validation run before dispatch (MCP): `effects` are the coarse effects.
    fn validate_scope(&self, request: &Value, effects: &[Effect]) -> Result<(), String>;
}

/// Invoke one Apple operation from a CLI or MCP request shape
/// (`{operation, arguments, policyContext}`): derive effects, authorise, run, attach receipts.
pub async fn invoke(gate: &dyn Gate, request: &Value) -> Result<Value, GateError> {
    let (operation, arguments) = request_parts(request).map_err(GateError::Invalid)?;
    let (effects, plan) = derive_effects(&operation, &arguments).await?;
    let receipts = if effects.is_empty() {
        Vec::new()
    } else {
        gate.authorize(
            request,
            &Call {
                operation: &operation,
                arguments: &arguments,
                effects: &effects,
                plan: plan.as_ref(),
            },
        )
        .map_err(GateError::Denied)?
    };
    let mut result = crate::invoke(&operation, &arguments)
        .await
        .map_err(GateError::Invalid)?;
    if !receipts.is_empty() {
        if let Some(object) = result.as_object_mut() {
            object.insert("guardReceipts".into(), Value::Array(receipts));
        } else {
            result = json!({"result": result, "guardReceipts": receipts});
        }
    }
    Ok(result)
}

/// Validate MCP scope before dispatch: read-only calls pass, execute calls need the host's
/// agreement. Synchronous because MCP scope validation runs before its async invocation hook.
pub fn validate_scope(gate: &dyn Gate, request: &Value) -> Result<(), String> {
    let (operation, arguments) = request_parts(request)?;
    let effects = required_effects(&operation, &arguments)?;
    if effects.is_empty() {
        return Ok(());
    }
    gate.validate_scope(request, &effects)
}

/// The `operation` and `arguments` of a request (arguments default to `{}`).
pub fn request_parts(request: &Value) -> Result<(String, Value), String> {
    let object = request
        .as_object()
        .ok_or_else(|| "Apple request must be an object".to_string())?;
    let operation = object
        .get("operation")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Apple operation is required".to_string())?
        .to_owned();
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    if !arguments.is_object() {
        return Err("Apple arguments must be an object".to_string());
    }
    Ok((operation, arguments))
}

/// The filesystem targets an operation binds, extracted only after the adapter has accepted the
/// typed request and produced its dry plan. Network identifiers (App Store Connect paths, app
/// ids) are never converted into filesystem paths. Returns the raw strings and the plan's
/// trusted working directory; the host canonicalises them with [`resolve_target`].
pub fn bound_targets(
    operation: &str,
    arguments: &Value,
    plan: Option<&Value>,
) -> Result<(Vec<String>, Option<String>), String> {
    let canonical = plan
        .and_then(|value| value.get("operation"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| canonical_operation(operation));
    let object = arguments
        .as_object()
        .ok_or_else(|| "Apple arguments must be an object".to_string())?;
    let mut raw_targets: Vec<String> = Vec::new();
    let cwd = plan
        .and_then(|value| value.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if operation == "app-store" && is_upload_request(arguments) {
        for key in ["artifact", "filePath", "file_path", "ipa", "pkg", "path"] {
            if let Some(value) = object.get(key).and_then(Value::as_str) {
                raw_targets.push(value.to_owned());
                break;
            }
        }
    } else {
        let keys: &[&str] = match canonical {
            "project.build" | "project.test" => &[
                "project",
                "project_path",
                "workspace",
                "workspace_path",
                "derived_data_path",
                "result_bundle_path",
            ],
            "project.clean" => &["project", "project_path", "workspace", "workspace_path"],
            "swiftpm.clean" => &["package_path"],
            "project.archive" => &[
                "project",
                "project_path",
                "workspace",
                "workspace_path",
                "archive_path",
            ],
            "project.export" => &["archive_path", "export_path", "export_options_plist"],
            "swiftpm.build" | "swiftpm.test" | "swiftpm.run" => &["package_path"],
            "simulator.install" | "device.install" => &["app_path"],
            "simulator.screenshot" | "simulator.record_video" => &["output", "path"],
            "profile" => &["output"],
            "profile.export" => &["output_path"],
            _ => &[],
        };
        for key in keys {
            if let Some(value) = object.get(*key).and_then(Value::as_str) {
                raw_targets.push(value.to_owned());
            }
        }
        if raw_targets.is_empty()
            && matches!(
                canonical,
                "project.build"
                    | "project.test"
                    | "project.clean"
                    | "swiftpm.clean"
                    | "swiftpm.build"
                    | "swiftpm.test"
                    | "swiftpm.run"
            )
        {
            if let Some(cwd) = &cwd {
                raw_targets.push(cwd.clone());
            }
        }
    }
    Ok((raw_targets, cwd))
}

/// Resolve one bound target to an absolute host path string with forward slashes: `.` means
/// the trusted `cwd`, a relative target is joined to `cwd` (else the current folder), and a
/// target that does not exist yet resolves through its nearest existing parent (symlinks in the
/// parent are resolved).
pub fn resolve_target(raw: &str, cwd: Option<&str>) -> Result<String, String> {
    let raw = if matches!(raw.trim(), "." | "./") {
        cwd.unwrap_or(raw)
    } else {
        raw
    };
    let input = PathBuf::from(raw);
    let absolute = if input.is_absolute()
        || (raw.len() >= 3
            && raw.as_bytes()[1] == b':'
            && matches!(raw.as_bytes()[2], b'/' | b'\\'))
    {
        input
    } else {
        let root = cwd
            .map(PathBuf::from)
            .or_else(|| env::current_dir().ok())
            .ok_or_else(|| "Apple target has no trusted working directory".to_string())?;
        root.join(input)
    };
    Ok(host_path_string(&resolve_existing_target(&absolute)?))
}

fn resolve_existing_target(path: &Path) -> Result<PathBuf, String> {
    let mut missing = Vec::new();
    let mut cursor = path;
    while fs::symlink_metadata(cursor).is_err() {
        let name = cursor
            .file_name()
            .ok_or_else(|| "Apple target has no existing parent".to_string())?;
        missing.push(name.to_os_string());
        cursor = cursor
            .parent()
            .ok_or_else(|| "Apple target has no existing parent".to_string())?;
    }
    let mut resolved = fs::canonicalize(cursor)
        .map_err(|error| format!("Apple target parent cannot be canonicalized: {error}"))?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

pub fn host_path_string(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/UNC/") {
        return format!("/{rest}");
    }
    value
        .strip_prefix("//?/")
        .map(str::to_owned)
        .unwrap_or(value)
}

pub async fn derive_effects(
    operation: &str,
    arguments: &Value,
) -> Result<(Vec<Effect>, Option<Value>), GateError> {
    let coarse = required_effects(operation, arguments).map_err(GateError::Invalid)?;
    if !arguments
        .get("execute")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok((coarse, None));
    }
    let mut dry_arguments = arguments.clone();
    if let Some(object) = dry_arguments.as_object_mut() {
        object.insert("execute".into(), Value::Bool(false));
    }
    let plan = match crate::invoke(operation, &dry_arguments).await {
        Ok(plan) => plan,
        Err(error) if operation == "memgraph" || operation == "memgraph-leaks" => {
            // Native memgraph capture intentionally rejects dry-run because
            // its plan is the bounded `/usr/bin/leaks` process itself.
            let _ = error;
            return Ok((coarse, None));
        }
        Err(error) => {
            return Err(GateError::Invalid(format!(
                "cannot derive Apple effects from operation plan: {error}"
            )))
        }
    };
    Ok((
        effects_from_plan(operation, arguments, &plan, coarse),
        Some(plan),
    ))
}

pub fn effects_from_plan(
    operation: &str,
    arguments: &Value,
    plan: &Value,
    coarse: Vec<Effect>,
) -> Vec<Effect> {
    let canonical = plan
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or(operation);
    // App Store upload plans expose `build-upload`, which is an adapter plan
    // name rather than a policy operation. Keep coarse ASC credential/network
    // effects when plan metadata uses such a name.
    let mut effects = if canonical == "build-upload" {
        coarse.clone()
    } else {
        required_effects(canonical, arguments).unwrap_or(coarse)
    };
    if let Some(classifications) = plan
        .get("effectClassification")
        .and_then(|value| value.get("classifications"))
        .and_then(Value::as_array)
    {
        let planned = classifications
            .iter()
            .filter_map(Value::as_str)
            .filter_map(effect_from_classification)
            .collect::<Vec<_>>();
        if !planned.is_empty() {
            effects.clear();
            effects.extend(planned);
        }
    }
    if let Some(mutation) = plan.get("mutation").and_then(Value::as_bool) {
        effects.retain(|effect| !matches!(effect, Effect::ExternalSideEffect | Effect::Publish));
        if mutation {
            let action = arguments
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("");
            let request = plan.get("request").and_then(Value::as_object);
            let method = request
                .and_then(|value| value.get("method"))
                .and_then(Value::as_str)
                .or_else(|| arguments.get("method").and_then(Value::as_str))
                .unwrap_or("GET");
            let path = request
                .and_then(|value| value.get("url"))
                .and_then(Value::as_str)
                .or_else(|| arguments.get("path").and_then(Value::as_str))
                .unwrap_or("");
            let body = request
                .and_then(|value| value.get("body"))
                .or_else(|| arguments.get("body"));
            let operation_id = plan
                .get("operationId")
                .and_then(Value::as_str)
                .or_else(|| arguments.get("operationId").and_then(Value::as_str))
                .or_else(|| arguments.get("operation").and_then(Value::as_str))
                .unwrap_or("");
            effects.push(
                if is_publish_route(action, operation_id, method, path, body) {
                    Effect::Publish
                } else {
                    Effect::ExternalSideEffect
                },
            );
        }
    }
    if operation == "app-store" && is_upload_request(arguments) {
        effects.push(Effect::CredentialAccess);
        effects.push(Effect::NetworkEgress);
        effects.push(Effect::ExternalSideEffect);
    }
    effects.sort();
    effects.dedup();
    effects
}

fn is_upload_request(arguments: &Value) -> bool {
    let Some(object) = arguments.as_object() else {
        return false;
    };
    let action = object.get("action").and_then(Value::as_str).unwrap_or("");
    action.eq_ignore_ascii_case("upload")
        || (action.eq_ignore_ascii_case("builds")
            && object
                .get("operation")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case("upload")))
}

pub fn is_publish_route(
    action: &str,
    operation: &str,
    method: &str,
    raw_path: &str,
    body: Option<&Value>,
) -> bool {
    if method.eq_ignore_ascii_case("GET") {
        return false;
    }
    if action.eq_ignore_ascii_case("submission")
        || matches!(
            operation.to_ascii_lowercase().as_str(),
            "submit" | "submission" | "release" | "review"
        )
    {
        return true;
    }
    let path = route_path(raw_path).to_ascii_lowercase();
    if [
        "submission",
        "submit",
        "release",
        "reviewsubmission",
        "releaserequest",
    ]
    .iter()
    .any(|segment| path.contains(segment))
    {
        return true;
    }
    let operation = operation.to_ascii_lowercase();
    if ["submit", "submission", "release", "review"]
        .iter()
        .any(|label| operation.contains(label))
    {
        return true;
    }
    body_implies_publish(body)
}

fn route_path(raw: &str) -> &str {
    let Some(scheme_end) = raw.find("://") else {
        return raw;
    };
    raw[scheme_end + 3..]
        .find('/')
        .map(|offset| &raw[scheme_end + 3 + offset..])
        .unwrap_or("")
}

fn body_implies_publish(body: Option<&Value>) -> bool {
    let Some(value) = body else {
        return false;
    };
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            let signal = matches!(
                key.as_str(),
                "publish"
                    | "release"
                    | "releasenow"
                    | "submit"
                    | "submitted"
                    | "submitforreview"
                    | "sendforreview"
                    | "reviewsubmission"
            );
            (signal && !matches!(value, Value::Bool(false) | Value::Null))
                || body_implies_publish(Some(value))
        }),
        Value::Array(values) => values.iter().any(|value| body_implies_publish(Some(value))),
        _ => false,
    }
}

fn effect_from_classification(classification: &str) -> Option<Effect> {
    match classification.to_ascii_lowercase().as_str() {
        "credentialaccess" | "credential_access" => Some(Effect::CredentialAccess),
        "networkegress" | "network_egress" => Some(Effect::NetworkEgress),
        "externalsideeffect" | "external_side_effect" => Some(Effect::ExternalSideEffect),
        "publish" => Some(Effect::Publish),
        "processspawn" | "process_spawn" => Some(Effect::ProcessSpawn),
        "filewrite" | "file_write" => Some(Effect::FileWrite),
        "filedelete" | "file_delete" => Some(Effect::FileDelete),
        _ => None,
    }
}

/// Process-backed Apple operations require PROCESS_SPAWN. App Store Connect
/// execution derives credential, network, & consequential mutation effects.
pub fn required_effects(operation: &str, arguments: &Value) -> Result<Vec<Effect>, String> {
    let object = arguments
        .as_object()
        .ok_or_else(|| "Apple arguments must be an object".to_string())?;
    let execute = object
        .get("execute")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !execute {
        return Ok(Vec::new());
    }
    if operation == "app-store" {
        let action = object
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("discover")
            .to_ascii_lowercase();
        let has_selector = ["operationId", "operation_id", "operation", "method", "path"]
            .iter()
            .any(|key| object.contains_key(*key));
        if matches!(action.as_str(), "catalog" | "template" | "dry-run")
            || (matches!(action.as_str(), "discover" | "openapi" | "operations") && !has_selector)
        {
            return Ok(Vec::new());
        }
        let mut effects = vec![Effect::CredentialAccess, Effect::NetworkEgress];
        let is_build_upload = action == "upload"
            || (action == "builds"
                && object
                    .get("operation")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.eq_ignore_ascii_case("upload")));
        if is_build_upload {
            // Upload plans intentionally report an empty classification during
            // dry-run, but reservation, transfer & commit always mutate ASC.
            effects.push(Effect::ExternalSideEffect);
        }
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let operation_name = object
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("list")
            .to_ascii_lowercase();
        let operation_id = object
            .get("operationId")
            .or_else(|| object.get("operation_id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let publish_operation = if operation_id.is_empty() {
            operation_name.as_str()
        } else {
            operation_id
        };
        let path = object.get("path").and_then(Value::as_str).unwrap_or("");
        if method != "GET"
            || matches!(
                operation_name.as_str(),
                "create" | "update" | "delete" | "submit"
            )
        {
            effects.push(
                if is_publish_route(
                    &action,
                    publish_operation,
                    &method,
                    path,
                    object.get("body"),
                ) {
                    Effect::Publish
                } else {
                    Effect::ExternalSideEffect
                },
            );
        }
        if matches!(action.as_str(), "openapi" | "operations")
            && ["operationId", "operation_id", "operation", "method", "path"]
                .iter()
                .any(|key| object.contains_key(*key))
            && !effects.contains(&Effect::ExternalSideEffect)
            && !effects.contains(&Effect::Publish)
        {
            // Synchronous MCP scope validation cannot resolve local OpenAPI
            // operation. Keep mutation coverage conservative; async dispatch
            // replaces it with resolved dry-run metadata before execution.
            effects.push(Effect::ExternalSideEffect);
            effects.push(Effect::Publish);
        }
        return Ok(effects);
    }
    if matches!(
        operation,
        "catalog"
            | "preflight"
            | "docs"
            | "build-analysis"
            | "flamegraph"
            | "flamegraph-json"
            | "build-log"
            | "build_log"
            | "memgraph.parse"
            | "memgraph-text"
            | "profile.parse"
            | "swiftui-trace"
    ) {
        return Ok(Vec::new());
    }
    let operation = canonical_operation(operation);
    if operation == "ui.read" {
        return Ok(vec![Effect::ProcessSpawn]);
    }
    if matches!(
        operation,
        "ui.tap"
            | "ui.type"
            | "ui.swipe"
            | "ui.key"
            | "ui.key_sequence"
            | "ui.button"
            | "ui.drag"
            | "ui.gesture"
            | "ui.long_press"
            | "ui.touch"
    ) {
        return Ok(vec![Effect::ProcessSpawn, Effect::ExternalSideEffect]);
    }
    let mut effects = vec![Effect::ProcessSpawn];
    if matches!(
        operation,
        "project.build"
            | "project.test"
            | "project.archive"
            | "project.export"
            | "simulator.install"
            | "device.install"
            | "simulator.screenshot"
            | "simulator.record_video"
            | "profile"
            | "profile.export"
    ) {
        effects.push(Effect::FileWrite);
    }
    if matches!(operation, "project.clean" | "swiftpm.clean") {
        effects.push(Effect::FileDelete);
    }
    if matches!(operation, "swiftpm.resolve" | "swiftpm.update") {
        effects.push(Effect::NetworkEgress);
    }
    if matches!(operation, "swiftpm.build" | "swiftpm.test" | "swiftpm.run") {
        // SwiftPM may write .build products & resolve package dependencies
        // during an ordinary build/test/run; no disable-resolution control is
        // exposed by native adapter, so both effects stay explicit.
        effects.push(Effect::FileWrite);
        effects.push(Effect::NetworkEgress);
    }
    if matches!(
        operation,
        "simulator.boot"
            | "simulator.install"
            | "simulator.launch"
            | "simulator.terminate"
            | "simulator.location"
            | "simulator.appearance"
            | "simulator.location_reset"
            | "simulator.statusbar"
            | "device.install"
            | "device.launch"
            | "device.terminate"
            | "mac.launch"
            | "mac.stop"
            | "debug.breakpoint"
            | "swiftpm.stop"
            | "debug.batch"
    ) {
        effects.push(Effect::ExternalSideEffect);
    }
    effects.sort();
    effects.dedup();
    Ok(effects)
}

pub fn canonical_operation(operation: &str) -> &str {
    match operation {
        "inspect.project" => "project.inspect",
        "list_schemes" => "project.schemes",
        "show_build_settings" => "project.settings",
        "list_destinations" => "project.destinations",
        "build" => "project.build",
        "build_sim" | "build_device" | "build_macos" => "project.build",
        "test" => "project.test",
        "test_sim" | "test_device" | "test_macos" => "project.test",
        "archive" => "project.archive",
        "export" => "project.export",
        "swift_package_build" => "swiftpm.build",
        "swift_package_test" => "swiftpm.test",
        "swift_package_run" => "swiftpm.run",
        "clean" => "project.clean",
        "swift_package_clean" => "swiftpm.clean",
        "simulator.list_devices" | "simulator.list_sims" => "simulator.list",
        "simulator.install_app" => "simulator.install",
        "simulator.launch_app" => "simulator.launch",
        "simulator.stop_app" => "simulator.terminate",
        "simulator.set_location" => "simulator.location",
        "simulator.set_appearance" => "simulator.appearance",
        "simulator.set_statusbar" => "simulator.statusbar",
        "set_sim_location" => "simulator.location",
        "set_sim_appearance" => "simulator.appearance",
        "sim_statusbar" => "simulator.statusbar",
        "reset_sim_location" => "simulator.location_reset",
        "record_sim_video" => "simulator.record_video",
        "boot_sim" => "simulator.boot",
        "list_sims" => "simulator.list",
        "install_app_sim" => "simulator.install",
        "launch_app_sim" => "simulator.launch",
        "stop_app_sim" => "simulator.terminate",
        "device.list_devices" => "device.list",
        "device.install_app" => "device.install",
        "device.launch_app" => "device.launch",
        "device.stop_app" | "stop_app_device" => "device.terminate",
        "install_app_device" => "device.install",
        "launch_app_device" => "device.launch",
        "launch_mac_app" => "mac.launch",
        "stop_mac_app" => "mac.stop",
        "swift_package_list" => "swiftpm.list",
        "swift_package_stop" => "swiftpm.stop",
        "tap" => "ui.tap",
        "type_text" | "type" => "ui.type",
        "swipe" => "ui.swipe",
        "key_press" | "key" => "ui.key",
        "key_sequence" => "ui.key_sequence",
        "button" => "ui.button",
        "drag" => "ui.drag",
        "gesture" => "ui.gesture",
        "long_press" => "ui.long_press",
        "touch" => "ui.touch",
        "snapshot_ui" => "ui.read",
        "symbolicate.atos" => "symbolicate",
        "debug_breakpoint_add" => "debug.breakpoint.add",
        "debug_breakpoint_remove" => "debug.breakpoint.remove",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Deny;

    impl Gate for Deny {
        fn authorize(&self, _: &Value, _: &Call<'_>) -> Result<Vec<Value>, String> {
            Err("denied".into())
        }
        fn validate_scope(&self, _: &Value, _: &[Effect]) -> Result<(), String> {
            Err("no context".into())
        }
    }

    #[test]
    fn read_only_operation_needs_no_gate() {
        let request = json!({"operation": "preflight", "arguments": {}});
        assert!(validate_scope(&Deny, &request).is_ok());
    }

    #[test]
    fn execution_defers_to_the_gate() {
        let request = json!({"operation": "project.build", "arguments": {"execute": true}});
        assert_eq!(validate_scope(&Deny, &request), Err("no context".into()));
    }

    #[test]
    fn malformed_requests_are_rejected() {
        assert!(request_parts(&json!([])).is_err());
        assert!(request_parts(&json!({"operation": " "})).is_err());
        assert!(request_parts(&json!({"operation": "x", "arguments": 1})).is_err());
        assert_eq!(
            request_parts(&json!({"operation": "x"})).unwrap().1,
            json!({})
        );
    }

    #[test]
    fn app_store_derives_network_and_mutation_independently_of_credentials() {
        let effects = required_effects(
            "app-store",
            &json!({"action": "request", "method": "POST", "execute": true}),
        )
        .unwrap();
        assert!(effects.contains(&Effect::CredentialAccess));
        assert!(effects.contains(&Effect::NetworkEgress));
        assert!(effects.contains(&Effect::ExternalSideEffect));
    }

    #[test]
    fn build_derives_process_and_write_effects() {
        let effects = required_effects("project.build", &json!({"execute": true})).unwrap();
        assert!(effects.contains(&Effect::ProcessSpawn));
        assert!(effects.contains(&Effect::FileWrite));
    }

    #[test]
    fn swiftpm_build_covers_products_and_dependency_resolution() {
        let effects = required_effects("swiftpm.build", &json!({"execute": true})).unwrap();
        assert!(effects.contains(&Effect::ProcessSpawn));
        assert!(effects.contains(&Effect::FileWrite));
        assert!(effects.contains(&Effect::NetworkEgress));
    }

    #[test]
    fn aliases_keep_ui_mutation_effects() {
        let effects = required_effects("tap", &json!({"execute": true})).unwrap();
        assert!(effects.contains(&Effect::ProcessSpawn));
        assert!(effects.contains(&Effect::ExternalSideEffect));
    }

    #[test]
    fn upload_keeps_external_effect_when_dry_plan_has_no_classifications() {
        let arguments = json!({"action": "upload", "artifact": "build/Demo.ipa", "execute": true});
        let coarse = required_effects("app-store", &arguments).unwrap();
        let plan = json!({
            "operation": "build-upload",
            "effectClassification": {"classifications": []}
        });
        let effects = effects_from_plan("app-store", &arguments, &plan, coarse);
        assert!(effects.contains(&Effect::CredentialAccess));
        assert!(effects.contains(&Effect::NetworkEgress));
        assert!(effects.contains(&Effect::ExternalSideEffect));
    }

    #[test]
    fn release_request_is_publish_even_without_submission_alias() {
        let effects = required_effects(
            "app-store",
            &json!({
                "action": "request",
                "method": "POST",
                "path": "/v1/appStoreVersionReleaseRequests",
                "execute": true
            }),
        )
        .unwrap();
        assert!(effects.contains(&Effect::Publish));
        assert!(!effects.contains(&Effect::ExternalSideEffect));
    }

    #[test]
    fn resolved_openapi_release_route_is_publish() {
        let arguments = json!({"action": "openapi", "method": "POST", "execute": true});
        let plan = json!({
            "request": {
                "method": "POST",
                "url": "https://api.appstoreconnect.apple.com/v1/appStoreVersionSubmissions",
                "body": {"data": {"type": "appStoreVersionSubmissions"}}
            },
            "mutation": true,
            "operationId": "createSubmission"
        });
        let effects = effects_from_plan(
            "app-store",
            &arguments,
            &plan,
            vec![Effect::CredentialAccess, Effect::NetworkEgress],
        );
        assert!(effects.contains(&Effect::Publish));
        assert!(!effects.contains(&Effect::ExternalSideEffect));
    }

    #[test]
    fn bound_targets_come_from_the_operation_not_from_network_paths() {
        let args = json!({"project": "App.xcodeproj", "derived_data_path": "dd"});
        let (targets, cwd) = bound_targets("project.build", &args, None).unwrap();
        assert_eq!(targets, ["App.xcodeproj", "dd"]);
        assert!(cwd.is_none());
        let (none, _) = bound_targets(
            "app-store",
            &json!({"action": "request", "path": "/v1/apps"}),
            None,
        )
        .unwrap();
        assert!(none.is_empty());
        let plan = json!({"cwd": "work"});
        let (from_cwd, cwd) = bound_targets("swiftpm.build", &json!({}), Some(&plan)).unwrap();
        assert_eq!(from_cwd, ["work"]);
        assert_eq!(cwd.as_deref(), Some("work"));
    }

    #[test]
    fn targets_resolve_below_the_working_folder() {
        let root = env::temp_dir().join(format!("rk-apple-gate-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let cwd = root.to_string_lossy().to_string();
        let dot = resolve_target(".", Some(&cwd)).unwrap();
        let fresh = resolve_target("build/Products", Some(&cwd)).unwrap();
        assert!(fresh.starts_with(&dot) && fresh.ends_with("build/Products"));
        assert!(!fresh.contains('\\'));
        let _ = std::fs::remove_dir_all(&root);
    }
}
