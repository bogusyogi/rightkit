//! Cloud workspaces: a durable, daemon-managed `git clone` of one GitHub repository that a session
//! can be bound to. Provider-neutral forge access lives in `api_cloud_forge`.
//!
//! State machine (exactly): `requested -> provisioning -> ready -> active`,
//! `provisioning -> failed`, `ready|active|failed -> deleting -> deleted`. A restart recovery pass
//! also moves a `requested` workspace to `failed`. Each transition is written to disk before the
//! work it announces, is idempotent (repeating a finished transition is a no-op) and appends one
//! journal event. `ready -> active` is the one exception in time: it needs the session id, so it is
//! written right after the session exists.
//!
//! Safety rules (each one compiles and passes a happy-path test when wrong):
//! - a workspace id is validated against `^cw_[0-9a-zA-Z]{8,40}$` before any path join;
//! - delete removes only `<managed root>/<id>/checkout`, after canonicalising it and proving it is
//!   exactly that child of the canonical managed root (a symlink or any other path is refused);
//! - the GitHub token travels only in the `git` process environment (`GIT_CONFIG_*`), never in
//!   argv, the remote URL, the journal, `state.json` or a log; failure text is scrubbed;
//! - the clone URL comes from the GitHub API answer, never from the client.
//!
//! The routes exist only on a host that turns `host_options::cloud_workspaces` on.

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Once};

use crate::api::DaemonState;
use crate::api_cloud_forge::{
    github_api_base, github_api_base_overridden, github_client, github_get_json, json_error,
    list_installation_repositories, resolve_token, scrub_token, GithubError,
};

const MAX_ACTIVE_WORKSPACES: usize = 20;
const MAX_ERROR_CHARS: usize = 300;
const SHALLOW_DEPTH: &str = "50";
const MANAGED_DIR: &str = "cloud-workspaces";

// ─── Process-global coordination ─────────────────────────────────────────────

/// Per-id lock: two requests never provision, bind or delete one workspace at once.
static ID_LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);
/// At most two simultaneous clones.
static CLONE_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));
/// Serialises create so the 20-workspace limit cannot be raced past.
static CREATE_GATE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);
/// Managed roots whose restart recovery already ran in this process.
static RECOVERED_ROOTS: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);
static RECOVERY_ONCE: Once = Once::new();

fn id_lock(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = ID_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.entry(id.to_owned()).or_default().clone()
}

// ─── Record, ids, time ───────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Workspace {
    id: String,
    provider: String,
    repository_id: u64,
    installation_id: u64,
    full_name: String,
    #[serde(rename = "ref")]
    git_ref: String,
    state: String,
    checkout_path: Option<String>,
    resolved_commit: Option<String>,
    branch: Option<String>,
    owner_session_id: Option<String>,
    error: Option<String>,
    created_at: String,
    updated_at: String,
    /// Persisted only: shallow clone requested.
    #[serde(default)]
    shallow: bool,
    /// Persisted only: sequence number of the last journal event this record announced.
    #[serde(default)]
    event_seq: u64,
}

impl Workspace {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "provider": self.provider,
            "repositoryId": self.repository_id,
            "installationId": self.installation_id,
            "fullName": self.full_name,
            "ref": self.git_ref,
            "state": self.state,
            "checkoutPath": self.checkout_path,
            "resolvedCommit": self.resolved_commit,
            "branch": self.branch,
            "ownerSessionId": self.owner_session_id,
            "error": self.error,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }
}

fn now_rfc3339() -> String {
    rightkit_time::DateTime::now_utc().format_rfc3339()
}

/// `^cw_[0-9a-zA-Z]{8,40}$`
fn valid_id(id: &str) -> bool {
    match id.strip_prefix("cw_") {
        Some(rest) => {
            (8..=40).contains(&rest.len()) && rest.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }
        None => false,
    }
}

fn new_id() -> Result<String, String> {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut id = String::from("cw_");
    while id.len() < 3 + 12 {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| format!("no randomness: {error}"))?;
        for byte in bytes {
            // 248 = 4 * 62: rejects the biased tail so every letter is equally likely.
            if byte < 248 && id.len() < 3 + 12 {
                id.push(char::from(ALPHABET[usize::from(byte % 62)]));
            }
        }
    }
    Ok(id)
}

