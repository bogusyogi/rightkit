//! Dispatch of an outgoing content brief to an editor engine, run as an external process. PostRight never
//! renders or edits media: `dispatch.run` runs `cutright-engine brief run` against a stored brief and hands the
//! delivery envelope it wrote to `delivery.accept`.
//!
//! Schema v20 of the channel store (one migration list, kept by the host). The only table is `dispatch_attempts`.
//! Briefs and envelopes stay in the contracts-io tables and are read and written only through that module's own
//! `dispatch` function (`brief_out.get`, `brief_out.list`) and its `accept_batch` function.
//!
//! - The engine executable comes only from `RIGHTKIT_EDITOR_ENGINE` / `RIGHTKIT_GENERATOR_ENGINE`: an absolute
//!   path to an existing file. No PATH search, no relative path. Anything else is `engine_missing`, nothing spawned.
//! - The child is an owned process (rightkit-child-process): no shell, a minimal explicit environment, a hard wall-clock
//!   timeout, a stdout cap, a stderr tail cap, and the whole tree killed on timeout, on output overflow and after
//!   a normal exit (stragglers of the engine do not outlive the call).
//! - An attempt row is written `running` before the spawn and finished after it. A crash in between leaves a
//!   `running` row with a dead owner; the next call marks it `interrupted` and refuses to run again until the
//!   caller passes `retry_interrupted`. No silent rerun.
//! - The engine's identity (pid, process group, start identity, executable path) is written on the attempt row
//!   right after the spawn. A `running` attempt whose owner is dead is swept before anything else touches it
//!   (every `dispatch.run`, and `dispatch.sweep`): the recorded engine's process group is terminated, bounded,
//!   only when a live process still matches the recorded identity; where identity cannot be read nothing is
//!   killed and the attempt says `sweep_unverified`. See `engine_proc`.
//! - A guard around the child terminates its tree, bounded, on every early return or unwind after the spawn.
//! - A request already `delivered` returns the stored envelopes and spawns nothing. A `partial` request runs again.
//! - The engine result is `done` (outcome `delivered`) or `partial` (outcome `partial`, a normal outcome). Every
//!   non-null envelope of its `deliverables` is accepted together (`contracts_io::accept_batch`, one transaction:
//!   if any is refused none is stored and the outcome is `delivery_invalid`). `skipped` is recorded verbatim.
//!   No spend field is ever passed to the engine.
//! - Schema v23 (in contracts_io.rs) rebuilds `dispatch_attempts` so its outcome check admits `partial`.
//! - GenRight's `brief run` takes the same command line and prints the same result as CutRight's.
//!
//! `now` is an input and the only source of stored timestamps. The wall clock is read for the timeout alone.
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::dispatch_protocol::{
    DispatchAttempt, DispatchListInput, DispatchListOutput, DispatchRunInput, DispatchRunOutput,
    DispatchSweepInput, DispatchSweepOutput, DispatchSwept, Scope,
};
use rightkit_child_process::{
    AdoptedProcess, OutputCapture, OwnedChild, OwnedCommand, ReapOutcome, StderrTail,
    TerminationOptions, DEFAULT_ENV_ALLOWLIST,
};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension, Row};
use rightkit_db::Migration;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::Fail;

use crate::dispatch_host::{
    host, BatchEnvelope, BriefOutRecord, DeliveryRecord, DEFAULT_DELIVERABLE,
};

/// Methods that write. A dry run reports these and runs nothing.
pub const WRITE_METHODS: &[&str] = &["dispatch.run", "dispatch.sweep"];

/// Every method this module routes.
pub const METHODS: &[&str] = &["dispatch.run", "dispatch.list", "dispatch.sweep"];

const SCHEMA_V20: &str = "
CREATE TABLE dispatch_attempts (
    attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    request_id TEXT NOT NULL,
    engine TEXT NOT NULL CHECK (engine IN ('cutright', 'genright')),
    owner_pid INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    exit_code INTEGER,
    outcome TEXT NOT NULL CHECK (outcome IN
        ('running', 'delivered', 'engine_failed', 'timed_out', 'delivery_invalid', 'engine_missing', 'interrupted')),
    detail_json TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY (venture, channel, account, request_id) REFERENCES outgoing_briefs (venture, channel, account, request_id)
);
CREATE INDEX dispatch_attempts_by_request ON dispatch_attempts (venture, channel, account, request_id, attempt_id);
";

/// Schema v20, appended to the channel store's migration list in channels.rs.
pub const MIGRATION_V20: Migration = Migration::sql(20, "dispatch-attempts", SCHEMA_V20);

/// Engine identity and leftover-sweep columns. `engine_*` are written right after the spawn; `sweep_*` by the
/// sweep; `abandoned_for_test` marks a `test_abandon_after_spawn` attempt (its `owner_pid` is rewritten to 0).
const SCHEMA_V22: &str = "
ALTER TABLE dispatch_attempts ADD COLUMN engine_pid INTEGER;
ALTER TABLE dispatch_attempts ADD COLUMN engine_pgid INTEGER;
ALTER TABLE dispatch_attempts ADD COLUMN engine_start TEXT;
ALTER TABLE dispatch_attempts ADD COLUMN engine_exe TEXT;
ALTER TABLE dispatch_attempts ADD COLUMN engine_identity_source TEXT;
ALTER TABLE dispatch_attempts ADD COLUMN sweep_action TEXT;
ALTER TABLE dispatch_attempts ADD COLUMN sweep_at TEXT;
ALTER TABLE dispatch_attempts ADD COLUMN sweep_detail TEXT;
ALTER TABLE dispatch_attempts ADD COLUMN abandoned_for_test INTEGER NOT NULL DEFAULT 0;
";

/// Schema v22, appended to the channel store's migration list in channels.rs after the migration of v21.
pub const MIGRATION_V22: Migration = Migration::sql(22, "dispatch-engine-identity", SCHEMA_V22);

/// The outcome `awaiting_approval` joins the attempt outcomes. SQLite cannot widen a CHECK in place, so the
/// table is rebuilt exactly as v23 left it (which had already added `partial`) and its rows copied.
const SCHEMA_V25: &str = "
CREATE TABLE dispatch_attempts_v25 (
    attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    request_id TEXT NOT NULL,
    engine TEXT NOT NULL CHECK (engine IN ('cutright', 'genright')),
    owner_pid INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    exit_code INTEGER,
    outcome TEXT NOT NULL CHECK (outcome IN
        ('running', 'delivered', 'partial', 'engine_failed', 'timed_out', 'delivery_invalid', 'engine_missing',
         'interrupted', 'awaiting_approval')),
    detail_json TEXT NOT NULL DEFAULT '{}',
    engine_pid INTEGER,
    engine_pgid INTEGER,
    engine_start TEXT,
    engine_exe TEXT,
    engine_identity_source TEXT,
    sweep_action TEXT,
    sweep_at TEXT,
    sweep_detail TEXT,
    abandoned_for_test INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (venture, channel, account, request_id) REFERENCES outgoing_briefs (venture, channel, account, request_id)
);
INSERT INTO dispatch_attempts_v25
    (attempt_id, venture, channel, account, request_id, engine, owner_pid, started_at, finished_at, exit_code,
     outcome, detail_json, engine_pid, engine_pgid, engine_start, engine_exe, engine_identity_source, sweep_action,
     sweep_at, sweep_detail, abandoned_for_test)
SELECT attempt_id, venture, channel, account, request_id, engine, owner_pid, started_at, finished_at, exit_code,
     outcome, detail_json, engine_pid, engine_pgid, engine_start, engine_exe, engine_identity_source, sweep_action,
     sweep_at, sweep_detail, abandoned_for_test
FROM dispatch_attempts ORDER BY attempt_id;
DROP TABLE dispatch_attempts;
ALTER TABLE dispatch_attempts_v25 RENAME TO dispatch_attempts;
CREATE INDEX dispatch_attempts_by_request ON dispatch_attempts (venture, channel, account, request_id, attempt_id);
";

/// Schema v25, appended to the channel store's migration list in channels.rs after the migration of v24.
pub const MIGRATION_V25: Migration = Migration::sql(25, "dispatch-awaiting-approval", SCHEMA_V25);

// The engine error code a producer returns when a person must approve the assets in its own app before a
// brief may be delivered (GenRight enforces it for `review.mode` `manual` and `auto_when_clean`). Defined by rightkit-contracts 0.1.7 (`EYES_APPROVAL_REQUIRED`).
use rightkit_contracts::EYES_APPROVAL_REQUIRED;

/// Environment variable that, set to exactly `1` under QA isolation, lets `test_abandon_after_spawn` work.
#[cfg(feature = "test-mode")]
const TEST_HOOKS_VAR: &str = "RIGHTKIT_DISPATCH_TEST_HOOKS";

const SWEEP_KILLED: &str = "swept_killed";
const SWEEP_ALREADY_GONE: &str = "swept_already_gone";
const SWEEP_UNVERIFIED: &str = "sweep_unverified";
const SWEEP_NO_ENGINE: &str = "sweep_no_engine";
const SWEEP_KILL_FAILED: &str = "sweep_kill_failed";

/// Environment variables naming the executables. Each holds an absolute path.
pub const EDITOR_ENGINE_VAR: &str = "RIGHTKIT_EDITOR_ENGINE";
pub const GENERATOR_ENGINE_VAR: &str = "RIGHTKIT_GENERATOR_ENGINE";

/// Variables with this prefix are passed to the child on top of the default allowlist. Only the test engine
/// reads them (`RIGHTKIT_FAKE_ENGINE_MODE` and friends); the real engines ignore them.
#[cfg(feature = "test-mode")]
const PASSTHROUGH_PREFIX: &str = "RIGHTKIT_FAKE_ENGINE_";

