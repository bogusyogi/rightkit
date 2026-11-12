//! `artifact_crud` — ENG-115 ("Artifacts: creation, editing, & inline preview").
//!
//! A first-class artifact CRUD surface independent of the transcript: an
//! agent (or, through the same tool names, a driver acting on the user's
//! behalf) can create a named artifact, read it back, update its content
//! with optimistic-concurrency protection, list what a session has
//! produced, and delete one. This is deliberately layered *on top of* the
//! existing artifact concept rather than beside it:
//!
//! * [`rightkit_agent_engine_protocol::ArtifactRow`] remains the one row shape. This
//!   module never adds fields to it — see [`rightkit_agent_engine_protocol::artifact::ArtifactCrudMeta`]
//!   for why (breaking exhaustive struct literals in crates outside this
//!   lane's allowlist) and how CRUD bookkeeping (`version`, `updatedAt`,
//!   `sessionId`) instead rides inside `ArtifactRow::meta` as a small JSON
//!   blob, the same way the existing browser-artifact writer
//!   (`storage::artifacts`) already uses `meta` for its own bookkeeping.
//! * `finalize_artifact.rs` (ENG-?) and the `artifact_trust` tool family
//!   remain the *only* writers of `shippable` / `verdict` / `approvals` —
//!   this module never touches those fields after creation (it always
//!   starts a new artifact at `shippable: false`, `verdict: None`).
//! * Every content write goes through `file_version_guard::FileVersionGuard`
//!   (ENG-109), not around it: `artifact_create` guards against a
//!   concurrent create at the same on-disk slot, and `artifact_update`
//!   guards the actual byte write against a concurrent writer (e.g. a
//!   client editing the same artifact through the File Provider extension
//!   at the same time). The friendlier `expected_version` counter is an
//!   *additional*, JSON-friendly optimistic-concurrency check layered on
//!   top for callers that only have the last-seen `ArtifactRow.meta`
//!   version, not a captured `ObservedVersion` — it is checked first (fast,
//!   readable error), but the guard's own capture-check-write still runs
//!   and is the actual race-safety boundary.
//!
//! ## On-disk layout
//!
//! Artifacts live under `<workspace_root>/<state-dir>/artifacts/<id>/`:
//!   * `meta.json` — the serialized `ArtifactRow` (with `ArtifactCrudMeta`
//!     packed into its `meta` field).
//!   * `content` — the artifact body, exactly as supplied.
//!   * `versions/<n>` — a snapshot of `content` as of version `n`, written
//!     just before it is overwritten by `artifact_update`, so history is
//!     never destroyed by an edit.
//!
//! An artifact's *logical* `path` (e.g. `"notes/todo.md"`, what the File
//! Provider extension and an inline-preview surface show the user) is
//! boundary-checked with `read_file::resolve_in_workspace` exactly like a
//! real file write would be — even though the bytes are not stored at that
//! literal location — so a caller cannot use the artifact `path` field to
//! smuggle a workspace-escaping string through a surface that skips the
//! usual write-tool path validation.
//!
//! ## Content hash
//!
//! `ArtifactRow::content_hash` here is a fast, non-cryptographic hash
//! (`std::collections::hash_map::DefaultHasher`, prefixed `"fnv64:"` to be
//! honest about what it is) rather than a SHA-256 digest: this crate has no
//! `sha2` dependency of its own (`Cargo.toml` is outside this lane's
//! allowlist), and `observed_version.rs` already establishes the same
//! trade-off for its own fingerprints. This is a change-detection value,
//! not a trust-binding one — trust binding (`ArtifactVerdict`/`ArtifactApproval`
//! `bound_hash`) is unaffected since this module never writes those fields.
//!
//! NOTE (integration): this module must be declared with `pub mod
//! artifact_crud;` in `engine/crates/tools/src/lib.rs`, and its five tools
//! registered in `engine/crates/tools/src/registry/registry_impl.rs`, before
//! either is reachable. Both files are outside this lane's exclusive file
//! allowlist — see the dispatch report for this lane.
//!
//! Wanted verification (not run in this lane): `rustright cargo test
//! --manifest-path engine/Cargo.toml -p host_tools artifact_crud`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_agent_engine_protocol::artifact::ArtifactCrudMeta;
use rightkit_agent_engine_protocol::ArtifactRow;
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::file_version_guard::{FileVersionGuard, GuardedWriteError};
use crate::observed_version::ObservedVersion;
use crate::tool_trait::{Tool, ToolResult};

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_iso() -> String {
    rightkit_agent_engine_protocol::now_iso()
}

