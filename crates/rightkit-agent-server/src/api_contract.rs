use axum::{response::Json, routing::get, Router};
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::api::DaemonState;

pub(crate) fn api_contract_routes() -> Router<DaemonState> {
    Router::new()
        .route("/api/contract", get(get_api_contract_handler))
        .route("/api/openapi.json", get(get_openapi_handler))
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiEndpoint {
    pub method: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
}

include!("api_contract/endpoints.rs");

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiContract {
    name: &'static str,
    version: u32,
    endpoints: &'static [ApiEndpoint],
}

async fn get_api_contract_handler() -> Json<ApiContract> {
    Json(ApiContract {
        name: "coderight-daemon",
        version: 1,
        endpoints: DAEMON_API_ENDPOINTS,
    })
}

async fn get_openapi_handler() -> Json<Value> {
    Json(openapi_document())
}

include!("api_contract/components.rs");

include!("api_contract/run_changes_components.rs");

include!("api_contract/document.rs");

include!("api_contract/operation.rs");

include!("api_contract/request_schemas.rs");

include!("api_contract/response_schemas.rs");

include!("api_contract/path_helpers.rs");

include!("api_contract/tests.rs");
