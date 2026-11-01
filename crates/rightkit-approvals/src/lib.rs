//! Approval of paid drafts and of assets for delivery. The host registers each draft kind it
//! approves with [`register_kind`] (for example recipes, video templates, training plans, or a
//! human-eyes release whose draft id is the hex of the asset's content hash); `blender_finish`
//! (a Blender product spec's finish confirmation) is built in.
//!
//! Preflight issues a single-use challenge (a random nonce from the operating system's generator,
//! the SHA-256 of the draft's canonical inputs, an expiry). If the OS random source fails, no
//! challenge is issued. `approve` is refused unless it carries a token that only a holder of the
//! launch secret can mint: `HMAC-SHA256(secret, kind | draft id | input hash | nonce)`. The secret
//! is the per-launch value the app shell passes the sidecar it spawned
//! (`InitPayload::approval_secret`) on the FIRST frame after start, once; the engine keeps it in
//! memory. An engine that holds no secret (the plain CLI, another app's engine, an agent's shell)
//! answers `approval_requires_app`; `operator_approved: true` alone is refused everywhere.
//!
//! WHAT THIS GUARANTEES: a caller of the engine's commands that does not hold the launch secret
//! cannot approve; a token is bound to its kind, draft id, input hash and nonce, expires after
//! [`CHALLENGE_TTL_SECS`], and approves once (the `.used` marker is created with `create_new`).
//!
//! PARENT BINDING (release builds): `arm` refuses a secret unless the engine's parent process is an
//! executable in the engine's own folder (the app shell, which ships its sidecar beside itself) and not
//! an engine or interpreter; an unreadable parent is refused (`Arm::ParentRefused`). A shell, script or
//! agent that starts its own engine is therefore refused unless it can place a program in the install
//! folder. OS probes use absolute paths so a PATH override cannot fake the parent.
//!
//! WHAT THIS DOES NOT GUARANTEE: the engine cannot authenticate its parent, and a per-launch
//! in-memory secret cannot be verified after that launch ends. Any process running as the same user
//! with file and shell access can start its own `<engine> --stdio`, send it an Init frame
//! with a secret it chose, request a challenge, mint the token (the HMAC construction is public)
//! and approve against the real data folder, or write the challenge, marker and sidecar files
//! directly. This mechanism is not a boundary against arbitrary local code; it binds approvals made
//! THROUGH THE ENGINE'S COMMANDS, which is how the shipped agent plugin and other apps reach
//! the host app.
//!
//! The record is honest rather than enforcing: every approval stores `channel: "sidecar_secret"`
//! (approved with a secret delivered over stdio; this says nothing about WHO delivered it) plus
//! `parent_pid` and `parent_exe`, the engine's own view of its parent process read from the OS when
//! the secret was accepted (null when unreadable). An approval made under the installed app shows
//! the app's executable; one made through a hand-started sidecar shows a shell or script. Nothing
//! enforces on these fields; they exist so the two can be told apart afterwards.
//!
//! Recipe, template and video-draft approvals are review state. `generate` does not check them, so
//! they do not gate a spend. What bounds a spend is the spend limits, the stored key and, for
//! character training, the token consumed immediately before the paid call.
//!
//! Test minting: a DEBUG build also accepts `RIGHTKIT_APPROVAL_TEST_SECRET`, and only with an
//! explicit data folder that, after canonicalising both paths (symlinks and `..` resolved, compared
//! case-insensitively on macOS and Windows), is not the user's real one; a folder that cannot be
//! canonicalised is refused. No environment marker (`RIGHTKIT_QA`) enables it, and a release build
//! ignores the variable entirely. Approvals made that way record `channel: "test_secret"`.
//!
//! State: challenges live beside the draft store under `<data>/approvals/<kind>/`, one file per
//! nonce, consumed by creating a `.used` marker with `create_new` (atomic, so two racing approvals
//! cannot both win).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rightkit_media_gen::host::approval as proto;
use serde_json::{json, Map, Value};

use rightkit_media_gen::{Context, GenError, GenResult};

/// The token primitives (mint, verify, input hash, challenge and receipt files) shared by the
/// shell that mints and the engine that verifies.
pub use rightkit_media_gen::host::approval as token;

pub const COMMANDS: &[&str] = &[
    "studio.approvals.challenge",
    "studio.blender.spec.preflight",
    "studio.blender.spec.approve",
];

