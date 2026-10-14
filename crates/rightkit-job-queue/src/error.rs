use std::fmt;
#[cfg(feature = "store")]
use std::path::Path;

#[cfg(feature = "store")]
use rightkit_db::{StorageProblem, UnreadableCause};

/// Where a [`JobError`] arose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorOrigin {
    /// The engine answered with it (its handler, or its dispatcher).
    #[default]
    Engine,
    /// The shell raised it on its own side of the connection: `down`,
    /// `not_ready`, `timeout`, a dead connection, a rejected hello.
    Supervisor,
}

/// Typed engine error: `code` is the stable wire identifier. `retryable`
/// errors pause a running job (it stays recoverable) instead of failing it.
///
/// Equality compares `code`, `message`, `retryable` and `origin`; the database classification
/// behind [`JobError::open_failure`] is derived from the same failure and does not take part.
#[derive(Debug, Clone)]
pub struct JobError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub origin: ErrorOrigin,
    /// What kind of database failure this error came from, when it came from one.
    #[cfg(feature = "store")]
    db_failure: Option<OpenFailure>,
}

impl PartialEq for JobError {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
            && self.message == other.message
            && self.retryable == other.retryable
            && self.origin == other.origin
    }
}

impl Eq for JobError {}

#[cfg(feature = "store")]
/// What kind of database failure an open (or any other database call) ran into, so a consumer
/// can react without matching message text. Read with [`JobError::open_failure`]; built from
/// the underlying `rightkit_job_queue::db::DbError`. The string `code` of the error (`db`,
/// `db_unreadable`) is unchanged and stays the wire identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenFailure {
    /// The file is not a usable SQLite database: SQLite reports it as not-a-database or
    /// corrupt, or an open-time integrity check failed. `JobStore::open_recovering` and
    /// `open_recovering_unreadable` can set such a file aside when their `RecoveryPolicy` says so.
    Corrupt,
    /// The file could not be read: permission denied or an I/O read failure. The cause is
    /// only ever `UnreadableCause::PermissionDenied` or `UnreadableCause::IoReadFailure`; a
    /// storage problem is [`OpenFailure::StorageUnavailable`].
    Unreadable(UnreadableCause),
    /// The storage is unavailable and the file may be healthy: disk full, read-only filesystem,
    /// a dropped share, a lock held by another process, a missing file. Retrying later, or
    /// asking the user to free space, can fix it; setting the file aside never does.
    StorageUnavailable(StorageProblem),
    /// A database failure that is none of the above (a bad option, a read pool that was
    /// closed, a migration refusal, ...).
    Other,
}

#[cfg(feature = "store")]
impl OpenFailure {
    fn from_cause(cause: UnreadableCause) -> Self {
        match cause {
            UnreadableCause::StorageUnavailable(problem) => Self::StorageUnavailable(problem),
            other => Self::Unreadable(other),
        }
    }

    /// Classify a SQLite error. `path` is the database file when the caller knows it: it is
    /// probed (opened for reading) only to tell a permission failure from the other causes of
    /// `SQLITE_CANTOPEN`.
    fn from_sqlite(e: &rightkit_db::rusqlite::Error, path: Option<&Path>) -> Self {
        use rightkit_db::rusqlite::ErrorCode as C;
        let rightkit_db::rusqlite::Error::SqliteFailure(f, _) = e else {
            return Self::Other;
        };
        let busy = Self::StorageUnavailable(StorageProblem::LockedOrBusy);
        match f.code {
            C::NotADatabase | C::DatabaseCorrupt => Self::Corrupt,
            C::PermissionDenied => Self::Unreadable(UnreadableCause::PermissionDenied),
            C::DiskFull => Self::StorageUnavailable(StorageProblem::DiskFull),
            C::ReadOnly => Self::StorageUnavailable(StorageProblem::ReadOnlyFilesystem),
            C::DatabaseBusy | C::DatabaseLocked | C::FileLockingProtocolFailed => busy,
            C::SystemIoFailure => match f.extended_code {
                // SQLITE_IOERR_READ, SQLITE_IOERR_SHORT_READ
                266 | 522 => Self::Unreadable(UnreadableCause::IoReadFailure),
                // SQLITE_IOERR_UNLOCK, _RDLOCK, _BLOCKED, _CHECKRESERVEDLOCK, _LOCK, _SHMLOCK
                2058 | 2314 | 2826 | 3594 | 3850 | 5130 => busy,
                // SQLITE_IOERR_DATA, SQLITE_IOERR_CORRUPTFS: the file system is damaged
                8202 | 8458 => Self::Other,
                _ => Self::StorageUnavailable(StorageProblem::Other),
            },
            C::CannotOpen => match path.map(std::fs::File::open) {
                Some(Err(io)) if io.kind() == std::io::ErrorKind::PermissionDenied => {
                    Self::Unreadable(UnreadableCause::PermissionDenied)
                }
                Some(Err(io)) if io.kind() == std::io::ErrorKind::NotFound => {
                    Self::StorageUnavailable(StorageProblem::FileMissing)
                }
                _ => Self::StorageUnavailable(StorageProblem::Other),
            },
            C::OutOfMemory => Self::StorageUnavailable(StorageProblem::Other),
            _ => Self::Other,
        }
    }

