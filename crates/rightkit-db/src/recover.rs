//! Open-time recovery: key rotation interrupted before the credential store was updated, and
//! corrupt databases.
//!
//! - **Key candidates**: the caller lists keys in preference order (current first, then a
//!   pending/previous key from a staged rotation). The first that decrypts wins and its index
//!   is reported so the app can promote it in its credential store (MailRight `open_at`).
//! - **Quarantine**: a database that is not a database (garbage, truncated header) is renamed
//!   aside with its sidecars and a fresh file is created, instead of crash-looping on every
//!   start. A *wrong key* is indistinguishable from corruption to SQLCipher, so a keyed
//!   database is only ever quarantined when the policy says so explicitly.

use crate::{DbError, DbOptions, Result};
use rusqlite::{Connection, ErrorCode};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecoveryPolicy {
    /// Quarantine-and-recreate an unreadable *plain* database.
    pub quarantine_corrupt: bool,
    /// Also allow it for a keyed database (destroys access to data a wrong key could have read).
    pub quarantine_keyed: bool,
    /// Suffix format of the quarantined file. Default `.corrupt-<unix-ms>`.
    pub quarantine_stamp: QuarantineStamp,
    /// What happens to the `-wal`, `-shm` and `-journal` sidecars. Default renames them alongside
    /// the quarantined file.
    pub sidecars: SidecarPolicy,
}

/// Suffix format of a quarantined database, appended to its file name.
#[derive(Clone, Copy, Debug, Default)]
pub enum QuarantineStamp {
    /// `.corrupt-<unix-ms>` (default).
    #[default]
    Millis,
    /// `.corrupt-<unix-secs>`.
    Seconds,
    /// The formatter returns the whole suffix, for example `.bad-<n>`. The result must be
    /// non-empty and contain no path separator or NUL, or quarantine fails with
    /// [`DbError::InvalidQuarantineSuffix`]. A name already taken gets `-1`, `-2`, ... appended,
    /// as for [`QuarantineStamp::Seconds`], so an earlier quarantine is never overwritten.
    Custom(fn(SystemTime) -> String),
}

// Function pointers are compared by address with `fn_addr_eq`, which is the sanctioned form:
// a derived `==` on `fn` pointers triggers `unpredictable_function_pointer_comparisons`.
// Equal addresses compare equal, so `Eq` holds.
impl PartialEq for QuarantineStamp {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Millis, Self::Millis) | (Self::Seconds, Self::Seconds) => true,
            (Self::Custom(a), Self::Custom(b)) => std::ptr::fn_addr_eq(*a, *b),
            _ => false,
        }
    }
}

impl Eq for QuarantineStamp {}

impl QuarantineStamp {
    fn suffix(self, now: SystemTime) -> String {
        let since_epoch = now.duration_since(UNIX_EPOCH).unwrap_or_default();
        match self {
            Self::Millis => format!(".corrupt-{}", since_epoch.as_millis()),
            Self::Seconds => format!(".corrupt-{}", since_epoch.as_secs()),
            Self::Custom(format) => format(now),
        }
    }
}

/// What happens to the `-wal`, `-shm` and `-journal` sidecars of a quarantined database.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SidecarPolicy {
    /// Move each sidecar beside the quarantined file, under the same suffix. Nothing is deleted
    /// (default).
    #[default]
    RenameSidecars,
    /// Delete the sidecars. Their bytes are not kept for diagnosis, and no stale WAL is left at
    /// the original path.
    DeleteSidecars,
}

#[derive(Debug)]
pub struct OpenOutcome {
    pub conn: Connection,
    /// Index into the candidate list that opened the database (0 for a plain open).
    pub key_index: usize,
    /// Where the unreadable database was moved, when recovery quarantined it.
    pub quarantined: Option<PathBuf>,
}