/// How long a challenge stays valid.
pub const CHALLENGE_TTL_SECS: u64 = 30 * 60;

/// Channel recorded for an approval made with a secret delivered over the sidecar's stdio.
pub const CHANNEL_SIDECAR: &str = "sidecar_secret";
/// Channel recorded for an approval made with the debug-only test secret.
pub const CHANNEL_TEST: &str = "test_secret";

/// The engine's own view of its parent process: for the record only, never for enforcement.
#[derive(Clone, Debug, Default)]
pub struct Origin {
    pub parent_pid: Option<u32>,
    pub parent_exe: Option<String>,
}

struct Armed {
    secret: String,
    origin: Origin,
}

static ARMED: OnceLock<Armed> = OnceLock::new();

/// Result of offering the engine a secret.
#[derive(Debug, PartialEq, Eq)]
pub enum Arm {
    Armed,
    TooShort,
    AlreadyArmed,
    /// Release builds only: the engine's parent is not the app executable that ships beside it, so
    /// the secret came from a hand-started engine (a shell, script or agent) and is refused.
    ParentRefused,
}

/// True when `parent_exe` is the app shell: an executable in the same folder as this engine binary
/// (the app bundles its sidecar next to its own executable) that is not itself an engine or a
/// command interpreter. An unreadable parent is not the app. Pure, so it is tested without a
/// process tree.
pub fn parent_is_app(parent_exe: Option<&str>, engine_exe: &Path) -> bool {
    let Some(parent) = parent_exe
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
    else {
        return false;
    };
    let (Some(parent_dir), Some(engine_dir)) = (parent.parent(), engine_exe.parent()) else {
        return false;
    };
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if !same_path(&resolve(parent_dir), &resolve(engine_dir)) {
        return false;
    }
    let stem = parent
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let engine_stem = engine_exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let denied = [
        "sh",
        "bash",
        "zsh",
        "fish",
        "dash",
        "python",
        "python3",
        "node",
        "powershell",
        "pwsh",
        "cmd",
        "perl",
        "ruby",
    ];
    !(stem.is_empty()
        || (!engine_stem.is_empty() && stem.starts_with(&engine_stem))
        || stem.ends_with("-engine")
        || denied.contains(&stem.as_str()))
}

/// Arm this engine process with the secret delivered over stdio. The caller (the stdio loop) offers
/// it only from the first frame after start; first call wins, a later one is `AlreadyArmed` and
/// changes nothing; a secret shorter than [`proto::MIN_SECRET_LEN`] is `TooShort`. The parent
/// process is read from the OS now and stored with the secret for the approval record.
pub fn arm(secret: &str) -> Arm {
    if secret.len() < proto::MIN_SECRET_LEN {
        return Arm::TooShort;
    }
    if ARMED.get().is_some() {
        return Arm::AlreadyArmed;
    }
    let origin = parent_origin();
    // Enforced in every build without `test-mode`: only the app shell that spawned this engine may arm it. A
    // test-mode build (the e2e suite, whose parent is the test harness) skips this and uses the test secret rules.
    if !cfg!(feature = "test-mode") {
        let engine = std::env::current_exe().unwrap_or_default();
        if !parent_is_app(origin.parent_exe.as_deref(), &engine) {
            return Arm::ParentRefused;
        }
    }
    let armed = Armed {
        secret: secret.to_string(),
        origin,
    };
    if ARMED.set(armed).is_ok() {
        Arm::Armed
    } else {
        Arm::AlreadyArmed
    }
}

/// Best-effort read of the parent process id and executable path from the OS. Any failure is null.
pub fn parent_origin() -> Origin {
    let me = std::process::id();
    let parent_pid = rightkit_child_process::process_table()
        .ok()
        .and_then(|table| {
            table
                .into_iter()
                .find(|e| e.pid == me)
                .map(|e| e.parent_pid)
        })
        .filter(|p| *p != 0);
    let parent_exe = parent_pid
        .and_then(std::num::NonZeroU32::new)
        .and_then(|pid| {
            rightkit_child_process::AdoptedIdentity::capture(pid)
                .ok()
                .flatten()
        })
        .and_then(|identity| identity.exe().map(|p| p.to_string_lossy().into_owned()));
    Origin {
        parent_pid,
        parent_exe,
    }
}

