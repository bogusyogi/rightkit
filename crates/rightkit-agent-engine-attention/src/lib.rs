// D4 — Attention: typed unread / working / needs-input / failed / complete
// status, transcript density modes (Summary / Normal / Verbose) that preserve
// durable history, deep-link notifications, quiet hours, per-project
// notification policy, and snooze.
//
// Standalone route + contract fragment for Wave 5 attention surfaces. Agent 4
// owns the central `api_router.rs` and central `api_contract` endpoint
// manifest; this module exposes builders and contract fragments the
// integration lane wires in. No central registration happens here.
//
// Coexistence with the existing /notifications surface:
//   - This module extends — not replaces — the existing `api_notifications`
//     handler by attaching typed attention states, deep links, and policy
//     fields. The /notifications store remains the durable backend; this lane
//     reads and writes through the same JSON file when present.
//   - Quiet hours and snooze live in `<state-dir>/attention/policy.json` so the
//     existing permissions/trust layer can read them without re-asking the
//     daemon.
//
// Extracted from `host-app/src/api_attention.rs` into this standalone
// crate (crate-extraction pilot). None of the handlers here ever read a
// `DaemonState` field — the module's own sub-state is empty — so it is
// mounted into the daemon's router via axum sub-state extraction
// (`FromRef<DaemonState>`) instead of depending on the binary crate's
// `DaemonState` type. `DaemonState` stays defined in the binary; this crate
// never names it.
//
// Naming note: the STEPS for this extraction specified a marker sub-state
// type named `AttentionState`. This crate already has a pre-existing, widely
// used domain enum of that exact name (`AttentionState::{Unread, Working,
// NeedsInput, Failed, Complete, Snoozed}` — the typed item/banner status, also
// the OpenAPI schema name `AttentionState`). The two cannot coexist under one
// name in one module, so the empty axum sub-state marker is named
// `AttentionRouterState` instead, preserving the pre-existing domain enum's
// name and its many call sites unchanged.

#![cfg_attr(not(feature = "server"), allow(dead_code))]

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub mod inbox;

pub mod act_plan;
pub mod duty;
pub mod phone_ask;
pub mod phone_relay;
#[cfg(feature = "server")]
mod server;
#[cfg(feature = "server")]
pub use server::{attention_routes, AttentionRouterState};

/// Host-specific names the attention routes use. Install once at startup with
/// [`configure`]; until then [`AttentionConfig::default`] applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionConfig {
    /// Hidden per-workspace state directory holding `notifications.json` and
    /// `attention/` (for example `.myapp`).
    pub state_dir: String,
    /// Request header carrying a lowered source trust (`untrusted`,
    /// `remote-agent-untrusted`, `tool-untrusted`); such requests are refused.
    pub source_trust_header: String,
    /// `source` reported for notification rows that do not name one.
    pub default_source: String,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self {
            state_dir: ".rightkit".to_string(),
            source_trust_header: "x-rightkit-source-trust".to_string(),
            default_source: "agent".to_string(),
        }
    }
}

static CONFIG: std::sync::OnceLock<AttentionConfig> = std::sync::OnceLock::new();

/// Install the host's [`AttentionConfig`]. Only the first call takes effect;
/// returns `false` if a configuration was already installed or read.
pub fn configure(config: AttentionConfig) -> bool {
    CONFIG.set(config).is_ok()
}

fn config() -> &'static AttentionConfig {
    CONFIG.get_or_init(AttentionConfig::default)
}

// ─── Typed attention states ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum AttentionState {
    Unread,
    Working,
    NeedsInput,
    Failed,
    Complete,
    Snoozed,
}