/// A git ref the client may name: no option-like start, no whitespace/control, no `..`.
fn valid_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && !value.starts_with('-')
        && !value.contains("..")
        && !value.contains('\\')
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn edge_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("requested", "provisioning")
            | ("requested", "failed")
            | ("provisioning", "ready")
            | ("provisioning", "failed")
            | ("ready", "active")
            | ("ready", "deleting")
            | ("active", "deleting")
            | ("failed", "deleting")
            | ("deleting", "deleted")
    )
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

// ─── Durable store ───────────────────────────────────────────────────────────

struct Store {
    root: PathBuf,
}

impl Store {
    fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn checkout(&self, id: &str) -> PathBuf {
        self.dir(id).join("checkout")
    }

    fn load(&self, id: &str) -> Result<Option<Workspace>, String> {
        let path = self.dir(id).join("state.json");
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Workspace>(&bytes)
                .map(Some)
                .map_err(|error| format!("unreadable state for {id}: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("cannot read state for {id}: {error}")),
        }
    }

    /// Temp file in the same directory, fsync, atomic rename.
    fn save(&self, ws: &Workspace) -> std::io::Result<()> {
        let dir = self.dir(&ws.id);
        let bytes = serde_json::to_vec_pretty(ws).map_err(std::io::Error::other)?;
        let temp = dir.join(format!("state.json.tmp-{}", std::process::id()));
        {
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&temp, dir.join("state.json"))?;
        #[cfg(unix)]
        if let Ok(handle) = std::fs::File::open(&dir) {
            if let Err(error) = handle.sync_all() {
                tracing::warn!(%error, workspace = %ws.id, "cloud workspace directory not synced");
            }
        }
        Ok(())
    }

    /// Append (and fsync) the journal line for `ws.event_seq`.
    fn append_event(&self, ws: &Workspace, state: &str, message: &str) -> std::io::Result<()> {
        let line = json!({
            "seq": ws.event_seq,
            "at": now_rfc3339(),
            "state": state,
            "message": message,
        });
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir(&ws.id).join("events.jsonl"))?;
        file.write_all(format!("{line}\n").as_bytes())?;
        file.sync_all()
    }

    /// Moves `ws` to `to`: state first, journal second. A repeated finished transition is a no-op
    /// (`Ok(false)`); an edge outside the machine is refused.
    fn transition(&self, ws: &mut Workspace, to: &str, message: &str) -> Result<bool, String> {
        if ws.state == to {
            return Ok(false);
        }
        if !edge_allowed(&ws.state, to) {
            return Err(format!("illegal transition {} -> {to}", ws.state));
        }
        ws.state = to.to_owned();
        ws.updated_at = now_rfc3339();
        ws.event_seq += 1;
        self.save(ws).map_err(|error| error.to_string())?;
        self.append_event(ws, to, message)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    fn read_events(&self, id: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir(id).join("events.jsonl")).unwrap_or_default();
        let mut events: Vec<Value> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event.get("seq").and_then(Value::as_u64).is_some())
            .collect();
        events.sort_by_key(|event| event["seq"].as_u64().unwrap_or(0));
        events
    }

    fn ids(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| valid_id(name))
            .collect()
    }

    fn list(&self) -> Vec<Workspace> {
        let mut rows: Vec<Workspace> = self
            .ids()
            .into_iter()
            .filter_map(|id| match self.load(&id) {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(%error, "cloud workspace record skipped");
                    None
                }
            })
            .collect();
        rows.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        rows
    }

    /// Removes ONLY `<root>/<id>/checkout`, after proving it is exactly that child of the
    /// canonical managed root. A symlink, or a checkout whose canonical path is anything else, is
    /// refused and nothing is deleted. A missing checkout is already removed.
    fn remove_checkout(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err("invalid workspace id".to_owned());
        }
        let checkout = self.checkout(id);
        let metadata = match std::fs::symlink_metadata(&checkout) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("cannot inspect checkout: {error}")),
        };
        if metadata.file_type().is_symlink() {
            return Err("checkout is a symbolic link; refusing to delete".to_owned());
        }
        let canonical_root = std::fs::canonicalize(&self.root)
            .map_err(|error| format!("cannot resolve managed root: {error}"))?;
        let canonical = std::fs::canonicalize(&checkout)
            .map_err(|error| format!("cannot resolve checkout: {error}"))?;
        let expected = canonical_root.join(id).join("checkout");
        if canonical != expected || !canonical.starts_with(&canonical_root) {
            return Err(
                "checkout is not a child of the managed root; refusing to delete".to_owned(),
            );
        }
        std::fs::remove_dir_all(&canonical)
            .map_err(|error| format!("cannot remove checkout: {error}"))
    }
}