const DEFAULT_TIMEOUT_SECS: u64 = 600;
const MAX_TIMEOUT_SECS: u64 = 3600;
/// stdout is read up to this many bytes; one more byte kills the child as `engine_failed`.
const OUTPUT_CAP: usize = 1024 * 1024;
/// An envelope file larger than this is `delivery_invalid`.
const ENVELOPE_CAP: usize = 1024 * 1024;
/// The stderr tail kept and reported, in bytes.
const STDERR_TAIL_BYTES: usize = 4096;
const POLL: Duration = Duration::from_millis(25);
/// Tree-wide grace between TERM and KILL on Unix.
const KILL_GRACE: Duration = Duration::from_millis(500);
/// Grace plus reap bound for the tree termination.
const REAP_BOUND: Duration = Duration::from_secs(5);
/// How long the pipe readers get to finish after the tree is gone.
const DRAIN: Duration = Duration::from_secs(2);
/// A `running` row older than this (by the `now` inputs) is dead whatever its owner pid says.
const STALE_AFTER_SECS: i64 = MAX_TIMEOUT_SECS as i64 + 120;

#[cfg(feature = "test-mode")]
const ATTEMPT_COLUMNS: &str = "attempt_id, request_id, engine, started_at, finished_at, exit_code, outcome, detail_json,
    engine_pid, engine_pgid, engine_start, engine_exe, engine_identity_source, sweep_action, sweep_at, sweep_detail,
    abandoned_for_test";
#[cfg(not(feature = "test-mode"))]
const ATTEMPT_COLUMNS: &str = "attempt_id, request_id, engine, started_at, finished_at, exit_code, outcome, detail_json,
    engine_pid, engine_pgid, engine_start, engine_exe, engine_identity_source, sweep_action, sweep_at, sweep_detail";

const INSERT_ATTEMPT: &str = "
INSERT INTO dispatch_attempts (venture, channel, account, request_id, engine, owner_pid, started_at, finished_at,
    outcome, detail_json)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

const FINISH_ATTEMPT: &str = "
UPDATE dispatch_attempts SET finished_at = ?2, exit_code = ?3, outcome = ?4, detail_json = ?5 WHERE attempt_id = ?1";

#[cfg(feature = "test-mode")]
const RECORD_ENGINE: &str = "
UPDATE dispatch_attempts SET engine_pid = ?2, engine_pgid = ?3, engine_start = ?4, engine_exe = ?5,
    engine_identity_source = ?6, owner_pid = CASE WHEN ?7 = 1 THEN 0 ELSE owner_pid END, abandoned_for_test = ?7
WHERE attempt_id = ?1 AND outcome = 'running'";
#[cfg(not(feature = "test-mode"))]
const RECORD_ENGINE: &str = "
UPDATE dispatch_attempts SET engine_pid = ?2, engine_pgid = ?3, engine_start = ?4, engine_exe = ?5,
    engine_identity_source = ?6
WHERE attempt_id = ?1 AND outcome = 'running'";

const RECORD_SWEEP: &str = "
UPDATE dispatch_attempts SET sweep_action = ?2, sweep_at = ?3, sweep_detail = ?4
WHERE attempt_id = ?1 AND outcome = 'running' AND sweep_action IS NULL";

/// One stored attempt row, before its `detail_json` is parsed.
struct AttemptRow {
    attempt_id: i64,
    request_id: String,
    engine: String,
    started_at: String,
    finished_at: Option<String>,
    exit_code: Option<i64>,
    outcome: String,
    detail_json: String,
    engine_pid: Option<i64>,
    engine_pgid: Option<i64>,
    engine_start: Option<String>,
    engine_exe: Option<String>,
    engine_identity_source: Option<String>,
    sweep_action: Option<String>,
    sweep_at: Option<String>,
    sweep_detail: Option<String>,
    #[cfg(feature = "test-mode")]
    abandoned_for_test: bool,
}

fn bad(msg: impl Into<String>) -> Fail {
    ("bad_input", msg.into())
}

fn db_err(e: impl std::fmt::Display) -> Fail {
    ("db_error", e.to_string())
}

fn internal(e: impl std::fmt::Display) -> Fail {
    ("internal", e.to_string())
}

fn encoded(value: serde_json::Result<Value>) -> Result<Value, Fail> {
    value.map_err(internal)
}

fn parse<T: DeserializeOwned>(input: Value, method: &str) -> Result<T, Fail> {
    serde_json::from_value(input).map_err(|e| bad(format!("{method} input: {e}")))
}

fn open_store(root: &Path) -> Result<Connection, Fail> {
    host()?.open_store(root)
}

// ------------------------------------------------------------------------------------------------ engines

#[derive(Clone, Copy, PartialEq, Eq)]
enum Engine {
    Cutright,
    Genright,
}

impl Engine {
    fn parse(raw: &str) -> Result<Engine, Fail> {
        match raw {
            "cutright" => Ok(Engine::Cutright),
            "genright" => Ok(Engine::Genright),
            other => Err(bad(format!(
                "engine must be cutright or genright, got {other:?}"
            ))),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Engine::Cutright => "cutright",
            Engine::Genright => "genright",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            Engine::Cutright => EDITOR_ENGINE_VAR,
            Engine::Genright => GENERATOR_ENGINE_VAR,
        }
    }

    /// The engine's brief command line (CutRight: `brief run --project <dir> --input <file>`), or None when the
    /// engine has no such command.
    fn brief_run_args(self, project: &Path, brief: &Path) -> Option<Vec<OsString>> {
        // Both engines take the same line (genright-engine brief.rs states it mirrors CutRight's).
        match self {
            Engine::Cutright | Engine::Genright => Some(vec![
                "brief".into(),
                "run".into(),
                "--project".into(),
                project.into(),
                "--input".into(),
                brief.into(),
            ]),
        }
    }
}

/// The executable named by the engine's variable: absolute, an existing file, executable on Unix. The message
/// says which rule failed; it never contains anything but the variable name and the configured path.
fn resolve_engine(engine: Engine) -> Result<PathBuf, String> {
    resolve_engine_from(engine, std::env::var_os(engine.env_var()))
}

/// [`resolve_engine`] with the variable's value passed in (the tests drive it; same rules in every build).
fn resolve_engine_from(engine: Engine, value: Option<OsString>) -> Result<PathBuf, String> {
    let var = engine.env_var();
    let raw = value.filter(|v| !v.is_empty()).ok_or_else(|| {
        format!(
            "{var} is not set: the {} engine is located only by that variable",
            engine.name()
        )
    })?;
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(format!(
            "{var} must be an absolute path, got {}",
            path.display()
        ));
    }
    let meta = fs::metadata(&path).map_err(|e| format!("{var} names {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!(
            "{var} names {}, which is not a file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "{var} names {}, which is not executable",
                path.display()
            ));
        }
    }
    Ok(path)
}

fn timeout_from(raw: Option<u64>) -> Result<Duration, Fail> {
    let secs = raw.unwrap_or(DEFAULT_TIMEOUT_SECS);
    if secs == 0 || secs > MAX_TIMEOUT_SECS {
        return Err(bad(format!(
            "timeout_secs must be 1 to {MAX_TIMEOUT_SECS}, got {secs}"
        )));
    }
    Ok(Duration::from_secs(secs))
}

/// The CutRight project directory: an absolute path to an existing directory, passed on with its separators
/// normalised first (the rule of `channels::dir_of`: callers pass either separator, and a Windows verbatim path
/// accepts only `\`).
fn project_dir(raw: Option<&str>) -> Result<PathBuf, Fail> {
    let raw = raw.ok_or_else(|| bad("project_dir is required (the engine's --project)"))?;
    let path = PathBuf::from(raw.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR));
    if !path.is_absolute() {
        return Err(bad(format!(
            "project_dir must be an absolute path, got {raw:?}"
        )));
    }
    if !path.is_dir() {
        return Err(bad(format!(
            "project_dir {raw:?} is not an existing directory"
        )));
    }
    Ok(path)
}

// ------------------------------------------------------------------------------------------------ in flight

static IN_FLIGHT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// One `dispatch.run` per request at a time inside this process, held until the call returns.
struct Flight(String);

fn flight_key(scope: &Scope, request_id: &str) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{request_id}",
        scope.venture, scope.channel, scope.account
    )
}

/// True when another call of this process is dispatching the request right now (its `running` row is live).
fn in_flight(scope: &Scope, request_id: &str) -> bool {
    let key = flight_key(scope, request_id);
    IN_FLIGHT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&key)
}

impl Flight {
    fn enter(scope: &Scope, request_id: &str) -> Result<Flight, Fail> {
        let key = flight_key(scope, request_id);
        let mut held = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if held.contains(&key) {
            return Err((
                "in_progress",
                format!("request {request_id} is already being dispatched by this process"),
            ));
        }
        held.push(key.clone());
        Ok(Flight(key))
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        let mut held = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        held.retain(|k| *k != self.0);
    }
}

// ------------------------------------------------------------------------------------------------ store

fn attempt_row(r: &Row<'_>) -> rightkit_db::rusqlite::Result<AttemptRow> {
    Ok(AttemptRow {
        attempt_id: r.get(0)?,
        request_id: r.get(1)?,
        engine: r.get(2)?,
        started_at: r.get(3)?,
        finished_at: r.get(4)?,
        exit_code: r.get(5)?,
        outcome: r.get(6)?,
        detail_json: r.get(7)?,
        engine_pid: r.get(8)?,
        engine_pgid: r.get(9)?,
        engine_start: r.get(10)?,
        engine_exe: r.get(11)?,
        engine_identity_source: r.get(12)?,
        sweep_action: r.get(13)?,
        sweep_at: r.get(14)?,
        sweep_detail: r.get(15)?,
        #[cfg(feature = "test-mode")]
        abandoned_for_test: r.get::<_, i64>(16)? != 0,
    })
}