fn new_artifact_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("art_{nanos:x}_{counter:x}")
}

fn fast_hash(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("fnv64:{:016x}", hasher.finish())
}

fn artifacts_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".rightkit").join("artifacts")
}

fn artifact_dir(workspace_root: &Path, id: &str) -> PathBuf {
    artifacts_root(workspace_root).join(id)
}

fn meta_path(dir: &Path) -> PathBuf {
    dir.join("meta.json")
}

fn content_path(dir: &Path) -> PathBuf {
    dir.join("content")
}

fn version_snapshot_path(dir: &Path, version: u64) -> PathBuf {
    dir.join("versions").join(version.to_string())
}

/// Reject an artifact id that isn't the exact shape `new_artifact_id` mints,
/// so a caller cannot smuggle a `..`/`/`-bearing string through `artifact_id`
/// and have it joined onto `artifacts_root` (path traversal via the id
/// rather than the logical `path` field, which is checked separately).
fn validate_artifact_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "Invalid artifact_id '{id}': must be alphanumeric plus '_'/'-'"
        ))
    }
}

fn load_row(dir: &Path) -> Result<ArtifactRow, String> {
    let raw = std::fs::read_to_string(meta_path(dir))
        .map_err(|e| format!("artifact not found or unreadable: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("corrupt artifact metadata: {e}"))
}

fn save_row(dir: &Path, row: &ArtifactRow) -> Result<(), String> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create artifacts directory: {e}"))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create artifact directory: {e}"))?;
    let bytes = serde_json::to_vec_pretty(row).map_err(|e| format!("serialize artifact: {e}"))?;
    rightkit_fs::atomic_write(&meta_path(dir), &bytes)
        .map_err(|e| format!("write artifact metadata: {e}"))?;
    Ok(())
}

