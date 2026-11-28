//! Cloud forge seam (cloud-git-connector.md, GitHub first): the daemon's own GitHub connection,
//! the App installations the signed-in user can reach, and the repositories both the App
//! installation and the user can reach.
//!
//! Credential rules (a mistake here still compiles and passes a smoke test, so they are stated):
//! - the GitHub token comes ONLY from `crate::github_signin::resolve_access_token()`;
//! - the API base comes ONLY from the start-up environment (`{ENV_PREFIX}_GITHUB_API_BASE_URL`,
//!   read once), never from a request, and the bearer is sent only to that base;
//! - no response and no log line carries a token, a device code or a refresh token;
//! - the App's client id is the host's (`host_identity`), never a literal here;
//! - not connected is a 409 with a message, never an empty 200 and never a 500.
//!
//! The routes exist only on a host that turns `host_options::cloud_workspaces` on.

use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::api::DaemonState;

const DEFAULT_API_BASE: &str = "https://api.github.com";
const API_BASE_ENV_SUFFIX: &str = "GITHUB_API_BASE_URL";
const API_TIMEOUT: Duration = Duration::from_secs(20);
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 10;

// ─── API base (read once, from the start-up environment only) ────────────────

/// `(base without trailing slash, whether the environment override is active)`.
static API_BASE: OnceLock<(String, bool)> = OnceLock::new();

/// The environment variable that overrides the API base: `{ENV_PREFIX}_GITHUB_API_BASE_URL`.
fn api_base_env() -> String {
    rightkit_agent_config::env_name(API_BASE_ENV_SUFFIX)
}

/// An override is accepted only as an `https://` URL or an `http://127.0.0.1` /
/// `http://localhost` URL, with no credentials, query or fragment.
fn validated_override(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed).ok()?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    let allowed = match parsed.scheme() {
        "https" => parsed.host_str().is_some(),
        "http" => matches!(parsed.host_str(), Some("127.0.0.1") | Some("localhost")),
        _ => false,
    };
    allowed.then(|| trimmed.trim_end_matches('/').to_owned())
}

fn api_base_state() -> &'static (String, bool) {
    API_BASE.get_or_init(|| {
        match std::env::var(api_base_env())
            .ok()
            .as_deref()
            .and_then(validated_override)
        {
            Some(base) => (base, true),
            None => (DEFAULT_API_BASE.to_owned(), false),
        }
    })
}

/// The GitHub REST API base in force for this daemon process.
pub(crate) fn github_api_base() -> &'static str {
    api_base_state().0.as_str()
}

/// `true` when `{ENV_PREFIX}_GITHUB_API_BASE_URL` (GHES or a local fake) is active.
pub(crate) fn github_api_base_overridden() -> bool {
    api_base_state().1
}

// ─── Shared helpers (also used by `api_cloud_workspaces`) ────────────────────

pub(crate) fn json_error(status: StatusCode, code: &str, message: Option<&str>) -> Response {
    let body = match message {
        Some(message) => json!({"error": code, "message": message}),
        None => json!({"error": code}),
    };
    (status, Json(body)).into_response()
}

pub(crate) fn not_connected_response() -> Response {
    json_error(
        StatusCode::CONFLICT,
        "github_not_connected",
        Some("Connect GitHub first: POST /forges/github/connect"),
    )
}

/// The stored token, resolved off the async threads. Blank counts as none.
pub(crate) async fn resolve_token() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(crate::github_signin::resolve_access_token)
        .await
        .map_err(|error| error.to_string())?
        .map(|token| token.filter(|token| !token.trim().is_empty()))
}

/// The stored token, or the 409 `github_not_connected` response.
pub(crate) async fn require_token() -> Result<String, Box<Response>> {
    match resolve_token().await {
        Ok(Some(token)) => Ok(token),
        _ => Err(Box::new(not_connected_response())),
    }
}

/// Replaces every occurrence of `token` in `text` with `***`.
pub(crate) fn scrub_token(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_owned()
    } else {
        text.replace(token, "***")
    }
}

pub(crate) fn github_client() -> Result<rightkit_http::AsyncClient, String> {
    rightkit_agent_daemon::http_client::endpoint_client(github_api_base(), API_TIMEOUT)
}

pub(crate) enum GithubError {
    /// GitHub answered 401: the stored token is no longer valid.
    Unauthorized,
    /// GitHub answered another non-2xx status.
    Status(u16, String),
    /// Transport, policy, timeout or body failure.
    Transport(String),
}