    fn from_db(e: &rightkit_db::DbError, path: Option<&Path>) -> Self {
        match e {
            rightkit_db::DbError::Unreadable { cause, .. } => Self::from_cause(*cause),
            rightkit_db::DbError::Integrity(_) => Self::Corrupt,
            rightkit_db::DbError::Sqlite(s) => Self::from_sqlite(s, path),
            _ => Self::Other,
        }
    }
}

/// Stable wire code for [`JobError::handoff`].
pub const HOST_HANDOFF: &str = "host_handoff";
/// Stable wire code for [`JobError::stalled`].
pub const STALLED: &str = "stalled";
/// Stable wire code for [`JobError::stale_attempt`].
pub const STALE_ATTEMPT: &str = "stale_attempt";
/// Stable wire code of the terminal error a `Recovery::Never` job gets when its runner is lost.
pub const INTERRUPTED: &str = "interrupted";
/// Stable wire code of the cancel error a single-flight submit leaves on the job it replaced.
pub const SUPERSEDED: &str = "superseded";
/// Stable wire code for [`JobError::contract_mismatch`].
pub const CONTRACT_VERSION_MISMATCH: &str = "contract_version_mismatch";
/// Stable code: a request's write to the engine stalled past its deadline, so the engine may have
/// received none, part, or all of it. The engine was killed (bounded) and will be restarted by its
/// policy. Not retryable on its own: resubmit only an operation that is safe to run twice.
pub const DELIVERY_UNKNOWN: &str = "delivery_unknown";
/// Stable code: a paging [`crate::Cursor`] is malformed, truncated or from an unknown version.
pub const INVALID_CURSOR: &str = "invalid_cursor";
/// Stable code: a paging [`crate::Cursor`] was issued for a different filter or sort order than
/// the listing it was passed to.
pub const CURSOR_MISMATCH: &str = "cursor_mismatch";
/// Stable code: `JobStore::import_terminal*` found a job with the same id (or request id) and
/// either was not asked to accept an identical record or the stored record differs.
pub const JOB_EXISTS: &str = "job_exists";
/// Stable code: the database file could not be read and the store was not allowed to set it
/// aside (`rightkit_db::DbError::Unreadable`). The message names the cause.
pub const DB_UNREADABLE: &str = "db_unreadable";
/// Stable code: a handler reported that it owns a job's outcome (`HandlerOutcome::Settled` or
/// `HandlerOutcome::Pending`) but the job's row says otherwise. A job that returned `Settled` is
/// not terminal, or a `Recovery::Never` job returned `Pending` in a status the runner would
/// interrupt. The runner fails the job with this error rather than leave it with no owner.
pub const HANDLER_UNSETTLED: &str = "handler_unsettled";

pub type JobResult<T> = Result<T, JobError>;

