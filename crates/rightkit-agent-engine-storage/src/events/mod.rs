//! Append-only event log. Powers SSE replay / catch-up.

mod append;
mod identity;
mod query;
mod recovery;
mod replay;
mod scrollback;
mod search;

pub(crate) use append::spill_oversized_payload;
pub use identity::{
    tool_call_origin_id, DecodedEvent, EventPersistenceSnapshot, EventPersistenceState,
    PersistenceGapError, PersistenceStateError,
};
pub use recovery::QuarantineOutcome;
pub use replay::{EventPage, RESUME_PAGE_ROWS};
pub use search::SessionMatch;

use rightkit_db::rusqlite;
use serde::{Deserialize, Serialize};

/// A single persisted event in the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    pub session_id: String,
    pub seq: i64,
    pub kind: String,
    pub payload_json: String,
    pub created_at: String,
}

pub(crate) fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        session_id: row.get(0)?,
        seq: row.get(1)?,
        kind: row.get(2)?,
        payload_json: row.get(3)?,
        created_at: row.get(4)?,
    })
}

/// The SQLite operation class a latency sample belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SqliteOpKind {
    /// `Storage::append_event`: durable event admission.
    AppendEvent,
    /// `Storage::all_events`: full per-session replay read.
    ReadEvents,
}

impl SqliteOpKind {
    /// Stable metric label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::AppendEvent => "append_event",
            Self::ReadEvents => "read_events",
        }
    }
}

/// Observer called with the duration of every timed SQLite operation.
pub type SqliteOpObserver = fn(SqliteOpKind, std::time::Duration);

static SQLITE_OP_OBSERVER: std::sync::OnceLock<SqliteOpObserver> = std::sync::OnceLock::new();

/// Install the process-wide SQLite latency observer (for example a metrics
/// recorder). Only the first call takes effect; returns `false` if one was
/// already installed.
pub fn set_sqlite_op_observer(observer: SqliteOpObserver) -> bool {
    SQLITE_OP_OBSERVER.set(observer).is_ok()
}

/// Records the wall-clock duration of one SQLite operation on every exit
/// path — success or error — so OPS-003 latency reflects real operations,
/// never only the fast/successful tail.
pub(crate) struct SqliteOpTimer {
    kind: SqliteOpKind,
    started: std::time::Instant,
}

impl SqliteOpTimer {
    pub(crate) fn start(kind: SqliteOpKind) -> Self {
        Self {
            kind,
            started: std::time::Instant::now(),
        }
    }
}

impl Drop for SqliteOpTimer {
    fn drop(&mut self) {
        if let Some(observer) = SQLITE_OP_OBSERVER.get() {
            observer(self.kind, self.started.elapsed());
        }
    }
}
