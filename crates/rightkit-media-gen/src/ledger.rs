//! One spend ledger for GUI and CLI submissions (§4.2), on the published `rightkit-spend` crate.
//!
//! Every submit attempt, including resubmissions, reserves its estimate atomically first: the
//! reservation is rightkit-job-queue's `StoreHook::on_attempt`, and it runs through
//! `rightkit_spend::Ledger::reserve_on` on the hook's own connection, so it commits (or rolls back)
//! in the same transaction that counts the attempt.
//!
//! WHO OWNS WHAT. RightKit owns the money accounting: allowances (limit, reserved, committed,
//! compare-and-swap revision), idempotent reservations, commit, release, overrun flags and
//! per-allowance reports, in integer micro-USD (`spend_*` tables, versioned in
//! `rightkit_spend_meta`). the host keeps only what the crate documents as the app's job: the
//! allowance amounts (the settings below), the mapping from a job attempt to its reservations
//! (`genright_spend_attempts`, one row per attempt, which also backs the per-job, per-day and
//! per-provider reports and the invoice reconciliation no allowance report covers), the audit
//! rows, and the estimate arithmetic. The schema of those tables is versioned with
//! `rightkit_db::migrate` in `genright_spend_meta`.
//!
//! THREE LIMITS, one path. A reservation is made against EACH of these allowances in turn, inside
//! the one transaction (a refusal rolls the whole attempt back, so the earlier reservations never
//! land): the daily cap (period `YYYY-MM-DD`), the monthly cap (period `YYYY-MM`; unlimited when
//! no monthly cap is set, so month totals still exist) and the optional per-device offline
//! allowance (period `device:<id>:<epoch>`). Every spender that reserves (`SpendHook::on_attempt`,
//! `reserve`, `reserve_scoped`, `reserve_budgeted`) goes through `reserve_attempt_on`, and
//! [`check`] is its advisory form.
//!
//! RESERVATIONS HELD ACROSS MIDNIGHT. An open reservation from an earlier UTC day still counts
//! against today's daily cap until it is settled or released. The crate keys an allowance by
//! period, so the carried amount is expressed in today's allowance: its limit is the daily cap
//! minus the estimates still open from earlier days, recomputed in the reserving transaction
//! (`limits_in`), so a settle or release of the old reservation gives the room back at once.
//!
//! DEVICE ALLOWANCE (PENDING 93). `device_allowance_usd` is the most THIS device may reserve in
//! total until it is refilled with `spend set_allowance`. Counting is the crate's: open
//! reservations at their estimate, then committed ones at their actual, over the period of the
//! current allowance epoch. A refill (or clear) starts a new epoch, which is a new period string.
//! Unset (the default) reserves nothing against it. The device id is a random UUID generated once
//! and kept in this data folder's `settings`; it is never a hardware identifier.
//!
//! WHAT THIS SLICE GUARANTEES, precisely: if the user splits a budget into per-device allowances
//! whose sum is the budget, then no set of devices that each enforce their own allowance can spend
//! more than the budget through this ledger (each device refuses at its own allowance, counting
//! open reservations and settled amounts). It guarantees nothing if the allowances sum to more
//! than the budget: nothing here knows another device exists. Spend made before the allowance was
//! set, or outside this ledger, is not counted.
//!
//! WHAT IT DOES NOT PROVIDE (gap for RightKit, reported by `spend status` as
//! `shared_allowance.available: false`): a shared allowance, one remainder that two machines draw
//! down together. `rightkit-spend` 0.1.1 keeps its allowances in one SQLite file and has no sync
//! transport, and `rightkit-sync` is declaration-only today, so the host does not pretend to.
//!
//! MONEY UNITS. The ledger counts integer micro-USD. Estimates are rounded UP to a micro-USD and
//! caps DOWN (a hair of float noise is tolerated), so rounding never admits spend the cap refuses.
//! Engine command output (`est_usd`, `reserved_usd`, ...) stays USD floats.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use rightkit_db::{migrate, DbError, DbOptions, MigrateOptions, Migration, VersionStorage};
use rightkit_job_queue::{JobError, JobRecord, JobResult, Resolution, StoreHook};
use rightkit_spend::{Ledger, ReserveOptions, Scope, SpendError};
use serde_json::{json, Value};

use crate::store::{je, now, today, Db};
use crate::{GenError, GenResult};

pub const DEFAULT_CAP_USD: f64 = 5.0;
const CAP_KEY: &str = "spend_cap_usd";
/// Optional UTC-calendar-month cap, stored beside the daily cap. Absent means no monthly cap.
const MONTHLY_CAP_KEY: &str = "spend_monthly_cap_usd";
/// Optional per-device offline allowance, in `settings`. Absent means no device allowance.
const ALLOWANCE_KEY: &str = "spend_device_allowance_usd";
/// When the allowance was last set (`store::now()` format), for display.
const ALLOWANCE_SET_AT_KEY: &str = "spend_device_allowance_set_at";
/// The allowance epoch: bumped on every set or clear; never deleted. The epoch is part of the
/// device allowance's period, so a refill starts counting from zero.
const ALLOWANCE_EPOCH_KEY: &str = "spend_device_allowance_epoch";
const DEVICE_ID_KEY: &str = "spend_device_id";

const MICRO: f64 = 1_000_000.0;
/// The limit of an allowance that has none (no monthly cap), and the legacy-import limit. Far
/// below `i64::MAX` so reserved plus committed can never overflow the crate's checked sums.
const UNLIMITED_MICRO: i64 = i64::MAX / 4;
/// The crate stores the price version it is given as opaque text; the host prices an attempt with
/// the engine's own estimate.
const PRICE_VERSION: &str = "estimate-v1";

/// The ledger scope of every host allowance: one device, one data folder. The venture is the
/// host settings namespace ([`crate::host::HostConfig::settings_namespace`]).
fn scope_key() -> String {
    let venture = crate::host::config().settings_namespace.as_str();
    Scope::new(venture, "spend", "local")
        .map(|s| s.key())
        .unwrap_or_else(|_| format!("{venture}/spend/local"))
}

// ------------------------------------------------------------------------------------ units

fn usd(micro: i64) -> f64 {
    micro as f64 / MICRO
}

fn invalid_amount(what: &str, v: f64) -> GenError {
    GenError::new(
        "spend_cap",
        format!("{what} must be a finite, non-negative USD amount, got {v}"),
    )
}

/// An estimate or actual in micro-USD, rounded UP (a hair of float noise is not rounded up).
/// Non-finite, negative or absurd amounts are refused, never read as zero.
pub fn micros_up(usd_amount: f64) -> GenResult<i64> {
    if !usd_amount.is_finite() || usd_amount < 0.0 {
        return Err(invalid_amount("amount", usd_amount));
    }
    let v = (usd_amount * MICRO - 1e-3).ceil().max(0.0);
    if v >= UNLIMITED_MICRO as f64 {
        return Err(invalid_amount("amount", usd_amount));
    }
    Ok(v as i64)
}

/// A cap or allowance in micro-USD, rounded DOWN, clamped to the unlimited limit.
fn micros_cap(usd_amount: f64) -> i64 {
    if !usd_amount.is_finite() || usd_amount < 0.0 {
        return 0;
    }
    let v = (usd_amount * MICRO + 1e-3).floor();
    if v >= UNLIMITED_MICRO as f64 {
        UNLIMITED_MICRO
    } else {
        v as i64
    }
}

/// `usd_amount` as the USD float of the micro-USD it is reserved at (rounded up). The estimate a
/// job stores and reports is quantised with this, so what is reported is what the ledger holds.
pub fn quantize_usd(usd_amount: f64) -> GenResult<f64> {
    micros_up(usd_amount).map(usd)
}

// ------------------------------------------------------------------------------- settings

fn setting(conn: &Connection, key: &str) -> rightkit_db::rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
        r.get(0)
    })
    .optional()
}

fn cap_in(conn: &Connection) -> rightkit_db::rusqlite::Result<f64> {
    // Absent: the default. Present but not a finite non-negative number (a hand-edited `inf` or
    // `NaN` would lift the cap): refuse every paid reservation (cap 0) instead.
    Ok(match setting(conn, CAP_KEY)? {
        None => DEFAULT_CAP_USD,
        Some(v) => v
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .unwrap_or(0.0),
    })
}

pub fn cap(db: &Db) -> GenResult<f64> {
    db.read(|c| Ok(cap_in(c)?)).map_err(je)
}

/// The monthly cap, or `None` when none is set. A stored value that does not parse as a
/// non-negative number refuses every paid reservation (cap 0) instead of lifting the limit.
fn monthly_cap_in(conn: &Connection) -> rightkit_db::rusqlite::Result<Option<f64>> {
    Ok(setting(conn, MONTHLY_CAP_KEY)?.map(|v| {
        v.parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .unwrap_or(0.0)
    }))
}

pub fn monthly_cap(db: &Db) -> GenResult<Option<f64>> {
    db.read(|c| Ok(monthly_cap_in(c)?)).map_err(je)
}

pub fn set_cap(db: &Db, cap_usd: f64) -> GenResult<()> {
    set_caps(db, Some(cap_usd), None)
}

/// Side table of spend-affecting setting changes (caps, routing preferences, job resolves, rental
/// authorisation). Created lazily by the first write; one row per change, who is unknown ("local user").
/// RightKit's ledger keeps no audit rows (its README leaves them to the app).
const SETTING_AUDIT_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS spend_setting_audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at TEXT NOT NULL,
  who TEXT NOT NULL,
  command TEXT NOT NULL,
  detail TEXT NOT NULL
);
";

fn audit_in(conn: &Connection, command: &str, detail: &Value) -> rightkit_db::rusqlite::Result<()> {
    conn.execute_batch(SETTING_AUDIT_SCHEMA)?;
    conn.execute(
        "INSERT INTO spend_setting_audit(at, who, command, detail) VALUES (?1,?2,?3,?4)",
        params![now(), "local user", command, detail.to_string()],
    )?;
    Ok(())
}

/// Record one audit row for `command` with a JSON `detail` (old and new values, ids). Used by the
/// commands that move money limits or preferences without an approval step.
pub fn audit_setting(db: &Db, command: &str, detail: &Value) -> GenResult<()> {
    db.write(|c| Ok(audit_in(c, command, detail)?)).map_err(je)
}