// ─── Managed root and restart recovery ───────────────────────────────────────

/// `cloud-workspaces/` beside the daemon database file; with no database path (a bare test
/// state) the host's state root.
fn managed_root(state: &DaemonState) -> Option<PathBuf> {
    let database = state.database_path.trim();
    let base = if database.is_empty() {
        rightkit_agent_config::state_root()
    } else {
        FsPath::new(database)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(FsPath::to_path_buf)
    };
    base.map(|base| base.join(MANAGED_DIR))
}

/// Runs restart recovery once per managed root in this process, before the root serves anything.
fn ensure_recovered(root: &FsPath) {
    let mut done = RECOVERED_ROOTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if done.insert(root.to_path_buf()) {
        recover_on_start(root);
    }
}

/// One pass over every workspace left by a previous process. `requested` and `provisioning`
/// become `failed` (`interrupted_by_restart`, partial checkout removed; the client re-POSTs);
/// `deleting` is completed; a journal that lags its record is repaired. Idempotent.
pub(crate) fn recover_on_start(managed_root: &FsPath) {
    let store = Store {
        root: managed_root.to_path_buf(),
    };
    for id in store.ids() {
        let mut ws = match store.load(&id) {
            Ok(Some(ws)) => ws,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(%error, "cloud workspace recovery skipped a record");
                continue;
            }
        };
        let last_journaled = store
            .read_events(&id)
            .last()
            .and_then(|event| event["seq"].as_u64())
            .unwrap_or(0);
        if last_journaled < ws.event_seq {
            let state = ws.state.clone();
            if let Err(error) = store.append_event(&ws, &state, "journal repaired after restart") {
                tracing::warn!(%error, "cloud workspace journal repair failed");
            }
        }
        match ws.state.as_str() {
            "requested" | "provisioning" => {
                if let Err(error) = store.remove_checkout(&id) {
                    tracing::warn!(%error, workspace = %id, "partial checkout not removed");
                }
                ws.checkout_path = None;
                ws.error = Some("interrupted_by_restart".to_owned());
                if let Err(error) = store.transition(&mut ws, "failed", "interrupted_by_restart") {
                    tracing::warn!(%error, workspace = %id, "restart recovery transition failed");
                }
            }
            "deleting" => match store.remove_checkout(&id) {
                Ok(()) => {
                    ws.checkout_path = None;
                    if let Err(error) =
                        store.transition(&mut ws, "deleted", "delete completed after restart")
                    {
                        tracing::warn!(%error, workspace = %id, "restart delete completion failed");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, workspace = %id, "restart delete could not remove checkout");
                }
            },
            _ => {}
        }
    }
}

// ─── Routes ──────────────────────────────────────────────────────────────────

pub fn cloud_workspace_routes() -> Router<DaemonState> {
    RECOVERY_ONCE.call_once(|| {
        if let Some(root) = rightkit_agent_config::state_root().map(|base| base.join(MANAGED_DIR)) {
            ensure_recovered(&root);
        }
    });
    Router::new()
        .route("/cloud-workspaces", post(create_handler).get(list_handler))
        .route(
            "/cloud-workspaces/{id}",
            get(get_handler).delete(delete_handler),
        )
        .route("/cloud-workspaces/{id}/events", get(events_handler))
        .route(
            "/cloud-workspaces/{id}/sessions",
            post(create_session_handler),
        )
}

fn bad_id() -> Response {
    json_error(StatusCode::BAD_REQUEST, "invalid_id", None)
}

fn not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, "not_found", None)
}