impl DbOptions {
    /// Try each passphrase in order; the first that decrypts opens the writer.
    pub fn open_writer_with_key_candidates(
        &self,
        candidates: &[Zeroizing<String>],
    ) -> Result<(Connection, usize)> {
        let mut first_error = None;
        for (index, candidate) in candidates.iter().enumerate() {
            let mut attempt = self.clone();
            attempt.key = Some(candidate.clone());
            attempt.raw_key = None;
            match attempt.open_writer() {
                Ok(conn) => return Ok((conn, index)),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or(DbError::NoKeyOpened))
    }

    /// Open the writer; if the file is unreadable and the policy allows it, quarantine it and
    /// open a fresh database at the same path.
    pub fn open_writer_recovering(&self, policy: RecoveryPolicy) -> Result<OpenOutcome> {
        match self.open_writer() {
            Ok(conn) => Ok(OpenOutcome {
                conn,
                key_index: 0,
                quarantined: None,
            }),
            Err(error) if is_unreadable(&error) && allowed(self, policy) => {
                let moved =
                    quarantine_database_with(&self.path, policy.quarantine_stamp, policy.sidecars)?;
                let conn = self.open_writer()?;
                Ok(OpenOutcome {
                    conn,
                    key_index: 0,
                    quarantined: Some(moved),
                })
            }
            Err(error) => Err(error),
        }
    }
}

pub(crate) fn allowed(options: &DbOptions, policy: RecoveryPolicy) -> bool {
    if options.is_keyed() {
        policy.quarantine_keyed
    } else {
        policy.quarantine_corrupt
    }
}

/// True when the file is not a usable SQLite database: SQLite reports it as not-a-database or
/// corrupt, or an open-time integrity check failed ([`DbError::Integrity`], from `Quick` or `Full`).
/// Busy, locked, permission and other I/O failures are not unreadable. They are transient or
/// environmental, and quarantining on them would move a healthy file aside.
pub(crate) fn is_unreadable(error: &DbError) -> bool {
    match error {
        DbError::Integrity(_) => true,
        DbError::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) => matches!(
            failure.code,
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt
        ),
        _ => false,
    }
}

/// Move `db` and its `-wal` / `-shm` / `-journal` sidecars to `<db>.corrupt-<unix-ms>[...]`.
/// Returns the new main-file path. Nothing is deleted.
pub fn quarantine_database(db: &Path) -> Result<PathBuf> {
    quarantine_database_with(db, QuarantineStamp::Millis, SidecarPolicy::RenameSidecars)
}

/// [`quarantine_database`] with an explicit suffix format and sidecar policy. Refuses to overwrite
/// an existing file at the destination. Sidecar failures are errors, not ignored: a `-wal` left
/// at the original path would be replayed into the replacement database.
pub fn quarantine_database_with(
    db: &Path,
    stamp: QuarantineStamp,
    sidecars: SidecarPolicy,
) -> Result<PathBuf> {
    quarantine_database_opts(
        db,
        &QuarantineOptions {
            name: stamp,
            sidecars,
            ..QuarantineOptions::default()
        },
    )
}

/// How a quarantined file leaves its original path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MoveMode {
    /// `rename(2)`: one step, within the directory (default).
    #[default]
    Rename,
    /// Hard-link the file to its destination, then remove the original, so the destination
    /// exists before the original name goes away. Fails, with the original still in place, where
    /// hard links are not supported.
    HardLinkThenRemove,
}

/// Options for [`quarantine_database_opts`]. `Default` matches [`quarantine_database`].
#[derive(Clone, Debug, Default)]
pub struct QuarantineOptions {
    /// Suffix format appended to the database file name.
    pub name: QuarantineStamp,
    /// What happens to the `-wal`, `-shm` and `-journal` sidecars.
    pub sidecars: SidecarPolicy,
    /// How the main file and the sidecars leave their original names.
    pub move_mode: MoveMode,
    /// Recovery marker written atomically (owner-only) before any file moves. It is JSON:
    /// `{"schema", "original", "quarantined", "unixMs"}`. It stays in place after success; the app
    /// removes it when recovery is complete.
    pub marker: Option<PathBuf>,
}

/// Attempts for a unique quarantine name under [`QuarantineStamp::Seconds`]: the base name, then
/// `-1`, `-2`, ... up to `-(QUARANTINE_NAME_ATTEMPTS - 1)`.
const QUARANTINE_NAME_ATTEMPTS: u32 = 100;

/// [`quarantine_database`] with full [`QuarantineOptions`]. [`QuarantineStamp::Millis`] refuses an
/// existing destination, and sidecar failures are errors for every stamp.
///
/// [`QuarantineStamp::Seconds`] and [`QuarantineStamp::Custom`] names are unique: a second
/// quarantine that takes the same name gets `-1`, `-2`, and so on (up to 99 retries), so a
/// quarantine within the same second takes `<db>.corrupt-<secs>-1`. A name is claimed with
/// create-new semantics (`rename_exclusive`, or `link` for [`MoveMode::HardLinkThenRemove`]), never
/// by check-then-rename, so a concurrent quarantine cannot be overwritten.
pub fn quarantine_database_opts(db: &Path, options: &QuarantineOptions) -> Result<PathBuf> {
    quarantine_steps(db, options).map_err(|failure| failure.error)
}

/// File name suffix of the journal [`quarantine_database_journaled`] keeps while a quarantine is
/// unfinished: `<db>.quarantine-journal`.
pub(crate) const QUARANTINE_JOURNAL_SUFFIX: &str = ".quarantine-journal";