/// The newest `limit` rows of [`audit_setting`], newest first (empty when none were written).
pub fn setting_audit(db: &Db, limit: i64) -> GenResult<Vec<Value>> {
    db.read(|c| {
        if !has_table(c, "spend_setting_audit")? {
            return Ok(Vec::new());
        }
        let mut st = c.prepare("SELECT id, at, who, command, detail FROM spend_setting_audit ORDER BY id DESC LIMIT ?1")?;
        let rows = st.query_map([limit], |r| {
            let detail: String = r.get(4)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "at": r.get::<_, String>(1)?, "who": r.get::<_, String>(2)?,
                "command": r.get::<_, String>(3)?,
                "detail": serde_json::from_str::<Value>(&detail).unwrap_or(Value::Null),
            }))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .map_err(je)
}

/// Set the daily cap (`Some`), and set (`Some(Some(n))`) or clear (`Some(None)`) the monthly cap.
/// `None` leaves that cap as it is. Both are validated before either is written. The allowances
/// follow at the next reservation, which sets each limit from these settings in its transaction.
pub fn set_caps(db: &Db, daily: Option<f64>, monthly: Option<Option<f64>>) -> GenResult<()> {
    if let Some(d) = daily {
        if !d.is_finite() || d < 0.0 {
            return Err(GenError::new(
                "invalid_input",
                "cap_usd must be a non-negative number",
            ));
        }
    }
    if let Some(Some(m)) = monthly {
        if !m.is_finite() || m < 0.0 {
            return Err(GenError::new(
                "invalid_input",
                "monthly_cap_usd must be a non-negative number or null",
            ));
        }
    }
    db.write(|c| {
        let old = json!({"cap_usd": cap_in(c)?, "monthly_cap_usd": monthly_cap_in(c)?});
        if let Some(d) = daily {
            c.execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![CAP_KEY, d.to_string()],
            )?;
        }
        match monthly {
            Some(Some(m)) => {
                c.execute(
                    "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![MONTHLY_CAP_KEY, m.to_string()],
                )?;
            }
            Some(None) => {
                c.execute("DELETE FROM settings WHERE key=?1", params![MONTHLY_CAP_KEY])?;
            }
            None => {}
        }
        let new = json!({"cap_usd": cap_in(c)?, "monthly_cap_usd": monthly_cap_in(c)?});
        audit_in(c, "spend.set_cap", &json!({"old": old, "new": new}))?;
        Ok(())
    })
    .map_err(je)
}

/// The UTC month (`YYYY-MM`) of a `YYYY-MM-DD` day.
fn month_of(day: &str) -> &str {
    day.get(..7).unwrap_or(day)
}

fn has_table(conn: &Connection, name: &str) -> rightkit_db::rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

// --------------------------------------------------------------------------- the handle

/// The ledger of one open database. [`Db`] holds it and the store's [`SpendHook`] shares it.
/// LOCK ORDER: the store's connection first, then this mutex (it is only ever locked inside a
/// `db.read` / `db.write` closure or by the hook); never call back into the store while holding it.
pub type SpendHandle = Arc<Mutex<Ledger>>;

pub(crate) fn lock(h: &SpendHandle) -> MutexGuard<'_, Ledger> {
    h.lock().unwrap_or_else(|p| p.into_inner())
}

fn spend_msg(e: &SpendError) -> String {
    format!("spend ledger error: {e}")
}

fn spend_jerr(e: SpendError) -> JobError {
    JobError::new("spend_ledger", spend_msg(&e))
}

fn spend_gerr(e: SpendError) -> GenError {
    GenError::new("spend_ledger", spend_msg(&e))
}

fn jerr(e: GenError) -> JobError {
    budget_job_error(&e)
}

fn db_gerr(e: DbError) -> GenError {
    GenError::new("db", format!("spend ledger schema: {e}"))
}

// ------------------------------------------------------------------- schema and migration

// RightKit ruling 12b1a381: #12 pending RightKit fix (attempt index)
const SIDECAR_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS genright_spend_attempts (
  job_id TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  day TEXT NOT NULL,
  est_micro INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('open','settled','released')),
  actual_micro INTEGER,
  overrun INTEGER NOT NULL DEFAULT 0,
  acked INTEGER NOT NULL DEFAULT 0,
  daily_rid TEXT NOT NULL,
  monthly_rid TEXT NOT NULL,
  device_rid TEXT,
  job_budget_rid TEXT,
  run_budget_rid TEXT,
  at TEXT NOT NULL,
  PRIMARY KEY (job_id, attempt)
);
CREATE INDEX IF NOT EXISTS genright_spend_attempts_day ON genright_spend_attempts(day);
CREATE INDEX IF NOT EXISTS genright_spend_attempts_state ON genright_spend_attempts(state, day);
";

const MIGRATIONS: &[Migration] = &[Migration::sql(1, "spend-attempt-index", SIDECAR_SCHEMA)];

/// Create or upgrade the ledger tables and open the ledger for `path`. Concurrent first opens race
/// inside `rightkit_db::migrate` (it reads the version before it takes the write lock), so one
/// attach at a time per database folder runs under a file lock (the in-process mutex covers
/// threads, the file lock covers processes).
pub(crate) fn attach(path: &Path) -> GenResult<SpendHandle> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    rightkit_fs::ensure_private_dir(dir)
        .map_err(|e| GenError::new("db", format!("spend ledger folder: {e}")))?;
    let lock_path = dir.join("spend-attach.lock");
    let _file_lock = rightkit_fs::FileLock::acquire(
        &lock_path,
        &rightkit_fs::LockOptions::wait(std::time::Duration::from_secs(30)),
    )
    .map_err(|e| GenError::new("db", format!("spend ledger attach lock: {e}")))?;
    try_attach(path).map(|l| Arc::new(Mutex::new(l)))
}

fn try_attach(path: &Path) -> GenResult<Ledger> {
    let open = || DbOptions::new(path).open_writer().map_err(db_gerr);
    let ledger = Ledger::attach(open()?).map_err(spend_gerr)?;
    let side = open()?;
    let options = MigrateOptions {
        version_storage: VersionStorage::Table {
            table: "genright_spend_meta".to_owned(),
            key: "schema".to_owned(),
        },
        backup: false,
        optimize_after: false,
        ..MigrateOptions::default()
    };
    migrate(&side, None, MIGRATIONS, &options).map_err(db_gerr)?;
    Ok(ledger)
}

// ------------------------------------------------------------------------ the hook

/// The ledger's part of the job store: the per-attempt reservation (in the transaction that
/// counts the attempt) and the settlement of a resolved held job. The hook is built before the
/// store opens; `SpendHook::bind` hands it the ledger once the database exists.
#[derive(Clone, Default)]
pub struct SpendHook {
    cell: Arc<OnceLock<SpendHandle>>,
}

impl SpendHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn bind(&self, h: SpendHandle) {
        let _ = self.cell.set(h);
    }

    fn handle(&self) -> JobResult<&SpendHandle> {
        self.cell.get().ok_or_else(|| {
            JobError::new(
                "spend_ledger",
                "the spend ledger is not attached; nothing was reserved",
            )
        })
    }
}

impl StoreHook for SpendHook {
    fn on_attempt(&self, conn: &Connection, job: &JobRecord, attempt: i64) -> JobResult<()> {
        let h = self.handle()?;
        // The scope rides in the stored request (`api::generate`); an unscoped job has none.
        // A stored budget that cannot be read refuses (money-safe), never reads as "no budget".
        let budget =
            CallerBudget::from_request(&job.job.request).map_err(|e| budget_job_error(&e))?;
        let route = format!(
            "{}/{}",
            job.ext_str("provider").unwrap_or("unrouted"),
            job.ext_str("model").unwrap_or("unknown")
        );
        let a = Attempt {
            job_id: &job.job.job_id,
            attempt,
            est_usd: job.ext_f64("est_usd").unwrap_or(0.0),
            route: &route,
            scope: job.job.request.get("scope"),
            budget: budget.as_ref(),
            process: process_snapshot(),
            day: today(),
        };
        reserve_attempt_on(&lock(h), conn, &a)
    }

    fn on_resolve(
        &self,
        conn: &Connection,
        job: &JobRecord,
        attempt: i64,
        how: &Resolution,
    ) -> JobResult<()> {
        resolve_on(&lock(self.handle()?), conn, job, attempt, how)
    }
}

// ----------------------------------------------------------------------- the limits

/// What one reservation is checked against, read in the reserving transaction.
struct Limits {
    day: String,
    cap_usd: f64,
    cap_micro: i64,
    /// Estimates still open from EARLIER UTC days: they count against today's daily cap.
    carried_micro: i64,
    month: String,
    monthly_cap_usd: Option<f64>,
    /// The monthly allowance's limit: the cap, or unlimited when none is set.
    monthly_micro: i64,
    device: Option<Device>,
}

struct Device {
    period: String,
    allowance_usd: f64,
    limit_micro: i64,
}

fn limits_in(
    conn: &Connection,
    day: &str,
    create_id: bool,
) -> rightkit_db::rusqlite::Result<Limits> {
    let cap_usd = cap_in(conn)?;
    let monthly_cap_usd = monthly_cap_in(conn)?;
    let carried_micro: i64 = conn.query_row(
        "SELECT COALESCE(SUM(est_micro),0) FROM genright_spend_attempts WHERE state='open' AND day < ?1",
        [day],
        |r| r.get(0),
    )?;
    let device = match allowance_in(conn)? {
        None => None,
        Some(allowance_usd) => {
            let id = if create_id {
                device_id_in(conn)?
            } else {
                setting(conn, DEVICE_ID_KEY)?.unwrap_or_default()
            };
            let epoch = allowance_epoch_in(conn)?;
            Some(Device {
                period: format!("device:{id}:{epoch}"),
                allowance_usd,
                limit_micro: micros_cap(allowance_usd),
            })
        }
    };
    Ok(Limits {
        day: day.to_string(),
        cap_usd,
        cap_micro: micros_cap(cap_usd),
        carried_micro,
        month: month_of(day).to_string(),
        monthly_cap_usd,
        monthly_micro: monthly_cap_usd.map(micros_cap).unwrap_or(UNLIMITED_MICRO),
        device,
    })
}

fn cap_error(cap: f64, used: f64, est: f64) -> GenError {
    GenError::new(
        "spend_cap",
        format!("daily cap ${cap:.2} would be exceeded (used ${used:.2}, this attempt ${est:.2})"),
    )
}

fn monthly_cap_error(month: &str, cap: f64, used: f64, est: f64) -> GenError {
    GenError::new(
        "spend_cap",
        format!("monthly cap ${cap:.2} would be exceeded (month {month} used ${used:.2}, this attempt ${est:.2})"),
    )
}

fn allowance_error(allowance: f64, counted: f64, est: f64) -> GenError {
    GenError::new(
        "spend_cap",
        format!("device allowance ${allowance:.2} would be exceeded (counted ${counted:.2} since it was set, this attempt ${est:.2}); this allowance applies to this device only"),
    )
}

fn unacknowledged_error(what: &str, count: i64) -> GenError {
    GenError::new(
        "spend_cap",
        format!("{count} overrun(s) await acknowledgement before the {what} reserves again; run `spend acknowledge_overrun` once you have seen them"),
    )
}

/// The `(reserved, committed)` micro-USD of one allowance; nothing when it does not exist yet.
fn totals(l: &Ledger, period: &str) -> Result<(i64, i64), SpendError> {
    Ok(l.allowance(&scope_key(), period)?
        .map(|a| (a.reserved_micro, a.committed_micro))
        .unwrap_or((0, 0)))
}