fn busy() -> Response {
    json_error(StatusCode::CONFLICT, "workspace_busy", None)
}

fn storage_error(message: &str) -> Response {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "storage_error",
        Some(message),
    )
}

/// The store for this daemon, with restart recovery already done.
fn store_for(state: &DaemonState) -> Result<Store, Box<Response>> {
    let root = managed_root(state).ok_or_else(|| {
        Box::new(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_unavailable",
            Some("the state directory is unavailable"),
        ))
    })?;
    ensure_recovered(&root);
    Ok(Store { root })
}

async fn create_handler(
    State(state): State<DaemonState>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("body must be JSON"),
        );
    };
    if body.get("provider").and_then(Value::as_str) != Some("github") {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("provider must be \"github\""),
        );
    }
    let Some(repository_id) = body.get("repositoryId").and_then(Value::as_u64) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("repositoryId is required"),
        );
    };
    let Some(installation_id) = body.get("installationId").and_then(Value::as_u64) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("installationId is required"),
        );
    };
    let git_ref = match body.get("ref") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) if valid_ref(value) => value.clone(),
        Some(_) => return json_error(StatusCode::BAD_REQUEST, "invalid_ref", None),
    };
    let shallow = match body.get("shallow") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                Some("shallow must be a boolean"),
            )
        }
    };
    let store = match store_for(&state) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    // Not connected: 409 and NO record.
    match resolve_token().await {
        Ok(Some(_)) => {}
        _ => return crate::api_cloud_forge::not_connected_response(),
    }

    let _gate = CREATE_GATE.lock().await;
    let root = store.root.clone();
    let created = tokio::task::spawn_blocking(move || {
        let store = Store { root };
        let live = store
            .list()
            .iter()
            .filter(|ws| ws.state != "deleted")
            .count();
        if live >= MAX_ACTIVE_WORKSPACES {
            return Err(Box::new(json_error(
                StatusCode::CONFLICT,
                "workspace_limit",
                None,
            )));
        }
        std::fs::create_dir_all(&store.root)
            .map_err(|error| Box::new(storage_error(&error.to_string())))?;
        // `create_dir` (not `create_dir_all`) so an id collision fails instead of sharing a record.
        let ws = loop {
            let id = new_id().map_err(|error| Box::new(storage_error(&error)))?;
            match std::fs::create_dir(store.dir(&id)) {
                Ok(()) => {
                    let now = now_rfc3339();
                    break Workspace {
                        id,
                        provider: "github".to_owned(),
                        repository_id,
                        installation_id,
                        full_name: String::new(),
                        git_ref: git_ref.clone(),
                        state: "requested".to_owned(),
                        checkout_path: None,
                        resolved_commit: None,
                        branch: None,
                        owner_session_id: None,
                        error: None,
                        created_at: now.clone(),
                        updated_at: now,
                        shallow,
                        event_seq: 1,
                    };
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(Box::new(storage_error(&error.to_string()))),
            }
        };
        store
            .save(&ws)
            .and_then(|()| store.append_event(&ws, "requested", "workspace requested"))
            .map_err(|error| Box::new(storage_error(&error.to_string())))?;
        Ok(ws)
    })
    .await;
    let ws = match created {
        Ok(Ok(ws)) => ws,
        Ok(Err(response)) => return *response,
        Err(error) => return storage_error(&error.to_string()),
    };
    tokio::spawn(provision(store.root.clone(), ws.id.clone()));
    (StatusCode::ACCEPTED, Json(ws.to_json())).into_response()
}

