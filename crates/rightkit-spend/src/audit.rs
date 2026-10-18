//! The cap-setting audit trail: one row per allowance create or limit change and per
//! device-allowance set or clear, written in the same transaction as the change.

use super::*;

/// One row of the cap audit trail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapAudit {
    pub id: i64,
    /// Unix seconds.
    pub at: i64,
    /// The ledger's actor ([`Ledger::with_actor`]) when the change was made.
    pub who: String,
    /// `allowance` or `device_allowance`.
    pub kind: String,
    pub scope: String,
    /// The allowance period the change touched. For a device allowance: the period string of
    /// the new epoch (`device:<id>:<epoch>`), which has no allowance row when the change cleared
    /// it.
    pub period: String,
    /// `None` when the allowance did not exist before (or no device allowance was set).
    pub old_limit_micro: Option<i64>,
    /// `None` when a device allowance was cleared.
    pub new_limit_micro: Option<i64>,
    /// Allowance revision before and after; for a device allowance, the epoch before and after.
    pub old_revision: Option<i64>,
    pub new_revision: Option<i64>,
    pub note: Option<String>,
}

/// Write one audit row. Called by the write that makes the change, inside its savepoint.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record(
    conn: &Connection,
    ctx: &Ctx,
    kind: &str,
    scope: &str,
    period: &str,
    limits: (Option<i64>, Option<i64>),
    revisions: (Option<i64>, Option<i64>),
    note: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO spend_cap_audit (at, who, kind, scope, period, old_limit_micro, \
         new_limit_micro, old_revision, new_revision, note) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            ctx.now,
            ctx.actor,
            kind,
            scope,
            period,
            limits.0,
            limits.1,
            revisions.0,
            revisions.1,
            note
        ],
    )?;
    Ok(())
}

impl Ledger {
    /// The newest `limit` audit rows, newest first, optionally only those of one `scope`.
    pub fn cap_audit(&self, scope: Option<&str>, limit: i64) -> Result<Vec<CapAudit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, at, who, kind, scope, period, old_limit_micro, new_limit_micro, \
             old_revision, new_revision, note FROM spend_cap_audit \
             WHERE (?1 IS NULL OR scope = ?1) ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![scope, limit], |row| {
            Ok(CapAudit {
                id: row.get(0)?,
                at: row.get(1)?,
                who: row.get(2)?,
                kind: row.get(3)?,
                scope: row.get(4)?,
                period: row.get(5)?,
                old_limit_micro: row.get(6)?,
                new_limit_micro: row.get(7)?,
                old_revision: row.get(8)?,
                new_revision: row.get(9)?,
                note: row.get(10)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