impl JobError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            origin: ErrorOrigin::Engine,
            #[cfg(feature = "store")]
            db_failure: None,
        }
    }
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
            origin: ErrorOrigin::Engine,
            #[cfg(feature = "store")]
            db_failure: None,
        }
    }
    /// What kind of database failure this error came from, or `None` for an error that did not
    /// come from the database (a bad configuration, a handler error, a refused input). Set on the
    /// errors of `JobStore::open`, `open_with`, `open_with_config`, `open_recovering` and
    /// `open_recovering_unreadable`, and on any other error converted from a `DbError` or a
    /// SQLite error. It is derived from the underlying error, never from the message, so a
    /// consumer can choose between "set the file aside", "ask the user to free space" and "try
    /// again" without parsing text. The `code` of the error is unchanged.
    #[cfg(feature = "store")]
    pub fn open_failure(&self) -> Option<OpenFailure> {
        self.db_failure
    }
    /// An open-time database error: classified like any `DbError`, with `path` (the database
    /// file) probed to tell a permission failure from the other causes of "cannot open".
    #[cfg(feature = "store")]
    pub(crate) fn from_open(e: rightkit_db::DbError, path: &Path) -> Self {
        let failure = OpenFailure::from_db(&e, Some(path));
        let mut err = Self::from(e);
        err.db_failure = Some(failure);
        err
    }
    /// A handler returned `Settled` but job `job_id` is still `status`.
    #[cfg(feature = "store")]
    pub(crate) fn handler_unsettled(job_id: &str, status: &str) -> Self {
        Self::new(
            HANDLER_UNSETTLED,
            format!(
                "the handler of job {job_id} reported the outcome as settled, but the job is still {status}; it was failed instead of left with no owner"
            ),
        )
    }
    /// A handler returned `Pending` for a `Recovery::Never` job that is not held (`status`): the
    /// next recovery scan would fail it as `interrupted` and lose the pending outcome.
    #[cfg(feature = "store")]
    pub(crate) fn handler_pending_unowned(job_id: &str, status: &str) -> Self {
        Self::new(
            HANDLER_UNSETTLED,
            format!(
                "the handler of job {job_id} reported the outcome as pending, but the job's recovery class is never and its status {status} is not held, so recovery would fail it as interrupted; it was failed instead (park such a job with hold_submission)"
            ),
        )
    }
    /// An error the shell raised itself, not relayed from an engine reply.
    pub fn supervisor(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message).with_origin(ErrorOrigin::Supervisor)
    }
    /// As [`Self::supervisor`], retryable.
    pub fn supervisor_retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::retryable(code, message).with_origin(ErrorOrigin::Supervisor)
    }
    /// Records where the error arose.
    pub fn with_origin(mut self, origin: ErrorOrigin) -> Self {
        self.origin = origin;
        self
    }
    /// The host is handing the service to a higher-ranked host. Retryable: the
    /// job pauses (still recoverable) without spending an attempt, and the
    /// client resubmits against the new host.
    pub fn handoff() -> Self {
        Self::retryable(
            HOST_HANDOFF,
            "host is handing the service to a higher-ranked host; retry",
        )
    }
    /// No progress for the stall timeout. Retryable, and it spends an attempt.
    pub fn stalled(idle_secs: f64) -> Self {
        Self::retryable(STALLED, format!("no progress for {idle_secs:.1}s"))
    }
    /// A write from an attempt that no longer owns the job (its generation was
    /// superseded by a newer run, e.g. after a stall watchdog fenced it). Not
    /// retryable: the stale attempt must stop, and its write has no effect.
    pub fn stale_attempt(job_id: &str) -> Self {
        Self::new(
            STALE_ATTEMPT,
            format!("job {job_id}: write from a superseded attempt was rejected"),
        )
    }
    /// The engine's hello advertises a contract version the shell does not
    /// require. Not retryable: the engine binary has to change.
    pub fn contract_mismatch(advertised: impl fmt::Display, required: &str) -> Self {
        Self::supervisor(
            CONTRACT_VERSION_MISMATCH,
            format!("engine speaks contract_version {advertised}; this shell requires {required}"),
        )
    }
    /// A `Recovery::Never` job began an attempt and lost its runner (crash or lost lock). It is
    /// failed with this error and never handed to a handler again.
    pub fn interrupted(job_id: &str) -> Self {
        Self::new(
            INTERRUPTED,
            format!(
                "job {job_id} was interrupted after it started and its recovery class is never; it was not run again"
            ),
        )
    }
    /// A `Recovery::Never` job's handler ended with a retryable error. The handler may already
    /// have had effects, so the job fails instead of pausing for a later re-run.
    pub fn interrupted_by(job_id: &str, cause: &JobError) -> Self {
        Self::new(
            INTERRUPTED,
            format!(
                "job {job_id} stopped after it started ({}: {}) and its recovery class is never; it was not run again",
                cause.code, cause.message
            ),
        )
    }
    /// The job was replaced by a newer single-flight submit (`successor`).
    pub fn superseded(successor: &str) -> Self {
        Self::new(
            SUPERSEDED,
            format!("superseded by job {successor} (single-flight submit)"),
        )
    }
    pub fn cancelled() -> Self {
        Self::new("cancelled", "job was cancelled")
    }
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self::new("bad_input", message)
    }
    pub fn to_json(&self) -> serde_json::Value {
        self.to_json_with(true)
    }
    /// Like [`JobError::to_json`], omitting `retryable` when `include_retryable`
    /// is false (an app whose wire `error` is `{code, message}`).
    pub fn to_json_with(&self, include_retryable: bool) -> serde_json::Value {
        if include_retryable {
            serde_json::json!({"code": self.code, "message": self.message, "retryable": self.retryable})
        } else {
            serde_json::json!({"code": self.code, "message": self.message})
        }
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for JobError {}

#[cfg(feature = "store")]
impl From<rightkit_db::rusqlite::Error> for JobError {
    fn from(e: rightkit_db::rusqlite::Error) -> Self {
        let mut err = Self::new("db", format!("database error: {e}"));
        err.db_failure = Some(OpenFailure::from_sqlite(&e, None));
        err
    }
}
#[cfg(feature = "store")]
impl From<rightkit_db::DbError> for JobError {
    fn from(e: rightkit_db::DbError) -> Self {
        // `Unreadable` only comes from the opt-in unreadable-database open
        // (`JobStore::open_recovering_unreadable`); every other open keeps the plain `db` code.
        let code = match &e {
            rightkit_db::DbError::Unreadable { .. } => DB_UNREADABLE,
            _ => "db",
        };
        let mut err = Self::new(code, format!("database error: {e}"));
        err.db_failure = Some(OpenFailure::from_db(&e, None));
        err
    }
}
impl From<std::io::Error> for JobError {
    fn from(e: std::io::Error) -> Self {
        Self::new("io", format!("io error: {e}"))
    }
}

#[cfg(all(test, feature = "store"))]
mod tests {
    use super::*;
    use rightkit_db::{DbError, UnreadableCause};

    #[test]
    fn an_unreadable_database_keeps_a_typed_code() {
        let unreadable = DbError::Unreadable {
            path: std::path::PathBuf::from("/data/jobs.sqlite"),
            cause: UnreadableCause::PermissionDenied,
            consecutive_failures: 2,
            quarantine_after: Some(3),
            source: Box::new(DbError::InvalidDatabasePath),
        };
        let e = JobError::from(unreadable);
        assert_eq!(e.code, DB_UNREADABLE);
        assert!(e.message.contains("permission denied"), "{}", e.message);
        assert!(!e.retryable);
        // Every other database error keeps the plain `db` code it always had.
        assert_eq!(JobError::from(DbError::ReadPoolClosed).code, "db");
    }

    fn sqlite(code: i32) -> DbError {
        DbError::Sqlite(rightkit_db::rusqlite::Error::SqliteFailure(
            rightkit_db::rusqlite::ffi::Error::new(code),
            None,
        ))
    }

    #[test]
    fn database_errors_carry_a_typed_classification() {
        // SQLITE_CORRUPT (11) and SQLITE_NOTADB (26): the file is not a usable database.
        assert_eq!(
            JobError::from(sqlite(11)).open_failure(),
            Some(OpenFailure::Corrupt)
        );
        assert_eq!(
            JobError::from(sqlite(26)).open_failure(),
            Some(OpenFailure::Corrupt)
        );
        assert_eq!(
            JobError::from(DbError::Integrity("page 3".into())).open_failure(),
            Some(OpenFailure::Corrupt)
        );
        // SQLITE_BUSY (5), SQLITE_FULL (13), SQLITE_READONLY (8): the storage, not the file.
        assert_eq!(
            JobError::from(sqlite(5)).open_failure(),
            Some(OpenFailure::StorageUnavailable(
                StorageProblem::LockedOrBusy
            ))
        );
        assert_eq!(
            JobError::from(sqlite(13)).open_failure(),
            Some(OpenFailure::StorageUnavailable(StorageProblem::DiskFull))
        );
        assert_eq!(
            JobError::from(sqlite(8)).open_failure(),
            Some(OpenFailure::StorageUnavailable(
                StorageProblem::ReadOnlyFilesystem
            ))
        );
        // SQLITE_PERM (3): the file cannot be read by this process.
        assert_eq!(
            JobError::from(sqlite(3)).open_failure(),
            Some(OpenFailure::Unreadable(UnreadableCause::PermissionDenied))
        );
        assert_eq!(
            JobError::from(DbError::ReadPoolClosed).open_failure(),
            Some(OpenFailure::Other)
        );
    }

    #[test]
    fn an_unreadable_open_error_splits_read_failures_from_storage_problems() {
        fn unreadable(cause: UnreadableCause) -> DbError {
            DbError::Unreadable {
                path: std::path::PathBuf::from("/data/jobs.sqlite"),
                cause,
                consecutive_failures: 0,
                quarantine_after: None,
                source: Box::new(DbError::InvalidDatabasePath),
            }
        }
        assert_eq!(
            JobError::from(unreadable(UnreadableCause::IoReadFailure)).open_failure(),
            Some(OpenFailure::Unreadable(UnreadableCause::IoReadFailure))
        );
        assert_eq!(
            JobError::from(unreadable(UnreadableCause::StorageUnavailable(
                StorageProblem::DeviceNotReady
            )))
            .open_failure(),
            Some(OpenFailure::StorageUnavailable(
                StorageProblem::DeviceNotReady
            ))
        );
    }

    #[test]
    fn errors_that_never_touched_the_database_have_no_classification() {
        assert_eq!(JobError::new("config", "bad").open_failure(), None);
        assert_eq!(JobError::bad_input("x").open_failure(), None);
        // The classification takes no part in equality.
        let a = JobError::from(sqlite(5));
        let mut b = a.clone();
        b.db_failure = None;
        assert_eq!(a, b);
    }
}
