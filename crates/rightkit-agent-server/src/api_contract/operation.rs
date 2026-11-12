fn openapi_operation(endpoint: &ApiEndpoint, openapi_path: &str, method: &str) -> Value {
    let mut operation = Map::new();
    operation.insert(
        "operationId".to_string(),
        Value::String(operation_id(method, openapi_path)),
    );
    operation.insert(
        "summary".to_string(),
        Value::String(endpoint.summary.to_string()),
    );
    if method == "post"
        && matches!(
            endpoint.path,
            "/triggers/{id}/deliver/github" | "/triggers/{id}/deliver/generic"
        )
    {
        // Trigger delivery uses provider HMAC. The driver-capability gateway
        // routes still require the daemon bearer (global middleware); their
        // scoped lease + instance headers are declared as parameters below.
        operation.insert("security".to_string(), Value::Array(Vec::new()));
    }

    let response_schema =
        openapi_response_schema(method, endpoint.path).unwrap_or_else(|| json!({}));
    let (success_status, success_description) = match (method, endpoint.path) {
        ("post", "/triggers") => ("201", "Created"),
        ("post", "/specs") => ("201", "Created"),
        ("post", "/recipes/install" | "/recipes/install-file" | "/recipes/install-url") => {
            ("201", "Created")
        }
        ("post", "/triggers/{id}/deliver/github" | "/triggers/{id}/deliver/generic") => {
            ("202", "Accepted")
        }
        ("post", "/provider-auth/{provider}/login") => ("202", "Accepted"),
        _ => ("200", "Successful response"),
    };
    let mut responses = Map::new();
    if endpoint.path == "/workspace/watch" {
        // Server-sent event stream: there is no application/json contract to
        // validate, so the document declares the real media type instead of a
        // JSON schema the handler could never satisfy.
        responses.insert(
            success_status.to_string(),
            json!({
                "description": success_description,
                "content": {
                    "text/event-stream": {
                        "schema": {"type": "string", "description": "SSE event stream"}
                    }
                }
            }),
        );
    } else {
        responses.insert(
            success_status.to_string(),
            json!({
                "description": success_description,
                "content": {
                    "application/json": {
                        "schema": response_schema
                    }
                }
            }),
        );
    }
    if let Some(unavailable_schema) = unavailable_response_schema(method, endpoint.path) {
        responses.insert(
            "503".to_string(),
            json!({
                "description": "Service unavailable",
                "content": {
                    "application/json": {
                        "schema": unavailable_schema
                    }
                }
            }),
        );
    }
    operation.insert("responses".to_string(), Value::Object(responses));

    let mut parameters = path_parameters(openapi_path);
    parameters.extend(trigger_delivery_header_parameters(endpoint.path));
    parameters.extend(driver_capability_header_parameters(endpoint.path));
    if !parameters.is_empty() {
        operation.insert("parameters".to_string(), Value::Array(parameters));
    }

    if matches!(method, "post" | "put" | "patch")
        || openapi_request_schema(method, endpoint.path).is_some()
    {
        let request_schema =
            openapi_request_schema(method, endpoint.path).unwrap_or_else(|| json!({}));
        let request_body_required = matches!(
            endpoint.path,
            "/triggers"
                | "/triggers/{id}"
                | "/triggers/{id}/deliver/github"
                | "/triggers/{id}/deliver/generic"
                | "/delivery/worktree"
                | "/delivery/stage"
                | "/runtime/executed-bakeoff"
                | "/runtime/corpus-bakeoff"
                | "/runtime/context/adapt/review-taste"
                | "/runtime/context/adapt/adjudicate-taste"
                | "/runtime/context/adapt/apply-insights"
                | "/runtime/context/adapt/insights"
                | "/runtime/context/adapt/apply-taste"
                | "/runtime/context/adapt/recall"
                | "/sessions/{id}/clone"
        );
        operation.insert(
            "requestBody".to_string(),
            json!({
                "required": request_body_required,
                "content": {
                    "application/json": {
                        "schema": request_schema
                    }
                }
            }),
        );
    }

    Value::Object(operation)
}

fn driver_capability_header_parameters(path: &str) -> Vec<Value> {
    let capability = matches!(
        path,
        "/sessions/{id}/driver-capabilities/tools"
            | "/sessions/{id}/driver-capabilities/call"
            | "/sessions/{id}/driver/start"
            | "/sessions/{id}/driver/prompt"
            | "/sessions/{id}/driver/interrupt"
            | "/sessions/{id}/driver/resume"
            | "/sessions/{id}/driver/stop"
    );
    if !capability {
        return Vec::new();
    }
    let mut parameters = vec![json!({
        "in": "header",
        "name": crate::host_identity::header("capability-token"),
        "required": true,
        "schema": {"type": "string"}
    })];
    if matches!(
        path,
        "/sessions/{id}/driver-capabilities/tools" | "/sessions/{id}/driver-capabilities/call"
    ) {
        parameters.push(json!({
            "in": "header",
            "name": crate::host_identity::header("driver-instance"),
            "required": true,
            "schema": {"type": "string"}
        }));
    }
    parameters
}

/// Machine-status runtime handlers (`src/api_health.rs`,
/// `machine_status`/`machine_info`) both return `machine_unavailable_response`
/// with HTTP 503 before the daemon has finished initializing (ra-004). The
/// success-only match in `response_schemas.rs` has no slot for a second
/// status code per operation, so the 503 contract is documented here,
/// alongside the other per-operation exceptions in this file, against the
/// same `rightkit_agent_engine_api_platform::openapi` schema the runtime error body is
/// shaped from.
fn unavailable_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/machine/status") | ("get", "/machine/info") => Some(
            rightkit_agent_engine_api_platform::openapi::machine_status_unavailable_response_schema(
            ),
        ),
        _ => None,
    }
}

fn trigger_delivery_header_parameters(path: &str) -> Vec<Value> {
    let (signature, delivery, event) = match path {
        "/triggers/{id}/deliver/github" => {
            ("X-Hub-Signature-256", "X-GitHub-Delivery", "X-GitHub-Event")
        }
        "/triggers/{id}/deliver/generic" => (
            "X-RightKit-Signature-256",
            "X-RightKit-Delivery-Id",
            "X-RightKit-Event",
        ),
        _ => return Vec::new(),
    };
    vec![
        json!({
            "in": "header",
            "name": signature,
            "required": true,
            "description": "HMAC-SHA256 over the exact raw request body.",
            "schema": {"type": "string", "pattern": "^sha256=[0-9A-Fa-f]{64}$"}
        }),
        json!({
            "in": "header",
            "name": delivery,
            "required": true,
            "description": "Provider-unique delivery ID used for replay prevention.",
            "schema": {"type": "string", "pattern": "^[A-Za-z0-9_.:-]{1,128}$"}
        }),
        json!({
            "in": "header",
            "name": event,
            "required": false,
            "description": "Optional provider event name.",
            "schema": {"type": "string", "pattern": "^[!-~]{1,128}$"}
        }),
    ]
}