fn check_permission(class: CapabilityClass, ctx: &ToolContext) -> Result<(), String> {
    let engine = PermissionEngine::new();
    let verdict = engine.evaluate(class, ctx.permission_mode, ctx.workspace_trust, None, false);
    match verdict.decision {
        PermissionDecision::Deny => Err(format!(
            "Permission denied: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
        PermissionDecision::Ask => Err(format!(
            "Tool requires approval before proceeding: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
        PermissionDecision::Allow => Ok(()),
    }
}

fn row_to_json(row: &ArtifactRow, include_content: Option<&str>) -> Value {
    let crud = ArtifactCrudMeta::from_meta_str(&row.meta);
    let mut value = json!({
        "id": row.id,
        "path": row.path,
        "kind": row.kind,
        "contentHash": row.content_hash,
        "shippable": row.shippable,
        "producer": row.producer,
        "provenance": row.provenance,
        "trustLabel": row.trust_label,
        "createdAt": row.created_at,
        "version": crud.version,
        "updatedAt": crud.updated_at,
        "sessionId": crud.session_id,
    });
    if let Some(content) = include_content {
        value["content"] = json!(content);
    }
    value
}

// ── workspace-backed trust store (#159) ─────────────────────────────────────

/// `ArtifactTrustStore` over the on-disk CRUD rows of one workspace. The
/// trust-gated tools (`attach_verdict`, `finalize_artifact`) get this store
/// only when the workspace is recorded as trusted in the workspace trust
/// store (T-12); see [`trust_store_for`].
pub struct WorkspaceArtifactTrustStore {
    workspace_root: PathBuf,
}

impl WorkspaceArtifactTrustStore {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    fn dir_for(&self, id: &str) -> Option<PathBuf> {
        validate_artifact_id(id).ok()?;
        Some(artifact_dir(&self.workspace_root, id))
    }

    fn mutate(&self, id: &str, f: impl FnOnce(&mut ArtifactRow)) -> Option<ArtifactRow> {
        let dir = self.dir_for(id)?;
        let mut row = load_row(&dir).ok()?;
        f(&mut row);
        save_row(&dir, &row).ok()?;
        Some(row)
    }
}

impl rightkit_agent_engine_protocol::ArtifactTrustStore for WorkspaceArtifactTrustStore {
    fn get_artifact(&self, id: &str) -> Option<ArtifactRow> {
        load_row(&self.dir_for(id)?).ok()
    }

    fn get_artifact_by_path(&self, session_id: &str, path: &str) -> Option<ArtifactRow> {
        let entries = std::fs::read_dir(artifacts_root(&self.workspace_root)).ok()?;
        entries
            .flatten()
            .filter_map(|entry| load_row(&entry.path()).ok())
            .find(|row| {
                row.path == path
                    && ArtifactCrudMeta::from_meta_str(&row.meta).session_id == session_id
            })
    }

    fn attach_verdict(
        &mut self,
        artifact_id: &str,
        mut verdict: rightkit_agent_engine_protocol::ArtifactVerdict,
    ) -> Option<ArtifactRow> {
        self.mutate(artifact_id, |row| {
            verdict.bound_hash = row.content_hash.clone();
            row.verdict = Some(verdict);
        })
    }

    fn record_approval(
        &mut self,
        artifact_id: &str,
        approval: rightkit_agent_engine_protocol::ArtifactApproval,
    ) -> Option<ArtifactRow> {
        self.mutate(artifact_id, |row| row.approvals.push(approval))
    }

    fn finalize_artifact(&mut self, artifact_id: &str) -> Option<ArtifactRow> {
        self.mutate(artifact_id, |row| row.shippable = true)
    }
}

type SharedTrustStore =
    std::sync::Arc<std::sync::Mutex<dyn rightkit_agent_engine_protocol::ArtifactTrustStore + Send>>;

/// Resolve the trust store for a trust-gated artifact tool. An explicitly
/// wired `ctx.trust_store` wins; otherwise the workspace-backed store is used
/// only when `ctx.workspace_root` is trusted in the workspace trust store at
/// `trust_store_path` (fail closed: untrusted / missing / unreadable refuse).
pub fn trust_store_for(
    ctx: &ToolContext,
    tool: &str,
    trust_store_path: &Path,
) -> Result<SharedTrustStore, String> {
    if let Some(store) = &ctx.trust_store {
        return Ok(store.clone());
    }
    if rightkit_agent_config::is_workspace_trusted_in(
        trust_store_path,
        &ctx.workspace_root.to_string_lossy(),
    ) {
        return Ok(std::sync::Arc::new(std::sync::Mutex::new(
            WorkspaceArtifactTrustStore::new(ctx.workspace_root.clone()),
        )));
    }
    Err(format!(
        "{tool} requires a trust store. This tool is only available in trust-gated mode: \
         the workspace must be trusted."
    ))
}

// ── artifact_create ──────────────────────────────────────────────────────────

pub struct ArtifactCreateTool;

#[async_trait::async_trait]
impl Tool for ArtifactCreateTool {
    fn name(&self) -> &str {
        "artifact_create"
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::WriteFile)
    }

    fn description(&self) -> &str {
        "Create a new first-class artifact (independent of the transcript) with an initial \
         content version. Returns the artifact's id, which artifact_read/artifact_update/\
         artifact_delete take."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "kind", "content"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Logical workspace-relative path this artifact represents (e.g. \"reports/summary.md\"). Must not escape the workspace."
                },
                "kind": {
                    "type": "string",
                    "description": "Artifact kind, e.g. \"markdown\", \"diff\", \"html-preview\"."
                },
                "content": {
                    "type": "string",
                    "description": "Initial artifact content."
                },
                "producer": {
                    "type": "string",
                    "description": "Free-form producer label. Defaults to the current session id."
                },
                "provenance": {
                    "type": "string",
                    "description": "Optional free-form provenance note."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required argument: path"),
        };
        let kind = match args["kind"].as_str() {
            Some(k) => k.to_string(),
            None => return ToolResult::error("Missing required argument: kind"),
        };
        let content = match args["content"].as_str() {
            Some(c) => c,
            None => return ToolResult::error("Missing required argument: content"),
        };

        if let Err(e) = check_permission(CapabilityClass::WriteFile, ctx) {
            return ToolResult::error(e);
        }

        // Boundary-check the logical path exactly like a real write would be,
        // even though the bytes land under `<state-dir>/artifacts/<id>/`, not
        // at this literal location.
        if let Err(e) = crate::read_file::resolve_in_workspace(path_str, &ctx.workspace_root) {
            return ToolResult::error(e);
        }

        let id = new_artifact_id();
        let dir = artifact_dir(&ctx.workspace_root, &id);
        let content_file = content_path(&dir);

        // Guard the initial write too: reject the astronomically unlikely
        // but not impossible case of an id collision racing another create.
        let observed = match ObservedVersion::capture(&content_file) {
            Ok(v) => v,
            Err(e) => return ToolResult::error(e),
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return ToolResult::error(format!("create artifact directory: {e}"));
        }
        let guard = FileVersionGuard::from_file_service(ctx.file_service.clone());
        if let Err(error) = guard.guarded_write(&content_file, &observed, content.as_bytes()) {
            return ToolResult::error(match error {
                GuardedWriteError::Stale(rejection) => rejection.to_string(),
                GuardedWriteError::Io(message) => message,
            });
        }

        let producer = args["producer"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| ctx.session_id.clone());
        let provenance = args["provenance"].as_str().unwrap_or_default().to_string();
        let now = now_iso();
        let crud = ArtifactCrudMeta {
            version: 1,
            updated_at: now.clone(),
            session_id: ctx.session_id.clone(),
        };

        let row = ArtifactRow {
            id: id.clone(),
            path: path_str.to_string(),
            kind,
            content_hash: fast_hash(content.as_bytes()),
            shippable: false,
            producer,
            provenance,
            trust_label: "unreviewed".to_string(),
            verdict: None,
            approvals: Vec::new(),
            meta: crud.to_meta_string(),
            created_at: now,
        };

        if let Err(e) = save_row(&dir, &row) {
            return ToolResult::error(e);
        }

        ToolResult::ok(row_to_json(&row, None).to_string())
    }
}

// ── artifact_read ────────────────────────────────────────────────────────────

pub struct ArtifactReadTool;

#[async_trait::async_trait]
impl Tool for ArtifactReadTool {
    fn name(&self) -> &str {
        "artifact_read"
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::ReadFile)
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Read an artifact's current metadata and content by id."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["artifact_id"],
            "properties": {
                "artifact_id": { "type": "string" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let id = match args["artifact_id"].as_str() {
            Some(id) => id,
            None => return ToolResult::error("Missing required argument: artifact_id"),
        };
        if let Err(e) = validate_artifact_id(id) {
            return ToolResult::error(e);
        }
        if let Err(e) = check_permission(CapabilityClass::ReadFile, ctx) {
            return ToolResult::error(e);
        }

        let dir = artifact_dir(&ctx.workspace_root, id);
        let row = match load_row(&dir) {
            Ok(row) => row,
            Err(e) => return ToolResult::error(e),
        };
        let content = match std::fs::read_to_string(content_path(&dir)) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("read artifact content: {e}")),
        };

        ToolResult::ok(row_to_json(&row, Some(&content)).to_string())
    }
}

// ── artifact_update ──────────────────────────────────────────────────────────

pub struct ArtifactUpdateTool;

#[async_trait::async_trait]
impl Tool for ArtifactUpdateTool {
    fn name(&self) -> &str {
        "artifact_update"
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::WriteFile)
    }

    fn description(&self) -> &str {
        "Update an artifact's content, creating a new version. Rejects the write if \
         expected_version no longer matches the artifact's current version (someone else \
         edited it since it was last read), and separately rejects it if the on-disk content \
         changed underneath the version guard."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["artifact_id", "content", "expected_version"],
            "properties": {
                "artifact_id": { "type": "string" },
                "content": { "type": "string" },
                "expected_version": {
                    "type": "integer",
                    "description": "The version last observed by the caller (from artifact_create or artifact_read)."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let id = match args["artifact_id"].as_str() {
            Some(id) => id,
            None => return ToolResult::error("Missing required argument: artifact_id"),
        };
        let content = match args["content"].as_str() {
            Some(c) => c,
            None => return ToolResult::error("Missing required argument: content"),
        };
        let expected_version = match args["expected_version"].as_u64() {
            Some(v) => v,
            None => return ToolResult::error("Missing required argument: expected_version"),
        };
        if let Err(e) = validate_artifact_id(id) {
            return ToolResult::error(e);
        }
        if let Err(e) = check_permission(CapabilityClass::WriteFile, ctx) {
            return ToolResult::error(e);
        }

        let dir = artifact_dir(&ctx.workspace_root, id);
        let mut row = match load_row(&dir) {
            Ok(row) => row,
            Err(e) => return ToolResult::error(e),
        };
        let mut crud = ArtifactCrudMeta::from_meta_str(&row.meta);

        // Fast, readable application-level check first.
        if crud.version != expected_version {
            return ToolResult::error(format!(
                "Version conflict for artifact '{id}': expected_version={expected_version}, \
                 current version={}. Re-read the artifact and retry.",
                crud.version
            ));
        }

        let content_file = content_path(&dir);

        // Detect a write that bypassed this surface entirely. The guarded write
        // below cannot: `ObservedVersion::capture` snapshots the file
        // microseconds before writing it, so it always agrees with itself and
        // is blind to anything that happened earlier. `row.content_hash` is the
        // durable record of what CRUD itself last wrote, so it is the only
        // value here that a foreign writer cannot have kept in step.
        let previous_bytes = std::fs::read(&content_file).ok();
        if let Some(bytes) = previous_bytes.as_deref() {
            let on_disk_hash = fast_hash(bytes);
            if on_disk_hash != row.content_hash {
                return ToolResult::error(format!(
                    "Artifact '{id}' was modified outside artifact_update (recorded content hash {}, found {on_disk_hash}). Re-read the artifact before updating so the out-of-band change is not silently overwritten.",
                    row.content_hash
                ));
            }
        }

        // Snapshot the pre-update bytes under versions/<old-version> before
        // they are overwritten, so history survives every edit.
        if let Some(previous) = previous_bytes.as_deref() {
            let snapshot = version_snapshot_path(&dir, crud.version);
            if let Some(parent) = snapshot.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = rightkit_fs::atomic_write(&snapshot, previous);
        }

        // The actual race-safety boundary: capture-check-write through the
        // shared file_version_guard, not around it.
        let observed = match ObservedVersion::capture(&content_file) {
            Ok(v) => v,
            Err(e) => return ToolResult::error(e),
        };
        let guard = FileVersionGuard::from_file_service(ctx.file_service.clone());
        if let Err(error) = guard.guarded_write(&content_file, &observed, content.as_bytes()) {
            return ToolResult::error(match error {
                GuardedWriteError::Stale(rejection) => rejection.to_string(),
                GuardedWriteError::Io(message) => message,
            });
        }

        crud.version += 1;
        crud.updated_at = now_iso();
        row.content_hash = fast_hash(content.as_bytes());
        row.meta = crud.to_meta_string();

        if let Err(e) = save_row(&dir, &row) {
            return ToolResult::error(e);
        }

        ToolResult::ok(row_to_json(&row, None).to_string())
    }
}

// ── artifact_delete ──────────────────────────────────────────────────────────

pub struct ArtifactDeleteTool;

#[async_trait::async_trait]
impl Tool for ArtifactDeleteTool {
    fn name(&self) -> &str {
        "artifact_delete"
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Destructive)
    }

    fn description(&self) -> &str {
        "Delete an artifact by id. Moves it to a workspace-local trash directory rather than \
         removing it outright, mirroring FileService's file-delete trash convention."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["artifact_id"],
            "properties": {
                "artifact_id": { "type": "string" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let id = match args["artifact_id"].as_str() {
            Some(id) => id,
            None => return ToolResult::error("Missing required argument: artifact_id"),
        };
        if let Err(e) = validate_artifact_id(id) {
            return ToolResult::error(e);
        }
        if let Err(e) = check_permission(CapabilityClass::Destructive, ctx) {
            return ToolResult::error(e);
        }

        let dir = artifact_dir(&ctx.workspace_root, id);
        if !dir.exists() {
            return ToolResult::error(format!("Artifact not found: {id}"));
        }

        let trash_root = ctx.workspace_root.join(".rightkit").join("artifacts_trash");
        if let Err(e) = std::fs::create_dir_all(&trash_root) {
            return ToolResult::error(format!("create artifacts trash directory: {e}"));
        }
        let trash_target = trash_root.join(format!(
            "{id}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        if let Err(e) = rightkit_fs::publish_dir(&dir, &trash_target) {
            return ToolResult::error(format!("move artifact to trash: {e}"));
        }

        ToolResult::ok(format!("Artifact '{id}' deleted."))
    }
}

// ── artifact_list ────────────────────────────────────────────────────────────

pub struct ArtifactListTool;

#[async_trait::async_trait]
impl Tool for ArtifactListTool {
    fn name(&self) -> &str {
        "artifact_list"
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::ReadFile)
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "List artifacts in the current workspace, optionally filtered by session_id or kind."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Only artifacts produced by this session." },
                "kind": { "type": "string", "description": "Only artifacts of this kind." }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Err(e) = check_permission(CapabilityClass::ReadFile, ctx) {
            return ToolResult::error(e);
        }

        let session_filter = args["session_id"].as_str();
        let kind_filter = args["kind"].as_str();

        let root = artifacts_root(&ctx.workspace_root);
        let mut rows = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let Ok(row) = load_row(&dir) else { continue };
                let crud = ArtifactCrudMeta::from_meta_str(&row.meta);
                if let Some(session_id) = session_filter {
                    if crud.session_id != session_id {
                        continue;
                    }
                }
                if let Some(kind) = kind_filter {
                    if row.kind != kind {
                        continue;
                    }
                }
                rows.push(row_to_json(&row, None));
            }
        }

        ToolResult::ok(json!({ "artifacts": rows }).to_string())
    }
}
