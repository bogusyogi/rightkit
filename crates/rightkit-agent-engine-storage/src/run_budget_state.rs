//! Persisted per-run sub-agent budget state (Lane C3 task 2, additive).
//!
//! The `RunBudgetLedger` lives in process memory inside the daemon; on
//! restart the daemon re-reads the cumulative count from this table so a
//! long-running run does not silently reset its cumulative cap. The
//! concurrent count is intentionally NOT persisted — it lives in-memory and
//! resets to 0 on restart, which is the correct behavior because no
//! sub-agents can be in-flight across a daemon restart.

use crate::error::Result;
use crate::Storage;
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;

/// Wall-clock seconds since epoch, zero-padded RFC 3339 UTC. Mirrors the
/// shape `events::append::iso8601_now_secs` produces so column comparisons
/// remain comparable.
fn iso8601_now_secs() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

impl Storage {
    /// Persist the cumulative sub-agent count for `session_id`. Called after
    /// every successful `RunBudgetLedger::mark` so a daemon restart restores
    /// the cap correctly. Uses `INSERT OR REPLACE` so re-persisting on the
    /// same session is a no-op on the row count.
    pub fn upsert_run_budget(&mut self, session_id: &str, cumulative_used: u32) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO run_budget_state(session_id, cumulative_used, last_admit_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![session_id, cumulative_used as i64, iso8601_now_secs(),],
        )?;
        Ok(())
    }

    /// Restore the cumulative sub-agent count for `session_id` on daemon
    /// restart. Returns `None` when the session has no persisted budget
    /// row — the daemon then constructs a fresh ledger with the effective
    /// policy defaults.
    pub fn restore_run_budget(&self, session_id: &str) -> Result<Option<u32>> {
        let row: Option<i64> = self
            .conn
            .query_row(
                "SELECT cumulative_used FROM run_budget_state WHERE session_id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.map(|value| value.max(0) as u32))
    }
}
