//! Delivery receipts at two levels.
//!
//! - `emitted`: the hook printed schema-valid output. Pulse's chat `delivered` is this level: the
//!   claim happens before the print, so a rejected or truncated output still counts.
//! - `observed`: the context's marker line was found in the host's own transcript (Claude JSONL
//!   transcript, Codex rollout JSONL), read back after the turn. Only [`ReceiptStore::observe`]
//!   sets it.
//!
//! The marker is the last line of the emitted context (`[<prefix>:<id>]`, Membrane's
//! `receipt_marker` with a caller-chosen prefix). A receipt is content-free: it keeps the
//! length of the context, never the context. [`ObserveModule`] is the transcript-scan hook.
//!
//! A module that timed out in the dispatcher keeps running on its detached thread and can still
//! write a receipt after the hook has replied; such a receipt is real but late.

use crate::dispatch::{HookModule, ModuleContext, ModuleOutput};
use crate::event::{EventKind, HookEvent, Wire};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Most of a transcript's tail one scan reads (Membrane's `CONFIRM_TAIL_BYTES`).
pub const SCAN_TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// Wall budget of one scan (Membrane's `CONFIRM_BUDGET`).
pub const SCAN_BUDGET: Duration = Duration::from_secs(2);
const RECEIPTS_DIR: &str = "receipts";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryLevel {
    Emitted,
    Observed,
}

/// Content-free record of one emitted context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub id: String,
    pub session: String,
    pub wire: String,
    pub event: String,
    pub level: DeliveryLevel,
    /// Bytes of context emitted, marker included.
    pub chars: usize,
    pub ts: u64,
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A fresh alphanumeric receipt id: nanosecond clock, process id and a per-process counter,
/// concatenated (not mixed), so ids from different processes or calls cannot collide unless the
/// clock, pid and counter all agree.
pub fn new_receipt_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed) & 0xffff;
    format!("{nanos:016x}{:08x}{count:04x}", std::process::id())
}

/// The marker line for `id`: `[prefix:id]`.
pub fn marker(prefix: &str, id: &str) -> String {
    format!("[{prefix}:{id}]")
}

/// Append the marker as the final line of `context`, cutting the body (never the marker) so the
/// whole stays within `cap_bytes`. When the cap is smaller than the marker line itself, the marker
/// is skipped and the context is cut to the cap: the result never exceeds `cap_bytes`.
pub fn tag_context(context: &str, prefix: &str, id: &str, cap_bytes: usize) -> String {
    let tail = format!("\n{}", marker(prefix, id));
    let (room, tail) = match cap_bytes.checked_sub(tail.len()) {
        Some(room) => (room, tail),
        None => (cap_bytes, String::new()),
    };
    let mut cut = context.len().min(room);
    while !context.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{tail}", &context[..cut])
}

/// Pure scan: which of `markers` appear in the last [`SCAN_TAIL_BYTES`] of the file, within
/// `budget`. A missing or unreadable file finds nothing.
pub fn scan_transcript_tail(path: &Path, markers: &[String], budget: Duration) -> Vec<String> {
    let started = Instant::now();
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(length) = file.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    if file
        .seek(SeekFrom::Start(length.saturating_sub(SCAN_TAIL_BYTES)))
        .is_err()
    {
        return Vec::new();
    }
    let mut tail = Vec::new();
    if file.take(SCAN_TAIL_BYTES).read_to_end(&mut tail).is_err() {
        return Vec::new();
    }
    let tail = String::from_utf8_lossy(&tail);
    markers
        .iter()
        .take_while(|_| started.elapsed() < budget)
        .filter(|marker| tail.contains(marker.as_str()))
        .cloned()
        .collect()
}

/// A host session id as a file name: characters outside `[A-Za-z0-9._-]` become `_`, cut at 128;
/// empty or all-dot ids have no name.
fn session_file_name(session: &str) -> Option<String> {
    let name: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    (!name.chars().all(|c| c == '.')).then_some(name)
}

/// Receipts on disk: `<root>/receipts/<session>/<id>.json`.
#[derive(Clone, Debug)]
pub struct ReceiptStore {
    root: PathBuf,
    prefix: String,
}