/// A failed quarantine. `stranded_at` is `Some(dest)` only when part of the store moved and the
/// move could not be undone: the main file is then at `dest`, with whatever sidecars moved.
pub(crate) struct QuarantineFailure {
    pub(crate) error: DbError,
    pub(crate) stranded_at: Option<PathBuf>,
}

impl QuarantineFailure {
    fn untouched(error: DbError) -> Self {
        Self {
            error,
            stranded_at: None,
        }
    }
}

/// `<db>.quarantine-journal` when an earlier quarantine left it, which means the store is split
/// between the original path and a quarantine name.
pub(crate) fn unfinished_quarantine(db: &Path) -> Option<PathBuf> {
    let journal = with_suffix(db, QUARANTINE_JOURNAL_SUFFIX);
    std::fs::symlink_metadata(&journal).ok().map(|_| journal)
}

/// Quarantine with the steps journaled: the recovery marker `<db>.quarantine-journal` is written
/// before any file moves, removed once the quarantine finished or was fully undone, and kept when
/// a partial move could not be undone.
pub(crate) fn quarantine_database_journaled(
    db: &Path,
    stamp: QuarantineStamp,
    sidecars: SidecarPolicy,
) -> std::result::Result<PathBuf, QuarantineFailure> {
    let journal = with_suffix(db, QUARANTINE_JOURNAL_SUFFIX);
    let result = quarantine_steps(
        db,
        &QuarantineOptions {
            name: stamp,
            sidecars,
            marker: Some(journal.clone()),
            ..QuarantineOptions::default()
        },
    );
    match &result {
        Err(failure) if failure.stranded_at.is_some() => {}
        _ => {
            let _ = std::fs::remove_file(&journal);
        }
    }
    result
}

/// One move of a quarantine, kept so it can be undone.
struct Moved {
    from: PathBuf,
    to: PathBuf,
}