/// Set an allowance's limit, creating it when it does not exist. The crate changes a limit only
/// under a compare-and-swap on the revision and has no read of the revision inside a caller's
/// transaction, so a revision of 0 is offered first: the conflict names the revision that is
/// really stored (0: none yet). `AllowanceBelowSpend` means the spend already held is above the
/// new limit.
fn ensure_limit(
    l: &Ledger,
    conn: &Connection,
    period: &str,
    limit_micro: i64,
) -> Result<(), SpendError> {
    let scope = scope_key();
    match l.set_allowance_on(conn, &scope, period, limit_micro, Some(0)) {
        Ok(_) => Ok(()),
        Err(SpendError::RevisionConflict { actual: 0, .. }) => l
            .set_allowance_on(conn, &scope, period, limit_micro, None)
            .map(|_| ()),
        Err(SpendError::RevisionConflict { actual, .. }) => l
            .set_allowance_on(conn, &scope, period, limit_micro, Some(actual))
            .map(|_| ()),
        Err(e) => Err(e),
    }
}

/// The refusal for one more reservation of `est`, or `None` when it fits under every limit.
/// Daily is checked first, then monthly, then the device allowance; the first one exceeded is named.
/// Advisory: it reads the allowances as committed and writes nothing.
fn advisory_refusal(
    l: &Ledger,
    lim: &Limits,
    est_usd: f64,
    est: i64,
) -> Result<Option<GenError>, SpendError> {
    let (r, c) = totals(l, &lim.day)?;
    let spent = r + c;
    let room = (lim.cap_micro - lim.carried_micro) - spent;
    if est > room {
        return Ok(Some(cap_error(
            lim.cap_usd,
            usd(spent + lim.carried_micro),
            est_usd,
        )));
    }
    if let Some(mcap) = lim.monthly_cap_usd {
        let (r, c) = totals(l, &lim.month)?;
        if est > lim.monthly_micro - (r + c) {
            return Ok(Some(monthly_cap_error(
                &lim.month,
                mcap,
                usd(r + c),
                est_usd,
            )));
        }
    }
    if let Some(d) = &lim.device {
        let (r, c) = totals(l, &d.period)?;
        if est > d.limit_micro - (r + c) {
            return Ok(Some(allowance_error(d.allowance_usd, usd(r + c), est_usd)));
        }
    }
    Ok(None)
}

/// Would one more reservation of `est` fit under today's cap, this month's cap and the device
/// allowance? (Advisory.)
pub fn check(db: &Db, est: f64) -> GenResult<()> {
    let est_micro = micros_up(est)?;
    let day = today();
    let h = db.spend().clone();
    let refusal = db
        .read(|c| {
            let lim = limits_in(c, &day, false)?;
            advisory_refusal(&lock(&h), &lim, est, est_micro).map_err(spend_jerr)
        })
        .map_err(|e| split_refusal(e).1)?;
    match refusal {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ------------------------------------------------------------------- device allowance

/// Side table of device allowance changes, created lazily by the first write that needs it; reads
/// treat a missing table as empty.
const ALLOWANCE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS device_allowance_audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at TEXT NOT NULL,
  who TEXT NOT NULL,
  device_id TEXT NOT NULL,
  old_usd REAL,
  new_usd REAL,
  note TEXT
);
";

/// The device allowance, or `None` when none is set. A stored value that does not parse as a
/// non-negative number refuses every paid reservation (allowance 0) instead of lifting the limit.
fn allowance_in(conn: &Connection) -> rightkit_db::rusqlite::Result<Option<f64>> {
    Ok(setting(conn, ALLOWANCE_KEY)?.map(|v| {
        v.parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .unwrap_or(0.0)
    }))
}

fn allowance_epoch_in(conn: &Connection) -> rightkit_db::rusqlite::Result<i64> {
    Ok(setting(conn, ALLOWANCE_EPOCH_KEY)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0))
}

