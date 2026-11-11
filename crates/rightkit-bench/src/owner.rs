//! Benchmarks against the owner's real app database (rightkit#1481).
//!
//! Adrian, 2026-11-03: "dev mode, is mine. im the fucking owner, my data. no dummy data, no
//! fucking made up shit. real data. snapshot and restore." So nothing here generates a corpus:
//!
//! - Reads ([`open_owner_read_only`], [`run_probes`], [`check_plans`], [`table_counts`]) open the
//!   owner's database file read-only (`SQLITE_OPEN_READ_ONLY` plus `PRAGMA query_only`), so they
//!   cannot change it and need no snapshot.
//! - Anything that writes (a sync-latency workload, a rekey round trip) needs a [`WriteGate`], and
//!   the only way to get one is `WriteGate::from_armed_snapshot` (feature `qa-snapshot`), which
//!   succeeds only while `rightkit-qa`'s snapshot of the app's data and credentials is armed and
//!   will be restored. There is no environment switch and no other constructor.

use crate::timing::{measure_many, BenchMetric};
use rightkit_db::rusqlite::{params_from_iter, types::Value, Connection};
use rightkit_db::DbOptions;

/// Open the owner's database exactly as given (path, key) read-only. Never creates the file and
/// never runs bootstrap, migrations or a journal fallback, and sets `query_only` on top.
pub fn open_owner_read_only(opts: &DbOptions) -> Result<Connection, String> {
    let conn = opts
        .open_read_only()
        .map_err(|e| format!("open owner database read-only: {e}"))?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .map_err(|e| format!("set query_only: {e}"))?;
    Ok(conn)
}

/// Proof that the owner's data is snapshotted and will be restored, required to write to it.
#[derive(Debug)]
pub struct WriteGate {
    snapshot_id: String,
}

impl WriteGate {
    /// The id of the armed snapshot this gate rests on (`worker` for a scenario worker that only
    /// holds the marker environment variable).
    pub fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    /// Succeeds only while a verified `rightkit-qa` snapshot of the app is armed in this process
    /// or named by `RIGHTKIT_QA_SNAPSHOT_MARKER` and still armed. Anything else is an error that
    /// names the missing snapshot; nothing was written.
    #[cfg(feature = "qa-snapshot")]
    pub fn from_armed_snapshot() -> Result<Self, String> {
        rightkit_qa::snapshot::require_armed().map_err(|e| e.to_string())?;
        Ok(Self {
            snapshot_id: rightkit_qa::snapshot::armed_snapshot_id()
                .unwrap_or_else(|| "worker".to_string()),
        })
    }
}

/// Open the owner's database read-write (never creating it). Requires the [`WriteGate`].
pub fn open_owner_writer(opts: &DbOptions, _gate: &WriteGate) -> Result<Connection, String> {
    opts.open_existing_writer()
        .map_err(|e| format!("open owner database read-write: {e}"))
}

/// A real query to time, with its bound values.
#[derive(Debug, Clone)]
pub struct Probe {
    /// Metric name.
    pub name: String,
    /// The SQL, read-only (a `SELECT`).
    pub sql: String,
    /// Bound values for `?N` placeholders, in order.
    pub params: Vec<Value>,
    /// Timed runs after one warmup.
    pub runs: usize,
}

impl Probe {
    /// A probe with no bound values and 20 timed runs.
    pub fn new(name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sql: sql.into(),
            params: Vec::new(),
            runs: 20,
        }
    }
}

/// Time every probe on `conn` (a connection from [`open_owner_read_only`]): each runs once as a
/// warmup and `runs` times timed, reading every result row. Stops at the first failure.
pub fn run_probes(conn: &Connection, probes: &[Probe]) -> Result<Vec<BenchMetric>, String> {
    probes
        .iter()
        .map(|p| {
            measure_many(&p.name, p.runs, || {
                let mut stmt = conn.prepare(&p.sql).map_err(|e| e.to_string())?;
                let mut rows = stmt
                    .query(params_from_iter(p.params.iter()))
                    .map_err(|e| e.to_string())?;
                let mut n = 0usize;
                while rows.next().map_err(|e| e.to_string())?.is_some() {
                    n += 1;
                }
                Ok(n)
            })
        })
        .collect()
}

