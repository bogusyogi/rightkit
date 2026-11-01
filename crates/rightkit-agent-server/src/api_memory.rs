use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::get,
    Router,
};
use coderight_skills::CommandResult;
use rightkit_memory::backend::{CallContext, MemoryBackendError, MemoryEntry};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

pub fn memory_routes() -> Router<DaemonState> {
    Router::new()
        .route("/memory", get(list_memory_handler).post(add_memory_handler))
        .route(
            "/memory/consolidation-status",
            get(consolidation_status_handler),
        )
}

pub(crate) fn memory_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "MemoryCausalProvenance".to_string(),
        json!({
            "type": "object",
            "required": ["origin", "source", "approved"],
            "properties": {
                "origin": {
                    "type": "string",
                    "enum": ["legacy", "session-origin", "tool-derived", "web-derived", "mcp-derived"]
                },
                "source": {"type": "string"},
                "sessionId": {"type": "string"},
                "approved": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "MemoryEntryRow".to_string(),
        json!({
            "type": "object",
            "required": ["id", "tier", "content", "keywords", "score", "createdAt", "accessCount"],
            "properties": {
                "id": {"type": "string"},
                "tier": {"type": "string"},
                "content": {"type": "string"},
                "keywords": {"type": "array", "items": {"type": "string"}},
                "score": {"type": "number"},
                "createdAt": {"type": "string"},
                "accessCount": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "MemoryListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["query", "limit", "count", "entries"],
            "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1},
                "count": {"type": "integer", "minimum": 0},
                "entries": {"type": "array", "items": {"$ref": "#/components/schemas/MemoryEntryRow"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AddMemoryRequest".to_string(),
        json!({
            "type": "object",
            "required": ["content"],
            "properties": {
                "content": {"type": "string"},
                "keywords": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AddMemoryResponse".to_string(),
        json!({
            "type": "object",
            "required": ["added", "entry"],
            "properties": {
                "added": {"type": "boolean"},
                "entry": {"$ref": "#/components/schemas/MemoryEntryRow"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "MemoryConsolidationStatusResponse".to_string(),
        memory_consolidation_status_openapi_response_schema(),
    );
    schemas
}

pub(crate) fn memory_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/memory") => Some(json!({"$ref": "#/components/schemas/AddMemoryRequest"})),
        _ => None,
    }
}

pub(crate) fn memory_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/memory") => Some(json!({"$ref": "#/components/schemas/MemoryListResponse"})),
        ("post", "/memory") => Some(json!({"$ref": "#/components/schemas/AddMemoryResponse"})),
        ("get", "/memory/consolidation-status") => {
            Some(json!({"$ref": "#/components/schemas/MemoryConsolidationStatusResponse"}))
        }
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryQuery {
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_memory_limit")]
    pub limit: usize,
}

fn default_memory_limit() -> usize {
    20
}

fn memory_call_context() -> CallContext {
    rightkit_memory::api_memright::current_memory_call_context()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMemoryRequest {
    pub content: String,
    #[serde(default)]
    pub keywords: Vec<String>,
}

async fn list_memory_handler(
    State(state): State<DaemonState>,
    Query(q): Query<MemoryQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let limit = q.limit.clamp(1, 100);
    let candidate_limit = limit.saturating_mul(2);
    let query = q.query.trim().to_string();
    let entries = if query.is_empty() {
        state
            .memory_backend
            .entries(&memory_call_context(), candidate_limit)
            .await
    } else {
        state
            .memory_backend
            .search(&memory_call_context(), &query, candidate_limit)
            .await
    };
    let entries = match entries {
        Ok(entries) => entries
            .into_iter()
            .filter(|entry| !rightkit_memory::memory_provenance::is_provenance_id(&entry.id))
            .take(limit)
            .collect::<Vec<_>>(),
        Err(error) => {
            let status = memory_backend_status(error.clone());
            return (
                status,
                Json(json!({"error": error.to_string(), "code": error.code()})),
            );
        }
    };
    let rows = entries
        .iter()
        .map(memory_entry_json)
        .collect::<Vec<serde_json::Value>>();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "query": query,
            "limit": limit,
            "count": rows.len(),
            "entries": rows,
        })),
    )
}

async fn add_memory_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<AddMemoryRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if req.content.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let entry = match state
        .memory_backend
        .remember(&memory_call_context(), &req.content, req.keywords)
        .await
    {
        Ok(entry) => entry,
        Err(error) => return Err(memory_backend_status(error)),
    };
    Ok(Json(serde_json::json!({
        "added": true,
        "entry": memory_entry_json(&entry),
    })))
}

/// FM-074 roster-status projection: reports the daily-analyzer consolidation
/// cycle's last-run outcome and lifetime counters (see
/// `rightkit_memory::ConsolidationStatusState`, populated by
/// `learning::daily_analysis::run_daily_analysis`). Requires
/// `state.memory_consolidation_status`, a hub-owned `DaemonState` field this
/// lane does not add — see the A11 wiring doc.
async fn consolidation_status_handler(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let snapshot = state.memory_consolidation_status.snapshot();
    Json(serde_json::to_value(&snapshot).unwrap_or(serde_json::json!({})))
}

pub(crate) fn memory_consolidation_status_openapi_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "lastRunStartedAt": {"type": ["string", "null"]},
            "lastRunCompletedAt": {"type": ["string", "null"]},
            "lastRunOk": {"type": "boolean"},
            "lastError": {"type": ["string", "null"]},
            "reviewCandidatesSeen": {"type": "integer", "minimum": 0},
            "memoriesWritten": {"type": "integer", "minimum": 0},
            "repeatedFailurePatterns": {"type": "integer", "minimum": 0},
            "proposalsMinted": {"type": "integer", "minimum": 0},
            "totalRuns": {"type": "integer", "minimum": 0},
            "totalMemoriesWritten": {"type": "integer", "minimum": 0}
        },
        "additionalProperties": false
    })
}

fn memory_entry_json(entry: &MemoryEntry) -> serde_json::Value {
    serde_json::json!({
        "id": entry.id,
        "tier": entry.tier,
        "content": entry.content,
        "keywords": entry.keywords,
        "score": entry.score,
        "createdAt": entry.created_at,
        "accessCount": entry.access_count,
    })
}

pub async fn render_memory_command(state: &DaemonState, args: &[String]) -> CommandResult {
    let query = args.join(" ");
    let query = query.trim();
    if query.is_empty() {
        // The Membrane CLI cannot return memory text without a query (`entries` is not
        // available), so list the ids and tiers it does expose and say so.
        return match state
            .memory_backend
            .list(&memory_call_context(), None)
            .await
        {
            Ok(rows) => {
                let rows = rows
                    .into_iter()
                    .filter(|row| !rightkit_memory::memory_provenance::is_provenance_id(&row.id))
                    .collect::<Vec<_>>();
                if rows.is_empty() {
                    return command_ok("No durable memories recorded yet.");
                }
                let mut output = format!(
                    "**Memory:** {} durable entries (text needs a query: /memory <query>)",
                    rows.len()
                );
                for row in rows.iter().take(20) {
                    output.push_str(&format!(
                        "
- {} [{}] {} chars",
                        row.id,
                        row.tier.as_str(),
                        row.chars
                    ));
                }
                command_ok(output)
            }
            Err(error) => command_error(format!("memory lookup failed: {error}")),
        };
    }
    let entries = {
        state
            .memory_backend
            .search(&memory_call_context(), query, 20)
            .await
    };
    let entries = match entries {
        Ok(entries) => entries
            .into_iter()
            .filter(|entry| !rightkit_memory::memory_provenance::is_provenance_id(&entry.id))
            .take(10)
            .collect::<Vec<_>>(),
        Err(error) => return command_error(format!("memory lookup failed: {error}")),
    };
    if entries.is_empty() {
        return command_ok(if query.is_empty() {
            "No durable memories recorded yet."
        } else {
            "No matching durable memories."
        });
    }

    let mut output = if query.is_empty() {
        format!("**Memory:** {} durable entries", entries.len())
    } else {
        format!(
            "**Memory:** {} durable entries matching '{}'",
            entries.len(),
            query
        )
    };
    for entry in entries {
        let keywords = if entry.keywords.is_empty() {
            String::new()
        } else {
            format!(" [{}]", entry.keywords.join(", "))
        };
        output.push_str(&format!("\n- {}{}: {}", entry.id, keywords, entry.content));
    }
    command_ok(output)
}

fn command_ok(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: true,
        output: output.into(),
        action: coderight_skills::CommandAction::None,
    }
}

fn command_error(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: false,
        output: output.into(),
        action: coderight_skills::CommandAction::None,
    }
}

fn memory_backend_status(error: MemoryBackendError) -> StatusCode {
    match error.kind() {
        rightkit_memory::backend::MemoryBackendErrorKind::Invalid => StatusCode::BAD_REQUEST,
        rightkit_memory::backend::MemoryBackendErrorKind::Denied => StatusCode::FORBIDDEN,
        rightkit_memory::backend::MemoryBackendErrorKind::BackendUnavailable
        | rightkit_memory::backend::MemoryBackendErrorKind::Unavailable
        | rightkit_memory::backend::MemoryBackendErrorKind::CommitUnknown
        | rightkit_memory::backend::MemoryBackendErrorKind::Timeout
        | rightkit_memory::backend::MemoryBackendErrorKind::Cancelled => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[test]
    fn memory_openapi_components_cover_routes() {
        let components = memory_openapi_components();
        for name in [
            "MemoryEntryRow",
            "MemoryListResponse",
            "AddMemoryRequest",
            "AddMemoryResponse",
        ] {
            assert!(components.contains_key(name), "{name} schema missing");
        }
        assert_eq!(
            memory_openapi_request_schema("post", "/memory").unwrap()["$ref"],
            "#/components/schemas/AddMemoryRequest"
        );
        assert_eq!(
            memory_openapi_response_schema("get", "/memory").unwrap()["$ref"],
            "#/components/schemas/MemoryListResponse"
        );
        assert_eq!(
            components["MemoryListResponse"]["properties"]["entries"]["items"]["$ref"],
            "#/components/schemas/MemoryEntryRow"
        );
    }

    #[test]
    fn memory_consolidation_status_schema_is_registered() {
        let components = memory_openapi_components();
        assert!(components.contains_key("MemoryConsolidationStatusResponse"));
        assert_eq!(
            memory_openapi_response_schema("get", "/memory/consolidation-status").unwrap()["$ref"],
            "#/components/schemas/MemoryConsolidationStatusResponse"
        );
    }

    #[tokio::test]
    async fn memory_write_rejects_lower_trust_sources() {
        let memory = crate::test_support::FakeMemoryBackend::new();
        let provenance =
            rightkit_memory::memory_provenance::MemoryProvenanceStore::from_backend(memory.clone());
        let app = crate::api::build_router(
            DaemonState::new("")
                .with_memory_backend(memory)
                .with_memory_provenance(provenance),
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/memory")
                    .header("content-type", "application/json")
                    .header("x-coderight-source-trust", "toolUntrusted")
                    .body(Body::from(
                        serde_json::json!({
                            "content": "untrusted memory should not persist",
                            "keywords": ["blocked"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let list = app
            .oneshot(
                Request::builder()
                    .uri("/memory")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = list.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["count"], 0);
    }
}