impl GithubError {
    pub(crate) fn into_response(self) -> Response {
        match self {
            GithubError::Unauthorized => json_error(
                StatusCode::CONFLICT,
                "github_reconnect_required",
                Some("GitHub rejected the stored sign-in: POST /forges/github/connect"),
            ),
            GithubError::Status(404, _) => {
                json_error(StatusCode::NOT_FOUND, "github_not_found", None)
            }
            GithubError::Status(status, message) => json_error(
                StatusCode::BAD_GATEWAY,
                "github_error",
                Some(&format!("GitHub answered {status}: {message}")),
            ),
            GithubError::Transport(message) => json_error(
                StatusCode::BAD_GATEWAY,
                "github_unreachable",
                Some(&message),
            ),
        }
    }
}

/// One authenticated GitHub GET. `url` must be built from [`github_api_base`].
pub(crate) async fn github_get_json(
    client: &rightkit_http::AsyncClient,
    url: &str,
    token: &str,
) -> Result<Value, GithubError> {
    let request = rightkit_http::Request::get(url)
        .bearer(token)
        .header("User-Agent", format!("{}/1.0", crate::host_identity::app()))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    let response = rightkit_agent_daemon::http_client::send_any(client, &request)
        .await
        .map_err(|error| GithubError::Transport(scrub_token(&error, token)))?;
    if response.status == 401 {
        return Err(GithubError::Unauthorized);
    }
    if !rightkit_agent_daemon::http_client::is_success(response.status) {
        let message = serde_json::from_str::<Value>(&response.text())
            .ok()
            .and_then(|body| {
                body.get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let message: String = scrub_token(&message, token).chars().take(200).collect();
        return Err(GithubError::Status(response.status, message));
    }
    response
        .json::<Value>()
        .map_err(|error| GithubError::Transport(scrub_token(&error.to_string(), token)))
}

/// Pages `{base}{path}?per_page=100&page=N` (cap [`MAX_PAGES`]) and collects the array under `key`.
async fn paged(
    client: &rightkit_http::AsyncClient,
    token: &str,
    path: &str,
    key: &str,
) -> Result<Vec<Value>, GithubError> {
    let base = github_api_base();
    let mut out = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = format!("{base}{path}?per_page={PAGE_SIZE}&page={page}");
        let body = github_get_json(client, &url, token).await?;
        let items = body
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let count = items.len();
        out.extend(items);
        if count < PAGE_SIZE {
            break;
        }
    }
    Ok(out)
}

/// One repository both the App installation and the user can reach.
#[derive(Clone)]
pub(crate) struct ForgeRepository {
    pub(crate) provider_id: u64,
    pub(crate) full_name: String,
    pub(crate) private: bool,
    pub(crate) default_branch: String,
    pub(crate) installation_id: u64,
}

impl ForgeRepository {
    fn to_json(&self) -> Value {
        json!({
            "providerId": self.provider_id,
            "fullName": self.full_name,
            "private": self.private,
            "defaultBranch": self.default_branch,
            "installationId": self.installation_id,
        })
    }
}

pub(crate) async fn list_installation_repositories(
    client: &rightkit_http::AsyncClient,
    token: &str,
    installation_id: u64,
) -> Result<Vec<ForgeRepository>, GithubError> {
    let path = format!("/user/installations/{installation_id}/repositories");
    let rows = paged(client, token, &path, "repositories").await?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            Some(ForgeRepository {
                provider_id: row.get("id")?.as_u64()?,
                full_name: row.get("full_name")?.as_str()?.to_owned(),
                private: row.get("private").and_then(Value::as_bool).unwrap_or(false),
                default_branch: row
                    .get("default_branch")
                    .and_then(Value::as_str)
                    .unwrap_or("main")
                    .to_owned(),
                installation_id,
            })
        })
        .collect())
}

struct Installation {
    id: u64,
    account: String,
    repository_selection: String,
}

async fn list_installations(
    client: &rightkit_http::AsyncClient,
    token: &str,
) -> Result<Vec<Installation>, GithubError> {
    let rows = paged(client, token, "/user/installations", "installations").await?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            Some(Installation {
                id: row.get("id")?.as_u64()?,
                account: row
                    .get("account")
                    .and_then(|account| account.get("login"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                repository_selection: match row.get("repository_selection").and_then(Value::as_str)
                {
                    Some("all") => "all".to_owned(),
                    _ => "selected".to_owned(),
                },
            })
        })
        .collect())
}

// ─── Device sign-in state (process-global) ───────────────────────────────────

