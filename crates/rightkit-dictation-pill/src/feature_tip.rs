//! The feature-tip cadence: after the first `first_at` completed transcripts a tip comes due, then
//! each next one `interval_base` plus a `0..interval_jitter` jitter (`now_ms % interval_jitter`)
//! completed transcripts later, cycling through the host's kinds in order. The state is a small
//! JSON object the host keeps in one file; its keys, thresholds, size bound and the list of kinds
//! are the host's, so a file already on users' disks keeps working.
//!
//! Hosts choose when and how to display a due tip.

use std::io::Read;
use std::path::Path;

use serde_json::{json, Map, Value};

/// The host's cadence values.
#[derive(Clone, Copy, Debug)]
pub struct Cadence<'a> {
    /// Completed transcripts before the first tip.
    pub first_at: u64,
    /// Transcripts between tips, before the jitter.
    pub interval_base: u64,
    /// Jitter modulus: `now_ms % interval_jitter` is added to the interval (`>= 1`; `0` reads as
    /// no jitter).
    pub interval_jitter: u64,
    /// A state file larger than this is not the host's and is read as absent.
    pub max_state_bytes: u64,
    /// JSON key of the completed-transcript count.
    pub completed_key: &'a str,
    /// JSON key of the count at which the next tip is due.
    pub next_at_key: &'a str,
    /// JSON key of the kind cursor.
    pub cursor_key: &'a str,
}

/// The outcome of counting one transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// The state to persist.
    pub state: Value,
    /// When a tip is due: the index (`cursor % kinds`) of the kind to show.
    pub due_index: Option<usize>,
    pub completed: u64,
    pub next_at: u64,
}

/// Milliseconds since the Unix epoch (`0` when the clock is before it).
pub fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// The most bytes of state file this module ever reads, whatever `Cadence::max_state_bytes` says.
pub const MAX_STATE_BYTES_LIMIT: u64 = 64 * 1024;

/// Read the cadence state through one handle with a byte cap (no metadata check and reopen). The
/// cap is `Cadence::max_state_bytes` bounded by [`MAX_STATE_BYTES_LIMIT`], and the read stops one
/// byte past it, so an enormous or endless file is never read whole; a larger file reads as absent.
pub fn read_state(cadence: &Cadence<'_>, path: &Path) -> Option<Value> {
    let cap = cadence.max_state_bytes.min(MAX_STATE_BYTES_LIMIT);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > cap {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Count one completed transcript against the saved state (missing or malformed fields read as a
/// fresh state) and say whether a tip is due.
pub fn advance(cadence: &Cadence<'_>, saved: &Value, now_ms: u64, kinds: usize) -> Step {
    let read = |key: &str| saved.get(key).and_then(Value::as_u64);
    let completed = read(cadence.completed_key).unwrap_or(0).saturating_add(1);
    let next_at = read(cadence.next_at_key).unwrap_or(cadence.first_at);
    let cursor = read(cadence.cursor_key).unwrap_or(0);
    let due = completed >= next_at;
    let next_cursor = if due { cursor.saturating_add(1) } else { cursor };
    let jitter = if cadence.interval_jitter == 0 {
        0
    } else {
        now_ms % cadence.interval_jitter
    };
    let next_interval = cadence.interval_base.saturating_add(jitter);
    let next_threshold = if due {
        completed.saturating_add(next_interval)
    } else {
        next_at
    };
    let mut state = Map::new();
    state.insert(cadence.completed_key.to_string(), json!(completed));
    state.insert(cadence.next_at_key.to_string(), json!(next_threshold));
    state.insert(cadence.cursor_key.to_string(), json!(next_cursor));
    Step {
        state: Value::Object(state),
        due_index: if due && kinds > 0 {
            Some((cursor % kinds as u64) as usize)
        } else {
            None
        },
        completed,
        next_at: next_threshold,
    }
}

/// Count one completed transcript: read the state at `path`, advance it, persist it with the host's
/// atomic writer (`replace(path, bytes)`; a failed write is ignored, as before) and return the step.
pub fn note_completed(
    cadence: &Cadence<'_>,
    path: &Path,
    kinds: usize,
    now_ms: u64,
    replace: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
) -> Step {
    let saved = read_state(cadence, path).unwrap_or_else(|| json!({}));
    let step = advance(cadence, &saved, now_ms, kinds);
    if let Ok(bytes) = serde_json::to_vec_pretty(&step.state) {
        let _ = replace(path, &bytes);
    }
    step
}
