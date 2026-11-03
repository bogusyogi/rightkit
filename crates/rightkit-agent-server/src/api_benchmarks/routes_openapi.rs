use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::api::{lock_recover, now_iso, now_millis, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;

pub fn benchmark_routes() -> Router<DaemonState> {
    Router::new()
        .route("/runtime/benchmark", post(run_benchmark_handler))
        .route("/runtime/promote", post(run_promotion_handler))
        .route("/runtime/benchmarks", get(get_benchmarks))
        .route("/runtime/benchmarks/proof", get(get_benchmark_proof))
        .route(
            "/runtime/terminal-bench/tasks",
            get(get_terminal_bench_tasks),
        )
        .route(
            "/runtime/terminal-bench/run",
            post(run_terminal_bench_handler),
        )
        .route("/runtime/compare", post(run_compare_handler))
        .route("/runtime/compare/judge", post(judge_compare_handler))
        .route(
            "/runtime/executed-bakeoff",
            post(run_executed_bakeoff_handler),
        )
        .route("/runtime/corpus-bakeoff", post(run_corpus_bakeoff_handler))
}

pub(crate) fn benchmark_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "BenchmarkRunRequest".to_string(),
        json!({
            "type": "object",
            "required": ["sessionId", "role", "goal"],
            "properties": {
                "sessionId": {"type": "string"},
                "role": {"type": "string"},
                "model": {"type": "string"},
                "goal": {"type": "string"},
                "repeats": {"type": "integer", "minimum": 1}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BenchmarkRunResult".to_string(),
        json!({
            "type": "object",
            "required": ["role", "model", "repeats", "successes", "successRate"],
            "properties": {
                "role": {"type": "string"},
                "model": {"type": "string"},
                "repeats": {"type": "integer"},
                "successes": {"type": "integer"},
                "successRate": {"type": "number"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PromotionRunRequest".to_string(),
        json!({
            "type": "object",
            "required": ["sessionId", "role", "challengerModel", "goal"],
            "properties": {
                "sessionId": {"type": "string"},
                "role": {"type": "string"},
                "challengerModel": {"type": "string"},
                "goal": {"type": "string"},
                "repeats": {"type": "integer", "minimum": 1}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PromotionRunResult".to_string(),
        json!({
            "type": "object",
            "required": ["promoted"],
            "properties": {
                "promoted": {"type": "boolean"},
                "role": {"type": "string"},
                "fromModel": {"type": "string"},
                "toModel": {"type": "string"},
                "delta": {"type": "number"},
                "reason": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "BenchmarkProofResponse".to_string(),
        json!({
            "type": "object",
            "required": ["generatedAt", "totalReports", "totalRepeats", "totalSuccesses", "successRate", "suites", "roleModels"],
            "properties": {
                "generatedAt": {"type": "string"},
                "totalReports": {"type": "integer"},
                "totalRepeats": {"type": "integer"},
                "totalSuccesses": {"type": "integer"},
                "successRate": {"type": "number"},
                "suites": {"type": "array", "items": {"type": "object"}},
                "roleModels": {"type": "array", "items": {"type": "object"}},
                "terminalBench": {"type": "object"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "TerminalBenchSetupFile".to_string(),
        json!({
            "type": "object",
            "required": ["name", "content"],
            "properties": {
                "name": {"type": "string"},
                "content": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "TerminalBenchTask".to_string(),
        json!({
            "type": "object",
            "required": ["id", "category", "difficulty", "prompt", "setupFiles", "verifyCommand", "expectedPassRate"],
            "properties": {
                "id": {"type": "string"},
                "category": {"type": "string"},
                "difficulty": {"type": "string"},
                "prompt": {"type": "string"},
                "setupFiles": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/TerminalBenchSetupFile"}
                },
                "verifyCommand": {"type": "string"},
                "expectedPassRate": {"type": "number"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "TerminalBenchTaskListResponse".to_string(),
        json!({
            "type": "array",
            "items": {"$ref": "#/components/schemas/TerminalBenchTask"}
        }),
    );
    schemas.insert(
        "TerminalBenchRunRequest".to_string(),
        json!({
            "type": "object",
            "required": ["sessionId", "taskId", "agent"],
            "properties": {
                "sessionId": {"type": "string"},
                "taskId": {"type": "string"},
                "agent": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "TerminalBenchRunResult".to_string(),
        json!({
            "allOf": [
                {"$ref": "#/components/schemas/BenchmarkRunResult"},
                {
                    "type": "object",
                    "required": ["benchmarkSuite", "taskId", "passed", "agentOutput", "verifyOutput", "verifyError"],
                    "properties": {
                        "benchmarkSuite": {"type": "string"},
                        "taskId": {"type": "string"},
                        "passed": {"type": "boolean"},
                        "agentOutput": {"type": "string"},
                        "verifyOutput": {"type": "string"},
                        "verifyError": {"type": "string"}
                    },
                    "additionalProperties": false
                }
            ]
        }),
    );
    schemas.insert(
        "RuntimeCompareRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string"},
                "models": {"type": "array", "items": {"type": "string"}},
                "task": {"type": "string"},
                "variantA": {"type": "string"},
                "variantB": {"type": "string"},
                "workspacePath": {"type": "string"},
                "maxTurns": {"type": "integer"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeCompareEntry".to_string(),
        json!({
            "type": "object",
            "required": ["label", "output"],
            "properties": {
                "label": {"type": "string"},
                "output": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeCompareResult".to_string(),
        json!({
            "type": "object",
            "required": ["id", "entries"],
            "properties": {
                "id": {"type": "string"},
                "prompt": {"type": "string"},
                "task": {"type": "string"},
                "variantA": {"type": "string"},
                "variantB": {"type": "string"},
                "entries": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/RuntimeCompareEntry"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeCompareJudgeRequest".to_string(),
        json!({
            "type": "object",
            "required": ["id", "winner"],
            "properties": {
                "id": {"type": "string"},
                "winner": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeCompareJudgeMapping".to_string(),
        json!({
            "type": "object",
            "required": ["label", "model"],
            "properties": {
                "label": {"type": "string"},
                "model": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RuntimeCompareJudgeResult".to_string(),
        json!({
            "type": "object",
            "required": ["winnerModel", "winnerLabel", "field", "mapping"],
            "properties": {
                "winnerModel": {"type": "string"},
                "winnerLabel": {"type": "string"},
                "field": {"type": "array", "items": {"type": "string"}},
                "mapping": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/RuntimeCompareJudgeMapping"}
                }
            },
            "additionalProperties": false
        }),
    );
    insert_executed_bakeoff_openapi_components(&mut schemas);
    insert_corpus_bakeoff_openapi_components(&mut schemas);
    schemas
}

fn benchmark_schema_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

pub(crate) fn benchmark_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/runtime/benchmark") => Some(benchmark_schema_ref("BenchmarkRunRequest")),
        ("post", "/runtime/promote") => Some(benchmark_schema_ref("PromotionRunRequest")),
        ("post", "/runtime/terminal-bench/run") => {
            Some(benchmark_schema_ref("TerminalBenchRunRequest"))
        }
        ("post", "/runtime/compare") => Some(benchmark_schema_ref("RuntimeCompareRequest")),
        ("post", "/runtime/compare/judge") => {
            Some(benchmark_schema_ref("RuntimeCompareJudgeRequest"))
        }
        ("post", "/runtime/executed-bakeoff") => {
            Some(benchmark_schema_ref("ExecutedBakeoffRequest"))
        }
        ("post", "/runtime/corpus-bakeoff") => Some(benchmark_schema_ref("CorpusBakeoffRequest")),
        _ => None,
    }
}

pub(crate) fn benchmark_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/runtime/benchmark") => Some(benchmark_schema_ref("BenchmarkRunResult")),
        ("post", "/runtime/promote") => Some(benchmark_schema_ref("PromotionRunResult")),
        ("get", "/runtime/benchmarks/proof") => {
            Some(benchmark_schema_ref("BenchmarkProofResponse"))
        }
        ("get", "/runtime/terminal-bench/tasks") => {
            Some(benchmark_schema_ref("TerminalBenchTaskListResponse"))
        }
        ("post", "/runtime/terminal-bench/run") => {
            Some(benchmark_schema_ref("TerminalBenchRunResult"))
        }
        ("post", "/runtime/compare") => Some(benchmark_schema_ref("RuntimeCompareResult")),
        ("post", "/runtime/compare/judge") => {
            Some(benchmark_schema_ref("RuntimeCompareJudgeResult"))
        }
        ("post", "/runtime/executed-bakeoff") => {
            Some(benchmark_schema_ref("ExecutedBakeoffResponse"))
        }
        ("post", "/runtime/corpus-bakeoff") => Some(benchmark_schema_ref("CorpusBakeoffResponse")),
        _ => None,
    }
}