impl AttentionState {
    pub fn as_str(self) -> &'static str {
        match self {
            AttentionState::Unread => "unread",
            AttentionState::Working => "working",
            AttentionState::NeedsInput => "needsInput",
            AttentionState::Failed => "failed",
            AttentionState::Complete => "complete",
            AttentionState::Snoozed => "snoozed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptDensity {
    Summary,
    Normal,
    Verbose,
}

impl TranscriptDensity {
    pub fn as_str(self) -> &'static str {
        match self {
            TranscriptDensity::Summary => "summary",
            TranscriptDensity::Normal => "normal",
            TranscriptDensity::Verbose => "verbose",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub source: String,
    pub session_id: Option<String>,
    pub schedule_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub state: AttentionState,
    pub deeplink: Option<AttentionDeeplink>,
    pub snoozed_until: Option<String>,
    pub read: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionDeeplink {
    pub target: String,
    pub params: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionListResponse {
    pub workspace_root: String,
    pub density: TranscriptDensity,
    pub items: Vec<AttentionItem>,
    pub counts: AttentionCounts,
    pub quiet_hours_active: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionCounts {
    pub unread: usize,
    pub working: usize,
    pub needs_input: usize,
    pub failed: usize,
    pub complete: usize,
    pub snoozed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionBanner {
    pub banner_id: String,
    pub state: AttentionState,
    pub title: String,
    pub body: String,
    pub deeplink: Option<AttentionDeeplink>,
    pub workspace_root: String,
    pub generated_at: String,
}

// ─── Density / policy / quiet hours ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DensityResponse {
    pub workspace_root: String,
    pub density: TranscriptDensity,
    pub durable_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectNotificationPolicy {
    pub workspace_root: String,
    pub enabled: bool,
    pub quiet_progress_spam: bool,
    pub max_items_per_session: usize,
    pub persist_complete_for_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietHoursWindow {
    pub start: String,
    pub end: String,
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietHoursResponse {
    pub workspace_root: String,
    pub enabled: bool,
    pub windows: Vec<QuietHoursWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeeplinkResolution {
    pub resolved: bool,
    pub target: String,
    pub path: String,
    pub params: serde_json::Map<String, Value>,
    pub note: Option<String>,
}

// ─── Storage paths ────────────────────────────────────────────────────────────

fn attention_dir(workspace_root: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(workspace_root)
        .join(&config().state_dir)
        .join("attention")
}

fn density_path(workspace_root: &str) -> std::path::PathBuf {
    attention_dir(workspace_root).join("density.json")
}

fn policy_path(workspace_root: &str) -> std::path::PathBuf {
    attention_dir(workspace_root).join("policy.json")
}

fn quiet_hours_path(workspace_root: &str) -> std::path::PathBuf {
    attention_dir(workspace_root).join("quiet-hours.json")
}

fn snooze_path(workspace_root: &str) -> std::path::PathBuf {
    attention_dir(workspace_root).join("snooze.json")
}

fn ensure_attention_dir(workspace_root: &str) -> Result<std::path::PathBuf, String> {
    let dir = attention_dir(workspace_root);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create attention dir '{}': {e}", dir.display()))?;
    Ok(dir)
}

/// Reads a JSON state file. A missing file is `None`. A file that exists but
/// cannot be read or parsed is damaged: it is set aside (see
/// [`set_aside_damaged`]) before `None` is returned, so the default the caller
/// starts with is never saved over the only copy (orchestration rules 23, 33).
fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => {
            set_aside_damaged(path);
            return None;
        }
    };
    match serde_json::from_str::<T>(&raw) {
        Ok(value) => Some(value),
        Err(_) => {
            set_aside_damaged(path);
            None
        }
    }
}

/// Moves a damaged state file to `<name>.damaged-<unix seconds>` (with `-1`,
/// `-2`, ... when that name is taken). Best effort: a failed rename leaves the
/// file where it is.
fn set_aside_damaged(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file()) {
        return None;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let base = path.file_name()?.to_string_lossy().into_owned();
    for attempt in 0..100u32 {
        let name = if attempt == 0 {
            format!("{base}.damaged-{stamp}")
        } else {
            format!("{base}.damaged-{stamp}-{attempt}")
        };
        let to = path.with_file_name(name);
        if to.exists() {
            continue;
        }
        if std::fs::rename(path, &to).is_ok() {
            return Some(to);
        }
        return None;
    }
    None
}

/// The notification rows stored in `notifications.json`: empty when the file is
/// missing, and a damaged file is set aside first.
fn read_notification_rows(path: &std::path::Path) -> Vec<Value> {
    read_json::<Vec<Value>>(path).unwrap_or_default()
}

static ATTENTION_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serialises read-modify-write cycles on the attention files (notifications, snooze map), so two
/// concurrent requests cannot lose each other's update. Hold it across the whole cycle.
pub fn lock_attention_state() -> std::sync::MutexGuard<'static, ()> {
    ATTENTION_STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create dir '{}': {e}", parent.display()))?;
    }
    rightkit_fs::atomic_write(
        path,
        serde_json::to_string_pretty(value)
            .unwrap_or_default()
            .as_bytes(),
    )
    .map_err(|e| format!("failed to write '{}': {e}", path.display()))
}

fn enforce_workspace_root(root: &str) -> bool {
    let path = std::path::Path::new(root);
    !root.trim().is_empty()
        && path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        && path
            .canonicalize()
            .is_ok_and(|canonical| canonical.is_dir())
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("unix:{secs}")
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SnoozeMap {
    #[serde(default)]
    by_id: std::collections::BTreeMap<String, String>,
}

fn load_density(workspace_root: &str) -> TranscriptDensity {
    let path = density_path(workspace_root);
    let value: Value = read_json(&path).unwrap_or_default();
    match value
        .get("density")
        .and_then(Value::as_str)
        .unwrap_or("normal")
    {
        "summary" => TranscriptDensity::Summary,
        "verbose" => TranscriptDensity::Verbose,
        _ => TranscriptDensity::Normal,
    }
}

fn load_policy(workspace_root: &str) -> ProjectNotificationPolicy {
    let path = policy_path(workspace_root);
    read_json::<ProjectNotificationPolicy>(&path).unwrap_or_else(|| ProjectNotificationPolicy {
        workspace_root: workspace_root.to_string(),
        enabled: true,
        quiet_progress_spam: true,
        max_items_per_session: 50,
        persist_complete_for_hours: 24,
    })
}

fn load_quiet_hours(workspace_root: &str) -> QuietHoursResponse {
    let path = quiet_hours_path(workspace_root);
    read_json::<QuietHoursResponse>(&path).unwrap_or_else(|| QuietHoursResponse {
        workspace_root: workspace_root.to_string(),
        enabled: false,
        windows: Vec::new(),
    })
}

/// Whether quiet hours are in force now, reading every window as UTC.
///
/// Deprecated: a window's `HH:MM` times are local times in the window's `timezone`, and this
/// ignores the zone, so a window set for another zone is off by that zone's offset. Use
/// [`quiet_hours_active_in_zones`] with the caller's zone offset function.
#[deprecated(
    since = "0.1.4",
    note = "reads every window as UTC and ignores its timezone; use quiet_hours_active_in_zones"
)]
pub fn quiet_hours_active(workspace_root: &str, now_seconds: u64) -> bool {
    let qh = load_quiet_hours(workspace_root);
    if !qh.enabled || qh.windows.is_empty() {
        return false;
    }
    let hhmm = seconds_to_hhmm(now_seconds);
    qh.windows
        .iter()
        .any(|w| window_contains(&w.start, &w.end, &hhmm))
}

/// Whether quiet hours are in force at `now_seconds`, reading each window in its own zone.
///
/// `offset_for(zone, unix)` returns that zone's UTC offset in seconds, east positive, at that
/// instant (`zone` is the window's `timezone`, `None` when it names none, which the caller reads
/// as its default zone). The crate has no zone database, so the caller's does the work, DST
/// included. Same window rules as [`quiet_hours_active`]: `start > end` crosses midnight and
/// `start == end` is empty; a window with an unparsable time is ignored. Quiet hours that are
/// not enabled are never active.
pub fn quiet_hours_active_in_zones(
    workspace_root: &str,
    now_seconds: u64,
    offset_for: &dyn Fn(Option<&str>, i64) -> i32,
) -> bool {
    let qh = load_quiet_hours(workspace_root);
    if !qh.enabled || qh.windows.is_empty() {
        return false;
    }
    let now = i64::try_from(now_seconds).unwrap_or(i64::MAX);
    inbox::quiet_windows_contain(now, &qh.windows, offset_for)
}

/// The offset (seconds, east positive) of a quiet-hours window's zone at `unix`, from
/// `rightkit_time::zones::offset_at`. A window naming no zone is read as UTC. A zone that table
/// does not cover (any zone but UTC, Asia/Kolkata, Europe/London, America/New_York, or a DST
/// zone outside 2026 and 2027) is also read as UTC, as every window was before zones were
/// honoured; this is a fallback, not a guess about the user's zone. Pass it as `offset_for`.
pub fn zone_offset_secs(zone: Option<&str>, unix: i64) -> i32 {
    rightkit_time::zones::offset_at(zone.unwrap_or("UTC"), unix).unwrap_or(0)
}

/// If quiet hours are in force for `workspace_root` at `now_seconds`, the Unix second at which
/// they end (the first instant outside every window), reading each window in its own zone;
/// `None` when they are not enabled, not in force, or never end. Same rules as the inbox's
/// `inbox::quiet_hours_release_at`, over the stored settings.
pub fn quiet_hours_release_for_workspace(workspace_root: &str, now_seconds: u64) -> Option<u64> {
    let qh = load_quiet_hours(workspace_root);
    let now = i64::try_from(now_seconds).ok()?;
    inbox::quiet_hours_release_at(&qh, now, &zone_offset_secs).and_then(|t| u64::try_from(t).ok())
}

// FM-174: cross-midnight quiet-hours predicate. Repaired (see disposition in
// docs/research/canon-provenance/refactor0-closure-ledger.md) to treat
// `start > end` as a window that wraps past midnight (active from `start`
// through `23:59`, then from `00:00` up to but excluding `end`) instead of
// the earlier defective same-day-only comparison. Boundary coverage below.
fn window_contains(start: &str, end: &str, hhmm: &str) -> bool {
    if start <= end {
        // Daytime window (e.g. 09:00-17:00).
        start <= hhmm && hhmm < end
    } else {
        // Midnight-crossing window (e.g. 22:00-07:00): active after start OR before end.
        hhmm >= start || hhmm < end
    }
}

fn valid_hhmm(value: &str) -> bool {
    let Some((hours, minutes)) = value.split_once(':') else {
        return false;
    };
    hours.len() == 2
        && minutes.len() == 2
        && hours.parse::<u8>().is_ok_and(|hours| hours < 24)
        && minutes.parse::<u8>().is_ok_and(|minutes| minutes < 60)
}

fn seconds_to_hhmm(seconds: u64) -> String {
    let tod = seconds % 86_400;
    let hours = tod / 3_600;
    let minutes = (tod % 3_600) / 60;
    format!("{:02}:{:02}", hours, minutes)
}

fn build_attention_items(workspace_root: &str, density: TranscriptDensity) -> Vec<AttentionItem> {
    let notifications_path = std::path::PathBuf::from(workspace_root)
        .join(&config().state_dir)
        .join("notifications.json");
    let mut rows: Vec<Value> = read_notification_rows(&notifications_path);
    // A row held through quiet hours carries `releaseAt` (Unix seconds); it is not an item until then.
    let now = now_seconds();
    rows.retain(|row| {
        row.get("releaseAt")
            .and_then(Value::as_u64)
            .is_none_or(|release_at| release_at <= now)
    });
    // Apply density: summary collapses non-failed, non-needs-input rows.
    if matches!(density, TranscriptDensity::Summary) {
        rows.retain(|row| {
            let state = row.get("state").and_then(Value::as_str).unwrap_or("unread");
            matches!(state, "failed" | "needsInput" | "complete")
        });
    }
    let snooze_map = read_json::<SnoozeMap>(&snooze_path(workspace_root)).unwrap_or_default();
    let mut items = Vec::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let state_str = row.get("state").and_then(Value::as_str).unwrap_or("unread");
        let state = match state_str {
            "working" => AttentionState::Working,
            "needsInput" => AttentionState::NeedsInput,
            "failed" => AttentionState::Failed,
            "complete" => AttentionState::Complete,
            "snoozed" => AttentionState::Snoozed,
            _ => AttentionState::Unread,
        };
        let snoozed_until = snooze_map.by_id.get(&id).cloned();
        let effective_state = if snoozed_until.is_some() {
            AttentionState::Snoozed
        } else {
            state
        };
        items.push(AttentionItem {
            id: id.clone(),
            kind: row
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("desktopNotification")
                .to_string(),
            title: row
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            body: row
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            source: row
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or(config().default_source.as_str())
                .to_string(),
            session_id: row
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string),
            schedule_id: row
                .get("scheduleId")
                .and_then(Value::as_str)
                .map(str::to_string),
            created_at: row
                .get("createdAt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            updated_at: row
                .get("updatedAt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            state: effective_state,
            deeplink: deeplink_from_notification(&row),
            snoozed_until,
            read: row.get("read").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    items
}

fn deeplink_from_notification(row: &Value) -> Option<AttentionDeeplink> {
    let source = row.get("source").and_then(Value::as_str).unwrap_or("");
    let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
    let target = match (source, kind) {
        ("pr", _) | (_, "prFeedback") => "pr",
        ("check", _) | (_, "check") => "check",
        ("schedule", _) => "schedule",
        ("session", _) => "session",
        ("approval", _) => "approval",
        ("task", _) => "task",
        _ => "result",
    };
    let mut params = serde_json::Map::new();
    if let Some(id) = row.get("id").and_then(Value::as_str) {
        params.insert("id".to_string(), Value::String(id.to_string()));
    }
    if let Some(session) = row.get("sessionId").and_then(Value::as_str) {
        params.insert("sessionId".to_string(), Value::String(session.to_string()));
    }
    if let Some(schedule) = row.get("scheduleId").and_then(Value::as_str) {
        params.insert(
            "scheduleId".to_string(),
            Value::String(schedule.to_string()),
        );
    }
    Some(AttentionDeeplink {
        target: target.to_string(),
        params,
    })
}

fn count_attention(items: &[AttentionItem]) -> AttentionCounts {
    let mut counts = AttentionCounts::default();
    for item in items {
        match item.state {
            AttentionState::Unread => counts.unread += 1,
            AttentionState::Working => counts.working += 1,
            AttentionState::NeedsInput => counts.needs_input += 1,
            AttentionState::Failed => counts.failed += 1,
            AttentionState::Complete => counts.complete += 1,
            AttentionState::Snoozed => counts.snoozed += 1,
        }
    }
    counts
}

fn now_seconds() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

// ─── OpenAPI fragment ─────────────────────────────────────────────────────────

pub fn attention_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "AttentionState".to_string(),
        json!({
            "type": "string",
            "enum": ["unread", "working", "needsInput", "failed", "complete", "snoozed"]
        }),
    );
    schemas.insert(
        "TranscriptDensity".to_string(),
        json!({"type": "string", "enum": ["summary", "normal", "verbose"]}),
    );
    schemas.insert(
        "AttentionDeeplink".to_string(),
        json!({
            "type": "object",
            "required": ["target", "params"],
            "properties": {
                "target": {"type": "string"},
                "params": {"type": "object", "additionalProperties": true}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AttentionItem".to_string(),
        json!({
            "type": "object",
            "required": [
                "id",
                "kind",
                "title",
                "body",
                "source",
                "createdAt",
                "updatedAt",
                "state",
                "read"
            ],
            "properties": {
                "id": {"type": "string"},
                "kind": {"type": "string"},
                "title": {"type": "string"},
                "body": {"type": "string"},
                "source": {"type": "string"},
                "sessionId": {"type": ["string", "null"]},
                "scheduleId": {"type": ["string", "null"]},
                "createdAt": {"type": "string"},
                "updatedAt": {"type": "string"},
                "state": {"$ref": "#/components/schemas/AttentionState"},
                "deeplink": {"oneOf": [
                    {"type": "null"},
                    {"$ref": "#/components/schemas/AttentionDeeplink"}
                ]},
                "snoozedUntil": {"type": ["string", "null"]},
                "read": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AttentionCounts".to_string(),
        json!({
            "type": "object",
            "properties": {
                "unread": {"type": "integer", "minimum": 0},
                "working": {"type": "integer", "minimum": 0},
                "needsInput": {"type": "integer", "minimum": 0},
                "failed": {"type": "integer", "minimum": 0},
                "complete": {"type": "integer", "minimum": 0},
                "snoozed": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AttentionListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "density", "items", "counts", "quietHoursActive"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "density": {"$ref": "#/components/schemas/TranscriptDensity"},
                "items": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/AttentionItem"}
                },
                "counts": {"$ref": "#/components/schemas/AttentionCounts"},
                "quietHoursActive": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AttentionBanner".to_string(),
        json!({
            "type": "object",
            "required": ["bannerId", "state", "title", "body", "workspaceRoot", "generatedAt"],
            "properties": {
                "bannerId": {"type": "string"},
                "state": {"$ref": "#/components/schemas/AttentionState"},
                "title": {"type": "string"},
                "body": {"type": "string"},
                "deeplink": {"oneOf": [
                    {"type": "null"},
                    {"$ref": "#/components/schemas/AttentionDeeplink"}
                ]},
                "workspaceRoot": {"type": "string"},
                "generatedAt": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DensityResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "density", "durableHistory"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "density": {"$ref": "#/components/schemas/TranscriptDensity"},
                "durableHistory": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ProjectNotificationPolicy".to_string(),
        json!({
            "type": "object",
            "required": [
                "workspaceRoot",
                "enabled",
                "quietProgressSpam",
                "maxItemsPerSession",
                "persistCompleteForHours"
            ],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "enabled": {"type": "boolean"},
                "quietProgressSpam": {"type": "boolean"},
                "maxItemsPerSession": {"type": "integer", "minimum": 1},
                "persistCompleteForHours": {"type": "integer", "minimum": 0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "QuietHoursWindow".to_string(),
        json!({
            "type": "object",
            "required": ["start", "end"],
            "properties": {
                "start": {"type": "string", "pattern": "^\\d{2}:\\d{2}$"},
                "end": {"type": "string", "pattern": "^\\d{2}:\\d{2}$"},
                "timezone": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "QuietHoursResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "enabled", "windows"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "enabled": {"type": "boolean"},
                "windows": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/QuietHoursWindow"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DeeplinkResolution".to_string(),
        json!({
            "type": "object",
            "required": ["resolved", "target", "path", "params"],
            "properties": {
                "resolved": {"type": "boolean"},
                "target": {"type": "string"},
                "path": {"type": "string"},
                "params": {"type": "object", "additionalProperties": true},
                "note": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub fn attention_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("patch", "/attention/items/{id}/state") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "state"],
            "properties": {"workspaceRoot": {"type": "string"}, "state": {"$ref": "#/components/schemas/AttentionState"}},
            "additionalProperties": false
        })),
        ("post", "/attention/items/{id}/snooze") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "snoozeUntil"],
            "properties": {"workspaceRoot": {"type": "string"}, "snoozeUntil": {"type": "string"}},
            "additionalProperties": false
        })),
        ("patch", "/attention/density") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "density"],
            "properties": {"workspaceRoot": {"type": "string"}, "density": {"$ref": "#/components/schemas/TranscriptDensity"}},
            "additionalProperties": false
        })),
        ("patch", "/attention/policy") => {
            Some(json!({"$ref": "#/components/schemas/ProjectNotificationPolicy"}))
        }
        ("patch", "/attention/quiet-hours") => {
            Some(json!({"$ref": "#/components/schemas/QuietHoursResponse"}))
        }
        ("post", "/attention/deeplink") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "target", "params"],
            "properties": {"workspaceRoot": {"type": "string"}, "target": {"type": "string"}, "params": {"type": "object", "additionalProperties": true}},
            "additionalProperties": false
        })),
        _ => None,
    }
}

pub fn attention_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/attention/items") => {
            Some(json!({"$ref": "#/components/schemas/AttentionListResponse"}))
        }
        ("get", "/attention/density") | ("patch", "/attention/density") => {
            Some(json!({"$ref": "#/components/schemas/DensityResponse"}))
        }
        ("get", "/attention/policy") | ("patch", "/attention/policy") => {
            Some(json!({"$ref": "#/components/schemas/ProjectNotificationPolicy"}))
        }
        ("get", "/attention/quiet-hours") | ("patch", "/attention/quiet-hours") => {
            Some(json!({"$ref": "#/components/schemas/QuietHoursResponse"}))
        }
        ("post", "/attention/deeplink") => {
            Some(json!({"$ref": "#/components/schemas/DeeplinkResolution"}))
        }
        ("get", "/attention/banner") => Some(
            json!({"oneOf": [{"type": "null"}, {"$ref": "#/components/schemas/AttentionBanner"}]}),
        ),
        _ if path.starts_with("/attention/") => {
            Some(json!({"type": "object", "additionalProperties": true}))
        }
        _ => None,
    }
}