/// The random id of this device, generated once and kept in `settings`. Inside the caller's
/// connection so a first reservation or set cannot race a second generation.
/// rightkit-device owns id creation; this only supplies the SQLite-backed store.
fn device_id_in(conn: &Connection) -> rightkit_db::rusqlite::Result<String> {
    rightkit_device::DeviceService::new(SettingsDeviceStore(conn))
        .get_or_create()
        .map_err(|e| rightkit_db::rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// `SecureStore` over the existing `settings` row, so ids already stored are kept.
struct SettingsDeviceStore<'a>(&'a Connection);

impl rightkit_device::SecureStore<String> for SettingsDeviceStore<'_> {
    fn get(&self) -> Result<Option<String>, String> {
        setting(self.0, DEVICE_ID_KEY)
            .map(|v| v.filter(|v| !v.is_empty()))
            .map_err(|e| e.to_string())
    }
    fn set(&self, value: &String) -> Result<(), String> {
        self.0
            .execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![DEVICE_ID_KEY, value],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn delete(&self) -> Result<(), String> {
        self.0
            .execute("DELETE FROM settings WHERE key=?1", [DEVICE_ID_KEY])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    /// Atomic: inserts, or replaces only an empty legacy value; never overwrites a real id.
    fn set_if_absent(&self, value: &String) -> Result<bool, String> {
        self.0
            .execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE settings.value=''",
                params![DEVICE_ID_KEY, value],
            )
            .map(|n| n > 0)
            .map_err(|e| e.to_string())
    }
}

pub fn allowance(db: &Db) -> GenResult<Option<f64>> {
    db.read(|c| Ok(allowance_in(c)?)).map_err(je)
}

/// Honest about the gap: there is no sync service, so no allowance is shared between machines.
fn shared_allowance_json() -> Value {
    json!({"available": false, "reason": "no sync service: each device enforces its own offline allowance"})
}

/// This device's allowance: id, amount (null when unset), counted since it was set, remaining, and
/// when it was set. Generates the device id on first read.
fn allowance_json(db: &Db) -> GenResult<Value> {
    let h = db.spend().clone();
    db.write(|c| {
        let device_id = device_id_in(c)?;
        let allowance = allowance_in(c)?;
        let period = format!("device:{device_id}:{}", allowance_epoch_in(c)?);
        let (r, k) = totals(&lock(&h), &period).map_err(spend_jerr)?;
        let counted = usd(r + k);
        let set_at = setting(c, ALLOWANCE_SET_AT_KEY)?;
        Ok(json!({
            "device_id": device_id,
            "device_allowance_usd": allowance,
            "counted_usd": allowance.map(|_| counted),
            "remaining_usd": allowance.map(|a| (a - counted).max(0.0)),
            "set_at": allowance.and(set_at),
        }))
    })
    .map_err(je)
}

/// `spend.allowance`: this device's allowance, the `shared_allowance` gap, and the last audit rows.
pub fn allowance_status(db: &Db) -> GenResult<Value> {
    let mut v = allowance_json(db)?;
    let audit = db
        .read(|c| {
            if !has_table(c, "device_allowance_audit")? {
                return Ok(Vec::new());
            }
            let mut st = c.prepare(
                "SELECT id, at, who, device_id, old_usd, new_usd, note FROM device_allowance_audit ORDER BY id DESC LIMIT 20",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?, "at": r.get::<_, String>(1)?, "who": r.get::<_, String>(2)?,
                        "device_id": r.get::<_, String>(3)?, "old_usd": r.get::<_, Option<f64>>(4)?,
                        "new_usd": r.get::<_, Option<f64>>(5)?, "note": r.get::<_, Option<String>>(6)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .map_err(je)?;
    v["shared_allowance"] = shared_allowance_json();
    v["audit"] = Value::Array(audit);
    Ok(v)
}

/// Set (`Some`) or clear (`None`) this device's allowance. Either way the counted baseline resets
/// to now (a new epoch, so a new allowance period) and an audit row records who (unknown: "local
/// user"), when, and the old and new amounts. Validated before anything is written; one transaction.
pub fn set_allowance(db: &Db, allowance: Option<f64>, note: Option<&str>) -> GenResult<Value> {
    if let Some(a) = allowance {
        if !a.is_finite() || a < 0.0 {
            return Err(GenError::new(
                "invalid_input",
                "device_allowance_usd must be a non-negative number or null",
            ));
        }
    }
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    if note.map(|n| n.chars().count() > 500).unwrap_or(false) {
        return Err(GenError::new(
            "invalid_input",
            "note must be at most 500 characters",
        ));
    }
    db.write(|c| {
        c.execute_batch(ALLOWANCE_SCHEMA)?;
        let device_id = device_id_in(c)?;
        let old = allowance_in(c)?;
        let epoch = allowance_epoch_in(c)? + 1;
        let upsert = "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value";
        c.execute(upsert, params![ALLOWANCE_EPOCH_KEY, epoch.to_string()])?;
        match allowance {
            Some(a) => {
                c.execute(upsert, params![ALLOWANCE_KEY, a.to_string()])?;
                c.execute(upsert, params![ALLOWANCE_SET_AT_KEY, now()])?;
            }
            None => {
                c.execute("DELETE FROM settings WHERE key=?1", params![ALLOWANCE_KEY])?;
                c.execute("DELETE FROM settings WHERE key=?1", params![ALLOWANCE_SET_AT_KEY])?;
            }
        }
        c.execute(
            "INSERT INTO device_allowance_audit(at, who, device_id, old_usd, new_usd, note) VALUES (?1,?2,?3,?4,?5,?6)",
            params![now(), "local user", device_id, old, allowance, note],
        )?;
        Ok(())
    })
    .map_err(je)?;
    allowance_status(db)
}

// ------------------------------------------------------------------------ reservation

/// One attempt to reserve for.
pub(crate) struct Attempt<'a> {
    pub job_id: &'a str,
    pub attempt: i64,
    pub est_usd: f64,
    pub route: &'a str,
    /// The job's venture/channel/account object (PENDING 92). It is recorded as the reservation's
    /// account and takes no part in any cap or total.
    pub scope: Option<&'a Value>,
    pub budget: Option<&'a CallerBudget>,
    /// The budget of this engine process and the id of its run, from [`process_snapshot`].
    pub process: Option<(CallerBudget, String)>,
    pub day: String,
}

/// The reservation's account text from a scope object: `venture/channel/account` (the crate's
/// `Scope` key) when all three parts are valid, else the parts that are present.
fn scope_account(scope: &Value) -> Option<String> {
    if !scope.is_object() {
        return None;
    }
    let part = |k: &str| {
        scope
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    if let (Some(v), Some(c), Some(a)) = (part("venture"), part("channel"), part("account")) {
        if let Ok(s) = Scope::new(v, c, a) {
            return Some(s.key());
        }
    }
    let parts: Vec<&str> = ["venture", "channel", "account"]
        .iter()
        .filter_map(|k| part(k))
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

struct AttemptRow {
    #[allow(dead_code)]
    day: String,
    est_micro: i64,
    state: String,
    actual_micro: Option<i64>,
    rids: Vec<String>,
}

fn attempt_row_in(
    conn: &Connection,
    job_id: &str,
    attempt: i64,
) -> rightkit_db::rusqlite::Result<Option<AttemptRow>> {
    conn.query_row(
        "SELECT day, est_micro, state, actual_micro, daily_rid, monthly_rid, device_rid, job_budget_rid, run_budget_rid
         FROM genright_spend_attempts WHERE job_id=?1 AND attempt=?2",
        params![job_id, attempt],
        |r| {
            let rids: Vec<String> = (4usize..9).filter_map(|i| r.get::<_, Option<String>>(i).transpose()).collect::<Result<_, _>>()?;
            Ok(AttemptRow { day: r.get(0)?, est_micro: r.get(1)?, state: r.get(2)?, actual_micro: r.get(3)?, rids })
        },
    )
    .optional()
}

fn job_budget_period(b: &CallerBudget, job_id: &str) -> String {
    format!("budget:{}:job:{job_id}", b.project)
}

fn run_budget_period(b: &CallerBudget, run_id: &str) -> String {
    format!("budget:{}:run:{run_id}", b.project)
}

/// Reserve `a.est_usd` for one attempt against every allowance, in the caller's transaction (the
/// store hook's, or a `db.write`). A refusal returns an error and the transaction rolls back, so
/// no earlier reservation of this call survives. An attempt that already holds an open reservation
/// of the same size is a replay and writes nothing; one that was already settled is refused.
pub(crate) fn reserve_attempt_on(l: &Ledger, conn: &Connection, a: &Attempt) -> JobResult<()> {
    let est = micros_up(a.est_usd).map_err(jerr)?;
    if let Some(row) = attempt_row_in(conn, a.job_id, a.attempt)? {
        return if row.state == "open" && row.est_micro == est {
            Ok(())
        } else {
            Err(jerr(GenError::new(
                "duplicate_reservation",
                format!(
                    "attempt {} of {} already has a {} reservation of ${:.6}",
                    a.attempt,
                    a.job_id,
                    row.state,
                    usd(row.est_micro)
                ),
            )))
        };
    }
    let lim = limits_in(conn, &a.day, true)?;
    let scope = scope_key();
    let rid = |what: &str| format!("{}:{}:{what}", a.job_id, a.attempt);
    let mut opts = ReserveOptions::default().currency("USD");
    opts.account = a.scope.and_then(scope_account);
    let hold = |period: &str, what: &str| {
        l.reserve_on(
            conn,
            &scope,
            &rid(what),
            period,
            a.route,
            PRICE_VERSION,
            est,
            &opts,
        )
    };

    // Daily: the limit is the cap less what is still open from earlier days.
    let daily_refusal = |used_micro: i64| jerr(cap_error(lim.cap_usd, usd(used_micro), a.est_usd));
    let target = lim.cap_micro - lim.carried_micro;
    if target < 0 {
        return Err(daily_refusal(lim.carried_micro));
    }
    match ensure_limit(l, conn, &lim.day, target) {
        Ok(()) => {}
        Err(SpendError::AllowanceBelowSpend { spent_micro, .. }) => {
            return Err(daily_refusal(spent_micro + lim.carried_micro))
        }
        Err(e) => return Err(spend_jerr(e)),
    }
    let daily = hold(&lim.day, "day").map_err(|e| match e {
        SpendError::AllowanceExceeded {
            remaining_micro, ..
        } => daily_refusal(lim.cap_micro - remaining_micro),
        SpendError::OverrunUnacknowledged { count, .. } => {
            jerr(unacknowledged_error("daily allowance", count))
        }
        e => spend_jerr(e),
    })?;

    // Monthly: an allowance always exists so month totals do; its limit is unlimited without a cap.
    let monthly_refusal = |used_micro: i64| match lim.monthly_cap_usd {
        Some(m) => jerr(monthly_cap_error(&lim.month, m, usd(used_micro), a.est_usd)),
        None => spend_jerr(SpendError::BadInput(
            "the monthly allowance refused with no monthly cap set".into(),
        )),
    };
    match ensure_limit(l, conn, &lim.month, lim.monthly_micro) {
        Ok(()) => {}
        Err(SpendError::AllowanceBelowSpend { spent_micro, .. }) => {
            return Err(monthly_refusal(spent_micro))
        }
        Err(e) => return Err(spend_jerr(e)),
    }
    let monthly = hold(&lim.month, "month").map_err(|e| match e {
        SpendError::AllowanceExceeded {
            remaining_micro, ..
        } => monthly_refusal(lim.monthly_micro - remaining_micro),
        SpendError::OverrunUnacknowledged { count, .. } => {
            jerr(unacknowledged_error("monthly allowance", count))
        }
        e => spend_jerr(e),
    })?;

    // Device: only while an allowance is set.
    let device = match &lim.device {
        None => None,
        Some(d) => {
            let refusal = |counted_micro: i64| {
                jerr(allowance_error(
                    d.allowance_usd,
                    usd(counted_micro),
                    a.est_usd,
                ))
            };
            match ensure_limit(l, conn, &d.period, d.limit_micro) {
                Ok(()) => {}
                Err(SpendError::AllowanceBelowSpend { spent_micro, .. }) => {
                    return Err(refusal(spent_micro))
                }
                Err(e) => return Err(spend_jerr(e)),
            }
            let out = hold(&d.period, "device").map_err(|e| match e {
                SpendError::AllowanceExceeded {
                    remaining_micro, ..
                } => refusal(d.limit_micro - remaining_micro),
                SpendError::OverrunUnacknowledged { count, .. } => {
                    jerr(unacknowledged_error("device allowance", count))
                }
                e => spend_jerr(e),
            })?;
            Some(out.reservation.reservation_id)
        }
    };

    // The caller's budgets are further, independent ceilings checked AFTER the caps above: they can
    // only refuse more, never admit what a cap refused. One allowance per job (counts that job's own
    // spend across attempts, so a retry cannot slip past it) and one per run of this process.
    let budget_hold = |b: &CallerBudget, period: String, what: &str| -> JobResult<String> {
        let refusal =
            |remaining_micro: i64| jerr(budget_error(&b.project, a.est_usd, usd(remaining_micro)));
        match ensure_limit(l, conn, &period, micros_cap(b.remaining_usd)) {
            Ok(()) => {}
            Err(SpendError::AllowanceBelowSpend { .. }) => return Err(refusal(0)),
            Err(e) => return Err(spend_jerr(e)),
        }
        hold(&period, what)
            .map(|o| o.reservation.reservation_id)
            .map_err(|e| match e {
                SpendError::AllowanceExceeded {
                    remaining_micro, ..
                } => refusal(remaining_micro),
                SpendError::OverrunUnacknowledged { .. } => refusal(0),
                e => spend_jerr(e),
            })
    };
    let job_budget = match a.budget {
        Some(b) => Some(budget_hold(b, job_budget_period(b, a.job_id), "jobbudget")?),
        None => None,
    };
    let run_budget = match &a.process {
        Some((b, run)) => Some(budget_hold(b, run_budget_period(b, run), "runbudget")?),
        None => None,
    };

    conn.execute(
        "INSERT INTO genright_spend_attempts(job_id, attempt, day, est_micro, state, daily_rid, monthly_rid, device_rid,
             job_budget_rid, run_budget_rid, at) VALUES (?1,?2,?3,?4,'open',?5,?6,?7,?8,?9,?10)",
        params![
            a.job_id,
            a.attempt,
            a.day,
            est,
            daily.reservation.reservation_id,
            monthly.reservation.reservation_id,
            device,
            job_budget,
            run_budget,
            now()
        ],
    )?;
    Ok(())
}

/// Atomically reserve `est` for attempt `attempt` of `job_id`, and record the
/// attempt count on the job. Refuses with `spend_cap`. (Standalone
/// reservations, e.g. character training; job runs reserve through the hook.)
pub fn reserve(db: &mut Db, job_id: &str, attempt: i64, est: f64) -> GenResult<()> {
    reserve_scoped(db, job_id, attempt, est, None)
}

/// [`reserve`] that also records the job's scope ids with the reservation (as its account).
pub fn reserve_scoped(
    db: &mut Db,
    job_id: &str,
    attempt: i64,
    est: f64,
    scope: Option<&serde_json::Value>,
) -> GenResult<()> {
    reserve_budgeted(db, job_id, attempt, est, scope, None)
}

/// [`reserve_scoped`] that also checks the caller's budget (`caller_budget_exceeded` over it). The
/// daily, monthly and device limits apply first and unchanged.
pub fn reserve_budgeted(
    db: &mut Db,
    job_id: &str,
    attempt: i64,
    est: f64,
    scope: Option<&serde_json::Value>,
    budget: Option<&CallerBudget>,
) -> GenResult<()> {
    let h = db.spend().clone();
    let a = Attempt {
        job_id,
        attempt,
        est_usd: est,
        route: "standalone",
        scope,
        budget,
        process: process_snapshot(),
        day: today(),
    };
    db.write(|c| {
        reserve_attempt_on(&lock(&h), c, &a)?;
        c.execute(
            "UPDATE jobs SET attempts=?2, updated_at=?3 WHERE job_id=?1",
            params![job_id, attempt, now()],
        )?;
        Ok(())
    })
    .map_err(|e| split_refusal(e).1)
}

// ------------------------------------------------------------------------- settlement

/// Release (`actual` 0) or commit each reservation; true when any commit exceeded its estimate (the
/// crate flags that as an overrun and refuses further reservations on the allowance until it is
/// acknowledged).
fn settle_rids(
    l: &Ledger,
    conn: &Connection,
    rids: &[&str],
    actual_micro: i64,
) -> Result<bool, SpendError> {
    let mut over = false;
    for rid in rids {
        if actual_micro == 0 {
            l.release_on(conn, rid, Some("released"))?;
        } else {
            over |= l
                .commit_on(conn, rid, actual_micro)?
                .reservation
                .over_reservation;
        }
    }
    Ok(over)
}

/// Mirror the job's running total from its settled attempts.
fn mirror_total_in(conn: &Connection, job_id: &str) -> rightkit_db::rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET actual_usd=(SELECT CAST(COALESCE(SUM(actual_micro),0) AS REAL)/1000000.0
                                     FROM genright_spend_attempts WHERE job_id=?1), updated_at=?2 WHERE job_id=?1",
        params![job_id, now()],
    )?;
    Ok(())
}

/// Settle one open attempt at `actual_micro` in the caller's transaction. An attempt with no row is
/// left alone; one already settled at the same amount is a no-op; one settled differently is
/// refused (the crate never rewrites a settled reservation, and a settle is not undone by a later one).
fn settle_in(
    l: &Ledger,
    conn: &Connection,
    job_id: &str,
    attempt: i64,
    actual_micro: i64,
) -> JobResult<()> {
    let Some(row) = attempt_row_in(conn, job_id, attempt)? else {
        return Ok(());
    };
    if row.state != "open" {
        let same = if actual_micro == 0 {
            row.state == "released"
        } else {
            row.actual_micro == Some(actual_micro)
        };
        return if same {
            Ok(())
        } else {
            Err(JobError::new(
                "spend_already_settled",
                format!("attempt {attempt} of {job_id} is already {}; a settled reservation is never changed", row.state),
            ))
        };
    }
    let rids: Vec<&str> = row.rids.iter().map(String::as_str).collect();
    let over = settle_rids(l, conn, &rids, actual_micro).map_err(spend_jerr)?;
    conn.execute(
        "UPDATE genright_spend_attempts SET state=?3, actual_micro=?4, overrun=?5 WHERE job_id=?1 AND attempt=?2",
        params![job_id, attempt, if actual_micro == 0 { "released" } else { "settled" }, actual_micro, i64::from(over)],
    )?;
    mirror_total_in(conn, job_id)?;
    Ok(())
}

/// What the ledger does when a held (`outcome_unknown`) job is resolved. Runs in the same
/// transaction as the status move, so the job and its reservation cannot disagree.
///
/// - `Resubmit`: the held attempt is released and the job records `outcome_unknown_resubmitted`. An
///   attempt whose reservation was already settled (a received, charged result) is never released:
///   `result_recovered`. The next `begin_submission` counts one new attempt, and its `on_attempt`
///   reserves again.
/// - `Abandon`: a still-open reservation is settled as spent at its estimate (the provider may have
///   charged). An already settled one stays as it is.
/// - `Adopt`: the result is in hand, so the attempt was charged: a still-open reservation is
///   settled as spent. No provider call exists on this path.
///
/// An abandon is the user's own decision, so it commits at the estimate (never at the crate's
/// billed-unknown ceiling, which would hold every later reservation until acknowledged).
fn resolve_on(
    l: &Ledger,
    conn: &Connection,
    job: &JobRecord,
    attempt: i64,
    how: &Resolution,
) -> JobResult<()> {
    let job_id = job.job.job_id.as_str();
    let row = attempt_row_in(conn, job_id, attempt)?;
    let open = row.as_ref().map(|r| r.state == "open");
    match how {
        Resolution::Resubmit => {
            if open == Some(false) {
                return Err(JobError::new(
                    crate::jobs::RESULT_RECOVERED,
                    format!("attempt {attempt} of job {job_id} is already settled; its paid result is never requested again"),
                ));
            }
            if open == Some(true) {
                settle_in(l, conn, job_id, attempt, 0)?;
            }
            let note = JobError::new(
                "outcome_unknown_resubmitted",
                format!("resubmit confirmed by the user; attempt {attempt} released"),
            );
            conn.execute(
                "UPDATE jobs SET error_json=?2 WHERE job_id=?1",
                params![job_id, note.to_json_with(false).to_string()],
            )?;
        }
        Resolution::Abandon(_) | Resolution::Adopt(_) => {
            if let Some(r) = row.filter(|r| r.state == "open") {
                settle_in(l, conn, job_id, attempt, r.est_micro)?;
            }
        }
        // `Resolution` is non-exhaustive: a kind this ledger does not know settles nothing.
        _ => {
            return Err(JobError::new(
                "unsupported_resolution",
                "the spend ledger does not know this resolution",
            ))
        }
    }
    mirror_total_in(conn, job_id)?;
    Ok(())
}

/// The amount reserved for one attempt, if it has a ledger row.
pub fn attempt_reserved(db: &Db, job_id: &str, attempt: i64) -> GenResult<Option<f64>> {
    db.read(|c| Ok(attempt_row_in(c, job_id, attempt)?.map(|r| usd(r.est_micro))))
        .map_err(je)
}

/// `(reserved, settled)` of one attempt's row: the reserved amount and, once settled (or released,
/// which settles at zero), the actual; `None` for an attempt with no row.
pub fn attempt_row(db: &Db, job_id: &str, attempt: i64) -> GenResult<Option<(f64, Option<f64>)>> {
    db.read(|c| {
        Ok(attempt_row_in(c, job_id, attempt)?.map(|r| {
            let actual = match r.state.as_str() {
                "open" => None,
                "released" => Some(0.0),
                _ => r.actual_micro.map(usd),
            };
            (usd(r.est_micro), actual)
        }))
    })
    .map_err(je)
}

/// How many attempts of `job_id` have a ledger row.
pub fn attempt_count(db: &Db, job_id: &str) -> GenResult<i64> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM genright_spend_attempts WHERE job_id=?1",
            [job_id],
            |r| r.get(0),
        )?)
    })
    .map_err(je)
}

