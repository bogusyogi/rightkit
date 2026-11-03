//! SQLite query-plan probes and throwaway database paths for perf proofs.

use rightkit_db::rusqlite::{Connection, Params};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A fresh, unused path under the system temp directory for a throwaway database.
/// `prefix` and `label` name it; the process id and a counter keep parallel tests apart.
pub fn fresh_db_path(prefix: &str, label: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "{prefix}_{label}_{}_{n}.sqlite",
        std::process::id()
    ))
}

/// Best-effort removal of a database file and its `-wal` and `-shm` siblings.
pub fn cleanup_db_files(path: &str) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{path}-wal"));
    let _ = std::fs::remove_file(format!("{path}-shm"));
}

/// `EXPLAIN QUERY PLAN` detail lines for `sql`, joined with `\n`, for substring
/// assertions such as "uses index X". The SQL must have no bound parameters.
pub fn explain_plan(conn: &Connection, sql: &str) -> Result<String, String> {
    explain_plan_with_params(conn, sql, [])
}

/// Same as [`explain_plan`], for SQL that carries bound `?N` placeholders: the plan still
/// needs every placeholder bound, although no result rows are read.
pub fn explain_plan_with_params<P: Params>(
    conn: &Connection,
    sql: &str,
    params: P,
) -> Result<String, String> {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .map_err(|e| format!("prepare EXPLAIN QUERY PLAN failed: {e}; SQL: {sql}"))?;
    let rows = stmt
        .query_map(params, |r| r.get::<_, String>(3))
        .map_err(|e| format!("EXPLAIN QUERY PLAN failed: {e}"))?;
    let mut lines = Vec::new();
    for row in rows {
        lines.push(row.map_err(|e| format!("EXPLAIN row: {e}"))?);
    }
    Ok(lines.join("\n"))
}
