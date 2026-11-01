//! Hook receipts: a hook run ends in one of six receipt states (`HookReceiptState`) and leaves a
//! per-session heartbeat under `membrane-hook-seen/` in the host app's format. A run that returns context
//! also leaves a durable receipt under `receipts/<session>/` (A5): `emitted` when the hook returned
//! the context, `delivered` only once `confirm_delivered` finds its marker in the host's own
//! transcript.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How one hook run ended (A5). `Emitted`: the hook returned schema-valid context within its
/// deadline. `Delivered`: the context's marker was seen in the host's own record of the turn;
/// only `confirm_delivered` sets it. `Empty`: the run was fine and had nothing to deliver.
/// `Off`: Membrane is not serving this run (no store, schema mismatch, model-free refusal).
/// `Refused`: the input or request was rejected. `Unknown`: an internal error, so nothing can be
/// said about delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookReceiptState {
    Emitted,
    Delivered,
    Empty,
    Off,
    Refused,
    Unknown,
}

impl HookReceiptState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Emitted => "emitted",
            Self::Delivered => "delivered",
            Self::Empty => "empty",
            Self::Off => "off",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
        }
    }
}

/// Content-free record of one hook run. `chars` is the length of the context emitted, never
/// the context itself. `id` is the receipt id carried by the context's marker; empty when the
/// run emitted nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookReceipt {
    pub state: HookReceiptState,
    pub reason: String,
    pub event: String,
    pub wire: String,
    pub chars: usize,
    pub id: String,
}

/// Directory of per-session heartbeats under the state root.
pub const HOOK_SEEN_DIR: &str = "membrane-hook-seen";

/// Directory of durable context receipts under the state root: `receipts/<session>/<id>.json`.
pub const RECEIPTS_DIR: &str = "receipts";

/// Wall budget for one `confirm_delivered` call.
const CONFIRM_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// A host-supplied session id as a file name: anything that is not `[A-Za-z0-9._-]` becomes
/// `_`, and it is cut at 128 characters; an id that is empty or only dots has no name.
fn session_file_name(session: &str) -> Option<String> {
    let name: String = session
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    if name.is_empty() || name.chars().all(|ch| ch == '.') {
        return None;
    }
    Some(name)
}

/// Where the heartbeat for `session` lives; an id with no usable name has no heartbeat.
pub fn hook_seen_path(state_root: &Path, session: &str) -> Option<PathBuf> {
    Some(
        state_root
            .join(HOOK_SEEN_DIR)
            .join(session_file_name(session)?),
    )
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Replace the heartbeat of `session` with now and `event`, as `<unix seconds> <tag>` (the
/// event name without non-alphanumerics, 32 characters at most). A heartbeat says the hook ran,
/// whatever it delivered.
pub fn write_hook_heartbeat(state_root: &Path, session: &str, event: &str) -> std::io::Result<()> {
    let Some(path) = hook_seen_path(state_root, session) else {
        return Ok(());
    };
    let tag: String = event
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(32)
        .collect();
    let written = write_atomic(&path, format!("{} {tag}", unix_seconds()).as_bytes());
    if let Some(dir) = path.parent() {
        prune_hook_seen(dir);
    }
    written
}

/// Heartbeats not written for this long are removed.
const HOOK_SEEN_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(14 * 24 * 3600);
/// Entries looked at per write, so a large folder never costs a hook run more than this.
const HOOK_SEEN_PRUNE_SCAN: usize = 256;

/// Remove heartbeat files older than 14 days (by modified time), looking at no more than
/// `HOOK_SEEN_PRUNE_SCAN` entries. Only plain files are removed.
fn prune_hook_seen(dir: &Path) {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in listing.flatten().take(HOOK_SEEN_PRUNE_SCAN) {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let old = meta.is_file()
            && meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > HOOK_SEEN_MAX_AGE);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rightkit_fs::atomic_write_private(path, bytes).map_err(std::io::Error::other)
}

/// A new receipt id: 16 lowercase hex characters, unique per run.
pub fn new_receipt_id() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as u64)
            .unwrap_or_default();
        bytes = (nanos ^ u64::from(std::process::id())).to_le_bytes();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The final line of an emitted context, which `confirm_delivered` looks for in the transcript.
pub fn receipt_marker(id: &str) -> String {
    rightkit_hooks::marker("membrane", id)
}

/// Durable record of one emitted context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextReceipt {
    pub id: String,
    pub session: String,
    pub harness: String,
    pub event: String,
    pub state: HookReceiptState,
    pub ts: u64,
}

