//! The benchmark harness when the `benchmarks` feature is off.
//!
//! The harness (`/runtime/benchmark`, `/runtime/compare`, the terminal-bench task pack, the
//! executed and corpus bake-offs, the soak run) is an evaluation surface a product opts into.
//! Without the feature none of its routes exist, `/benchmarks` reports none recorded,
//! `/terminal-bench` and a skill proposal's terminal-bench run answer `501 Not Implemented`, and a
//! role benchmark run for a skill proposal is scored but not added to the benchmark history.

use axum::http::StatusCode;
use axum::Router;
use serde_json::{Map, Value};

use crate::api::DaemonState;

/// No benchmark routes.
pub(crate) fn benchmark_routes() -> Router<DaemonState> {
    Router::new()
}

pub(crate) mod soak_routes {
    use crate::api::DaemonState;
    use axum::Router;

    /// No soak route.
    pub(crate) fn soak_routes() -> Router<DaemonState> {
        Router::new()
    }
}

pub(crate) fn benchmark_openapi_components() -> Map<String, Value> {
    Map::new()
}

pub(crate) fn benchmark_openapi_request_schema(_method: &str, _path: &str) -> Option<Value> {
    None
}

pub(crate) fn benchmark_openapi_response_schema(_method: &str, _path: &str) -> Option<Value> {
    None
}

/// The benchmark history is not kept without the harness.
pub(crate) async fn record_benchmark_history(
    _state: &DaemonState,
    _report: &rightkit_agent_orchestration::coding::BenchmarkReport,
) {
}

pub(crate) async fn benchmark_history_json(_state: &DaemonState) -> Value {
    Value::Array(Vec::new())
}

pub(crate) async fn benchmark_proof_json(_state: &DaemonState) -> Value {
    serde_json::json!({ "totalRepeats": 0, "totalSuccesses": 0, "successRate": 0.0 })
}

/// A terminal-bench run request (same shape as with the harness).
pub struct TerminalBenchRunRequest {
    /// Task id.
    pub task_id: String,
    /// Agent role.
    pub agent: String,
    /// Session id.
    pub session_id: String,
}

pub(crate) async fn run_terminal_bench_task(
    _state: &DaemonState,
    _body: TerminalBenchRunRequest,
) -> Result<Value, StatusCode> {
    Err(StatusCode::NOT_IMPLEMENTED)
}