async fn list_handler(State(state): State<DaemonState>) -> Response {
    let store = match store_for(&state) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || store.list()).await {
        Ok(rows) => Json(json!({
            "workspaces": rows.iter().map(Workspace::to_json).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => storage_error(&error.to_string()),
    }
}

async fn get_handler(State(state): State<DaemonState>, Path(id): Path<String>) -> Response {
    if !valid_id(&id) {
        return bad_id();
    }
    let store = match store_for(&state) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    match store.load(&id) {
        Ok(Some(ws)) => Json(ws.to_json()).into_response(),
        Ok(None) => not_found(),
        Err(error) => storage_error(&error),
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<u64>,
}

async fn events_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Response {
    if !valid_id(&id) {
        return bad_id();
    }
    let store = match store_for(&state) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    match store.load(&id) {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(error) => return storage_error(&error),
    }
    let after = query.after.unwrap_or(0);
    let events: Vec<Value> = store
        .read_events(&id)
        .into_iter()
        .filter(|event| event["seq"].as_u64().unwrap_or(0) > after)
        .collect();
    Json(json!({"events": events})).into_response()
}

/// A session is not ended while it is live or still persisted un-archived. Archiving and
/// deleting a session both take it out of those two places.
async fn session_is_live(state: &DaemonState, session_id: &str) -> bool {
    let live = state
        .sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(session_id);
    if live {
        return true;
    }
    let Some(storage) = state.storage.clone() else {
        return false;
    };
    let session_id = session_id.to_owned();
    tokio::task::spawn_blocking(move || {
        storage
            .blocking_lock()
            .list_sessions()
            .unwrap_or_default()
            .into_iter()
            .any(|row| row.id == session_id && row.status != "archived")
    })
    .await
    .unwrap_or(false)
}

async fn delete_handler(State(state): State<DaemonState>, Path(id): Path<String>) -> Response {
    if !valid_id(&id) {
        return bad_id();
    }
    let store = match store_for(&state) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let lock = id_lock(&id);
    let Ok(_guard) = lock.try_lock() else {
        return busy();
    };
    let mut ws = match store.load(&id) {
        Ok(Some(ws)) => ws,
        Ok(None) => return not_found(),
        Err(error) => return storage_error(&error),
    };
    match ws.state.as_str() {
        "deleted" => return Json(json!({"deleted": true, "id": id})).into_response(),
        "requested" | "provisioning" => {
            return json_error(StatusCode::CONFLICT, "workspace_provisioning", None)
        }
        "deleting" => {}
        _ => {
            if let Some(owner) = ws.owner_session_id.as_deref() {
                if session_is_live(&state, owner).await {
                    return json_error(StatusCode::CONFLICT, "workspace_active", None);
                }
            }
        }
    }
    if let Err(error) = store.transition(&mut ws, "deleting", "delete requested") {
        return storage_error(&error);
    }
    let root = store.root.clone();
    let removal_id = id.clone();
    let removed =
        tokio::task::spawn_blocking(move || Store { root }.remove_checkout(&removal_id)).await;
    match removed {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "delete_failed",
                Some(&error),
            )
        }
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "delete_failed",
                Some(&error.to_string()),
            )
        }
    }
    ws.checkout_path = None;
    if let Err(error) = store.transition(&mut ws, "deleted", "checkout removed") {
        return storage_error(&error);
    }
    Json(json!({"deleted": true, "id": id})).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionBody {
    #[serde(default)]
    title: Option<String>,
}

async fn create_session_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Json(body): Json<SessionBody>,
) -> Response {
    if !valid_id(&id) {
        return bad_id();
    }
    let store = match store_for(&state) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let lock = id_lock(&id);
    let Ok(_guard) = lock.try_lock() else {
        return busy();
    };
    let mut ws = match store.load(&id) {
        Ok(Some(ws)) => ws,
        Ok(None) => return not_found(),
        Err(error) => return storage_error(&error),
    };
    let checkout = match (ws.state.as_str(), ws.checkout_path.clone()) {
        ("ready" | "active", Some(path)) => path,
        _ => return json_error(StatusCode::CONFLICT, "workspace_not_ready", None),
    };
    let title = body
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| ws.full_name.clone());
    // `trustWorkspace` stays false: a clone is never silently trusted.
    let request = match serde_json::from_value::<crate::api::CreateSession>(json!({
        "workspaceRoot": checkout,
        "title": title,
        "trustWorkspace": false,
    })) {
        Ok(request) => request,
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "session_request_invalid",
                Some(&error.to_string()),
            )
        }
    };
    let created =
        crate::api_sessions::lifecycle::create_session_handler(State(state.clone()), Json(request))
            .await;
    let Json(mut session) = match created {
        Ok(session) => session,
        Err(error) => return error.into_response(),
    };
    let Some(session_id) = session.get("id").and_then(Value::as_str).map(str::to_owned) else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session_missing_id",
            Some("the session was created without an id"),
        );
    };
    ws.owner_session_id = Some(session_id.clone());
    let message = format!("session {session_id} bound");
    let recorded = if ws.state == "ready" {
        store.transition(&mut ws, "active", &message).map(|_| ())
    } else {
        // Already active (a second session): record the new owner and journal it.
        ws.updated_at = now_rfc3339();
        ws.event_seq += 1;
        store
            .save(&ws)
            .and_then(|()| store.append_event(&ws, "active", &message))
            .map_err(|error| error.to_string())
    };
    if let Err(error) = recorded {
        return storage_error(&error);
    }
    match session.as_object_mut() {
        Some(object) => {
            object.insert("workspaceId".to_owned(), json!(id));
            Json(session).into_response()
        }
        None => Json(json!({"session": session, "workspaceId": id})).into_response(),
    }
}