fn quarantine_steps(
    db: &Path,
    options: &QuarantineOptions,
) -> std::result::Result<PathBuf, QuarantineFailure> {
    let name = db
        .file_name()
        .ok_or(DbError::InvalidDatabasePath)
        .map_err(QuarantineFailure::untouched)?
        .to_string_lossy()
        .into_owned();
    let suffix = options.name.suffix(SystemTime::now());
    if suffix.is_empty() || suffix.contains(['/', '\\', '\0']) {
        return Err(QuarantineFailure::untouched(
            DbError::InvalidQuarantineSuffix(suffix),
        ));
    }
    let attempts = match options.name {
        QuarantineStamp::Seconds | QuarantineStamp::Custom(_) => QUARANTINE_NAME_ATTEMPTS,
        QuarantineStamp::Millis => 1,
    };
    let mut last = None;
    for attempt in 0..attempts {
        let dest = quarantine_candidate(db, &name, &suffix, attempt);
        if quarantine_name_taken(&dest, options.sidecars) {
            last = Some(dest);
            continue;
        }
        if let Some(marker) = &options.marker {
            write_marker(marker, db, &dest).map_err(QuarantineFailure::untouched)?;
        }
        match move_file(db, &dest, options.move_mode) {
            Ok(()) => {}
            // Another quarantine claimed the name after the check: try the next one.
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists && attempt + 1 < attempts =>
            {
                last = Some(dest);
                continue;
            }
            Err(error) => {
                // The main file did not move, so nothing did.
                return Err(QuarantineFailure::untouched(DbError::Fs(
                    rightkit_fs::FsError::io(db, error),
                )));
            }
        }
        let mut done = vec![Moved {
            from: db.to_path_buf(),
            to: dest.clone(),
        }];
        let mut deleted = false;
        for sidecar in ["-wal", "-shm", "-journal"] {
            let from = with_suffix(db, sidecar);
            let step = match options.sidecars {
                SidecarPolicy::RenameSidecars => {
                    let to = with_suffix(&dest, sidecar);
                    match move_if_present(&from, &to, options.move_mode) {
                        Ok(true) => {
                            done.push(Moved { from, to });
                            Ok(())
                        }
                        Ok(false) => Ok(()),
                        Err(error) => Err(error),
                    }
                }
                SidecarPolicy::DeleteSidecars => match remove_if_present(&from) {
                    Ok(removed) => {
                        deleted |= removed;
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
            };
            if let Err(error) = step {
                return Err(undo_quarantine(db, &dest, done, deleted, error, options));
            }
        }
        return Ok(dest);
    }
    let dest = last.unwrap_or_else(|| quarantine_candidate(db, &name, &suffix, 0));
    Err(QuarantineFailure::untouched(DbError::Fs(
        rightkit_fs::FsError::io(
            &dest,
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "quarantine destination already exists",
            ),
        ),
    )))
}

/// A sidecar step failed after the main file moved. Move everything back in reverse order. A
/// deleted sidecar cannot come back, so after one the move is not undone. If any undo step
/// fails, the journal stays and the failure names where the main file is.
fn undo_quarantine(
    db: &Path,
    dest: &Path,
    done: Vec<Moved>,
    deleted: bool,
    error: DbError,
    options: &QuarantineOptions,
) -> QuarantineFailure {
    let stranded = |reason: String| QuarantineFailure {
        error: DbError::Fs(rightkit_fs::FsError::io(
            dest,
            std::io::Error::other(format!(
                "quarantine incomplete: {error}; {reason}; the store is split between {} and {}",
                db.display(),
                dest.display()
            )),
        )),
        stranded_at: Some(dest.to_path_buf()),
    };
    if deleted {
        return stranded("a deleted sidecar cannot be restored, so the move was not undone".into());
    }
    for step in done.iter().rev() {
        if let Err(e) = unmove_file(&step.to, &step.from, options.move_mode) {
            return stranded(format!("rolling back {} failed: {e}", step.from.display()));
        }
    }
    if let Some(marker) = &options.marker {
        let _ = std::fs::remove_file(marker);
    }
    QuarantineFailure::untouched(error)
}

/// Undo [`move_file`]: bring `to` back to `from`, never overwriting.
fn unmove_file(to: &Path, from: &Path, mode: MoveMode) -> std::io::Result<()> {
    move_file(to, from, mode)
}

/// The `attempt`-th destination for a quarantined `db`: `<db><suffix>`, then `<db><suffix>-1`, ...
fn quarantine_candidate(db: &Path, name: &str, suffix: &str, attempt: u32) -> PathBuf {
    if attempt == 0 {
        db.with_file_name(format!("{name}{suffix}"))
    } else {
        db.with_file_name(format!("{name}{suffix}-{attempt}"))
    }
}

/// True when the destination, or (under [`SidecarPolicy::RenameSidecars`]) one of its sidecar
/// names, already exists. A dangling link counts as existing.
fn quarantine_name_taken(dest: &Path, sidecars: SidecarPolicy) -> bool {
    fn exists(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }
    exists(dest)
        || (sidecars == SidecarPolicy::RenameSidecars
            && ["-wal", "-shm", "-journal"]
                .iter()
                .any(|sidecar| exists(&with_suffix(dest, sidecar))))
}

/// Moves `from` to `to`, failing with `AlreadyExists` (and leaving both names) when `to` exists.
/// Both modes claim `to` atomically: `rename_exclusive` for `Rename`, `link` for `HardLinkThenRemove`.
fn move_file(from: &Path, to: &Path, mode: MoveMode) -> std::io::Result<()> {
    match mode {
        MoveMode::Rename => rightkit_fs::rename_exclusive(from, to).map_err(std::io::Error::from),
        MoveMode::HardLinkThenRemove => {
            std::fs::hard_link(from, to)?;
            std::fs::remove_file(from)
        }
    }
}

/// `Ok(true)` when the file moved, `Ok(false)` when there was nothing to move.
fn move_if_present(from: &Path, to: &Path, mode: MoveMode) -> Result<bool> {
    match move_file(from, to, mode) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(DbError::Fs(rightkit_fs::FsError::io(from, error))),
    }
}

fn write_marker(marker: &Path, original: &Path, quarantined: &Path) -> Result<()> {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let body = format!(
        "{{\"schema\":\"rightkit-db-quarantine.v1\",\"original\":{},\"quarantined\":{},\"unixMs\":{}}}\n",
        json_string(&original.to_string_lossy()),
        json_string(&quarantined.to_string_lossy()),
        unix_ms
    );
    rightkit_fs::atomic_write_private(marker, body.as_bytes())?;
    Ok(())
}

/// A JSON string literal for `value`, escaping quotes, backslashes and control characters.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// `Ok(true)` when the file was removed, `Ok(false)` when it was not there.
fn remove_if_present(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(DbError::Fs(rightkit_fs::FsError::io(path, error))),
    }
}

/// Rotate the SQLCipher key in place (`PRAGMA rekey`, atomic inside SQLCipher) and verify the
/// connection still reads. The caller stages the new key in its credential store *before* calling
/// this and promotes it after; an interruption between is recovered with
/// [`DbOptions::open_writer_with_key_candidates`].
#[cfg(feature = "cipher")]
pub fn rekey(conn: &Connection, new_key: &str) -> Result<()> {
    crate::validate_key(Some(new_key))?;
    let statement = Zeroizing::new(format!("PRAGMA rekey = '{new_key}';"));
    conn.execute_batch(statement.as_str())?;
    crate::verify_key(conn)
}
