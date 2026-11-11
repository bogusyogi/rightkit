use std::io::Write as _;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdaptScopeQuery {
    #[serde(default = "default_adapt_scope")]
    scope: String,
}

fn default_adapt_scope() -> String {
    "global".into()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewTasteBody {
    input: std::path::PathBuf,
    installation_id: String,
    canonical_pool_sha256: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdjudicateTasteBody {
    manifest: Option<std::path::PathBuf>,
    manifest_value: Option<Value>,
    decisions: std::path::PathBuf,
    validated_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyInsightsBody {
    input: std::path::PathBuf,
    installation_id: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InsightsBody {
    #[serde(default)]
    pub(crate) session_ids: Vec<String>,
    #[serde(default)]
    pub(crate) workspace_root: Option<String>,
    #[serde(default = "default_adapt_scope")]
    pub(crate) scope: String,
    #[serde(default = "default_min_recurrence")]
    pub(crate) min_recurrence: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyTasteBody {
    manifest: Option<std::path::PathBuf>,
    manifest_value: Option<Value>,
    #[serde(default)]
    dry_run: bool,
}

enum ManifestInput {
    Path(std::path::PathBuf),
    Inline(Value),
}

fn manifest_input(
    path: Option<std::path::PathBuf>,
    value: Option<Value>,
) -> Result<ManifestInput, String> {
    match (path, value) {
        (Some(_), Some(_)) => Err("provide exactly one of manifest or manifestValue".into()),
        (None, None) => Err("one of manifest or manifestValue is required".into()),
        (Some(path), None) => Ok(ManifestInput::Path(path)),
        (None, Some(value)) => Ok(ManifestInput::Inline(value)),
    }
}

fn with_manifest_path<T>(
    input: ManifestInput,
    call: impl FnOnce(std::path::PathBuf) -> Result<T, rightkit_memory_client::membrane::MembraneError>,
) -> Result<T, rightkit_memory_client::membrane::MembraneError> {
    match input {
        ManifestInput::Path(path) => call(path),
        ManifestInput::Inline(value) => {
            let mut file = tempfile::NamedTempFile::new().map_err(|error| {
                rightkit_memory_client::membrane::MembraneError::Spawn(format!(
                    "create temporary Adapt manifest: {error}"
                ))
            })?;
            serde_json::to_writer(&mut file, &value).map_err(|error| {
                rightkit_memory_client::membrane::MembraneError::Parse(format!(
                    "serialize inline Adapt manifest: {error}"
                ))
            })?;
            file.flush().map_err(|error| {
                rightkit_memory_client::membrane::MembraneError::Spawn(format!(
                    "flush temporary Adapt manifest: {error}"
                ))
            })?;
            call(file.path().to_path_buf())
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecallBody {
    query: String,
    #[serde(default = "default_recall_k")]
    k: usize,
    #[serde(default = "default_adapt_scope")]
    scope: String,
    #[serde(default)]
    dimensions: Vec<String>,
}

/// No Membrane CLI equivalent yet (Orthic-Labs/pulse#129).
const ADAPT_NOT_AVAILABLE: &str = "not available via Membrane CLI (Orthic-Labs/pulse#129)";

async fn get_adapt_status(
    Query(_query): Query<AdaptScopeQuery>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    Err(adapt_not_available())
}

async fn post_adapt_review_taste(
    State(state): State<DaemonState>,
    Json(body): Json<ReviewTasteBody>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    run_adapt(&state, move |backend| {
        backend.adapt_taste_review(
            &body.input,
            &body.installation_id,
            &body.created_at,
            body.canonical_pool_sha256.as_deref(),
        )
    })
    .await
}

async fn post_adapt_adjudicate_taste(
    Json(_body): Json<AdjudicateTasteBody>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    Err(adapt_not_available())
}

async fn post_adapt_apply_insights(
    State(state): State<DaemonState>,
    Json(body): Json<ApplyInsightsBody>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    run_adapt(&state, move |backend| {
        backend.adapt_apply_insights(&body.input, &body.installation_id, body.dry_run)
    })
    .await
}

/// `mine_session` has no Membrane CLI equivalent yet (Orthic-Labs/pulse#129); bounds are still
/// validated so callers get a request error before the availability error.
pub(crate) async fn post_adapt_insights(
    State(_state): State<DaemonState>,
    Json(body): Json<InsightsBody>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    if body.session_ids.len() > 128
        || body.session_ids.iter().any(|id| id.trim().is_empty())
        || body.scope.trim().is_empty()
        || !(1..=100).contains(&body.min_recurrence)
    {
        return Err(adapt_bad_request("invalid Insights mining bounds".into()));
    }
    Err(adapt_not_available())
}

async fn post_adapt_apply_taste(
    State(state): State<DaemonState>,
    Json(body): Json<ApplyTasteBody>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let manifest = manifest_input(body.manifest, body.manifest_value).map_err(adapt_bad_request)?;
    run_adapt(&state, move |backend| {
        with_manifest_path(manifest, |path| {
            backend.adapt_apply_manifest(&path, body.dry_run)
        })
    })
    .await
}

async fn post_adapt_recall(
    State(state): State<DaemonState>,
    Json(body): Json<RecallBody>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    run_adapt(&state, move |backend| {
        backend.adapt_recall(&body.query, body.k, &body.scope, &body.dimensions)
    })
    .await
}

/// Run one bounded Membrane CLI call off the async runtime (the adapter enforces its timeout).
async fn run_adapt<F>(
    state: &DaemonState,
    call: F,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)>
where
    F: FnOnce(
            &dyn rightkit_memory_client::membrane::MembraneBackend,
        ) -> Result<Value, rightkit_memory_client::membrane::MembraneError>
        + Send
        + 'static,
{
    let backend = state.membrane.clone();
    tokio::task::spawn_blocking(move || call(&*backend))
        .await
        .map_err(|_| adapt_unavailable())?
        .map(Json)
        .map_err(adapt_error)
}

fn default_recall_k() -> usize {
    6
}
fn default_min_recurrence() -> u32 {
    2
}

fn adapt_unavailable() -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "schemaVersion": "coderight.context-adapt.v1",
            "error": "adapt_unavailable",
            "reason": rightkit_memory_client::membrane::OFF_REASON
        })),
    )
}

fn adapt_not_available() -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "schemaVersion": "coderight.context-adapt.v1",
            "error": "adapt_not_available",
            "reason": ADAPT_NOT_AVAILABLE
        })),
    )
}

fn adapt_error(
    error: rightkit_memory_client::membrane::MembraneError,
) -> (axum::http::StatusCode, Json<Value>) {
    use rightkit_memory_client::membrane::MembraneError;
    if matches!(error, MembraneError::Unavailable(_)) {
        return adapt_unavailable();
    }
    let status = match error {
        MembraneError::Usage(_) => axum::http::StatusCode::BAD_REQUEST,
        MembraneError::Timeout(_) => axum::http::StatusCode::GATEWAY_TIMEOUT,
        _ => axum::http::StatusCode::BAD_GATEWAY,
    };
    (
        status,
        Json(serde_json::json!({
            "schemaVersion": "coderight.context-adapt.v1",
            "error": "adapt_operation_failed",
            "detail": error.to_string()
        })),
    )
}

fn adapt_bad_request(message: String) -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "schemaVersion": "coderight.context-adapt.v1",
            "error": "invalid_adapt_request",
            "reason": message,
        })),
    )
}