struct Pending {
    /// `None` while the device code is still being requested.
    public: Option<PendingPublic>,
}

struct PendingPublic {
    user_code: String,
    verification_uri: String,
    expires_at: String,
    deadline: Instant,
}

impl Pending {
    fn live(&self) -> bool {
        match &self.public {
            Some(public) => Instant::now() < public.deadline,
            None => true,
        }
    }
}

/// The flow itself is moved into the blocking completion task (`complete` consumes it); this
/// holds only the public verification data, never the device code.
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn locked<T>(mutex: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn set_last_error(message: Option<String>) {
    *locked(&LAST_ERROR) = message.map(|message| message.chars().take(300).collect());
}

fn rfc3339_in(seconds: u64) -> String {
    let now = rightkit_time::DateTime::now_utc().unix_seconds();
    let at = now.saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX / 4));
    rightkit_time::DateTime::from_unix(at, 0)
        .map(|time| time.format_rfc3339())
        .unwrap_or_default()
}

// ─── Routes ──────────────────────────────────────────────────────────────────

pub fn cloud_forge_routes() -> Router<DaemonState> {
    // Read the API base now so it is fixed at daemon start, not at first request.
    api_base_state();
    Router::new()
        .route("/forges", get(list_forges_handler))
        .route(
            "/forges/github/connect",
            post(connect_handler).delete(disconnect_handler),
        )
        .route("/forges/github/installations", get(installations_handler))
        .route("/forges/github/repositories", get(repositories_handler))
}

async fn list_forges_handler() -> Response {
    let mut last_error: Option<String> = locked(&LAST_ERROR).clone();
    let token = match resolve_token().await {
        Ok(token) => token,
        Err(error) => {
            last_error = Some(error.chars().take(300).collect());
            None
        }
    };
    let pending = {
        let mut guard = locked(&PENDING);
        if guard.as_ref().is_some_and(|pending| !pending.live()) {
            *guard = None;
        }
        guard.as_ref().map(|pending| {
            pending.public.as_ref().map(|public| {
                json!({
                    "userCode": public.user_code,
                    "verificationUri": public.verification_uri,
                    "expiresAt": public.expires_at,
                })
            })
        })
    };
    let (state, login) = match &token {
        Some(token) => (
            "connected",
            match github_client() {
                Ok(client) => {
                    github_get_json(&client, &format!("{}/user", github_api_base()), token)
                        .await
                        .ok()
                        .and_then(|user| {
                            user.get("login").and_then(Value::as_str).map(str::to_owned)
                        })
                }
                Err(_) => None,
            },
        ),
        None if pending.is_some() => ("pending", None),
        None => ("disconnected", None),
    };
    Json(json!({
        "forges": [{
            "id": "github",
            "state": state,
            "login": login,
            "pending": pending.flatten(),
            "lastError": last_error,
        }]
    }))
    .into_response()
}

async fn connect_handler() -> Response {
    // A host without a GitHub App cannot sign in; say so before any state changes.
    if crate::host_identity::host()
        .github_client_id()
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "github_not_configured",
            Some("This build has no GitHub App configured."),
        );
    }
    {
        let mut guard = locked(&PENDING);
        if guard.as_ref().is_some_and(Pending::live) {
            return json_error(StatusCode::CONFLICT, "connect_pending", None);
        }
        *guard = Some(Pending { public: None });
    }
    let started = tokio::task::spawn_blocking(crate::github_signin::begin_device_sign_in).await;
    let flow = match started {
        Ok(Ok(flow)) => flow,
        Ok(Err(error)) => {
            *locked(&PENDING) = None;
            set_last_error(Some(error.clone()));
            return json_error(StatusCode::BAD_GATEWAY, "github_unreachable", Some(&error));
        }
        Err(error) => {
            *locked(&PENDING) = None;
            let message = error.to_string();
            set_last_error(Some(message.clone()));
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "connect_failed",
                Some(&message),
            );
        }
    };
    let (user_code, verification_uri, expires_in, interval) = {
        let authorization = flow.authorization();
        (
            authorization.user_code.clone(),
            authorization.verification_uri.clone(),
            authorization.expires_in,
            authorization.interval,
        )
    };
    set_last_error(None);
    *locked(&PENDING) = Some(Pending {
        public: Some(PendingPublic {
            user_code: user_code.clone(),
            verification_uri: verification_uri.clone(),
            expires_at: rfc3339_in(expires_in.as_secs()),
            deadline: Instant::now() + expires_in,
        }),
    });
    // `complete` consumes the flow and blocks until approval, denial or expiry.
    tokio::task::spawn_blocking(move || {
        let outcome = flow.complete(&crate::github_signin::DeviceWait);
        *locked(&PENDING) = None;
        match outcome {
            Ok(()) => set_last_error(None),
            Err(error) => set_last_error(Some(error)),
        }
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "state": "pending",
            "userCode": user_code,
            "verificationUri": verification_uri,
            "expiresIn": expires_in.as_secs(),
            "interval": interval.as_secs(),
        })),
    )
        .into_response()
}