fn attempt_record(row: AttemptRow) -> Result<DispatchAttempt, Fail> {
    Ok(DispatchAttempt {
        attempt_id: row.attempt_id,
        request_id: row.request_id,
        engine: row.engine,
        started_at: row.started_at,
        finished_at: row.finished_at,
        exit_code: row.exit_code,
        outcome: row.outcome,
        detail: serde_json::from_str(&row.detail_json).map_err(internal)?,
        engine_pid: row.engine_pid,
        engine_pgid: row.engine_pgid,
        engine_start: row.engine_start,
        engine_exe: row.engine_exe,
        engine_identity_source: row.engine_identity_source,
        sweep_action: row.sweep_action,
        sweep_at: row.sweep_at,
        sweep_detail: row.sweep_detail,
        #[cfg(feature = "test-mode")]
        abandoned_for_test: row.abandoned_for_test,
    })
}

fn load_attempt(conn: &Connection, attempt_id: i64) -> Result<DispatchAttempt, Fail> {
    let sql = format!("SELECT {ATTEMPT_COLUMNS} FROM dispatch_attempts WHERE attempt_id = ?1");
    let row = conn
        .query_row(&sql, params![attempt_id], attempt_row)
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| ("not_found", format!("no dispatch attempt {attempt_id}")))?;
    attempt_record(row)
}

fn insert_attempt(
    conn: &Connection,
    scope: &Scope,
    request_id: &str,
    engine: Engine,
    stamp: &str,
    outcome: &str,
    detail: &Value,
) -> Result<i64, Fail> {
    let finished: Option<&str> = (outcome != "running").then_some(stamp);
    conn.execute(
        INSERT_ATTEMPT,
        params![
            scope.venture,
            scope.channel,
            scope.account,
            request_id,
            engine.name(),
            i64::from(std::process::id()),
            stamp,
            finished,
            outcome,
            detail.to_string()
        ],
    )
    .map_err(db_err)?;
    Ok(conn.last_insert_rowid())
}

fn finish_attempt(
    conn: &Connection,
    attempt_id: i64,
    stamp: &str,
    exit_code: Option<i32>,
    outcome: &str,
    detail: &Value,
) -> Result<(), Fail> {
    conn.execute(
        FINISH_ATTEMPT,
        params![
            attempt_id,
            stamp,
            exit_code.map(i64::from),
            outcome,
            detail.to_string()
        ],
    )
    .map_err(db_err)?;
    Ok(())
}