/// The secret this process may verify against, the channel it records and the parent it saw.
fn active_secret() -> Option<(String, &'static str, Origin)> {
    if let Some(armed) = ARMED.get() {
        return Some((armed.secret.clone(), CHANNEL_SIDECAR, armed.origin.clone()));
    }
    test_secret().map(|secret| (secret, CHANNEL_TEST, parent_origin()))
}

/// Resolve a folder for comparison: symlinks and `..` resolved. The real per-user folder may not
/// exist yet; then its nearest existing ancestor is resolved and the remaining names appended, so
/// the comparison still has a definite answer. `None` when nothing can be resolved.
fn resolve_folder(path: &Path, allow_missing_tail: bool) -> Option<PathBuf> {
    if let Ok(real) = std::fs::canonicalize(path) {
        return Some(real);
    }
    if !allow_missing_tail {
        return None;
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        let name = cursor.file_name()?.to_os_string();
        tail.push(name);
        cursor = cursor.parent()?.to_path_buf();
        if let Ok(mut base) = std::fs::canonicalize(&cursor) {
            for part in tail.iter().rev() {
                base.push(part);
            }
            return Some(base);
        }
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(any(target_os = "macos", windows)) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// True when `explicit` is, or cannot be shown not to be, the `real` per-user folder: both are
/// canonicalised (an explicit folder that cannot be is refused; so is a real folder with no
/// resolvable ancestor) and compared case-insensitively on macOS and Windows.
pub fn folder_is_or_may_be_real(explicit: &Path, real: &Path) -> bool {
    match (resolve_folder(explicit, false), resolve_folder(real, true)) {
        (Some(e), Some(r)) => same_path(&e, &r),
        _ => true,
    }
}

/// Release builds have no test secret: this stub is all they contain.
#[cfg(not(feature = "test-mode"))]
fn test_secret() -> Option<String> {
    None
}

/// The test secret (compiled only with the `test-mode` feature, so the variable name is not in a release binary).
#[cfg(feature = "test-mode")]
fn test_secret() -> Option<String> {
    let secret = std::env::var(proto::TEST_SECRET_ENV)
        .ok()
        .filter(|s| s.len() >= proto::MIN_SECRET_LEN)?;
    // Never against the user's real folder: the folder must be named and must not resolve to it.
    let explicit = rightkit_media_gen::host::env_var("APP_DATA")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)?;
    if folder_is_or_may_be_real(&explicit, &rightkit_media_gen::host::user_data_root()) {
        return None;
    }
    Some(secret)
}

/// What a caller presented with `approve`, already checked to be possible at all.
pub struct Gate {
    secret: String,
    channel: &'static str,
    origin: Origin,
    token: String,
    nonce: String,
}

/// First check of every approve: this engine can verify, and the caller brought a token.
/// Runs before the draft is read, so a caller without the launch secret learns nothing.
pub fn gate(input: &Value) -> GenResult<Gate> {
    let refusal = || {
        GenError::new(
            "approval_requires_app",
            "Approval is a user action in the app. Approve this draft in the app; an agent, the plain engine CLI or `operator_approved: true` cannot approve it.",
        )
    };
    let (secret, channel, origin) = active_secret().ok_or_else(refusal)?;
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let token = text("approval_token").ok_or_else(refusal)?;
    let nonce = text("approval_nonce").ok_or_else(|| {
        GenError::new(
            "approval_nonce_mismatch",
            "approve needs the approval_nonce of the challenge the token was minted for",
        )
    })?;
    Ok(Gate {
        secret,
        channel,
        origin,
        token: token.to_string(),
        nonce: nonce.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Canonical hashing
// ---------------------------------------------------------------------------

/// SHA-256 of the canonical JSON of a draft's approval inputs (shared with providers that verify
/// receipts: see `rightkit_media_gen::host::approval::input_hash`).
pub fn input_hash(inputs: &Value) -> String {
    proto::input_hash(inputs)
}

// ---------------------------------------------------------------------------
// Challenge store
// ---------------------------------------------------------------------------

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn iso_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn os_user() -> String {
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "unknown".into())
}

fn safe_part(label: &str, value: &str, allow_dots: bool) -> GenResult<()> {
    let ok = !value.is_empty()
        && value.len() <= 160
        && !value.contains("..")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || (allow_dots && c == '.'));
    if ok {
        Ok(())
    } else {
        Err(GenError::new(
            "invalid_input",
            format!("invalid {label} \"{value}\""),
        ))
    }
}

fn challenge_path(ctx: &Context, kind: &str, draft_id: &str, nonce: &str) -> GenResult<PathBuf> {
    safe_part("approval kind", kind, false)?;
    safe_part("draft id", draft_id, true)?;
    safe_part("nonce", nonce, false)?;
    Ok(ctx
        .data_dir
        .join("approvals")
        .join(kind)
        .join(format!("{draft_id}.{nonce}.json")))
}

fn used_marker(path: &std::path::Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".used");
    path.with_file_name(name)
}

/// Issue a fresh challenge for a draft. `inputs` is the kind's approval-input view of the draft;
/// `legacy_hash` is the draft's existing audit hash, echoed so the UI can compare what it showed.
pub fn issue(
    ctx: &Context,
    kind: &str,
    draft_id: &str,
    inputs: &Value,
    legacy_hash: &str,
) -> GenResult<Value> {
    let nonce = rightkit_daemon::PrivateSecret::generate()
        .map(|secret| secret.expose().to_string())
        .map_err(|e| {
            GenError::new(
                "approval_entropy",
                format!("{e}; no approval challenge was issued"),
            )
        })?;
    let path = challenge_path(ctx, kind, draft_id, &nonce)?;
    let created = unix_now();
    let record = json!({
        "kind": kind,
        "draft_id": draft_id,
        "nonce": nonce,
        "input_hash": input_hash(inputs),
        "legacy_hash": legacy_hash,
        "created_at": iso_now(),
        "created_unix": created,
        "expires_unix": created + CHALLENGE_TTL_SECS,
    });
    if let Some(parent) = path.parent() {
        rightkit_fs::ensure_private_dir(parent)
            .map_err(|e| GenError::new("io", format!("{}: {e}", parent.display())))?;
    }
    rightkit_fs::atomic_write(
        &path,
        serde_json::to_string_pretty(&record)
            .unwrap_or_default()
            .as_bytes(),
    )
    .map_err(|e| GenError::new("io", e.to_string()))?;
    Ok(json!({
        "nonce": record["nonce"],
        "input_hash": record["input_hash"],
        "legacy_hash": record["legacy_hash"],
        "expires_unix": record["expires_unix"],
        "single_use": true,
    }))
}

/// [`issue`] plus the `kind` and `draft_id` the app needs to mint against.
pub fn issue_labeled(
    ctx: &Context,
    kind: &str,
    draft_id: &str,
    inputs: &Value,
    legacy_hash: &str,
) -> GenResult<Value> {
    let mut challenge = issue(ctx, kind, draft_id, inputs, legacy_hash)?;
    if let Some(map) = challenge.as_object_mut() {
        map.insert("kind".into(), json!(kind));
        map.insert("draft_id".into(), json!(draft_id));
    }
    Ok(challenge)
}

/// Verify the presented token against its challenge and the draft as it is now, then consume the
/// challenge. Call after the draft's own integrity checks. Returns the approval record to store.
pub fn consume(
    ctx: &Context,
    gate: &Gate,
    kind: &str,
    draft_id: &str,
    inputs: &Value,
    legacy_hash: &str,
) -> GenResult<Value> {
    let path = challenge_path(ctx, kind, draft_id, &gate.nonce)?;
    let text = std::fs::read_to_string(&path).map_err(|_| {
        GenError::new(
            "approval_nonce_mismatch",
            "No approval challenge matches that nonce for this draft; ask the app for a new one.",
        )
    })?;
    let challenge: Value = serde_json::from_str(&text).map_err(|e| {
        GenError::new(
            "approval_nonce_mismatch",
            format!("approval challenge unreadable: {e}"),
        )
    })?;
    let challenge_hash = challenge["input_hash"].as_str().unwrap_or_default();
    if !proto::token_matches(
        &gate.secret,
        kind,
        draft_id,
        challenge_hash,
        &gate.nonce,
        &gate.token,
    ) {
        return Err(GenError::new(
            "approval_token_invalid",
            "The approval token does not match this draft, its challenge and the app's secret.",
        ));
    }
    let current = input_hash(inputs);
    if current != challenge_hash {
        return Err(GenError::new(
            "approval_input_changed",
            "The draft's inputs changed after the challenge was issued; preflight again.",
        ));
    }
    if challenge["expires_unix"].as_u64().unwrap_or(0) < unix_now() {
        return Err(GenError::new(
            "approval_expired",
            "The approval challenge expired; ask the app for a new one.",
        ));
    }
    let marker = used_marker(&path);
    match rightkit_fs::create_private_new(&marker) {
        Ok(mut file) => {
            use std::io::Write;
            let _ = writeln!(file, "{}", iso_now());
        }
        // `create_private_new` reports an existing marker as `AlreadyExists` on Unix but as an `Io` error carrying
        // `ErrorKind::AlreadyExists` on Windows, so test the kind, not the variant.
        Err(e) if e.kind() == Some(std::io::ErrorKind::AlreadyExists) => {
            return Err(GenError::new(
                "approval_token_used",
                "This approval token was already used; a token approves once.",
            ));
        }
        Err(e) => return Err(GenError::new("io", format!("{}: {e}", marker.display()))),
    }
    Ok(json!({
        "channel": gate.channel,
        "parent_pid": gate.origin.parent_pid,
        "parent_exe": gate.origin.parent_exe,
        "approved_by": os_user(),
        "approved_at": iso_now(),
        "approved_unix": unix_now(),
        "input_hash": current,
        "legacy_hash": legacy_hash,
        "nonce": gate.nonce,
        "kind": kind,
    }))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// `studio.approvals.challenge {kind, draft_id, params?}` (`params` only for `character_training` and `eyes`): a fresh single-use challenge for the draft as it
/// stands. Anyone may ask; the challenge alone approves nothing.
/// Resolves a host draft kind's approval inputs: `(ctx, draft_id, params)` to
/// `(inputs, legacy hash)`, where `inputs` is the kind's approval-input view of the draft as it
/// stands and `legacy hash` its existing audit hash.
pub type InputsResolver = fn(&Context, &str, &Value) -> GenResult<(Value, String)>;

fn resolvers() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, InputsResolver>> {
    static RESOLVERS: OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, InputsResolver>>,
    > = OnceLock::new();
    RESOLVERS.get_or_init(Default::default)
}

/// Register the inputs resolver of a host draft kind (`recipes`, `eyes`, ...). A later
/// registration of the same kind replaces the earlier one. `blender_finish` is built in.
pub fn register_kind(kind: &str, resolver: InputsResolver) {
    resolvers()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(kind.to_string(), resolver);
}

fn handle_challenge(ctx: &Context, input: &Value) -> GenResult<Value> {
    let kind = input
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = input
        .get("draft_id")
        .or_else(|| input.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = input.get("params").unwrap_or(&Value::Null);
    let (inputs, legacy) = if kind == proto::BLENDER_KIND {
        blender_inputs(params, id).map(|(_, inputs, hash)| (inputs, hash))?
    } else {
        let resolver = resolvers()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(kind)
            .copied();
        match resolver {
            Some(resolve) => resolve(ctx, id, params)?,
            None => {
                let mut known: Vec<String> = resolvers()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .keys()
                    .cloned()
                    .collect();
                known.push(proto::BLENDER_KIND.to_string());
                return Err(GenError::new(
                    "invalid_input",
                    format!("unknown approval kind \"{kind}\" ({})", known.join(", ")),
                ));
            }
        }
    };
    issue_labeled(ctx, kind, id, &inputs, &legacy)
}

// ---------------------------------------------------------------------------
// Blender product spec: the finish confirmation as an in-app approval
// ---------------------------------------------------------------------------
//
// `local_blender.rs` adopts a render only for a product spec its owner confirmed and whose finishes
// they approved. Those used to be booleans any caller could write into the spec file. Now one
// approval (kind `blender_finish`, draft id = hash of the spec's approval inputs, see
// `proto::blender_spec_view`) releases both: `studio.blender.spec.preflight` issues the challenge
// for the spec as it stands, the app mints the token, and `studio.blender.spec.approve` consumes it
// and WRITES the confirmation (`confirmed`, `confirmed_by`, each finish's `approved`/`approved_by`,
// plus the engine's record in `confirmation` and each finish's `approval`). The provider accepts the
// spec only when those records name a challenge this engine issued and a token consumed for exactly
// these inputs (`proto::receipt_valid`). Editing parts, slots or finish versions changes the draft
// id, so the confirmation must be given again. Limits as above: file-backed, not a boundary against
// local code that can write the data folder.

/// `(spec path, approval inputs, draft id)` for the spec `params.path` names. When `draft_id` is
/// non-empty it must equal the current draft id (`draft_changed` otherwise).
fn blender_inputs(params: &Value, draft_id: &str) -> GenResult<(PathBuf, Value, String)> {
    let raw = params
        .get("path")
        .or_else(|| params.get("product_spec"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            GenError::new(
                "invalid_input",
                "a Blender approval needs params.path, the product spec file",
            )
        })?;
    let path = std::fs::canonicalize(raw)
        .map_err(|e| GenError::new("asset_not_found", format!("{raw}: {e}")))?;
    let spec = read_spec(&path)?;
    spec["owner"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GenError::new("invalid_input", "the product spec has no owner"))?;
    let view = proto::blender_spec_view(&spec);
    let hash = proto::input_hash(&view);
    if !draft_id.is_empty() && draft_id != hash {
        return Err(GenError::new(
            "draft_changed",
            "The product spec is not the one that was shown; review it again.",
        ));
    }
    Ok((path, view, hash))
}

fn read_spec(path: &Path) -> GenResult<Value> {
    let bytes =
        std::fs::read(path).map_err(|e| GenError::new("io", format!("{}: {e}", path.display())))?;
    serde_json::from_slice(&bytes).map_err(|e| {
        GenError::new(
            "invalid_input",
            format!("{}: not valid JSON: {e}", path.display()),
        )
    })
}

/// `studio.blender.spec.preflight {path}`: the spec's draft id and a fresh single-use challenge for
/// the app to mint against. Approves nothing.
fn blender_preflight(ctx: &Context, input: &Value) -> GenResult<Value> {
    let (path, view, hash) = blender_inputs(input, "")?;
    let challenge = issue_labeled(ctx, proto::BLENDER_KIND, &hash, &view, &hash)?;
    Ok(
        json!({"path": path.to_string_lossy(), "draft_id": hash, "spec": view["spec"], "approval_challenge": challenge}),
    )
}

/// Stamp the engine's record onto a spec: the confirmation and every finish's approval.
fn stamp_blender_approval(spec: &mut Value, record: &Value) {
    let mut stamped = Value::Object(approval_fields(record));
    stamped["kind"] = json!(proto::BLENDER_KIND);
    let owner = spec["owner"].clone();
    spec["confirmed"] = json!(true);
    spec["confirmed_by"] = owner.clone();
    spec["confirmation"] = stamped.clone();
    if let Some(finishes) = spec.get_mut("finishes").and_then(Value::as_object_mut) {
        for finish in finishes.values_mut() {
            if let Some(f) = finish.as_object_mut() {
                f.insert("approved".into(), json!(true));
                f.insert("approved_by".into(), owner.clone());
                f.insert("approval".into(), stamped.clone());
            }
        }
    }
}

/// `studio.blender.spec.approve {path, draft_id, approval_token, approval_nonce}`: the WRITER. Needs
/// the app's token (refused with `approval_requires_app` otherwise), consumes it once, then writes
/// the confirmation into the spec atomically, refusing if the file changed since it was hashed.
fn blender_approve(ctx: &Context, input: &Value) -> GenResult<Value> {
    // First, before the spec is read: a caller without the app's token learns nothing.
    let gate = gate(input)?;
    let draft_id = input
        .get("draft_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            GenError::new(
                "invalid_input",
                "approve needs the draft_id the challenge was issued for",
            )
        })?;
    let (path, view, hash) = blender_inputs(input, draft_id)?;
    let before = std::fs::read(&path)
        .map_err(|e| GenError::new("io", format!("{}: {e}", path.display())))?;
    let record = consume(ctx, &gate, proto::BLENDER_KIND, draft_id, &view, &hash)?;
    let current = std::fs::read(&path)
        .map_err(|e| GenError::new("io", format!("{}: {e}", path.display())))?;
    if current != before {
        return Err(GenError::new(
            "approval_input_changed",
            "The product spec changed while it was being approved; review it again.",
        ));
    }
    let mut spec: Value = serde_json::from_slice(&current)
        .map_err(|e| GenError::new("invalid_input", e.to_string()))?;
    stamp_blender_approval(&mut spec, &record);
    let mut out =
        serde_json::to_vec_pretty(&spec).map_err(|e| GenError::new("io", e.to_string()))?;
    out.push(b'\n');
    rightkit_fs::atomic_write(&path, &out).map_err(|e| GenError::new("io", e.to_string()))?;
    Ok(
        json!({"path": path.to_string_lossy(), "draft_id": hash, "confirmed": true, "approval": spec["confirmation"]}),
    )
}

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.approvals.challenge" => handle_challenge(ctx, input),
        "studio.blender.spec.preflight" => blender_preflight(ctx, input),
        "studio.blender.spec.approve" => blender_approve(ctx, input),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}