/// Attempts of `job_id` whose reservation was never reconciled (read-only), oldest
/// first. An open reservation means the call may have run: its outcome is unknown.
pub fn open_attempts(db: &Db, job_id: &str) -> GenResult<Vec<i64>> {
    db.read(|c| {
        let mut st = c.prepare("SELECT attempt FROM genright_spend_attempts WHERE job_id=?1 AND state='open' ORDER BY attempt")?;
        let attempts = st
            .query_map([job_id], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(attempts)
    })
    .map_err(je)
}

/// Release every reservation of a job that was never reconciled (a cancelled
/// job owes nothing for attempts that are still only reserved).
pub fn release_unreconciled(db: &mut Db, job_id: &str) -> GenResult<()> {
    let h = db.spend().clone();
    db.write(|c| {
        let open: Vec<i64> = {
            let mut st = c.prepare("SELECT attempt FROM genright_spend_attempts WHERE job_id=?1 AND state='open' ORDER BY attempt")?;
            let rows = st.query_map([job_id], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let l = lock(&h);
        for attempt in open {
            settle_in(&l, c, job_id, attempt, 0)?;
        }
        mirror_total_in(c, job_id)?;
        Ok(())
    })
    .map_err(je)
}

/// Record the actual charge for an attempt (the estimate when the provider
/// reports none) and mirror the job's running total. An actual of zero releases the reservation.
pub fn reconcile(db: &mut Db, job_id: &str, attempt: i64, actual: f64) -> GenResult<()> {
    let actual_micro = micros_up(actual)?;
    let h = db.spend().clone();
    db.write(|c| settle_in(&lock(&h), c, job_id, attempt, actual_micro))
        .map_err(je)
}

fn abandon_prefix(scope: &str) -> String {
    format!("abandoned:{scope}:")
}

/// Settle one open attempt as spent at `actual` and record that `ordinal` of `scope` must never be
/// sent again, in one transaction (a crash cannot leave the attempt settled but unmarked). Read
/// back by [`abandoned_ordinals`].
pub fn abandon(db: &mut Db, scope: &str, attempt: i64, ordinal: i64, actual: f64) -> GenResult<()> {
    let key = format!("{}{ordinal}", abandon_prefix(scope));
    let actual_micro = micros_up(actual)?;
    let h = db.spend().clone();
    db.write(|c| {
        settle_in(&lock(&h), c, scope, attempt, actual_micro)?;
        c.execute(
            "INSERT INTO settings(key,value) VALUES(?1,'1') ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key],
        )?;
        Ok(())
    })
    .map_err(je)
}

/// Ordinals (1-based segment numbers) of `scope` that were abandoned, ascending.
pub fn abandoned_ordinals(db: &Db, scope: &str) -> GenResult<Vec<i64>> {
    let prefix = abandon_prefix(scope);
    let keys: Vec<String> = db
        .read(|c| {
            let mut st = c.prepare("SELECT key FROM settings WHERE substr(key,1,length(?1))=?1")?;
            let keys = st
                .query_map([&prefix], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(keys)
        })
        .map_err(je)?;
    let mut out: Vec<i64> = keys
        .iter()
        .filter_map(|k| k.strip_prefix(prefix.as_str())?.parse::<i64>().ok())
        .collect();
    out.sort_unstable();
    Ok(out)
}

/// `spend.acknowledge_overrun` `{note?}`: record that a person saw every settle that came in above
/// its reservation. RightKit's ledger refuses further reservations on an allowance while one is
/// unacknowledged. Returns how many reservations were acknowledged.
pub fn acknowledge_overruns(db: &Db, note: Option<&str>) -> GenResult<Value> {
    let h = db.spend().clone();
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    let acknowledged = db
        .write(|c| {
            let rows: Vec<(String, i64, Vec<String>)> = {
                let mut st = c.prepare("SELECT job_id, attempt FROM genright_spend_attempts WHERE overrun=1 AND acked=0")?;
                let keys = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut out = Vec::new();
                for (job, attempt) in keys {
                    if let Some(row) = attempt_row_in(c, &job, attempt)? {
                        out.push((job, attempt, row.rids));
                    }
                }
                out
            };
            let l = lock(&h);
            let mut n = 0i64;
            for (job, attempt, rids) in rows {
                for rid in &rids {
                    // A reservation that did not exceed its estimate is not flagged; the crate says so.
                    match l.acknowledge_overrun_on(c, rid, note) {
                        Ok(()) => n += 1,
                        Err(SpendError::BadInput(_)) => {}
                        Err(e) => return Err(spend_jerr(e)),
                    }
                }
                c.execute("UPDATE genright_spend_attempts SET acked=1 WHERE job_id=?1 AND attempt=?2", params![job, attempt])?;
            }
            audit_in(c, "spend.acknowledge_overrun", &json!({"acknowledged": n, "note": note}))?;
            Ok(n)
        })
        .map_err(je)?;
    Ok(json!({"acknowledged": acknowledged}))
}

// ------------------------------------------------------------------------------ status

/// `(reserved_not_yet_reconciled, actual)` for a day for the DAILY cap: the day's own allowance plus
/// every reservation still open from an EARLIER UTC day.
pub fn day_totals(db: &Db, day: &str) -> GenResult<(f64, f64)> {
    let h = db.spend().clone();
    db.read(|c| {
        let lim = limits_in(c, day, false)?;
        let (r, k) = totals(&lock(&h), day).map_err(spend_jerr)?;
        Ok((usd(r + lim.carried_micro), usd(k)))
    })
    .map_err(je)
}

/// The overruns not yet acknowledged on the current day, month and device allowances.
fn unacknowledged_total(l: &Ledger, lim: &Limits) -> Result<i64, SpendError> {
    let mut periods = vec![lim.day.clone(), lim.month.clone()];
    if let Some(d) = &lim.device {
        periods.push(d.period.clone());
    }
    let mut n = 0;
    for p in periods {
        match l.report(&scope_key(), &p) {
            Ok(r) => n += r.unacknowledged_overruns,
            Err(SpendError::AllowanceMissing { .. }) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

pub fn status(db: &Db) -> GenResult<Value> {
    let day = today();
    let h = db.spend().clone();
    let (lim, (dr, da), (mr, ma), unacked) = db
        .read(|c| {
            let lim = limits_in(c, &day, false)?;
            let l = lock(&h);
            let (r, a) = totals(&l, &lim.day).map_err(spend_jerr)?;
            let month = totals(&l, &lim.month).map_err(spend_jerr)?;
            let unacked = unacknowledged_total(&l, &lim).map_err(spend_jerr)?;
            let carried = lim.carried_micro;
            Ok((lim, (r + carried, a), month, unacked))
        })
        .map_err(je)?;
    let month = lim.month.clone();
    let mcap = lim.monthly_cap_usd;
    let (mr, ma) = (usd(mr), usd(ma));
    let counted = mr + ma;
    Ok(json!({
        "day": day, "cap_usd": lim.cap_usd, "reserved_usd": usd(dr), "actual_usd": usd(da),
        "month": month, "monthly_cap_usd": mcap,
        "month_reserved_usd": mr, "month_actual_usd": ma, "month_counted_usd": counted,
        "monthly_remaining_usd": mcap.map(|c| (c - counted).max(0.0)),
        // Additive: the third limit. `device_allowance_usd` is null when none is set.
        "device": allowance_json(db)?,
        "device_allowance_usd": allowance(db)?,
        "shared_allowance": shared_allowance_json(),
        // Additive: settles above their reservation that no one has acknowledged yet (they hold
        // further reservations until `spend acknowledge_overrun`).
        "unacknowledged_overruns": unacked,
        // Additive: the four separate amounts per period (see [`agg_json`]); the caps count none
        // of them except through `reserved_usd` / `actual_usd` above.
        "amounts": {
            "day": amounts_json(db, &day, &day)?,
            "month": amounts_json(db, &format!("{month}-01"), &format!("{month}-31"))?,
        },
    }))
}

// ---------------------------------------------------------------------------------------------
// Separate amounts (PENDING 58, 119).
//
// An attempt carries four amounts that are never merged:
//   - `estimated_usd`: the engine's estimate at submit (`jobs.est_usd`; null for a row with no job).
//   - `reserved_usd`: what was reserved. THE CAPS COUNT THIS WHILE THE ROW IS OPEN, and the
//     settled actual once it is settled (RightKit's allowance totals).
//   - `provider_reported_usd`: what the provider's response stated. Null until an adapter calls
//     [`record_provider_reported`].
//   - `invoice_reconciled_usd`: set only by `spend reconcile` from the user's invoice lines.
// The last two live in the side table `ledger_amounts`; a row with no side-table row reads both as
// null. Invoice totals live in `invoice_reconciliations`. Both tables are created lazily by the
// first write that needs them; reads treat a missing table as empty. They record USD figures that
// others reported and are never read by a cap.
// ---------------------------------------------------------------------------------------------

const AMOUNTS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ledger_amounts (
  job_id TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  provider_reported_usd REAL,
  provider_reported_source TEXT,
  provider_reported_at TEXT,
  invoice_reconciled_usd REAL,
  invoice_ref TEXT,
  invoice_at TEXT,
  PRIMARY KEY (job_id, attempt)
);
CREATE TABLE IF NOT EXISTS invoice_reconciliations (
  invoice_ref TEXT PRIMARY KEY,
  ref_generated INTEGER NOT NULL,
  provider TEXT NOT NULL,
  period_from TEXT NOT NULL,
  period_to TEXT NOT NULL,
  total_usd REAL NOT NULL,
  recorded_at TEXT NOT NULL
);
";

/// Stands in for `ledger_amounts` in a read when the table does not exist yet.
const NO_AMOUNTS: &str = "(SELECT NULL AS job_id, NULL AS attempt, NULL AS provider_reported_usd, \
NULL AS provider_reported_source, NULL AS provider_reported_at, NULL AS invoice_reconciled_usd, \
NULL AS invoice_ref, NULL AS invoice_at WHERE 0)";

fn amounts_src(conn: &Connection) -> rightkit_db::rusqlite::Result<&'static str> {
    Ok(if has_table(conn, "ledger_amounts")? {
        "ledger_amounts"
    } else {
        NO_AMOUNTS
    })
}

/// Sums over the attempts with `from <= day <= to` (UTC `YYYY-MM-DD`, string order), optionally
/// only the jobs of one provider. Read-only; nothing here is a cap input.
#[derive(Debug, Default, Clone)]
struct Agg {
    rows: i64,
    job_rows: i64,
    estimated: f64,
    /// Settled actual of the rows that have a job (so `estimated` compares like with like).
    job_rows_settled: f64,
    reserved: f64,
    open_reserved: f64,
    settled: f64,
    pr_rows: i64,
    pr_sum: f64,
    /// Settled actual of the rows that have a provider-reported amount.
    pr_settled: f64,
    inv_rows: i64,
    inv_sum: f64,
    /// Settled actual of the rows that have an invoice-reconciled amount.
    inv_settled: f64,
}

fn aggregate_in(
    conn: &Connection,
    from: &str,
    to: &str,
    provider: Option<&str>,
) -> rightkit_db::rusqlite::Result<Agg> {
    let sql = format!(
        "SELECT COUNT(*), COUNT(j.job_id),
                COALESCE(SUM(j.est_usd),0),
                COALESCE(SUM(CASE WHEN j.job_id IS NOT NULL THEN COALESCE(l.actual_micro,0) END),0),
                COALESCE(SUM(l.est_micro),0),
                COALESCE(SUM(CASE WHEN l.state='open' THEN l.est_micro ELSE 0 END),0),
                COALESCE(SUM(l.actual_micro),0),
                COUNT(a.provider_reported_usd), COALESCE(SUM(a.provider_reported_usd),0),
                COALESCE(SUM(CASE WHEN a.provider_reported_usd IS NOT NULL THEN COALESCE(l.actual_micro,0) END),0),
                COUNT(a.invoice_reconciled_usd), COALESCE(SUM(a.invoice_reconciled_usd),0),
                COALESCE(SUM(CASE WHEN a.invoice_reconciled_usd IS NOT NULL THEN COALESCE(l.actual_micro,0) END),0)
         FROM genright_spend_attempts l
         LEFT JOIN jobs j ON j.job_id = l.job_id
         LEFT JOIN {} a ON a.job_id = l.job_id AND a.attempt = l.attempt
         WHERE l.day >= ?1 AND l.day <= ?2 AND (?3 IS NULL OR j.provider = ?3)",
        amounts_src(conn)?
    );
    conn.query_row(&sql, params![from, to, provider], |r| {
        Ok(Agg {
            rows: r.get(0)?,
            job_rows: r.get(1)?,
            estimated: r.get(2)?,
            job_rows_settled: usd(r.get(3)?),
            reserved: usd(r.get(4)?),
            open_reserved: usd(r.get(5)?),
            settled: usd(r.get(6)?),
            pr_rows: r.get(7)?,
            pr_sum: r.get(8)?,
            pr_settled: usd(r.get(9)?),
            inv_rows: r.get(10)?,
            inv_sum: r.get(11)?,
            inv_settled: usd(r.get(12)?),
        })
    })
}

/// The four amounts, each separate and null when no row supplies it:
/// `estimated_usd` (null with no job rows), `reserved_usd` (sum of the reservations made, open or
/// settled), `provider_reported_usd` / `invoice_reconciled_usd` (null when no row has one).
/// `settled_usd` is the sum of settled actuals, `open_reserved_usd` the still-open reservations, and
/// `cap_counted_usd` their sum: exactly what the caps counted for these rows.
fn agg_json(a: &Agg) -> Value {
    json!({
        "rows": a.rows,
        "estimated_usd": (a.job_rows > 0).then_some(a.estimated),
        "reserved_usd": a.reserved,
        "provider_reported_usd": (a.pr_rows > 0).then_some(a.pr_sum),
        "provider_reported_rows": a.pr_rows,
        "provider_reported_available": a.pr_rows > 0,
        "invoice_reconciled_usd": (a.inv_rows > 0).then_some(a.inv_sum),
        "invoice_reconciled_rows": a.inv_rows,
        "settled_usd": a.settled,
        "open_reserved_usd": a.open_reserved,
        "cap_counted_usd": a.settled + a.open_reserved,
    })
}

fn amounts_json(db: &Db, from: &str, to: &str) -> GenResult<Value> {
    let a = db
        .read(|c| Ok(aggregate_in(c, from, to, None)?))
        .map_err(je)?;
    Ok(agg_json(&a))
}

/// The four amounts of one attempt's ledger row (`None` when it has no row). Read-only.
pub fn attempt_amounts(db: &Db, job_id: &str, attempt: i64) -> GenResult<Option<Value>> {
    db.read(|c| {
        let sql = format!(
            "SELECT j.est_usd, l.est_micro, l.state, l.actual_micro, a.provider_reported_usd, a.provider_reported_source, a.invoice_reconciled_usd
             FROM genright_spend_attempts l
             LEFT JOIN jobs j ON j.job_id = l.job_id
             LEFT JOIN {} a ON a.job_id = l.job_id AND a.attempt = l.attempt
             WHERE l.job_id = ?1 AND l.attempt = ?2",
            amounts_src(c)?
        );
        Ok(c.query_row(&sql, params![job_id, attempt], |r| {
            let state: String = r.get(2)?;
            let actual = match state.as_str() {
                "open" => None,
                "released" => Some(0.0),
                _ => r.get::<_, Option<i64>>(3)?.map(usd),
            };
            Ok(json!({
                "estimated_usd": r.get::<_, Option<f64>>(0)?,
                "reserved_usd": usd(r.get::<_, i64>(1)?),
                "actual_usd": actual,
                "provider_reported_usd": r.get::<_, Option<f64>>(4)?,
                "provider_reported_source": r.get::<_, Option<String>>(5)?,
                "invoice_reconciled_usd": r.get::<_, Option<f64>>(6)?,
            }))
        })
        .optional()?)
    })
    .map_err(je)
}

/// Record what the provider's response stated for one attempt. For an adapter to call once it
/// reads a cost from a response. It touches only `ledger_amounts`: the reservation, the caps and
/// the job total are unchanged, so a provider figure never moves what the caps count. The attempt
/// must have a ledger row. A second call replaces the figure.
pub fn record_provider_reported(
    db: &Db,
    job_id: &str,
    attempt: i64,
    usd: f64,
    source: &str,
) -> GenResult<()> {
    if !usd.is_finite() || usd < 0.0 {
        return Err(GenError::new(
            "invalid_input",
            "provider-reported usd must be a non-negative number",
        ));
    }
    if source.trim().is_empty() {
        return Err(GenError::new(
            "invalid_input",
            "provider-reported source must name where the figure came from",
        ));
    }
    let found = db
        .write(|c| {
            if attempt_row_in(c, job_id, attempt)?.is_none() {
                return Ok(false);
            }
            c.execute_batch(AMOUNTS_SCHEMA)?;
            c.execute(
                "INSERT INTO ledger_amounts(job_id, attempt, provider_reported_usd, provider_reported_source, provider_reported_at)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(job_id, attempt) DO UPDATE SET
                   provider_reported_usd=excluded.provider_reported_usd,
                   provider_reported_source=excluded.provider_reported_source,
                   provider_reported_at=excluded.provider_reported_at",
                params![job_id, attempt, usd, source.trim(), now()],
            )?;
            Ok(true)
        })
        .map_err(je)?;
    if found {
        Ok(())
    } else {
        Err(GenError::new(
            "not_found",
            format!("no ledger row for attempt {attempt} of job {job_id}"),
        ))
    }
}

fn valid_day(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && matches!(s[5..7].parse::<u32>(), Ok(1..=12))
        && matches!(s[8..10].parse::<u32>(), Ok(1..=31))
}

/// `"YYYY-MM"` → the whole UTC month; `{from, to}` → inclusive `YYYY-MM-DD` days.
fn parse_period(v: Option<&Value>) -> GenResult<(String, String)> {
    let bad = || {
        GenError::new(
            "invalid_input",
            "period must be \"YYYY-MM\" or {from: \"YYYY-MM-DD\", to: \"YYYY-MM-DD\"}",
        )
    };
    match v.ok_or_else(bad)? {
        Value::String(m) => {
            let first = format!("{m}-01");
            if m.len() != 7 || !valid_day(&first) {
                return Err(bad());
            }
            Ok((first, format!("{m}-31")))
        }
        Value::Object(o) => {
            let from = o.get("from").and_then(Value::as_str).ok_or_else(bad)?;
            let to = o.get("to").and_then(Value::as_str).ok_or_else(bad)?;
            if !valid_day(from) || !valid_day(to) || from > to {
                return Err(bad());
            }
            Ok((from.to_string(), to.to_string()))
        }
        _ => Err(bad()),
    }
}

fn money(v: &Value, what: &str) -> GenResult<f64> {
    v.as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or_else(|| {
            GenError::new(
                "invalid_input",
                format!("{what} must be a non-negative number"),
            )
        })
}

struct InvoiceLine {
    job_id: Option<String>,
    provider_job_id: Option<String>,
    attempt: Option<i64>,
    usd: f64,
}

fn parse_lines(v: Option<&Value>) -> GenResult<Vec<InvoiceLine>> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(vec![]);
    };
    let arr = v
        .as_array()
        .ok_or_else(|| GenError::new("invalid_input", "lines must be an array"))?;
    arr.iter()
        .enumerate()
        .map(|(i, l)| {
            let text = |k: &str| {
                l.get(k)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            };
            let (job_id, provider_job_id) = (text("job_id"), text("provider_job_id"));
            if job_id.is_some() == provider_job_id.is_some() {
                return Err(GenError::new(
                    "invalid_input",
                    format!("lines[{i}] needs exactly one of job_id or provider_job_id"),
                ));
            }
            let attempt = match l.get("attempt") {
                None | Some(Value::Null) => None,
                Some(a) => Some(a.as_i64().filter(|n| *n >= 1).ok_or_else(|| {
                    GenError::new(
                        "invalid_input",
                        format!("lines[{i}].attempt must be a positive integer"),
                    )
                })?),
            };
            let usd = money(
                l.get("usd").unwrap_or(&Value::Null),
                &format!("lines[{i}].usd"),
            )?;
            Ok(InvoiceLine {
                job_id,
                provider_job_id,
                attempt,
                usd,
            })
        })
        .collect()
}

enum ReconcileOutcome {
    Refused(GenError),
    Done(Value),
}

/// `spend.reconcile` `{period, provider, invoice_total_usd, invoice_ref?, lines?}`.
///
/// Records the user's invoice figures beside the engine's amounts. It NEVER changes a cap, a
/// reservation, a reserved or settled amount or a job total: it writes only `ledger_amounts`
/// (`invoice_reconciled_usd`, per matched row) and `invoice_reconciliations` (the total).
///
/// - `invoice_ref` is the idempotency key. The same ref with the same provider, period and total is
///   a no-op for the invoice record (`already_recorded: true`; any lines are applied idempotently);
///   the same ref with anything different is refused `invoice_ref_conflict`. Without a ref the key
///   is generated from provider and period (`invoice_ref_generated: true`), so a second different
///   total for that provider and period is refused the same way: pass distinct refs for distinct
///   invoices.
/// - `lines` match a ledger row of that provider in the period by `job_id` or `provider_job_id`
///   (optional `attempt` when a job has several rows). Unmatched lines and unmatched rows are
///   reported, never guessed. A row already reconciled by another invoice, or to another amount,
///   is not overwritten (reported as an unmatched line).
/// - With only a total the difference is reported against the period's settled amounts and is not
///   allocated to any row.
pub fn reconcile_invoice(db: &Db, input: &Value) -> GenResult<Value> {
    let provider = input
        .get("provider")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GenError::new("invalid_input", "missing field: provider"))?
        .to_string();
    let (from, to) = parse_period(input.get("period"))?;
    let total = money(
        input.get("invoice_total_usd").unwrap_or(&Value::Null),
        "invoice_total_usd",
    )?;
    let lines = parse_lines(input.get("lines"))?;
    let (invoice_ref, generated) = match input.get("invoice_ref") {
        None | Some(Value::Null) => (format!("auto:{provider}:{from}:{to}"), true),
        Some(v) => match v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => (s.to_string(), false),
            None => {
                return Err(GenError::new(
                    "invalid_input",
                    "invoice_ref must be a non-empty string",
                ))
            }
        },
    };
    let lines_total: f64 = lines.iter().map(|l| l.usd).sum();

    let outcome = db
        .write(|c| {
            c.execute_batch(AMOUNTS_SCHEMA)?;
            let existing: Option<(String, String, String, f64)> = c
                .query_row(
                    "SELECT provider, period_from, period_to, total_usd FROM invoice_reconciliations WHERE invoice_ref=?1",
                    [&invoice_ref],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let already = existing.is_some();
            if let Some((p, f, t, tot)) = &existing {
                if p != &provider || f != &from || t != &to || (tot - total).abs() > 1e-9 {
                    return Ok(ReconcileOutcome::Refused(GenError::new(
                        "invoice_ref_conflict",
                        format!(
                            "invoice_ref {invoice_ref} is already recorded for {p} {f}..{t} with total ${tot:.6}; it is not changed by {provider} {from}..{to} ${total:.6}"
                        ),
                    )));
                }
            } else {
                c.execute(
                    "INSERT INTO invoice_reconciliations(invoice_ref, ref_generated, provider, period_from, period_to, total_usd, recorded_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![invoice_ref, generated, provider, from, to, total, now()],
                )?;
            }

            let mut matched = Vec::new();
            let mut unmatched_lines = Vec::new();
            let mut seen: Vec<(String, i64)> = Vec::new();
            for (i, line) in lines.iter().enumerate() {
                let (col, key) = match (&line.job_id, &line.provider_job_id) {
                    (Some(j), _) => ("l.job_id", j.as_str()),
                    (_, Some(p)) => ("j.provider_job_id", p.as_str()),
                    _ => continue,
                };
                let sql = format!(
                    "SELECT l.job_id, l.attempt, COALESCE(l.actual_micro,0) FROM genright_spend_attempts l JOIN jobs j ON j.job_id=l.job_id
                     WHERE {col}=?1 AND j.provider=?2 AND l.day>=?3 AND l.day<=?4 ORDER BY l.attempt"
                );
                let mut st = c.prepare(&sql)?;
                let rows: Vec<(String, i64, f64)> = st
                    .query_map(params![key, provider, from, to], |r| Ok((r.get(0)?, r.get(1)?, usd(r.get(2)?))))?
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .filter(|(_, a, _)| line.attempt.is_none_or(|want| *a == want))
                    .collect();
                let unmatched = |reason: &str| json!({"index": i, "job_id": line.job_id, "provider_job_id": line.provider_job_id, "attempt": line.attempt, "usd": line.usd, "reason": reason});
                let (job_id, attempt, settled) = match rows.as_slice() {
                    [] => {
                        unmatched_lines.push(unmatched("no_ledger_row_for_provider_and_period"));
                        continue;
                    }
                    [one] => one.clone(),
                    _ => {
                        unmatched_lines.push(unmatched("ambiguous_attempt: the job has several rows in the period; name the attempt"));
                        continue;
                    }
                };
                if seen.contains(&(job_id.clone(), attempt)) {
                    unmatched_lines.push(unmatched("duplicate_line_for_row"));
                    continue;
                }
                let prior: Option<(Option<f64>, Option<String>)> = c
                    .query_row(
                        "SELECT invoice_reconciled_usd, invoice_ref FROM ledger_amounts WHERE job_id=?1 AND attempt=?2",
                        params![job_id, attempt],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((Some(v), r)) = &prior {
                    if r.as_deref() != Some(invoice_ref.as_str()) || (v - line.usd).abs() > 1e-9 {
                        unmatched_lines.push(unmatched("row_already_reconciled_by_another_invoice_or_amount"));
                        continue;
                    }
                }
                c.execute(
                    "INSERT INTO ledger_amounts(job_id, attempt, invoice_reconciled_usd, invoice_ref, invoice_at)
                     VALUES (?1,?2,?3,?4,?5)
                     ON CONFLICT(job_id, attempt) DO UPDATE SET
                       invoice_reconciled_usd=excluded.invoice_reconciled_usd,
                       invoice_ref=excluded.invoice_ref,
                       invoice_at=excluded.invoice_at",
                    params![job_id, attempt, line.usd, invoice_ref, now()],
                )?;
                seen.push((job_id.clone(), attempt));
                matched.push(json!({"job_id": job_id, "attempt": attempt, "invoice_usd": line.usd, "settled_usd": settled, "difference_usd": line.usd - settled}));
            }

            let unmatched_rows: Vec<Value> = if lines.is_empty() {
                vec![]
            } else {
                let mut st = c.prepare(
                    "SELECT l.job_id, l.attempt, l.state, l.actual_micro, l.est_micro FROM genright_spend_attempts l JOIN jobs j ON j.job_id=l.job_id
                     LEFT JOIN ledger_amounts a ON a.job_id=l.job_id AND a.attempt=l.attempt
                     WHERE j.provider=?1 AND l.day>=?2 AND l.day<=?3 AND a.invoice_reconciled_usd IS NULL
                       AND COALESCE(l.actual_micro, l.est_micro) > 0
                     ORDER BY l.day, l.job_id, l.attempt",
                )?;
                let rows = st
                    .query_map(params![provider, from, to], |r| {
                        let state: String = r.get(2)?;
                        let settled = if state == "open" { None } else { r.get::<_, Option<i64>>(3)?.map(usd).or(Some(0.0)) };
                        Ok(json!({"job_id": r.get::<_, String>(0)?, "attempt": r.get::<_, i64>(1)?,
                                  "settled_usd": settled, "reserved_usd": usd(r.get::<_, i64>(4)?)}))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };

            let a = aggregate_in(c, &from, &to, Some(&provider))?;
            let mut out = agg_json(&a);
            let obj = out.as_object_mut().expect("agg_json is an object");
            obj.insert("provider".into(), json!(provider));
            obj.insert("period".into(), json!({"from": from, "to": to}));
            obj.insert("invoice_ref".into(), json!(invoice_ref));
            obj.insert("invoice_ref_generated".into(), json!(generated));
            obj.insert("already_recorded".into(), json!(already));
            obj.insert("invoice_total_usd".into(), json!(total));
            // Invoice total against everything the engine settled for the provider and period (not
            // allocated to any row); settled against the estimate over the rows that have a job;
            // provider-reported against settled over the rows that have a provider figure; invoice
            // lines against settled over the rows that have an invoice amount.
            let settled_vs_estimated = if a.job_rows > 0 { Some(a.job_rows_settled - a.estimated) } else { None };
            let provider_vs_settled = if a.pr_rows > 0 { Some(a.pr_sum - a.pr_settled) } else { None };
            let invoice_rows_vs_settled = if a.inv_rows > 0 { Some(a.inv_sum - a.inv_settled) } else { None };
            let lines_vs_total = if lines.is_empty() { None } else { Some(lines_total - total) };
            obj.insert(
                "differences".into(),
                json!({
                    "invoice_total_minus_settled_usd": total - a.settled,
                    "settled_minus_estimated_usd": settled_vs_estimated,
                    "provider_reported_minus_settled_usd": provider_vs_settled,
                    "invoice_reconciled_minus_settled_usd": invoice_rows_vs_settled,
                    "lines_total_minus_invoice_total_usd": lines_vs_total,
                }),
            );
            if !lines.is_empty() {
                obj.insert(
                    "lines".into(),
                    json!({"given": lines.len(), "matched": matched, "unmatched_lines": unmatched_lines, "unmatched_rows": unmatched_rows,
                           "lines_total_usd": lines_total}),
                );
            }
            Ok(ReconcileOutcome::Done(out))
        })
        .map_err(je)?;
    match outcome {
        ReconcileOutcome::Refused(e) => Err(e),
        ReconcileOutcome::Done(v) => Ok(v),
    }
}

// ------------------------------------------------------------------ caller budget (issue #37)

/// Typed refusal when a reservation would exceed the caller's budget.
pub const BUDGET_EXCEEDED: &str = "caller_budget_exceeded";
/// Typed refusal for a budget that cannot be trusted (non-finite or negative remaining, a half pair).
pub const BUDGET_INVALID: &str = "invalid_caller_budget";
/// `JobError` carries no details, so the details ride after this separator in its message until
/// [`split_refusal`] turns it back into a [`GenError`] with `details`.
const DETAIL_SEP: char = '\u{1f}';

/// A budget a caller (an app, via Pulse) hands the host: the project it belongs to and the USD
/// allowance still unspent. the host never issues or raises one; it only refuses spend beyond it.
/// It sits beside the daily cap and never replaces or loosens it. Absent, behaviour is unchanged.
/// The refusal is the crate's: the budget is an allowance per job (and per run of a process) that
/// the reservation is made against after the caps.
#[derive(Debug, Clone, PartialEq)]
pub struct CallerBudget {
    pub project: String,
    pub remaining_usd: f64,
}

fn invalid_budget(message: impl Into<String>) -> GenError {
    GenError::new(BUDGET_INVALID, message)
}

impl CallerBudget {
    /// Validated: a non-empty project and a finite, non-negative remaining.
    pub fn new(project: &str, remaining_usd: f64) -> GenResult<Self> {
        let project = project.trim();
        if project.is_empty()
            || project.chars().count() > 128
            || project.chars().any(char::is_control)
        {
            return Err(invalid_budget(
                "budget project must be 1 to 128 characters with no control characters",
            ));
        }
        if !remaining_usd.is_finite() || remaining_usd < 0.0 {
            return Err(invalid_budget(format!(
                "budget remaining must be a finite, non-negative number of USD, got {remaining_usd}"
            )));
        }
        Ok(Self {
            project: project.to_string(),
            remaining_usd,
        })
    }

    /// From text (a flag or environment value); `NaN`, `inf` and negatives are refused.
    pub fn parse(project: &str, remaining: &str) -> GenResult<Self> {
        let value = remaining.trim().parse::<f64>().map_err(|_| {
            invalid_budget(format!(
                "budget remaining must be a number of USD, got {remaining:?}"
            ))
        })?;
        Self::new(project, value)
    }

    /// Environment-free core of [`Self::from_flags_and_env`]: a flag wins over its environment value;
    /// an empty value counts as unset. Both halves or neither: a lone project or a lone remaining is
    /// refused rather than read as "no budget".
    pub fn from_parts(
        flag_project: Option<&str>,
        flag_remaining: Option<&str>,
        env_project: Option<&str>,
        env_remaining: Option<&str>,
    ) -> GenResult<Option<Self>> {
        fn set(v: Option<&str>) -> Option<&str> {
            v.filter(|s| !s.trim().is_empty())
        }
        match (set(flag_project).or(set(env_project)), set(flag_remaining).or(set(env_remaining))) {
            (None, None) => Ok(None),
            (Some(p), Some(r)) => Self::parse(p, r).map(Some),
            _ => Err(invalid_budget(
                "a budget needs both a project and a remaining amount (--budget-project and --budget-remaining, or RIGHTKIT_BUDGET_PROJECT and RIGHTKIT_BUDGET_REMAINING)",
            )),
        }
    }

    /// `--budget-project` / `--budget-remaining`, else `RIGHTKIT_BUDGET_PROJECT` /
    /// `RIGHTKIT_BUDGET_REMAINING`.
    pub fn from_flags_and_env(
        flag_project: Option<&str>,
        flag_remaining: Option<&str>,
    ) -> GenResult<Option<Self>> {
        Self::from_parts(
            flag_project,
            flag_remaining,
            std::env::var(BUDGET_PROJECT_ENV).ok().as_deref(),
            std::env::var(BUDGET_REMAINING_ENV).ok().as_deref(),
        )
    }

    /// The `caller_budget` object of a generate input or stored request, if any.
    pub fn from_request(request: &Value) -> GenResult<Option<Self>> {
        match request.get("caller_budget") {
            None | Some(Value::Null) => Ok(None),
            Some(v) => match (
                v.get("project").and_then(Value::as_str),
                v.get("remaining_usd").and_then(Value::as_f64),
            ) {
                (Some(project), Some(remaining)) => Self::new(project, remaining).map(Some),
                _ => Err(invalid_budget(
                    "caller_budget must be {project: string, remaining_usd: finite number >= 0}",
                )),
            },
        }
    }

    /// [`Self::from_request`] over a stored request's JSON text.
    pub fn from_request_json(text: &str) -> GenResult<Option<Self>> {
        match serde_json::from_str::<Value>(text) {
            Ok(v) => Self::from_request(&v),
            Err(_) => Ok(None),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({"project": self.project, "remaining_usd": self.remaining_usd})
    }
}

/// Environment variables that carry a budget to child processes (the detached job runner).
pub const BUDGET_PROJECT_ENV: &str = "RIGHTKIT_BUDGET_PROJECT";
pub const BUDGET_REMAINING_ENV: &str = "RIGHTKIT_BUDGET_REMAINING";

/// The refusal: `needed` (this reservation) and `remaining` (what the budget still allows after the
/// spend already counted against it) with the `project`, as typed `details`.
pub fn budget_error(project: &str, needed: f64, remaining: f64) -> GenError {
    GenError::new(
        BUDGET_EXCEEDED,
        format!("caller budget for project {project} would be exceeded (needed ${needed:.2}, remaining ${remaining:.2}); nothing was reserved"),
    )
    .with_details(json!({"needed": needed, "remaining": remaining, "project": project}))
}

/// Advisory early check (no ledger): would `est` alone fit in a fresh budget? A NaN or negative
/// estimate refuses instead of passing.
pub fn check_budget(budget: &CallerBudget, est: f64) -> GenResult<()> {
    match micros_up(est) {
        Ok(m) if m <= micros_cap(budget.remaining_usd) => Ok(()),
        _ => Err(budget_error(&budget.project, est, budget.remaining_usd)),
    }
}

fn budget_job_error(e: &GenError) -> JobError {
    match &e.details {
        Some(d) => JobError::new(e.code.clone(), format!("{}{DETAIL_SEP}{d}", e.message)),
        None => JobError::new(e.code.clone(), e.message.clone()),
    }
}

/// A refusal from a reservation as the clean `JobError` to store and the `GenError` (with `details`
/// for `caller_budget_exceeded`) to return.
pub fn split_refusal(e: JobError) -> (JobError, GenError) {
    let split = e
        .message
        .split_once(DETAIL_SEP)
        .map(|(m, d)| (m.to_string(), d.to_string()));
    match split {
        Some((message, details)) => {
            let g = GenError::new(e.code.clone(), message.clone());
            let g = match serde_json::from_str::<Value>(&details) {
                Ok(d) => g.with_details(d),
                Err(_) => g,
            };
            (JobError::new(e.code, message), g)
        }
        None => {
            let g = je(JobError::new(e.code.clone(), e.message.clone()));
            (e, g)
        }
    }
}

struct ProcessBudget {
    budget: CallerBudget,
    /// One id per `set_process_budget`: the allowance period of this run.
    run_id: String,
}

static PROCESS_BUDGET: Mutex<Option<ProcessBudget>> = Mutex::new(None);

/// Set (or clear) the budget this whole engine process spends under (`brief run`, `job run`,
/// `generate`). Every reservation made afterwards in this process is refused beyond it: they all
/// reserve against one allowance of the run.
pub fn set_process_budget(budget: Option<CallerBudget>) {
    *PROCESS_BUDGET.lock().unwrap_or_else(|p| p.into_inner()) =
        budget.map(|budget| ProcessBudget {
            budget,
            run_id: uuid::Uuid::new_v4().simple().to_string(),
        });
}

pub(crate) fn process_snapshot() -> Option<(CallerBudget, String)> {
    PROCESS_BUDGET
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|p| (p.budget.clone(), p.run_id.clone()))
}

/// `(reserved, actual)` micro-USD over one job's attempts: reserved counts attempts not released
/// (open, or settled above zero); actual is the settled amount.
fn job_totals_in(conn: &Connection, job_id: &str) -> rightkit_db::rusqlite::Result<(i64, i64)> {
    conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN state != 'released' THEN est_micro ELSE 0 END),0), COALESCE(SUM(actual_micro),0)
         FROM genright_spend_attempts WHERE job_id=?1",
        [job_id],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )
}

/// The run allowance's `(reserved, actual)` micro-USD from the crate's report: committed
/// estimates plus open ones, and committed actuals.
fn run_totals(l: &Ledger, b: &CallerBudget, run_id: &str) -> Result<(i64, i64), SpendError> {
    match l.report(&scope_key(), &run_budget_period(b, run_id)) {
        Ok(r) => Ok((r.forecast_micro, r.measured_micro)),
        Err(SpendError::AllowanceMissing { .. }) => Ok((0, 0)),
        Err(e) => Err(e),
    }
}

/// The `spend` object of a job record: `{project, reserved_usd, actual_usd}` for this job and
/// `run_reserved_usd` / `run_actual_usd` cumulative over everything this process reserved under its
/// budget (this job alone when there is no process budget). `None` when neither the job's stored
/// `request_budget` nor this process carries a budget.
pub fn spend_report(
    db: &Db,
    job_id: &str,
    request_budget: Option<&CallerBudget>,
) -> GenResult<Option<Value>> {
    let process = process_snapshot();
    let project = match (request_budget, process.as_ref()) {
        (Some(b), _) => b.project.clone(),
        (None, Some((b, _))) => b.project.clone(),
        (None, None) => return Ok(None),
    };
    let h = db.spend().clone();
    db.read(|c| {
        let (reserved, actual) = job_totals_in(c, job_id)?;
        let (run_reserved, run_actual) = match &process {
            Some((b, run)) => run_totals(&lock(&h), b, run).map_err(spend_jerr)?,
            None => (reserved, actual),
        };
        Ok(Some(json!({
            "project": project,
            "reserved_usd": usd(reserved),
            "actual_usd": usd(actual),
            "run_reserved_usd": usd(run_reserved),
            "run_actual_usd": usd(run_actual),
        })))
    })
    .map_err(je)
}

/// Cumulative `{project, reserved_usd, actual_usd}` for the whole process run (`brief run`'s
/// result); `None` without a process budget.
pub fn process_spend(db: &Db) -> GenResult<Option<Value>> {
    let Some((budget, run)) = process_snapshot() else {
        return Ok(None);
    };
    let h = db.spend().clone();
    db.read(|_| {
        let (reserved, actual) = run_totals(&lock(&h), &budget, &run).map_err(spend_jerr)?;
        Ok(Some(json!({"project": budget.project, "reserved_usd": usd(reserved), "actual_usd": usd(actual)})))
    })
    .map_err(je)
}