fn latest_outcome(
    conn: &Connection,
    scope: &Scope,
    request_id: &str,
) -> Result<Option<String>, Fail> {
    conn.query_row(
        "SELECT outcome FROM dispatch_attempts
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND request_id = ?4
         ORDER BY attempt_id DESC LIMIT 1",
        params![scope.venture, scope.channel, scope.account, request_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_err)
}

fn last_delivered_attempt(
    conn: &Connection,
    scope: &Scope,
    request_id: &str,
) -> Result<Option<DispatchAttempt>, Fail> {
    let sql = format!(
        "SELECT {ATTEMPT_COLUMNS} FROM dispatch_attempts
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND request_id = ?4 AND outcome = 'delivered'
         ORDER BY attempt_id DESC LIMIT 1"
    );
    conn.query_row(
        &sql,
        params![scope.venture, scope.channel, scope.account, request_id],
        attempt_row,
    )
    .optional()
    .map_err(db_err)?
    .map(attempt_record)
    .transpose()
}

/// A process is a live owner only when it is another process that still runs. This process's own `running` rows
/// are leftovers (a panic): the in-flight guard already excludes a concurrent run of the same request here.
fn owner_alive(pid: i64) -> bool {
    if pid == i64::from(std::process::id()) {
        return false;
    }
    u32::try_from(pid)
        .ok()
        .and_then(NonZeroU32::new)
        .is_some_and(|p| AdoptedProcess::new(p).is_running().unwrap_or(false))
}

/// Settles the `running` rows of one request whose owner is gone (or whose age passes any possible run).
/// A request that is not delivered: they become `interrupted` and a live owner is `in_progress`. A request that
/// is delivered: they become `delivered` (the crash fell after the envelope was accepted) and a live owner is
/// left alone. Returns the settled attempt ids.
fn settle_running(
    conn: &Connection,
    scope: &Scope,
    request_id: &str,
    delivered: bool,
    now_secs: i64,
    stamp: &str,
) -> Result<Vec<i64>, Fail> {
    let mut stmt = conn
        .prepare(
            "SELECT attempt_id, owner_pid, started_at FROM dispatch_attempts
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND request_id = ?4 AND outcome = 'running'
             ORDER BY attempt_id ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![scope.venture, scope.channel, scope.account, request_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(db_err)?;
    let mut dead = Vec::new();
    let mut live = Vec::new();
    for row in rows {
        let (id, pid, started_at) = row.map_err(db_err)?;
        let age = now_secs.saturating_sub(time::parse_instant(&started_at, "started_at")?);
        if age <= STALE_AFTER_SECS && owner_alive(pid) {
            live.push(id);
        } else {
            dead.push(id);
        }
    }
    if !delivered && !live.is_empty() {
        return Err((
            "in_progress",
            format!(
                "request {request_id} has attempt {} still running in another process",
                live[0]
            ),
        ));
    }
    let (outcome, detail) = settle_outcome(delivered);
    for id in &dead {
        finish_attempt(conn, *id, stamp, None, outcome, &detail)?;
    }
    Ok(dead)
}

/// The outcome and detail a dead `running` attempt is settled to.
fn settle_outcome(delivered: bool) -> (&'static str, Value) {
    if delivered {
        (
            "delivered",
            json!({ "recovered": true, "message": "the envelope was accepted before the attempt was finished" }),
        )
    } else {
        (
            "interrupted",
            json!({ "message": "the run ended without recording a result: the engine process died or the call crashed" }),
        )
    }
}

/// True when a `running` row is dead: its owner is gone, or its age passes any possible run.
fn attempt_is_dead(owner_pid: i64, started_at: &str, now_secs: i64) -> Result<bool, Fail> {
    let age = now_secs.saturating_sub(time::parse_instant(started_at, "started_at")?);
    Ok(age > STALE_AFTER_SECS || !owner_alive(owner_pid))
}

// ------------------------------------------------------------------------------------------------ leftover sweep

/// A `running` attempt with a dead owner, as the sweep reads it.
struct DeadAttempt {
    attempt_id: i64,
    request_id: String,
    record: EngineRecord,
    /// Set when an earlier sweep already acted on this row (the settle step after it did not happen).
    earlier: Option<(String, String)>,
}

/// What the sweep decided about one recorded engine.
struct SweepDone {
    action: &'static str,
    detail: String,
}

fn done(action: &'static str, detail: impl Into<String>) -> SweepDone {
    SweepDone {
        action,
        detail: detail.into(),
    }
}

/// Decides, and does, the sweep of one recorded engine. Kills only a live process that matches the recorded
/// pid, start identity and identity source, all at once (rightkit-child-process verifies before every signal).
fn sweep_engine(record: &EngineRecord) -> SweepDone {
    let Some(pid) = record.pid else {
        return done(
            SWEEP_NO_ENGINE,
            "no engine process was recorded: the run ended before the spawn was recorded",
        );
    };
    match engine_proc::standing(record, pid) {
        engine_proc::Standing::Gone => done(SWEEP_ALREADY_GONE, format!("pid {pid} is not running")),
        engine_proc::Standing::Replaced => done(
            SWEEP_ALREADY_GONE,
            format!("pid {pid} now belongs to a different process (start time differs); it was not touched"),
        ),
        engine_proc::Standing::Unverified(why) => {
            done(SWEEP_UNVERIFIED, format!("pid {pid} could not be verified, nothing was killed: {why}"))
        }
        engine_proc::Standing::Ours(identity) => match engine_proc::terminate(&identity, KILL_GRACE) {
            Ok(()) => done(SWEEP_KILLED, format!("the recorded engine {pid} and its descendants were terminated")),
            Err(why) => done(SWEEP_KILL_FAILED, why),
        },
    }
}

/// Reads the dead `running` attempts of one scope: owner gone, or older than any possible run.
///
/// A row owned by this very process counts as dead (a leftover of a panic) except while another call of this
/// process is dispatching its request: that engine is live and is never swept. `own_request` is the caller's
/// request, whose own flight is held by the caller itself.
fn read_dead_attempts(
    conn: &Connection,
    scope: &Scope,
    now_secs: i64,
    own_request: Option<&str>,
) -> Result<Vec<DeadAttempt>, Fail> {
    let mut stmt = conn
        .prepare(
            "SELECT attempt_id, request_id, owner_pid, started_at, engine_pid, engine_pgid, engine_start, engine_exe,
                    engine_identity_source, sweep_action, sweep_detail
             FROM dispatch_attempts
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND outcome = 'running'
             ORDER BY attempt_id ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(params![scope.venture, scope.channel, scope.account], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                EngineRecord {
                    pid: r
                        .get::<_, Option<i64>>(4)?
                        .and_then(|p| u32::try_from(p).ok()),
                    pgid: r
                        .get::<_, Option<i64>>(5)?
                        .and_then(|p| u32::try_from(p).ok()),
                    start: r.get(6)?,
                    exe: r.get(7)?,
                    source: r.get(8)?,
                },
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })
        .map_err(db_err)?;
    let mut dead = Vec::new();
    for row in rows {
        let (attempt_id, request_id, owner_pid, started_at, record, action, detail) =
            row.map_err(db_err)?;
        let ours_and_live = owner_pid == i64::from(std::process::id())
            && own_request != Some(request_id.as_str())
            && in_flight(scope, &request_id);
        if !ours_and_live && attempt_is_dead(owner_pid, &started_at, now_secs)? {
            let earlier = action.map(|a| (a, detail.unwrap_or_default()));
            dead.push(DeadAttempt {
                attempt_id,
                request_id,
                record,
                earlier,
            });
        }
    }
    Ok(dead)
}

/// Terminates the recorded engines of the dead `running` attempts of one scope and records on each row what was
/// done. No transaction is held while a process is killed. When `settle` is set the attempts are then finished
/// (`interrupted`, or `delivered` when their request is delivered); otherwise they stay `running` for the caller's
/// own settle step. Returns what was done to each attempt, oldest first.
fn sweep_dead(
    root: &Path,
    scope: &Scope,
    now_secs: i64,
    stamp: &str,
    settle: bool,
    own_request: Option<&str>,
) -> Result<Vec<DispatchSwept>, Fail> {
    let dead = {
        let conn = open_store(root)?;
        read_dead_attempts(&conn, scope, now_secs, own_request)?
    };
    let mut acted = Vec::new();
    for attempt in dead {
        let (action, detail) = match attempt.earlier {
            Some((action, detail)) => (action, detail),
            None => {
                let outcome = sweep_engine(&attempt.record);
                (outcome.action.to_string(), outcome.detail)
            }
        };
        acted.push((attempt.attempt_id, attempt.request_id, action, detail));
    }
    if acted.is_empty() {
        return Ok(Vec::new());
    }
    let mut delivered_requests = Vec::new();
    if settle {
        for (_, request_id, _, _) in &acted {
            if !delivered_requests
                .iter()
                .any(|(r, _): &(String, bool)| r == request_id)
            {
                let delivered = load_brief(root, scope, request_id)?.state == "delivered";
                delivered_requests.push((request_id.clone(), delivered));
            }
        }
    }
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let mut swept = Vec::new();
    for (attempt_id, request_id, action, detail) in acted {
        tx.execute(RECORD_SWEEP, params![attempt_id, action, stamp, detail])
            .map_err(db_err)?;
        let mut outcome = "running";
        if settle {
            let delivered = delivered_requests
                .iter()
                .any(|(r, d)| *d && *r == request_id);
            let (settled, message) = settle_outcome(delivered);
            let changed = tx
                .execute(
                    "UPDATE dispatch_attempts SET finished_at = ?2, outcome = ?3, detail_json = ?4
                     WHERE attempt_id = ?1 AND outcome = 'running'",
                    params![attempt_id, stamp, settled, message.to_string()],
                )
                .map_err(db_err)?;
            if changed == 1 {
                outcome = settled;
            }
        }
        swept.push(DispatchSwept {
            attempt_id,
            request_id,
            action,
            detail,
            outcome: outcome.to_string(),
        });
    }
    tx.commit().map_err(db_err)?;
    Ok(swept)
}

// ------------------------------------------------------------------------------------------------ contracts-io

fn scope_value(scope: &Scope) -> Result<Value, Fail> {
    serde_json::to_value(scope).map_err(internal)
}

/// The outgoing brief, through contracts-io (which also refuses a foreign scope).
fn load_brief(root: &Path, scope: &Scope, request_id: &str) -> Result<BriefOutRecord, Fail> {
    let input = json!({ "scope": scope_value(scope)?, "request_id": request_id });
    let value = host()?
        .contracts(root, "brief_out.get", input)
        .map_err(|(code, message)| {
            if code == "not_found" {
                (
                    "unknown_request",
                    format!("no outgoing brief for request {request_id} in this scope"),
                )
            } else {
                (code, message)
            }
        })?;
    serde_json::from_value(value).map_err(internal)
}

fn blank(scope: &Scope, request_id: &str, engine: Engine, outcome: &str) -> DispatchRunOutput {
    DispatchRunOutput {
        scope: scope.clone(),
        request_id: request_id.to_string(),
        engine: engine.name().to_string(),
        outcome: outcome.to_string(),
        spawned: false,
        brief_state: "issued".to_string(),
        attempt: None,
        delivery: None,
        deliveries: Vec::new(),
        skipped: Vec::new(),
        message: None,
        stderr_tail: None,
        engine_error: None,
        interrupted: Vec::new(),
    }
}

// ------------------------------------------------------------------------------------------------ running a child

/// Reading the identity of an engine process and terminating it for the leftover sweep, through
/// rightkit-child-process `AdoptedIdentity` (0.4.x): `capture` reads the pid and its start time right after the
/// spawn, `verify` and `signal` refuse a pid that now names another process, and `sweep` and `sweep_descendants`
/// stop a recorded process and its descendants, each signal preceded by that identity check.
///
/// The identity is recorded as `start` = `<major>:<minor>` (the library's `StartToken`, comparable only on the host
/// that produced it) and `exe` (recorded for a person to read; the library does not compare it). `source` names this
/// encoding; a row recorded by anything else is never killed (`sweep_unverified`). Signals go to the recorded
/// process and to its descendants found by identity, never to a bare unchecked pid and never to a process group.
mod engine_proc {
    use std::num::NonZeroU32;
    use std::path::PathBuf;
    use std::time::Duration;

    use rightkit_child_process::{
        sweep, sweep_descendants, AdoptedIdentity, IdentityCheck, OwnedChild, StartToken,
        SweepReport,
    };

    /// The identity encoding recorded on the attempt row.
    pub const SOURCE: &str = "rightkit-adopted-v1";

    /// What is recorded on the attempt row right after the spawn.
    pub struct EngineRecord {
        pub pid: Option<u32>,
        pub pgid: Option<u32>,
        pub start: Option<String>,
        pub exe: Option<String>,
        pub source: Option<String>,
    }

    /// What the recorded pid names now.
    pub enum Standing {
        /// The same process as recorded.
        Ours(AdoptedIdentity),
        /// No process has the pid.
        Gone,
        /// The pid names a different process.
        Replaced,
        /// No comparable identity was recorded, or it could not be read; the string says why.
        Unverified(String),
    }

    /// The identity of a child just spawned (its exec has already happened when `spawn` returns).
    pub fn capture(child: &OwnedChild) -> EngineRecord {
        let pid = child.id();
        let identity =
            NonZeroU32::new(pid).and_then(|p| AdoptedIdentity::capture(p).ok().flatten());
        match identity {
            Some(identity) => {
                let token = identity.start_token();
                EngineRecord {
                    pid: Some(pid),
                    pgid: None,
                    start: Some(format!("{}:{}", token.major, token.minor)),
                    exe: identity.exe().map(|p| p.display().to_string()),
                    source: Some(SOURCE.to_string()),
                }
            }
            None => EngineRecord {
                pid: Some(pid),
                pgid: None,
                start: None,
                exe: None,
                source: None,
            },
        }
    }

    fn parse_token(start: &str) -> Option<StartToken> {
        let (major, minor) = start.split_once(':')?;
        Some(StartToken {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }

    /// Compares the recorded identity with what the pid names now. Only `Ours` may be signalled.
    pub fn standing(record: &EngineRecord, pid: u32) -> Standing {
        let Some(non_zero) = NonZeroU32::new(pid) else {
            return Standing::Gone;
        };
        let recorded = match (
            record.source.as_deref(),
            record.start.as_deref().and_then(parse_token),
        ) {
            (Some(SOURCE), Some(token)) => Some(AdoptedIdentity::from_recorded(
                non_zero,
                token,
                record.exe.as_ref().map(PathBuf::from),
            )),
            _ => None,
        };
        let Some(recorded) = recorded else {
            // Nothing comparable was recorded: a missing pid is still a finished one, a live pid is never touched.
            return match AdoptedIdentity::capture(non_zero) {
                Ok(None) => Standing::Gone,
                Ok(Some(_)) => Standing::Unverified(
                    "no identity this platform can compare was recorded for the running process"
                        .to_string(),
                ),
                Err(e) => Standing::Unverified(e.to_string()),
            };
        };
        match recorded.verify() {
            Ok(IdentityCheck::Same) => Standing::Ours(recorded),
            Ok(IdentityCheck::Gone) => Standing::Gone,
            Ok(IdentityCheck::Replaced) => Standing::Replaced,
            Err(e) => Standing::Unverified(e.to_string()),
        }
    }

    /// Stops the descendants of `identity`, then the process itself, each signal identity-checked by the library.
    /// `Ok` when everything signalled is gone (or was a different process by then); `Err` says what survived.
    pub fn terminate(identity: &AdoptedIdentity, grace: Duration) -> Result<(), String> {
        let below = sweep_descendants(identity, grace)
            .map_err(|e| format!("sweeping the engine's descendants: {e}"))?;
        check(&below)?;
        let own = sweep(std::slice::from_ref(identity), grace)
            .map_err(|e| format!("stopping the engine: {e}"))?;
        check(&own)
    }

    fn check(report: &SweepReport) -> Result<(), String> {
        if report.survivors.is_empty() && report.failed.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "processes still running after the forced stop: {:?}; signal failed for: {:?}",
                report.survivors, report.failed
            ))
        }
    }
}

use engine_proc::EngineRecord;

/// Owns the spawned child and terminates its whole tree (bounded) if the function holding it unwinds or returns
/// before a termination was confirmed. A confirmed reap sets `ended`, so the normal paths never kill twice.
struct ChildGuard {
    child: OwnedChild,
    ended: bool,
}

impl ChildGuard {
    fn terminate(&mut self, options: TerminationOptions) -> std::io::Result<ReapOutcome> {
        let result = self
            .child
            .terminate_tree_bounded_with_options(REAP_BOUND, options);
        if matches!(result, Ok(ReapOutcome::Exited(_))) {
            self.ended = true;
        }
        result
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.ended {
            let _ = self.child.terminate_tree_bounded_with_options(
                REAP_BOUND,
                TerminationOptions::default().with_grace(KILL_GRACE),
            );
        }
    }
}

enum Ended {
    /// `test_abandon_after_spawn`: the child was left running on purpose (never reaches `judge`).
    #[cfg(feature = "test-mode")]
    Abandoned,
    Exited,
    TimedOut,
    OutputCap,
    WaitFailed(String),
    SpawnFailed(String),
    /// Nothing was started: the brief file could not be written.
    NotStarted(String),
}

struct Execution {
    ended: Ended,
    exit_code: Option<i32>,
    success: bool,
    /// False when the tree could not be confirmed reaped within the bound (the owner's drop still kills it).
    reaped: bool,
    stdout: Vec<u8>,
    stderr_tail: String,
    elapsed_ms: u64,
}

/// What a producer engine child gets under QA isolation: a home of its own, and a validated test root of its own
/// (bound to the child's app id) so a child that has adopted the owner's run mode also isolates its suite, not only
/// its HOME. Both live inside this process's validated suite root.
#[cfg_attr(not(feature = "test-mode"), allow(dead_code))]
struct ChildIsolation {
    /// `<suite root>/engine-homes/<engine>`: HOME, APPDATA and the data folders of the child.
    home: PathBuf,
    /// `<suite root>/engine-homes/<engine>-qa-root`: the child's test root (private, with the runner binding).
    qa_root: PathBuf,
}

/// Names the owner's test mode reads (rightkit-daemon `mode.rs` `SELECTOR_ACTIVATION` and `SELECTOR_ROOT`, the latter
/// also `rightkit_fs::app_dirs::QA_DATA_DIR_VAR`). The owner keeps the first one private, so both are repeated here,
/// compiled only into a test-mode build, and pinned by `child_env_names_the_owner_selectors`.
#[cfg(feature = "test-mode")]
const CHILD_ACTIVATION_VAR: &str = "RIGHTKIT_UNDER_TEST";
#[cfg(feature = "test-mode")]
const CHILD_ROOT_VAR: &str = "RIGHTKIT_QA_DATA_DIR";

/// `Some(isolation)` when this process runs under QA isolation (the owner's validated test mode); `None` outside it,
/// where the engine uses the user's own data, which is the product behaviour. A run mode that cannot be decided is an
/// error (no spawn), never "not isolated".
#[cfg(not(feature = "test-mode"))]
fn isolated_engine_home(_engine_name: &str) -> Result<Option<ChildIsolation>, String> {
    Ok(None)
}

#[cfg(feature = "test-mode")]
fn isolated_engine_home(engine_name: &str) -> Result<Option<ChildIsolation>, String> {
    isolate_child(
        host().map_err(|e| e.1).and_then(|h| h.qa_isolation()),
        || host().map_err(|e| e.1)?.qa_suite_root(),
        engine_name,
    )
}

/// The decision with its inputs injected (the tests drive it): the isolation answer and the validated suite root.
#[cfg(feature = "test-mode")]
fn isolate_child(
    isolation: Result<bool, String>,
    suite_root: impl FnOnce() -> Result<PathBuf, String>,
    engine_name: &str,
) -> Result<Option<ChildIsolation>, String> {
    let isolated = isolation.map_err(|e| {
        format!("the run mode could not be decided, so no producer engine is started: {e}")
    })?;
    if !isolated {
        return Ok(None);
    }
    // The validated test suite root from the owner's mode (never read from the environment).
    let suite_root = suite_root()?;
    let homes = suite_root.join("engine-homes");
    let home = homes.join(engine_name);
    fs::create_dir_all(&home).map_err(|e| {
        format!(
            "could not create the isolated engine home {}: {e}",
            home.display()
        )
    })?;
    let qa_root = homes.join(format!("{engine_name}-qa-root"));
    rightkit_fs::private::ensure_private_dir(&qa_root).map_err(|e| {
        format!(
            "could not create the engine test root {}: {e}",
            qa_root.display()
        )
    })?;
    // The runner binding the child's mode check reads: the root belongs to that engine's app id.
    let binding = serde_json::json!({ "app": engine_name }).to_string();
    rightkit_fs::atomic_write_private(&qa_root.join(".rightkit-app.json"), binding.as_bytes())
        .map_err(|e| {
            format!(
                "could not bind the engine test root {}: {e}",
                qa_root.display()
            )
        })?;
    Ok(Some(ChildIsolation { home, qa_root }))
}

/// Every variable the isolated child gets on top of the default allowlist: the owner's activation pair (a child that
/// has adopted the run mode resolves its suite and data under its own validated root; a release-mode child reads
/// nothing, and HOME alone cannot move a suite that is resolved from the account database), then HOME and the
/// per-platform data folders inside its home.
#[cfg(feature = "test-mode")]
fn child_env(isolation: &ChildIsolation) -> Vec<(&'static str, PathBuf)> {
    let home = &isolation.home;
    vec![
        (CHILD_ACTIVATION_VAR, PathBuf::from("1")),
        (CHILD_ROOT_VAR, isolation.qa_root.clone()),
        ("HOME", home.clone()),
        ("USERPROFILE", home.clone()),
        ("APPDATA", home.join("AppData").join("Roaming")),
        ("LOCALAPPDATA", home.join("AppData").join("Local")),
        ("XDG_DATA_HOME", home.join("xdg-data")),
        ("XDG_CONFIG_HOME", home.join("xdg-config")),
        ("XDG_CACHE_HOME", home.join("xdg-cache")),
        // GenRight's documented data-root override (genright-protocol `app_data_root`).
        ("GENRIGHT_APP_DATA", home.join("genright-app-data")),
    ]
}

fn spawn_failure(message: String, started: Instant) -> Execution {
    Execution {
        ended: Ended::SpawnFailed(message),
        exit_code: None,
        success: false,
        reaped: true,
        stdout: Vec::new(),
        stderr_tail: String::new(),
        elapsed_ms: elapsed_ms(started),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Runs `exe args` as an owned child with a bounded wall clock. Never returns while the child's tree lives,
/// except when the tree cannot be reaped within `REAP_BOUND` (`reaped` false) or `abandon` is set.
///
/// Right after the spawn `record` receives the child's identity and must persist it; if it fails the child is
/// terminated and the run is `engine_failed`. A `ChildGuard` terminates the tree on every other early return or
/// unwind after the spawn. `abandon` (the test hook) records the identity, forgets the guard and the child so
/// nothing terminates it, drops the parent-exit tie, and returns `Ended::Abandoned` with the engine running.
fn execute(
    exe: &Path,
    args: &[OsString],
    cwd: &Path,
    timeout: Duration,
    engine_name: &str,
    record: &mut dyn FnMut(&EngineRecord) -> Result<(), String>,
    abandon: bool,
) -> Execution {
    let started = Instant::now();
    #[cfg(not(any(target_os = "linux", feature = "test-mode")))]
    let _ = abandon;
    // Under QA isolation the engine gets a home of its own inside the isolated suite root, so a test can
    // never read or write the user's real app data. No isolated home, no spawn.
    let isolated_home = match isolated_engine_home(engine_name) {
        Ok(home) => home,
        Err(message) => return spawn_failure(message, started),
    };
    let mut command = OwnedCommand::new(exe);
    command.env_allowlist(DEFAULT_ENV_ALLOWLIST.iter().copied());
    // The parent-exit tie exists on Linux only (rightkit-child-process: elsewhere `spawn` reports it as
    // unsupported). An abandoned engine must outlive this process, so the tie is off for it alone.
    // On macOS and Windows an engine cannot outlive a normal return (the drop guard kills its tree);
    // one left by a killed caller is found by the leftover sweep on the next call.
    #[cfg(target_os = "linux")]
    command.kill_on_parent_exit(!abandon);
    {
        let std_command = command.command_mut();
        #[cfg(feature = "test-mode")]
        for (name, value) in std::env::vars_os() {
            if name
                .to_str()
                .is_some_and(|n| n.starts_with(PASSTHROUGH_PREFIX))
            {
                std_command.env(name, value);
            }
        }
        #[cfg(feature = "test-mode")]
        if let Some(isolation) = &isolated_home {
            for (name, value) in child_env(isolation) {
                std_command.env(name, value);
            }
        }
        std_command.args(args);
        // GenRight takes its data folder as an explicit option too (`--data-dir`, the same as
        // GENRIGHT_APP_DATA), so isolation does not rest on environment variables alone.
        if let (Some(isolation), "genright") = (&isolated_home, engine_name) {
            std_command
                .arg("--data-dir")
                .arg(isolation.home.join("genright-app-data"));
        }
        std_command
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }
    let mut guard = match command.spawn() {
        Ok(child) => ChildGuard {
            child,
            ended: false,
        },
        Err(e) => return spawn_failure(format!("could not start {}: {e}", exe.display()), started),
    };

    // Identity first, before anything can wait on or lose the child.
    let recorded = record(&engine_proc::capture(&guard.child));
    #[cfg(not(feature = "test-mode"))]
    let _ = recorded;
    #[cfg(feature = "test-mode")]
    if abandon && recorded.is_ok() {
        // Deliberately leaks the guard and the child: no drop runs, so the tree is not terminated. The caller
        // (a test) owns the clean-up, through the sweep.
        std::mem::forget(guard);
        return Execution {
            ended: Ended::Abandoned,
            ..spawn_failure(String::new(), started)
        };
    }

    let mut stdout_capture = guard
        .child
        .take_stdout()
        .map(|pipe| OutputCapture::capture(pipe, OUTPUT_CAP));
    let mut tail = guard
        .child
        .take_stderr()
        .map(|pipe| StderrTail::plain(pipe, STDERR_TAIL_BYTES));

    let deadline = started + timeout;
    let ended = match (recorded, guard.child.process_handle()) {
        (Err(message), _) => Ended::WaitFailed(format!(
            "the engine's identity could not be recorded, so it was stopped: {message}"
        )),
        (Ok(()), Ok(handle)) => loop {
            if stdout_capture
                .as_ref()
                .is_some_and(|c| c.snapshot().truncated)
            {
                break Ended::OutputCap;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Ended::TimedOut;
            }
            match handle.wait_exited_timeout(left.min(POLL)) {
                Ok(true) => break Ended::Exited,
                Ok(false) => {}
                Err(e) => break Ended::WaitFailed(format!("waiting for the child failed: {e}")),
            }
        },
        (Ok(()), Err(e)) => Ended::WaitFailed(format!("no process handle for the child: {e}")),
    };

    // Every path ends the whole tree: a hung engine, an over-chatty one, and the stragglers of one that exited.
    // The guard records a confirmed reap; one that is not confirmed is tried once more when the guard drops.
    let options = TerminationOptions::default().with_grace(KILL_GRACE);
    let (exit_code, success, reaped) = match guard.terminate(options) {
        Ok(ReapOutcome::Exited(status)) => (status.code(), status.success(), true),
        Ok(ReapOutcome::TimedOut) => (None, false, false),
        // A failed signal must not turn an exited engine into a failure: read its status directly.
        Err(_) => match guard.child.try_wait() {
            Ok(Some(status)) => {
                // The leader is reaped: group signalling is over for this child, a second kill has no target.
                guard.ended = true;
                (status.code(), status.success(), false)
            }
            _ => (None, false, false),
        },
    };

    let stderr_tail = tail
        .as_mut()
        .map(|t| t.finish_within(DRAIN))
        .unwrap_or_default();
    // The bounded drain returns whatever it holds when the budget runs out, so a result is final only when the stream
    // ended cleanly. Order matters (the drain can finish between two reads): wait for the budget, prove
    // `is_finished()` first, and only then read `take_error()` and take the final snapshot with `finish()` (the join).
    // When it is not finished the bytes are a partial snapshot, kept only to report the size, never as the result.
    // (rightkit-child-process 0.5.4 `OutputCapture`.)
    let (captured, capture_problem) = match stdout_capture.as_mut() {
        Some(c) => {
            let _ = c.finish_within(DRAIN);
            if c.is_finished() {
                let error = c.take_error();
                let out = c.finish();
                let problem = error.map(|e| format!("reading the engine's stdout failed: {e}"));
                (out, problem)
            } else {
                let partial = c.finish_within(Duration::ZERO);
                (partial, Some("the engine's stdout did not close within the drain budget (a process still holds the pipe)".to_string()))
            }
        }
        None => (Default::default(), None),
    };
    // Output that overflowed between the last poll and the exit is still an over-chatty engine.
    let ended = match ended {
        Ended::Exited if captured.truncated => Ended::OutputCap,
        Ended::Exited => match capture_problem {
            Some(problem) => Ended::WaitFailed(problem),
            None => Ended::Exited,
        },
        other => other,
    };
    let stdout = captured.bytes;
    Execution {
        ended,
        exit_code,
        success,
        reaped,
        stdout,
        stderr_tail,
        elapsed_ms: elapsed_ms(started),
    }
}

// ------------------------------------------------------------------------------------------------ the result

/// The last line of stdout that is a JSON object: CutRight prints exactly one.
fn parse_result(stdout: &[u8]) -> Option<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find_map(|l| {
            serde_json::from_str::<Value>(l)
                .ok()
                .filter(Value::is_object)
        })
}

/// `rel` must stay inside `project`: relative, plain components only, and still inside after symlinks resolve.
fn resolve_inside(project: &Path, rel: &str) -> Result<PathBuf, String> {
    rightkit_fs::contain(project, rel).map_err(|e| match e {
        rightkit_fs::ContainError::InvalidRelative => {
            format!("the envelope path {rel:?} is not a plain relative path")
        }
        rightkit_fs::ContainError::Escapes => {
            format!("the envelope path {rel:?} leaves the project")
        }
        other => format!("the envelope file {rel:?} cannot be read: {other}"),
    })
}

/// The deliverable key of one result row: CutRight names it `preset`, GenRight `deliverable`. A row with neither has
/// the default (empty) key.
fn row_key(row: &Value) -> String {
    ["preset", "deliverable"]
        .iter()
        .find_map(|name| row.get(*name).and_then(Value::as_str))
        .unwrap_or(DEFAULT_DELIVERABLE)
        .to_string()
}

/// One envelope file, read (bounded) and parsed.
fn read_envelope_file(project: &Path, rel: &str) -> Result<Value, String> {
    let path = resolve_inside(project, rel)?;
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .and_then(|f| f.take(ENVELOPE_CAP as u64 + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("the envelope file {rel:?} cannot be read: {e}"))?;
    if bytes.len() > ENVELOPE_CAP {
        return Err(format!(
            "the envelope file {rel:?} is over {ENVELOPE_CAP} bytes"
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("the envelope file {rel:?} is not JSON: {e}"))
}

/// The `deliverables` rows of a result.
fn result_rows(result: &Value) -> Result<&Vec<Value>, String> {
    result
        .get("deliverables")
        .and_then(Value::as_array)
        .ok_or_else(|| "the engine result has no deliverables list".to_string())
}

/// Every non-null envelope the result names (one per deliverable row), read and parsed, in row order. A row whose
/// `envelope` is null or absent produced none.
fn read_envelopes(rows: &[Value], project: &Path) -> Result<Vec<BatchEnvelope>, String> {
    let mut found = Vec::new();
    for row in rows {
        let Some(rel) = row.get("envelope").and_then(Value::as_str) else {
            continue;
        };
        found.push(BatchEnvelope {
            deliverable: row_key(row),
            document: read_envelope_file(project, rel)?,
        });
    }
    Ok(found)
}

struct Verdict {
    outcome: &'static str,
    message: Option<String>,
    engine_error: Option<Value>,
    /// The engine's own `status` (`done` or `partial`), when it printed a usable result.
    engine_status: Option<String>,
    /// The envelopes this call accepted, in the engine's row order.
    deliveries: Vec<DeliveryRecord>,
    /// The engine's `skipped` list, verbatim.
    skipped: Vec<Value>,
}

impl Verdict {
    fn new(outcome: &'static str, message: impl Into<String>) -> Verdict {
        Verdict {
            outcome,
            message: Some(message.into()),
            engine_error: None,
            engine_status: None,
            deliveries: Vec::new(),
            skipped: Vec::new(),
        }
    }
}

/// Turns a finished run into an outcome. Every non-null envelope of the result goes to
/// `contracts_io::accept_batch` (one transaction, all or none); a refusal there that is about the envelopes is
/// `delivery_invalid`, anything else is returned as the command's own error. Engine status `done` is outcome
/// `delivered`, `partial` is outcome `partial`.
fn judge(
    root: &Path,
    req: &DispatchRunInput,
    project: &Path,
    run: &Execution,
) -> Result<Verdict, Fail> {
    match &run.ended {
        #[cfg(feature = "test-mode")]
        Ended::Abandoned => {
            return Ok(Verdict::new(
                "abandoned_for_test",
                "the engine was left running on purpose after the spawn",
            ))
        }
        Ended::SpawnFailed(message) => return Ok(Verdict::new("engine_missing", message.clone())),
        Ended::TimedOut => {
            return Ok(Verdict::new(
                "timed_out",
                format!("the engine did not finish within {} ms", run.elapsed_ms),
            ))
        }
        Ended::OutputCap => {
            return Ok(Verdict::new(
                "engine_failed",
                format!("the engine wrote more than {OUTPUT_CAP} bytes to stdout"),
            ))
        }
        Ended::WaitFailed(message) | Ended::NotStarted(message) => {
            return Ok(Verdict::new("engine_failed", message.clone()))
        }
        Ended::Exited => {}
    }
    let result = parse_result(&run.stdout);
    let engine_error = result
        .as_ref()
        .and_then(|r| r.get("error"))
        .filter(|e| !e.is_null())
        .cloned();
    let waits_for_person = engine_error
        .as_ref()
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        .is_some_and(|code| code == EYES_APPROVAL_REQUIRED);
    if waits_for_person {
        // A waiting state, not a failure: nothing was delivered, the brief stays issued, and the same brief is
        // run again by a person after the approval. Never retried here.
        let message = format!(
            "waiting for approval in the {} app: a person must approve the assets there, then run this brief again",
            req.engine
        );
        return Ok(Verdict {
            engine_error,
            ..Verdict::new("awaiting_approval", message)
        });
    }
    if !run.success || engine_error.is_some() {
        let message = match run.exit_code {
            Some(code) => format!("the engine exited with code {code}"),
            None => "the engine ended without an exit code".to_string(),
        };
        return Ok(Verdict {
            engine_error,
            ..Verdict::new("engine_failed", message)
        });
    }
    let Some(result) = result else {
        return Ok(Verdict::new(
            "delivery_invalid",
            "the engine printed no JSON result",
        ));
    };
    let engine_status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let skipped = result
        .get("skipped")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let seen = |mut verdict: Verdict| {
        verdict.engine_status = Some(engine_status.clone());
        verdict.skipped = skipped.clone();
        verdict
    };
    if engine_status != "done" && engine_status != "partial" {
        return Ok(seen(Verdict::new(
            "delivery_invalid",
            format!("the engine result status is {engine_status:?}; expected done or partial"),
        )));
    }
    let is_done = engine_status == "done";
    let rows = match result_rows(&result) {
        Ok(rows) => rows,
        Err(message) => return Ok(seen(Verdict::new("delivery_invalid", message))),
    };
    let envelopes = match read_envelopes(rows, project) {
        Ok(envelopes) => envelopes,
        Err(message) => return Ok(seen(Verdict::new("delivery_invalid", message))),
    };
    if envelopes.is_empty() {
        let statuses: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.get("status").and_then(Value::as_str))
            .collect();
        let statuses = statuses.join(", ");
        return Ok(seen(if is_done {
            Verdict::new(
                "delivery_invalid",
                format!("the engine reported done but delivered no envelope (deliverable statuses: {statuses})"),
            )
        } else {
            Verdict::new(
                "partial",
                format!("the engine finished partial and delivered no envelope (deliverable statuses: {statuses}); nothing was stored"),
            )
        }));
    }
    match host()?.accept_batch(
        root,
        &req.scope,
        &req.request_id,
        &envelopes,
        is_done,
        &req.now,
    ) {
        Ok(deliveries) => {
            let count = deliveries.len();
            let (outcome, message) = if is_done {
                (
                    "delivered",
                    format!(
                        "{count} envelope(s) accepted; the engine produced everything requested"
                    ),
                )
            } else {
                ("partial", format!("{count} envelope(s) accepted; the engine skipped or refused part of the request"))
            };
            Ok(seen(Verdict {
                deliveries,
                ..Verdict::new(outcome, message)
            }))
        }
        Err((code, message))
            if matches!(
                code,
                "contract_invalid" | "brief_mismatch" | "envelope_mismatch" | "already_delivered"
            ) =>
        {
            Ok(seen(Verdict::new(
                "delivery_invalid",
                format!("{code}: {message}"),
            )))
        }
        Err(other) => Err(other),
    }
}

// ------------------------------------------------------------------------------------------------ commands

/// Route one method of this module. Unknown methods are `unknown_method`.
pub fn dispatch(root: &Path, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "dispatch.run" => dispatch_run(root, input),
        "dispatch.list" => dispatch_list(root, input),
        "dispatch.sweep" => dispatch_sweep(root, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// `dispatch.run`: see the module comment.
/// The dispatch pre-flight (see `dispatch_run`): contract check, text gate on the hook, engine `--dry-run`.
/// Every refusal is `preflight_refused` (or the gate's own error) and nothing is written or spawned for spend.
fn preflight(
    root: &Path,
    req: &DispatchRunInput,
    document: &Value,
    engine: Engine,
    exe: &Path,
    project: &Path,
    timeout: Duration,
) -> Result<(), Fail> {
    let checked = host()?.contracts(
        root,
        "contract.check",
        json!({ "contract": "content-brief", "document": document }),
    )?;
    if checked.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err((
            "preflight_refused",
            format!(
                "the brief check refused request {}: {}",
                req.request_id,
                checked.get("errors").unwrap_or(&Value::Null)
            ),
        ));
    }

    let hook = document.get("hook").and_then(Value::as_str).unwrap_or("");
    let gated = host()?.gate(
        root,
        "gate.check",
        json!({
            "scope": scope_value(&req.scope)?,
            "subject_id": req.request_id,
            "text": hook,
            "record": false,
            "now": req.now,
        }),
    )?;
    let disposition = gated
        .get("disposition")
        .and_then(Value::as_str)
        .unwrap_or("block");
    if disposition != "pass" {
        return Err((
            "preflight_refused",
            format!(
                "the text gate returned {disposition} for the hook of request {}",
                req.request_id
            ),
        ));
    }

    // The brief is written to a file the engine reads, under the store root, and removed afterwards.
    let work_dir = root.join("dispatch").join("preflight");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let brief_path = work_dir.join(format!("brief-{}-{stamp}.json", std::process::id()));
    let written = fs::create_dir_all(&work_dir).and_then(|()| {
        let bytes = serde_json::to_vec(document).map_err(std::io::Error::other)?;
        rightkit_fs::atomic_write_private(&brief_path, &bytes).map_err(std::io::Error::other)
    });
    if let Err(e) = written {
        return Err((
            "io_error",
            format!("could not write the pre-flight brief: {e}"),
        ));
    }
    let args = engine.brief_run_args(project, &brief_path).map(|mut args| {
        args.push("--dry-run".into());
        args
    });
    let run = match args {
        Some(args) => {
            let mut no_record = |_: &EngineRecord| -> Result<(), String> { Ok(()) };
            execute(
                exe,
                &args,
                project,
                timeout,
                engine.name(),
                &mut no_record,
                false,
            )
        }
        None => spawn_failure(
            format!("engine {} has no brief command", engine.name()),
            Instant::now(),
        ),
    };
    let _ = fs::remove_file(&brief_path);

    if !matches!(run.ended, Ended::Exited) || run.exit_code != Some(0) {
        return Err((
            "preflight_refused",
            format!(
                "engine {} --dry-run did not accept request {} (exit {:?}): {}",
                engine.name(),
                req.request_id,
                run.exit_code,
                run.stderr_tail.trim()
            ),
        ));
    }
    Ok(())
}

fn dispatch_run(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DispatchRunInput = parse(input, "dispatch.run")?;
    let now_secs = time::parse_instant(&req.now, "now")?;
    let stamp = time::format_instant(now_secs);
    let engine = Engine::parse(&req.engine)?;
    let timeout = timeout_from(req.timeout_secs)?;
    #[cfg(feature = "test-mode")]
    let abandon = test_abandon_requested(&req)?;
    #[cfg(not(feature = "test-mode"))]
    let abandon = false;
    let brief = load_brief(root, &req.scope, &req.request_id)?;
    let _flight = Flight::enter(&req.scope, &req.request_id)?;

    // Leftovers of any request in this scope come first: a dead owner's engine is terminated (when it is
    // provably the recorded one) before this call settles or runs anything. The rows stay `running` here; the
    // settle step below finishes the ones of this request.
    sweep_dead(
        root,
        &req.scope,
        now_secs,
        &stamp,
        false,
        Some(req.request_id.as_str()),
    )?;

    if brief.state == "delivered" {
        return encoded(serde_json::to_value(already_delivered(
            &req, &brief, engine, root, now_secs, &stamp,
        )?));
    }
    let project = project_dir(req.project_dir.as_deref())?;

    // Pre-flight (decided 2026-10-16): the brief must pass the contract check and the text gate on its hook, and
    // the engine must accept it under `--dry-run`. A refusal happens before any attempt row is written, so nothing
    // is spent. A missing engine skips the pre-flight and keeps the `engine_missing` record below.
    if let Ok(exe) = resolve_engine(engine) {
        preflight(root, &req, &brief.document, engine, &exe, &project, timeout)?;
    }

    // Settle dead attempts, gate on an interrupted one, then record the new attempt before anything spawns.
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let settled = settle_running(&tx, &req.scope, &req.request_id, false, now_secs, &stamp)?;
    let latest = latest_outcome(&tx, &req.scope, &req.request_id)?;
    let retry = req.retry_interrupted.unwrap_or(false);
    if !retry && (!settled.is_empty() || latest.as_deref() == Some("interrupted")) {
        tx.commit().map_err(db_err)?;
        return Err((
            "attempt_interrupted",
            format!(
                "an earlier attempt for request {} ended without a recorded result (marked now: {settled:?}); the engine may have \
                 written output. Run dispatch.list, then pass retry_interrupted=true to run again",
                req.request_id
            ),
        ));
    }
    let exe = match resolve_engine(engine) {
        Ok(exe) => exe,
        Err(message) => {
            let detail = json!({ "message": message });
            let id = insert_attempt(
                &tx,
                &req.scope,
                &req.request_id,
                engine,
                &stamp,
                "engine_missing",
                &detail,
            )?;
            let attempt = load_attempt(&tx, id)?;
            tx.commit().map_err(db_err)?;
            let mut out = blank(&req.scope, &req.request_id, engine, "engine_missing");
            out.attempt = Some(attempt);
            out.message = Some(message);
            out.interrupted = settled;
            return encoded(serde_json::to_value(out));
        }
    };
    let attempt_id = insert_attempt(
        &tx,
        &req.scope,
        &req.request_id,
        engine,
        &stamp,
        "running",
        &json!({}),
    )?;
    tx.commit().map_err(db_err)?;

    // The brief goes to the engine as a file (`--input`), under the store root, removed afterwards.
    let work_dir = root.join("dispatch").join(format!("attempt-{attempt_id}"));
    let brief_path = work_dir.join("brief.json");
    let written = fs::create_dir_all(&work_dir).and_then(|()| {
        let bytes = serde_json::to_vec(&brief.document).map_err(std::io::Error::other)?;
        rightkit_fs::atomic_write_private(&brief_path, &bytes).map_err(std::io::Error::other)
    });
    let mut record =
        |engine_record: &EngineRecord| record_engine(root, attempt_id, engine_record, abandon);
    let run = match written {
        Ok(()) => match engine.brief_run_args(&project, &brief_path) {
            Some(args) => execute(
                &exe,
                &args,
                &project,
                timeout,
                engine.name(),
                &mut record,
                abandon,
            ),
            None => spawn_failure(
                format!("engine {} has no brief command", engine.name()),
                Instant::now(),
            ),
        },
        Err(e) => Execution {
            ended: Ended::NotStarted(format!("could not write the brief file: {e}")),
            ..spawn_failure(String::new(), Instant::now())
        },
    };
    let _ = fs::remove_dir_all(&work_dir);

    #[cfg(feature = "test-mode")]
    if matches!(run.ended, Ended::Abandoned) {
        // The test hook: the engine lives, the attempt stays `running` with owner 0, nothing is judged.
        let conn = open_store(root)?;
        let mut out = blank(&req.scope, &req.request_id, engine, "abandoned_for_test");
        out.spawned = true;
        out.attempt = Some(load_attempt(&conn, attempt_id)?);
        out.message = Some(
            "test hook: the engine was left running and its attempt left running with a dead owner"
                .into(),
        );
        out.interrupted = settled;
        return encoded(serde_json::to_value(out));
    }

    let verdict = match judge(root, &req, &project, &run) {
        Ok(verdict) => verdict,
        Err(fail) => {
            // The envelope may be accepted or not; the attempt must not stay `running` either way.
            let conn = open_store(root)?;
            let detail =
                json!({ "message": fail.1, "code": fail.0, "stderr_tail": run.stderr_tail });
            finish_attempt(
                &conn,
                attempt_id,
                &stamp,
                run.exit_code,
                "interrupted",
                &detail,
            )?;
            return Err(fail);
        }
    };

    let produced: Vec<&str> = verdict
        .deliveries
        .iter()
        .map(|d| d.deliverable.as_str())
        .collect();
    let detail = json!({
        "message": verdict.message,
        "engine_error": verdict.engine_error,
        "engine_status": verdict.engine_status,
        "deliverables": produced,
        "skipped": verdict.skipped,
        "stderr_tail": run.stderr_tail,
        "stdout_bytes": run.stdout.len(),
        "elapsed_ms": run.elapsed_ms,
        "timeout_secs": timeout.as_secs(),
        "tree_reaped": run.reaped,
    });
    let conn = open_store(root)?;
    finish_attempt(
        &conn,
        attempt_id,
        &stamp,
        run.exit_code,
        verdict.outcome,
        &detail,
    )?;
    let attempt = load_attempt(&conn, attempt_id)?;
    let spawned = !matches!(run.ended, Ended::SpawnFailed(_) | Ended::NotStarted(_));
    let state = load_brief(root, &req.scope, &req.request_id)?.state;

    let mut out = blank(&req.scope, &req.request_id, engine, verdict.outcome);
    out.spawned = spawned;
    out.brief_state = state;
    out.attempt = Some(attempt);
    out.delivery = verdict.deliveries.first().cloned();
    out.deliveries = verdict.deliveries;
    out.skipped = verdict.skipped;
    out.message = verdict.message;
    out.stderr_tail = spawned.then_some(run.stderr_tail);
    out.engine_error = verdict.engine_error;
    out.interrupted = settled;
    encoded(serde_json::to_value(out))
}

/// The test hook: `test_abandon_after_spawn` is honoured only with `RIGHTKIT_DISPATCH_TEST_HOOKS=1` exactly, under
/// QA isolation, on Unix (Windows ties the tree to a job object that closes with this process, so an abandoned
/// engine cannot outlive the call there). Anything else asking for it is `bad_input`, before any effect.
#[cfg(feature = "test-mode")]
fn test_abandon_requested(req: &DispatchRunInput) -> Result<bool, Fail> {
    if req.test_abandon_after_spawn != Some(true) {
        return Ok(false);
    }
    let hooks_on = std::env::var(TEST_HOOKS_VAR).is_ok_and(|v| v == "1");
    if !hooks_on || !host().map(|h| h.qa_isolated()).unwrap_or(false) {
        return Err(bad(format!(
            "test_abandon_after_spawn needs {TEST_HOOKS_VAR}=1 and a QA-isolated run (the owner's validated test mode)"
        )));
    }
    if cfg!(not(unix)) {
        return Err(bad("test_abandon_after_spawn is supported on Unix only"));
    }
    Ok(true)
}

/// Writes the engine's identity on its attempt row. With `abandon` it also rewrites `owner_pid` to 0 and sets
/// `abandoned_for_test`. Pid 0 is never a process (`owner_alive` is false for it), so the row reads as a dead
/// owner's with no pid to recycle and no helper process to spawn and reap.
fn record_engine(
    root: &Path,
    attempt_id: i64,
    record: &EngineRecord,
    abandon: bool,
) -> Result<(), String> {
    let conn = open_store(root).map_err(|fail| fail.1)?;
    #[cfg(feature = "test-mode")]
    let changed = conn
        .execute(
            RECORD_ENGINE,
            params![
                attempt_id,
                record.pid.map(i64::from),
                record.pgid.map(i64::from),
                record.start,
                record.exe,
                record.source,
                i64::from(abandon)
            ],
        )
        .map_err(|e| e.to_string())?;
    #[cfg(not(feature = "test-mode"))]
    let changed = {
        // A release build never writes `abandoned_for_test` (the column stays in the schema for existing databases).
        let _ = abandon;
        conn.execute(
            RECORD_ENGINE,
            params![
                attempt_id,
                record.pid.map(i64::from),
                record.pgid.map(i64::from),
                record.start,
                record.exe,
                record.source
            ],
        )
        .map_err(|e| e.to_string())?
    };
    if changed == 1 {
        Ok(())
    } else {
        Err(format!("attempt {attempt_id} is no longer running"))
    }
}

/// A delivered request: the stored envelopes (all of the brief's) and the attempt that produced them, nothing
/// spawned. A `running` row left by a crash after the envelopes were accepted is settled as delivered.
fn already_delivered(
    req: &DispatchRunInput,
    brief: &BriefOutRecord,
    engine: Engine,
    root: &Path,
    now_secs: i64,
    stamp: &str,
) -> Result<DispatchRunOutput, Fail> {
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    settle_running(&tx, &req.scope, &req.request_id, true, now_secs, stamp)?;
    let attempt = last_delivered_attempt(&tx, &req.scope, &req.request_id)?;
    tx.commit().map_err(db_err)?;
    let mut out = blank(&req.scope, &req.request_id, engine, "delivered");
    out.brief_state = "delivered".to_string();
    out.attempt = attempt;
    out.delivery = brief.deliveries.first().cloned();
    out.deliveries = brief.deliveries.clone();
    out.message = Some("the request is already delivered; nothing was run".to_string());
    Ok(out)
}

/// `dispatch.sweep`: finds the `running` attempts of the scope whose owner is dead, terminates each recorded engine
/// that is provably still the recorded process (see `engine_proc`), records what was done on the attempt
/// (`sweep_action`, `sweep_at`, `sweep_detail`), and settles the attempt (`interrupted`, or `delivered` when its
/// request was already delivered). Returns one entry per attempt, oldest first.
fn dispatch_sweep(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DispatchSweepInput = parse(input, "dispatch.sweep")?;
    let now_secs = time::parse_instant(&req.now, "now")?;
    let stamp = time::format_instant(now_secs);
    // contracts-io refuses a scope that matches no stored channel.
    host()?.contracts(
        root,
        "brief_out.list",
        json!({ "scope": scope_value(&req.scope)? }),
    )?;
    let swept = sweep_dead(root, &req.scope, now_secs, &stamp, true, None)?;
    encoded(serde_json::to_value(DispatchSweepOutput {
        scope: req.scope,
        swept,
    }))
}

/// `dispatch.list`: attempts of one scope, or of one request. Reads only.
fn dispatch_list(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DispatchListInput = parse(input, "dispatch.list")?;
    // The scope check and the request check are contracts-io's: a foreign scope or unknown request is refused there.
    if let Some(request_id) = &req.request_id {
        load_brief(root, &req.scope, request_id)?;
    } else {
        host()?.contracts(
            root,
            "brief_out.list",
            json!({ "scope": scope_value(&req.scope)? }),
        )?;
    }
    let conn = open_store(root)?;
    let sql = format!(
        "SELECT {ATTEMPT_COLUMNS} FROM dispatch_attempts
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND (?4 IS NULL OR request_id = ?4)
         ORDER BY attempt_id ASC"
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.request_id
            ],
            attempt_row,
        )
        .map_err(db_err)?;
    let mut attempts = Vec::new();
    for row in rows {
        attempts.push(attempt_record(row.map_err(db_err)?)?);
    }
    encoded(serde_json::to_value(DispatchListOutput {
        scope: req.scope,
        attempts,
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::MAIN_SEPARATOR;

    use super::project_dir;

    /// A caller may write the project path with either separator, or with both; each resolves to the same directory.
    #[test]
    fn project_dir_accepts_either_separator() {
        let base =
            std::env::temp_dir().join(format!("rightkit-dispatch-test-{}", std::process::id()));
        let project = base.join("project");
        fs::create_dir_all(&project).expect("create the project directory");
        let native = project.display().to_string();
        let forward = native.replace('\\', "/");
        let backward = native.replace('/', "\\");
        // The last separator of the path written the other way: mixed, as a caller joining two parts can produce.
        let other = if MAIN_SEPARATOR == '/' { "\\" } else { "/" };
        let mixed = format!("{}{other}project", base.display());
        for raw in [
            native.as_str(),
            forward.as_str(),
            backward.as_str(),
            mixed.as_str(),
        ] {
            let resolved =
                project_dir(Some(raw)).unwrap_or_else(|e| panic!("{raw:?} was refused: {}", e.1));
            assert_eq!(
                resolved, project,
                "{raw:?} must resolve to the project directory"
            );
        }
        let _ = fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[test]
    fn a_set_variable_returns_the_path() {
        let exe = std::env::current_exe().expect("this test binary is an existing absolute file");
        for engine in [Engine::Cutright, Engine::Genright] {
            assert_eq!(
                resolve_engine_from(engine, Some(exe.clone().into_os_string())),
                Ok(exe.clone())
            );
        }
    }

    #[test]
    fn an_unset_or_empty_variable_returns_the_clear_error() {
        for (engine, var) in [
            (Engine::Cutright, "RIGHTKIT_EDITOR_ENGINE"),
            (Engine::Genright, "RIGHTKIT_GENERATOR_ENGINE"),
        ] {
            for value in [None, Some(OsString::new())] {
                let message = resolve_engine_from(engine, value).unwrap_err();
                assert_eq!(
                    message,
                    format!(
                        "{var} is not set: the {} engine is located only by that variable",
                        engine.name()
                    )
                );
            }
        }
    }

    #[test]
    fn a_relative_or_missing_path_is_refused() {
        let relative =
            resolve_engine_from(Engine::Cutright, Some("cutright-engine".into())).unwrap_err();
        assert!(relative.contains("must be an absolute path"), "{relative}");
        let missing = std::env::temp_dir().join("rightkit-no-such-engine");
        assert!(resolve_engine_from(Engine::Genright, Some(missing.into_os_string())).is_err());
    }
}