async fn disconnect_handler() -> Response {
    match tokio::task::spawn_blocking(crate::github_signin::sign_out).await {
        Ok(Ok(())) => {
            set_last_error(None);
            Json(json!({"state": "disconnected"})).into_response()
        }
        Ok(Err(error)) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "sign_out_failed",
            Some(&error),
        ),
        Err(error) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "sign_out_failed",
            Some(&error.to_string()),
        ),
    }
}

async fn installations_handler() -> Response {
    let token = match require_token().await {
        Ok(token) => token,
        Err(response) => return *response,
    };
    let client = match github_client() {
        Ok(client) => client,
        Err(error) => {
            return json_error(StatusCode::BAD_GATEWAY, "github_unreachable", Some(&error))
        }
    };
    match list_installations(&client, &token).await {
        Ok(rows) => Json(json!({
            "installations": rows.iter().map(|row| json!({
                "id": row.id,
                "account": row.account,
                "repositorySelection": row.repository_selection,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => error.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoriesQuery {
    installation_id: Option<String>,
}

async fn repositories_handler(Query(query): Query<RepositoriesQuery>) -> Response {
    let token = match require_token().await {
        Ok(token) => token,
        Err(response) => return *response,
    };
    let requested = match query.installation_id.as_deref() {
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(id) => Some(id),
            Err(_) => {
                return json_error(StatusCode::BAD_REQUEST, "invalid_installation_id", None);
            }
        },
        None => None,
    };
    let client = match github_client() {
        Ok(client) => client,
        Err(error) => {
            return json_error(StatusCode::BAD_GATEWAY, "github_unreachable", Some(&error))
        }
    };
    let installation_ids = match requested {
        Some(id) => vec![id],
        None => match list_installations(&client, &token).await {
            Ok(rows) => rows.into_iter().map(|row| row.id).collect(),
            Err(error) => return error.into_response(),
        },
    };
    let mut seen = std::collections::HashSet::new();
    let mut repositories = Vec::new();
    for installation_id in installation_ids {
        match list_installation_repositories(&client, &token, installation_id).await {
            Ok(rows) => {
                for row in rows {
                    if seen.insert(row.provider_id) {
                        repositories.push(row.to_json());
                    }
                }
            }
            Err(error) => return error.into_response(),
        }
    }
    Json(json!({"repositories": repositories})).into_response()
}

// ─── OpenAPI fragment (both cloud modules) ───────────────────────────────────

fn schema_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

pub(crate) fn cloud_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "ForgesResponse".to_string(),
        json!({
            "type": "object",
            "required": ["forges"],
            "properties": {
                "forges": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id", "state", "login", "pending", "lastError"],
                        "properties": {
                            "id": {"type": "string", "enum": ["github"]},
                            "state": {"type": "string", "enum": ["disconnected", "pending", "connected"]},
                            "login": {"type": ["string", "null"]},
                            "pending": {
                                "type": ["object", "null"],
                                "required": ["userCode", "verificationUri", "expiresAt"],
                                "properties": {
                                    "userCode": {"type": "string"},
                                    "verificationUri": {"type": "string"},
                                    "expiresAt": {"type": "string", "format": "date-time"}
                                },
                                "additionalProperties": false
                            },
                            "lastError": {"type": ["string", "null"]}
                        },
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ForgeConnectResponse".to_string(),
        json!({
            "type": "object",
            "required": ["state"],
            "properties": {
                "state": {"type": "string", "enum": ["pending", "disconnected"]},
                "userCode": {"type": "string"},
                "verificationUri": {"type": "string"},
                "expiresIn": {"type": "integer", "minimum": 0},
                "interval": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ForgeInstallationsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["installations"],
            "properties": {
                "installations": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id", "account", "repositorySelection"],
                        "properties": {
                            "id": {"type": "integer"},
                            "account": {"type": "string"},
                            "repositorySelection": {"type": "string", "enum": ["all", "selected"]}
                        },
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ForgeRepositoriesResponse".to_string(),
        json!({
            "type": "object",
            "required": ["repositories"],
            "properties": {
                "repositories": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["providerId", "fullName", "private", "defaultBranch", "installationId"],
                        "properties": {
                            "providerId": {"type": "integer"},
                            "fullName": {"type": "string"},
                            "private": {"type": "boolean"},
                            "defaultBranch": {"type": "string"},
                            "installationId": {"type": "integer"}
                        },
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CloudWorkspace".to_string(),
        json!({
            "type": "object",
            "required": [
                "id", "provider", "repositoryId", "installationId", "fullName", "ref", "state",
                "checkoutPath", "resolvedCommit", "branch", "ownerSessionId", "error",
                "createdAt", "updatedAt"
            ],
            "properties": {
                "id": {"type": "string", "pattern": "^cw_[0-9a-zA-Z]{8,40}$"},
                "provider": {"type": "string", "enum": ["github"]},
                "repositoryId": {"type": "integer"},
                "installationId": {"type": "integer"},
                "fullName": {"type": "string"},
                "ref": {"type": "string"},
                "state": {
                    "type": "string",
                    "enum": ["requested", "provisioning", "ready", "active", "failed", "deleting", "deleted"]
                },
                "checkoutPath": {"type": ["string", "null"]},
                "resolvedCommit": {"type": ["string", "null"]},
                "branch": {"type": ["string", "null"]},
                "ownerSessionId": {"type": ["string", "null"]},
                "error": {"type": ["string", "null"]},
                "createdAt": {"type": "string", "format": "date-time"},
                "updatedAt": {"type": "string", "format": "date-time"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CloudWorkspaceList".to_string(),
        json!({
            "type": "object",
            "required": ["workspaces"],
            "properties": {
                "workspaces": {"type": "array", "items": schema_ref("CloudWorkspace")}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CloudWorkspaceEvents".to_string(),
        json!({
            "type": "object",
            "required": ["events"],
            "properties": {
                "events": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["seq", "at", "state", "message"],
                        "properties": {
                            "seq": {"type": "integer", "minimum": 1},
                            "at": {"type": "string", "format": "date-time"},
                            "state": {"type": "string"},
                            "message": {"type": "string"}
                        },
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateCloudWorkspaceRequest".to_string(),
        json!({
            "type": "object",
            "required": ["provider", "repositoryId", "installationId"],
            "properties": {
                "provider": {"type": "string", "enum": ["github"]},
                "repositoryId": {"type": "integer", "minimum": 0},
                "installationId": {"type": "integer", "minimum": 0},
                "ref": {"type": ["string", "null"]},
                "shallow": {"type": ["boolean", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateCloudWorkspaceSessionRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "title": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

/// Request body schema of a cloud route (method lower-case, path as templated), else `None`.
pub(crate) fn cloud_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/cloud-workspaces") => Some(schema_ref("CreateCloudWorkspaceRequest")),
        ("post", "/cloud-workspaces/{id}/sessions") => {
            Some(schema_ref("CreateCloudWorkspaceSessionRequest"))
        }
        ("post", "/forges/github/connect") => Some(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
        _ => None,
    }
}

/// Success response schema of a cloud route (method lower-case, path as templated), else `None`.
pub(crate) fn cloud_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/forges") => Some(schema_ref("ForgesResponse")),
        ("post", "/forges/github/connect") | ("delete", "/forges/github/connect") => {
            Some(schema_ref("ForgeConnectResponse"))
        }
        ("get", "/forges/github/installations") => Some(schema_ref("ForgeInstallationsResponse")),
        ("get", "/forges/github/repositories") => Some(schema_ref("ForgeRepositoriesResponse")),
        ("post", "/cloud-workspaces") | ("get", "/cloud-workspaces/{id}") => {
            Some(schema_ref("CloudWorkspace"))
        }
        ("get", "/cloud-workspaces") => Some(schema_ref("CloudWorkspaceList")),
        ("get", "/cloud-workspaces/{id}/events") => Some(schema_ref("CloudWorkspaceEvents")),
        ("delete", "/cloud-workspaces/{id}") => Some(json!({
            "type": "object",
            "required": ["deleted", "id"],
            "properties": {"deleted": {"type": "boolean"}, "id": {"type": "string"}},
            "additionalProperties": false
        })),
        ("post", "/cloud-workspaces/{id}/sessions") => Some(json!({
            "type": "object",
            "required": ["workspaceId"],
            "properties": {"workspaceId": {"type": "string"}},
            "additionalProperties": true
        })),
        _ => None,
    }
}