impl ReceiptStore {
    /// `prefix` is the marker prefix this producer uses (for example `pulse`).
    pub fn new(root: impl Into<PathBuf>, prefix: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            prefix: prefix.into(),
        }
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn path(&self, session: &str, id: &str) -> Option<PathBuf> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        Some(
            self.root
                .join(RECEIPTS_DIR)
                .join(session_file_name(session)?)
                .join(format!("{id}.json")),
        )
    }

    fn write(&self, receipt: &Receipt) -> std::io::Result<()> {
        let Some(path) = self.path(&receipt.session, &receipt.id) else {
            return Err(std::io::Error::other(
                "receipt id or session is not a file name",
            ));
        };
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("no parent"))?;
        std::fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(".{}.tmp", receipt.id));
        std::fs::write(
            &tmp,
            serde_json::to_vec(receipt).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(&tmp, &path)
    }

    /// Record context just printed: level `emitted`. `context` is the text including its marker.
    pub fn record_emitted(
        &self,
        event: &HookEvent,
        id: &str,
        context: &str,
    ) -> std::io::Result<Receipt> {
        let session = event
            .session_id
            .as_deref()
            .ok_or_else(|| std::io::Error::other("event has no session id"))?;
        let receipt = Receipt {
            id: id.to_owned(),
            session: session.to_owned(),
            wire: event.wire.as_str().to_owned(),
            event: event.kind.to_string(),
            level: DeliveryLevel::Emitted,
            chars: context.len(),
            ts: unix_seconds(),
        };
        self.write(&receipt)?;
        Ok(receipt)
    }

    /// Tag `context` with a fresh marker, record it `emitted`, and return the tagged text to print.
    pub fn tag_and_record(
        &self,
        event: &HookEvent,
        context: &str,
        cap_bytes: usize,
    ) -> std::io::Result<String> {
        let id = new_receipt_id();
        let tagged = tag_context(context, &self.prefix, &id, cap_bytes);
        self.record_emitted(event, &id, &tagged)?;
        Ok(tagged)
    }

    /// Every receipt of `session`, oldest first.
    pub fn list(&self, session: &str) -> Vec<Receipt> {
        let Some(name) = session_file_name(session) else {
            return Vec::new();
        };
        let mut receipts: Vec<Receipt> = std::fs::read_dir(self.root.join(RECEIPTS_DIR).join(name))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read(e.path()).ok())
            .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
            .collect();
        receipts.sort_by(|a: &Receipt, b| (a.ts, &a.id).cmp(&(b.ts, &b.id)));
        receipts
    }

    /// Flip to `observed` every `emitted` receipt of `session` and `wire` whose marker is in the
    /// transcript tail. A miss leaves it `emitted`. Returns how many flipped.
    pub fn observe(&self, session: &str, wire: Wire, transcript: &Path) -> usize {
        let pending: Vec<Receipt> = self
            .list(session)
            .into_iter()
            .filter(|r| r.level == DeliveryLevel::Emitted && r.wire == wire.as_str())
            .collect();
        if pending.is_empty() {
            return 0;
        }
        let markers: Vec<String> = pending
            .iter()
            .map(|r| marker(&self.prefix, &r.id))
            .collect();
        let found = scan_transcript_tail(transcript, &markers, SCAN_BUDGET);
        pending
            .into_iter()
            .zip(markers)
            .filter(|(_, marker)| found.contains(marker))
            .filter(|(receipt, _)| {
                let mut receipt = receipt.clone();
                receipt.level = DeliveryLevel::Observed;
                self.write(&receipt).is_ok()
            })
            .count()
    }
}

/// The transcript-scan hook: on the chat's own `Stop`, mark emitted receipts `observed`.
pub struct ObserveModule {
    store: ReceiptStore,
}

impl ObserveModule {
    pub fn new(store: ReceiptStore) -> Self {
        Self { store }
    }
}

impl HookModule for ObserveModule {
    fn id(&self) -> &str {
        "rightkit.receipts.observe"
    }

    fn handle(&self, event: &HookEvent, _ctx: &ModuleContext) -> Result<ModuleOutput, String> {
        let (EventKind::Stop, true) = (&event.kind, event.is_own_turn()) else {
            return Ok(ModuleOutput::empty());
        };
        let (Some(session), Some(path)) = (&event.session_id, &event.transcript_path) else {
            return Ok(ModuleOutput::empty());
        };
        let observed = self.store.observe(session, event.wire, Path::new(path));
        Ok(ModuleOutput {
            detail: json!({"observed": observed}),
            ..ModuleOutput::default()
        })
    }
}