/// A query plan the owner's schema must keep: substrings the `EXPLAIN QUERY PLAN` text must and
/// must not contain.
#[derive(Debug, Clone)]
pub struct PlanExpect {
    /// Check name.
    pub name: String,
    /// The SQL to explain.
    pub sql: String,
    /// Bound values for `?N` placeholders, in order.
    pub params: Vec<Value>,
    /// Every one of these must appear in the plan (an index name, `SEARCH`).
    pub must_contain: Vec<String>,
    /// None of these may appear (`SCAN`, a temp b-tree).
    pub must_not_contain: Vec<String>,
}

/// Outcome of one [`PlanExpect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanResult {
    /// Check name.
    pub name: String,
    /// The plan text SQLite returned.
    pub plan: String,
    /// What was missing or forbidden-but-present; empty when the plan holds.
    pub problems: Vec<String>,
}

impl PlanResult {
    /// Whether the plan met every expectation.
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Explain each query on the owner's real schema and compare to its expectations.
pub fn check_plans(conn: &Connection, checks: &[PlanExpect]) -> Result<Vec<PlanResult>, String> {
    checks
        .iter()
        .map(|c| {
            let plan = crate::sqlite::explain_plan_with_params(
                conn,
                &c.sql,
                params_from_iter(c.params.iter()),
            )?;
            let mut problems = Vec::new();
            for want in &c.must_contain {
                if !plan.contains(want.as_str()) {
                    problems.push(format!("plan lacks `{want}`"));
                }
            }
            for bad in &c.must_not_contain {
                if plan.contains(bad.as_str()) {
                    problems.push(format!("plan contains forbidden `{bad}`"));
                }
            }
            Ok(PlanResult {
                name: c.name.clone(),
                plan,
                problems,
            })
        })
        .collect()
}

/// Row count of every user table, so a result reports the size of the real corpus it ran on.
pub fn table_counts(conn: &Connection) -> Result<Vec<(String, u64)>, String> {
    let names: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    names
        .into_iter()
        .map(|n| {
            let escaped = n.replace('"', "\"\"");
            let count: i64 = conn
                .query_row(&format!("SELECT count(*) FROM \"{escaped}\""), [], |r| {
                    r.get(0)
                })
                .map_err(|e| format!("count {n}: {e}"))?;
            Ok((n, count.max(0) as u64))
        })
        .collect()
}

/// Rotate the SQLCipher key of the owner's database to `temp_key`, prove the data still reads
/// (same table counts, `quick_check` ok), rotate back to `original_key` and prove it again.
/// `opts` must open the database with `original_key`. Returns the milliseconds of each rotation.
/// The database ends on its original key; the snapshot restore covers an interruption.
#[cfg(any(feature = "sqlcipher", feature = "sqlcipher-system"))]
pub fn rekey_round_trip(
    opts: &DbOptions,
    original_key: &str,
    temp_key: &str,
    gate: &WriteGate,
) -> Result<(u128, u128), String> {
    use std::time::Instant;
    fn verify(conn: &Connection, before: &[(String, u64)]) -> Result<(), String> {
        let check: String = conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .map_err(|e| format!("quick_check: {e}"))?;
        if check != "ok" {
            return Err(format!("quick_check after rekey: {check}"));
        }
        if table_counts(conn)? != before {
            return Err("table counts changed across the rekey".to_string());
        }
        Ok(())
    }
    let conn = open_owner_writer(opts, gate)?;
    let before = table_counts(&conn)?;
    let t = Instant::now();
    rightkit_db::rekey(&conn, temp_key).map_err(|e| format!("rekey to temp key: {e}"))?;
    let forward = t.elapsed().as_millis();
    verify(&conn, &before)?;
    let t = Instant::now();
    rightkit_db::rekey(&conn, original_key).map_err(|e| format!("rekey back: {e}"))?;
    let back = t.elapsed().as_millis();
    verify(&conn, &before)?;
    Ok((forward, back))
}