/// Build the object stored in a draft's approval record from the verified record.
pub fn approval_fields(record: &Value) -> Map<String, Value> {
    let mut map = Map::new();
    for key in [
        "channel",
        "parent_pid",
        "parent_exe",
        "approved_by",
        "approved_at",
        "input_hash",
        "legacy_hash",
        "nonce",
    ] {
        map.insert(key.into(), record[key].clone());
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_app_beside_the_engine_may_arm_it() {
        let dir =
            std::env::temp_dir().join(format!("rightkit-approvals-parent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = dir.join("demo-engine.exe");
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        assert!(
            parent_is_app(Some(&s(dir.join("demo.exe"))), &engine),
            "the app next to its sidecar"
        );
        assert!(
            !parent_is_app(None, &engine),
            "an unreadable parent is not the app"
        );
        assert!(!parent_is_app(Some(""), &engine));
        for shell in [
            "powershell.exe",
            "pwsh.exe",
            "cmd.exe",
            "bash",
            "python3",
            "node.exe",
            "demo-engine.exe",
        ] {
            assert!(
                !parent_is_app(Some(&s(dir.join(shell))), &engine),
                "{shell} beside the engine is still not the app"
            );
        }
        let elsewhere = std::env::temp_dir()
            .join("rightkit-approvals-parent-elsewhere")
            .join("demo.exe");
        assert!(
            !parent_is_app(Some(&s(elsewhere)), &engine),
            "a same-named program in another folder"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_comparison_resolves_dotdot_symlinks_and_refuses_unresolvable() {
        let base =
            std::env::temp_dir().join(format!("rightkit-approvals-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        let other = base.join("other");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        assert!(folder_is_or_may_be_real(&real, &real), "same path");
        assert!(
            folder_is_or_may_be_real(&other.join("..").join("real"), &real),
            "a `..` spelling of the real folder is the real folder"
        );
        assert!(
            !folder_is_or_may_be_real(&other, &real),
            "a different folder is not"
        );
        assert!(
            folder_is_or_may_be_real(&base.join("missing"), &real),
            "an explicit folder that cannot be canonicalised is refused"
        );
        // The real folder not existing yet: resolved through its nearest existing ancestor.
        let future = base.join("not-yet").join("DemoApp");
        assert!(!folder_is_or_may_be_real(&other, &future));
        #[cfg(unix)]
        {
            let link = base.join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(
                folder_is_or_may_be_real(&link, &real),
                "a symlink to the real folder is the real folder"
            );
        }
        #[cfg(any(target_os = "macos", windows))]
        {
            let upper = PathBuf::from(real.to_string_lossy().to_uppercase());
            // Case-insensitive file systems resolve the upper-cased spelling to the same folder; the
            // comparison itself is case-insensitive either way.
            if std::fs::canonicalize(&upper).is_ok() {
                assert!(
                    folder_is_or_may_be_real(&upper, &real),
                    "case-insensitive on macOS and Windows"
                );
            }
            assert!(same_path(&real, &upper), "comparison ignores case here");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    // --- Blender finish approval: issue, consume, replay, tamper ---

    fn blender_ctx(label: &str) -> (Context, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "rightkit-blender-approval-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Context::new(dir.clone()), dir)
    }

    fn write_spec(dir: &Path) -> PathBuf {
        let path = dir.join("spec.json");
        let spec = json!({"schema": 1, "owner": "adrian", "product_id": "p", "variant": "v",
            "confirmed": true, "confirmed_by": "adrian",
            "parts": {"body": {"slots": {"main": "matte"}}},
            "finishes": {"matte": {"version": "1", "approved": true, "approved_by": "adrian"}}});
        std::fs::write(&path, spec.to_string()).unwrap();
        path
    }

    fn test_gate(nonce: &str, secret: &str, draft: &str, hash: &str) -> Gate {
        Gate {
            secret: secret.to_string(),
            channel: CHANNEL_SIDECAR,
            origin: Origin::default(),
            token: proto::mint_token(secret, proto::BLENDER_KIND, draft, hash, nonce),
            nonce: nonce.to_string(),
        }
    }

    #[test]
    fn blender_issue_and_consume_write_a_receipt_the_provider_check_accepts() {
        let (ctx, dir) = blender_ctx("ok");
        let path = write_spec(&dir);
        let secret = "s".repeat(proto::MIN_SECRET_LEN);
        let (_, view, hash) = blender_inputs(&json!({"path": path.to_string_lossy()}), "").unwrap();
        let challenge = issue_labeled(&ctx, proto::BLENDER_KIND, &hash, &view, &hash).unwrap();
        let nonce = challenge["nonce"].as_str().unwrap().to_string();
        let ch_hash = challenge["input_hash"].as_str().unwrap().to_string();
        let gate = test_gate(&nonce, &secret, &hash, &ch_hash);
        let record = consume(&ctx, &gate, proto::BLENDER_KIND, &hash, &view, &hash).unwrap();
        let mut spec: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        stamp_blender_approval(&mut spec, &record);
        // The stamped spec keeps the same draft id, and both records validate against it.
        assert_eq!(proto::blender_draft_id(&spec), hash);
        assert!(proto::receipt_valid(
            &ctx.data_dir,
            proto::BLENDER_KIND,
            &hash,
            &ch_hash,
            &spec["confirmation"]
        ));
        assert!(proto::receipt_valid(
            &ctx.data_dir,
            proto::BLENDER_KIND,
            &hash,
            &ch_hash,
            &spec["finishes"]["matte"]["approval"]
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blender_token_replay_and_tampering_are_refused() {
        let (ctx, dir) = blender_ctx("tamper");
        let path = write_spec(&dir);
        let secret = "s".repeat(proto::MIN_SECRET_LEN);
        let (_, view, hash) = blender_inputs(&json!({"path": path.to_string_lossy()}), "").unwrap();
        let challenge = issue_labeled(&ctx, proto::BLENDER_KIND, &hash, &view, &hash).unwrap();
        let nonce = challenge["nonce"].as_str().unwrap().to_string();
        let ch_hash = challenge["input_hash"].as_str().unwrap().to_string();

        // A token minted with another secret does not verify.
        let other_secret = test_gate(&nonce, &"x".repeat(proto::MIN_SECRET_LEN), &hash, &ch_hash);
        let forged = Gate {
            secret: secret.clone(),
            ..other_secret
        };
        assert_eq!(
            consume(&ctx, &forged, proto::BLENDER_KIND, &hash, &view, &hash)
                .unwrap_err()
                .code,
            "approval_token_invalid"
        );

        // The spec edited after the challenge: refused, and the challenge is not spent.
        let mut edited = view.clone();
        edited["spec"]["finishes"]["matte"]["version"] = json!("2");
        let gate = test_gate(&nonce, &secret, &hash, &ch_hash);
        assert_eq!(
            consume(&ctx, &gate, proto::BLENDER_KIND, &hash, &edited, &hash)
                .unwrap_err()
                .code,
            "approval_input_changed"
        );

        // A token for another draft id does not approve this one.
        let other_draft = test_gate(&nonce, &secret, &"d".repeat(64), &ch_hash);
        assert!(consume(&ctx, &other_draft, proto::BLENDER_KIND, &hash, &view, &hash).is_err());

        // The right inputs consume once; the replay is refused.
        consume(&ctx, &gate, proto::BLENDER_KIND, &hash, &view, &hash).unwrap();
        assert_eq!(
            consume(&ctx, &gate, proto::BLENDER_KIND, &hash, &view, &hash)
                .unwrap_err()
                .code,
            "approval_token_used"
        );

        // A caller with no token cannot reach the writer: `operator_approved` approves nothing.
        let refused = gate_of(
            &json!({"path": path.to_string_lossy(), "draft_id": hash, "operator_approved": true}),
        );
        assert_eq!(refused, "approval_requires_app");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn gate_of(input: &Value) -> String {
        match gate(input) {
            Ok(_) => "ok".into(),
            Err(e) => e.code,
        }
    }
}

// Test binaries must never reach the OS credential store or the user's real data folder.
#[cfg(test)]
mod no_keychain_in_tests {
    #[ctor::ctor(unsafe)]
    fn guard() {
        std::env::set_var("RIGHTKIT_NO_KEYCHAIN", "1");
        if std::env::var_os("RIGHTKIT_APP_DATA")
            .filter(|v| !v.is_empty())
            .is_none()
        {
            let dir = std::env::temp_dir()
                .join(format!("rightkit-approvals-unit-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("isolated test data folder");
            std::env::set_var("RIGHTKIT_APP_DATA", dir);
        }
    }
}