// ─── Provisioning ────────────────────────────────────────────────────────────

fn describe(error: GithubError) -> String {
    match error {
        GithubError::Unauthorized => "github_reconnect_required".to_owned(),
        GithubError::Status(status, message) => format!("github_error_{status}: {message}"),
        GithubError::Transport(message) => format!("github_unreachable: {message}"),
    }
}

/// `scheme://host[:port]` of an acceptable clone URL, else the reason it is refused. The clone URL
/// is taken from the GitHub API answer. By default only `https://github.com/...` is accepted; with
/// the API-base override active only a URL on the override's own host is.
fn clone_origin(clone_url: &str) -> Result<String, String> {
    let refused = || "clone_url_refused".to_owned();
    let parsed = url::Url::parse(clone_url).map_err(|_| refused())?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(refused());
    }
    let host = parsed.host_str().ok_or_else(refused)?;
    if github_api_base_overridden() {
        let base = url::Url::parse(github_api_base()).map_err(|_| refused())?;
        let scheme_ok = parsed.scheme() == "https"
            || (parsed.scheme() == "http" && matches!(host, "127.0.0.1" | "localhost"));
        if !scheme_ok || base.host_str() != Some(host) {
            return Err(refused());
        }
    } else if !clone_url.starts_with("https://github.com/") || host != "github.com" {
        return Err(refused());
    }
    Ok(match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    })
}