impl ContextReceipt {
    /// A receipt for context just returned by the hook: `emitted`, stamped now.
    pub fn emitted(id: &str, session: &str, harness: &str, event: &str) -> Self {
        Self {
            id: id.to_owned(),
            session: session.to_owned(),
            harness: harness.to_owned(),
            event: event.to_owned(),
            state: HookReceiptState::Emitted,
            ts: unix_seconds(),
        }
    }
}

/// A receipt id a caller names: 1 to 128 letters, digits, `-` or `_`. Hook receipt ids are hex,
/// and a caller's own request id (`context pull --request-id`) may carry `-` or `_`.
pub fn valid_receipt_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn receipt_path(state_root: &Path, session: &str, id: &str) -> Option<PathBuf> {
    if !valid_receipt_id(id) {
        return None;
    }
    Some(
        state_root
            .join(RECEIPTS_DIR)
            .join(session_file_name(session)?)
            .join(format!("{id}.json")),
    )
}

/// Write (or replace) the durable record of `receipt`.
pub fn write_context_receipt(state_root: &Path, receipt: &ContextReceipt) -> std::io::Result<()> {
    let Some(path) = receipt_path(state_root, &receipt.session, &receipt.id) else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(receipt).map_err(std::io::Error::other)?;
    write_atomic(&path, &bytes)
}

/// Every durable context receipt of `session`, or of all sessions, oldest first.
pub fn list_context_receipts(state_root: &Path, session: Option<&str>) -> Vec<ContextReceipt> {
    let root = state_root.join(RECEIPTS_DIR);
    let directories: Vec<PathBuf> = match session {
        Some(session) => session_file_name(session)
            .map(|name| vec![root.join(name)])
            .unwrap_or_default(),
        None => std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect(),
    };
    let mut receipts: Vec<ContextReceipt> = directories
        .iter()
        .flat_map(|directory| std::fs::read_dir(directory).into_iter().flatten().flatten())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<ContextReceipt>(&bytes).ok())
        .collect();
    receipts.sort_by(|a, b| (a.ts, &a.id).cmp(&(b.ts, &b.id)));
    receipts
}

/// Set `delivered` on every `emitted` receipt of `session` and `harness` whose marker appears in the tail of
/// the host transcript at `transcript` (Claude JSONL transcript, Codex rollout JSONL). Reads at
/// most the last 4 MiB, and nothing when the session has no emitted receipt; gives up after 2 s.
/// A miss leaves the receipt `emitted`. Returns how many receipts it flipped.
pub fn confirm_delivered(
    state_root: &Path,
    session: &str,
    harness: &str,
    transcript: &Path,
) -> usize {
    let pending: Vec<ContextReceipt> = list_context_receipts(state_root, Some(session))
        .into_iter()
        .filter(|receipt| receipt.state == HookReceiptState::Emitted && receipt.harness == harness)
        .collect();
    if pending.is_empty() {
        return 0;
    }
    let markers: Vec<String> = pending
        .iter()
        .map(|receipt| receipt_marker(&receipt.id))
        .collect();
    let found = rightkit_hooks::scan_transcript_tail(transcript, &markers, CONFIRM_BUDGET);
    let mut flipped = 0;
    for mut receipt in pending {
        if found.contains(&receipt_marker(&receipt.id)) {
            receipt.state = HookReceiptState::Delivered;
            if write_context_receipt(state_root, &receipt).is_ok() {
                flipped += 1;
            }
        }
    }
    flipped
}

/// Set `delivered` on the `emitted` receipts named `id`, in any session, and return every receipt
/// with that id as it stands after the call. A receipt in any other state is returned unchanged.
/// This is how a caller that injected context itself (CodeRight) reports the delivery.
pub fn mark_delivered(state_root: &Path, id: &str) -> std::io::Result<Vec<ContextReceipt>> {
    let mut found: Vec<ContextReceipt> = list_context_receipts(state_root, None)
        .into_iter()
        .filter(|receipt| receipt.id == id)
        .collect();
    for receipt in &mut found {
        if receipt.state == HookReceiptState::Emitted {
            receipt.state = HookReceiptState::Delivered;
            write_context_receipt(state_root, receipt)?;
        }
    }
    Ok(found)
}
