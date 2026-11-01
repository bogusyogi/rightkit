//! Durable source exclusions outlive rebuildable Ledger projections.
//!
//! The Membrane catalog holds the policy record. The Ledger index contains
//! only a deny-only cache; deleting/rebuilding it cannot re-enroll an erased
//! source. A failed catalog read is unavailable, never an empty permission set.
use super::{limits::WorkBudget, LedgerDb};
use crate::catalog::ContextCatalog;
use rightkit_db::rusqlite::{self, params};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ledger_source_exclusions (
 repository_root TEXT NOT NULL,path_digest TEXT NOT NULL,excluded_at_ms INTEGER NOT NULL,
 PRIMARY KEY(repository_root,path_digest));";

/// Read-path erasure handling. Retrieval connections never take a write
/// transaction against the index file — a maintenance sync may hold one for
/// a whole corpus walk — so the durable catalog exclusions are mirrored into
/// a per-connection TEMP table (`ledger_read_exclusions`) that the fence
/// checks union with `ledger_erasure_fences`. TEMP objects live in the
/// connection's own temp store, never in the index file, so installing the
/// view cannot contend with a writer. The catalog remains the authority:
/// a source erased after the last fence propagation is still excluded from
/// reads without resurrecting.
///
/// This writes only the connection's TEMP view, never the index file.
pub(crate) fn synchronize_read(
    catalog: &ContextCatalog,
    db: &LedgerDb,
    root: &str,
    budget: &WorkBudget,
) -> Result<(), String> {
    budget.check()?;
    let excluded = {
        let conn = catalog.lock();
        conn.execute_batch(SCHEMA)
            .map_err(|_| "ledger_policy_store_unavailable")?;
        let mut statement=conn.prepare("SELECT path_digest FROM ledger_source_exclusions WHERE repository_root=?1 ORDER BY path_digest LIMIT 16385")
            .map_err(|_|"ledger_policy_store_unavailable")?;
        let values = statement
            .query_map([root], |r| r.get::<_, String>(0))
            .map_err(|_| "ledger_policy_store_unavailable")?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "ledger_policy_store_unavailable")?;
        values
    };
    if excluded.len() > 16384 {
        return Err("ledger_policy_budget_exhausted".into());
    }
    let conn = db.lock();
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS ledger_read_exclusions (
        path_digest TEXT NOT NULL PRIMARY KEY); DELETE FROM ledger_read_exclusions;",
    )
    .map_err(|_| "ledger_policy_cache_unavailable")?;
    {
        let mut insert = conn
            .prepare("INSERT OR IGNORE INTO ledger_read_exclusions VALUES (?1)")
            .map_err(|_| "ledger_policy_cache_unavailable")?;
        for digest in excluded {
            budget.visit()?;
            insert
                .execute(rusqlite::params![digest])
                .map_err(|_| "ledger_policy_cache_unavailable")?;
        }
    }
    Ok(())
}

/// Ensure the TEMP exclusion view exists on a connection so fence checks can
/// union against it. Retrieval ops populate it via `synchronize_read`; any
/// other caller gets an empty view, which reduces the union to the
/// index-fence cache exactly as before.
pub(crate) fn ensure_read_exclusions(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS ledger_read_exclusions (
        path_digest TEXT NOT NULL PRIMARY KEY);",
    )
    .map_err(|_| "ledger_policy_cache_unavailable".to_string())
}

pub(crate) fn synchronize(
    catalog: &ContextCatalog,
    db: &LedgerDb,
    root: &str,
    budget: &WorkBudget,
) -> Result<(), String> {
    budget.check()?;
    // The catalog is the authority; the index fences are a projection of it and
    // never a read grant.
    let excluded = {
        let conn = catalog.lock();
        conn.execute_batch(SCHEMA)
            .map_err(|_| "ledger_policy_store_unavailable")?;
        let mut statement=conn.prepare("SELECT path_digest,excluded_at_ms FROM ledger_source_exclusions WHERE repository_root=?1 ORDER BY path_digest LIMIT 16385")
            .map_err(|_|"ledger_policy_store_unavailable")?;
        let values = statement
            .query_map([root], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(|_| "ledger_policy_store_unavailable")?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "ledger_policy_store_unavailable")?;
        values
    };
    if excluded.len() > 16384 {
        return Err("ledger_policy_budget_exhausted".into());
    }
    let conn = db.lock();
    for (digest, time) in excluded {
        budget.visit()?;
        conn.execute(
            "INSERT OR IGNORE INTO ledger_erasure_fences VALUES (?1,?2,?3)",
            params![root, digest, time],
        )
        .map_err(|_| "ledger_policy_cache_unavailable")?;
    }
    Ok(())
}