fn git_output(args: &[&str], dir: Option<&FsPath>) -> Result<std::process::Output, String> {
    let mut command = rightkit_child_process::command_std("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("git is not available: {error}"))
}

fn git_line(args: &[&str], dir: &FsPath) -> Result<String, String> {
    let output = git_output(args, Some(dir))?;
    if !output.status.success() {
        return Err(truncate_chars(
            String::from_utf8_lossy(&output.stderr).trim(),
            MAX_ERROR_CHARS,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

struct CloneJob {
    token: String,
    origin: String,
    clone_url: String,
    git_ref: String,
    shallow: bool,
    checkout: PathBuf,
}

/// Runs the clone. The credential is passed ONLY as `GIT_CONFIG_*` environment; the error text
/// is the first 300 characters of stderr with the token (and its base64 form) replaced by `***`.
fn clone_blocking(job: CloneJob) -> Result<(PathBuf, String, String), String> {
    let basic = rightkit_crypto::base64_encode(format!("x-access-token:{}", job.token).as_bytes());
    let scrub = |text: &str| {
        let text = scrub_token(text, &job.token);
        truncate_chars(scrub_token(&text, &basic).trim(), MAX_ERROR_CHARS)
    };
    let mut command = rightkit_child_process::command_std("git");
    command.arg("clone").arg("--no-tags");
    if job.shallow {
        command.arg("--depth").arg(SHALLOW_DEPTH);
    }
    command
        .arg("--branch")
        .arg(&job.git_ref)
        .arg("--")
        .arg(&job.clone_url)
        .arg(&job.checkout)
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("http.{}/.extraheader", job.origin),
        )
        .env(
            "GIT_CONFIG_VALUE_0",
            format!("AUTHORIZATION: basic {basic}"),
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    let output = command
        .output()
        .map_err(|error| scrub(&format!("git is not available: {error}")))?;
    if !output.status.success() {
        return Err(scrub(&String::from_utf8_lossy(&output.stderr)));
    }
    let commit = git_line(&["rev-parse", "HEAD"], &job.checkout).map_err(|error| scrub(&error))?;
    let branch = git_line(&["rev-parse", "--abbrev-ref", "HEAD"], &job.checkout)
        .map_err(|error| scrub(&error))?;
    let canonical = std::fs::canonicalize(&job.checkout)
        .map_err(|error| scrub(&format!("cannot resolve checkout: {error}")))?;
    Ok((canonical, commit, branch))
}

/// The spawned task behind a 202: queued as `requested` until a clone slot is free.
async fn provision(root: PathBuf, id: String) {
    let lock = id_lock(&id);
    let _guard = lock.lock().await;
    let Ok(_slot) = CLONE_SLOTS.clone().acquire_owned().await else {
        return;
    };
    let store = Store { root };
    let mut ws = match store.load(&id) {
        Ok(Some(ws)) if ws.state == "requested" => ws,
        _ => return,
    };
    // Written before the work it announces.
    if let Err(error) = store.transition(&mut ws, "provisioning", "provisioning started") {
        tracing::warn!(%error, workspace = %id, "cloud workspace could not start provisioning");
        return;
    }
    match provision_inner(&store, &mut ws).await {
        Ok(()) => {}
        Err(message) => fail(&store, &mut ws, &message).await,
    }
}

async fn provision_inner(store: &Store, ws: &mut Workspace) -> Result<(), String> {
    let token = match resolve_token().await {
        Ok(Some(token)) => token,
        _ => return Err("github_not_connected".to_owned()),
    };
    let client = github_client().map_err(|error| scrub_token(&error, &token))?;
    let base = github_api_base();
    let repository = github_get_json(
        &client,
        &format!("{base}/repositories/{}", ws.repository_id),
        &token,
    )
    .await
    .map_err(describe)?;
    let reachable = list_installation_repositories(&client, &token, ws.installation_id)
        .await
        .map_err(describe)?;
    if !reachable
        .iter()
        .any(|row| row.provider_id == ws.repository_id)
    {
        return Err("repository_not_authorized".to_owned());
    }
    let text = |key: &str| {
        repository
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let clone_url = text("clone_url").ok_or_else(|| "clone_url_missing".to_owned())?;
    let origin = clone_origin(&clone_url)?;
    ws.full_name = text("full_name").unwrap_or_default();
    if ws.git_ref.is_empty() {
        ws.git_ref = text("default_branch").unwrap_or_else(|| "main".to_owned());
    }
    if !valid_ref(&ws.git_ref) {
        return Err("invalid_ref".to_owned());
    }
    ws.updated_at = now_rfc3339();
    store.save(ws).map_err(|error| error.to_string())?;

    let job = CloneJob {
        token: token.clone(),
        origin,
        clone_url,
        git_ref: ws.git_ref.clone(),
        shallow: ws.shallow,
        checkout: store.checkout(&ws.id),
    };
    let (path, commit, branch) = tokio::task::spawn_blocking(move || clone_blocking(job))
        .await
        .map_err(|error| error.to_string())??;
    ws.checkout_path = Some(path.to_string_lossy().into_owned());
    ws.resolved_commit = Some(commit.clone());
    ws.branch = Some(branch);
    store
        .transition(ws, "ready", &format!("clone complete at {commit}"))
        .map(|_| ())
}

/// Any failure: partial checkout removed, then `failed` with the (already scrubbed) reason.
async fn fail(store: &Store, ws: &mut Workspace, message: &str) {
    let root = store.root.clone();
    let id = ws.id.clone();
    let removed = tokio::task::spawn_blocking(move || Store { root }.remove_checkout(&id)).await;
    if !matches!(removed, Ok(Ok(()))) {
        tracing::warn!(workspace = %ws.id, "partial checkout was not removed after a failed clone");
    }
    let message = truncate_chars(message, MAX_ERROR_CHARS);
    ws.checkout_path = None;
    ws.error = Some(message.clone());
    if let Err(error) = store.transition(ws, "failed", &message) {
        tracing::warn!(%error, workspace = %ws.id, "cloud workspace could not record failure");
    }
}
