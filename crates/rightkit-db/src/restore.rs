//! A restore transaction: replace the live database with a prepared copy, keep the previous store
//! at a named path, and put it back if anything fails.
//!
//! [`replace_database`](crate::replace_database) drops the old file. A restore must not: the
//! previous store is the user's data until the new one is proven. Under the store-wide exclusive
//! lock [`restore_database`]:
//!
//! 1. recovers a restore a crash left half done (see [`recover_interrupted_restore`]);
//! 2. checks the replacement (opened read-only with the options' key, every `integrity_check` row
//!    `ok`) before touching anything, so a bad replacement leaves the live store alone;
//! 3. writes a journal `<db>.restore-journal` naming where the previous store will be kept, then
//!    moves the live file and its `-wal`, `-shm`, `-journal` sidecars to
//!    `<db><previous_infix><unix-ms>` (claimed exclusively, never overwriting, never deleted);
//! 4. places the replacement with [`replace_database_locked`](crate::replace_database_locked);
//! 5. runs [`RestoreOptions::verify`] on the placed store;
//! 6. removes the journal and returns a [`RestoreReceipt`] naming the kept path.
//!
//! A failure in steps 3 to 5 sets the placed copy aside at `<db><failed_infix><unix-ms>` (also
//! kept) and moves the previous store back. If that rollback itself fails, the journal stays and
//! the error says where each file is; [`recover_interrupted_restore`] (also run first by every
//! restore) puts the previous store back on the next open.
//!
//! The journal is a small text file: `rightkit-db-restore.v1`, `state=quarantining|placed`,
//! `previous=<file name>` (or `previous=none` when there was no store). Recovery acts only on a
//! journal this module wrote, never on a stray `.pre-restore-*` file.
//!
//! Callers must close every connection to the live database first (required on Windows), as for
//! `replace_database`. The replacement keeps the options' key.

use crate::backup::{replace_database_locked, validated_backup, StoreLock};
use crate::{DbError, DbOptions, Result};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];
const JOURNAL_MAGIC: &str = "rightkit-db-restore.v1";
const NAME_ATTEMPTS: u32 = 100;

/// A check run on the placed store; `Err(reason)` rolls the restore back. The `DbOptions` are the
/// ones given to the restore. Drop every connection the check opens before it returns.
pub type RestoreVerify<'a> = &'a dyn Fn(&DbOptions) -> std::result::Result<(), String>;

/// Names and the optional check for [`restore_database`].
#[derive(Clone, Copy)]
pub struct RestoreOptions<'a> {
    /// The previous store is kept as `<db><previous_infix><unix-ms>`. Default `.pre-restore-`.
    pub previous_infix: &'a str,
    /// A restored copy that failed its check is kept as `<db><failed_infix><unix-ms>`. Default
    /// `.restore-failed-`.
    pub failed_infix: &'a str,
    /// Check the placed store. `None` trusts the integrity check made before placing.
    pub verify: Option<RestoreVerify<'a>>,
}

impl Default for RestoreOptions<'_> {
    fn default() -> Self {
        Self {
            previous_infix: ".pre-restore-",
            failed_infix: ".restore-failed-",
            verify: None,
        }
    }
}

/// What a finished restore left behind, for a receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreReceipt {
    /// Where the previous store is kept (main file; sidecars sit beside it), `None` when there was
    /// no store before.
    pub previous: Option<PathBuf>,
}

/// What [`recover_interrupted_restore`] found and did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InterruptedRestore {
    /// The live store was missing; the previous store kept at `from` was moved back (sidecars
    /// included).
    PutBack { from: PathBuf },
    /// The replacement was already placed when the crash happened. It stays; the previous store
    /// is still kept at `previous`. Nothing was verified, so the caller may run its own checks.
    KeptPlaced { previous: PathBuf },
}

/// [`restore_database`] with the store-wide exclusive lock held for the call, waiting per `lock`.
pub fn restore_database(
    options: &DbOptions,
    replacement: &Path,
    restore: &RestoreOptions<'_>,
    lock: &rightkit_fs::LockOptions,
) -> Result<RestoreReceipt> {
    let _guard = StoreLock::exclusive(&options.path, lock)?;
    restore_database_locked(options, replacement, restore)
}

/// The restore transaction for a caller that already holds the exclusive [`StoreLock`].
pub fn restore_database_locked(
    options: &DbOptions,
    replacement: &Path,
    restore: &RestoreOptions<'_>,
) -> Result<RestoreReceipt> {
    let live = options.path.as_path();
    recover_interrupted_restore_locked(options)?;
    if same_file(live, replacement) {
        return Err(failed(
            "the replacement is the live database".into(),
            None,
            None,
            true,
        ));
    }
    // Nothing has moved yet: a bad replacement fails here and the live store is untouched.
    drop(validated_backup(options, replacement)?);

    let journal = journal_path(live);
    let previous = if present(live) {
        Some(free_name(live, restore.previous_infix)?)
    } else {
        None
    };
    write_journal(&journal, "quarantining", previous.as_deref())
        .map_err(|e| failed(format!("write the restore journal: {e}"), None, None, true))?;

    if let Some(prev) = &previous {
        if let Err(e) = move_store(live, prev) {
            // The main file moves first, so a failure leaves nothing or the main file moved.
            return Err(rollback(
                live,
                Some(prev),
                restore,
                format!("could not move the current store aside: {e}"),
            ));
        }
    }
    if let Err(e) = replace_database_locked(options, replacement) {
        return Err(rollback(
            live,
            previous.as_deref(),
            restore,
            format!("could not put the restored store in place: {e}"),
        ));
    }
    if let Err(e) = write_journal(&journal, "placed", previous.as_deref()) {
        return Err(rollback(
            live,
            previous.as_deref(),
            restore,
            format!("update the restore journal: {e}"),
        ));
    }
    if let Some(verify) = restore.verify {
        if let Err(reason) = verify(options) {
            return Err(rollback(
                live,
                previous.as_deref(),
                restore,
                format!("the restored store failed its checks: {reason}"),
            ));
        }
    }
    let _ = std::fs::remove_file(&journal);
    rightkit_fs::atomic::sync_dir(parent_of(live));
    Ok(RestoreReceipt { previous })
}

/// [`recover_interrupted_restore_locked`] under the exclusive store lock. Call it before opening
/// the database at startup: a crash inside a restore can leave the live path empty with the
/// previous store sitting at its kept name.
pub fn recover_interrupted_restore(
    options: &DbOptions,
    lock: &rightkit_fs::LockOptions,
) -> Result<Option<InterruptedRestore>> {
    let _guard = StoreLock::exclusive(&options.path, lock)?;
    recover_interrupted_restore_locked(options)
}

/// Finish a restore a crash left half done, driven by the restore journal. No journal returns
/// `Ok(None)`.
///
/// - live store missing, previous kept: the previous store (main file and any sidecars still at
///   the kept name) is moved back, whatever moved before the crash; the journal is removed.
/// - previous main file gone from the kept name but its sidecars still there (a stop between the
///   renames of a put-back, whether or not the live main file exists): the sidecars are moved
///   back independently, and the journal is removed only after every kept file is back.
/// - live store present and the previous kept too: the replacement was placed; both stay
///   ([`InterruptedRestore::KeptPlaced`]); the journal is removed.
/// - live store present, previous never moved: nothing happened; the journal is removed.
///
/// A state that fits none of these (the journal names a previous store that is gone while the
/// live store is missing, or an unreadable journal) is `DbError::RestoreFailed` and nothing is
/// touched.
pub fn recover_interrupted_restore_locked(
    options: &DbOptions,
) -> Result<Option<InterruptedRestore>> {
    let live = options.path.as_path();
    let journal = journal_path(live);
    let text = match std::fs::read_to_string(&journal) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(failed(
                format!("read {}: {e}", journal.display()),
                None,
                None,
                false,
            ))
        }
    };
    let previous = parse_journal(live, &text).map_err(|m| {
        failed(
            format!("{}: {m}; nothing was changed", journal.display()),
            None,
            None,
            false,
        )
    })?;
    let done = |outcome: Option<InterruptedRestore>| -> Result<Option<InterruptedRestore>> {
        let _ = std::fs::remove_file(&journal);
        rightkit_fs::atomic::sync_dir(parent_of(live));
        Ok(outcome)
    };
    let Some(prev) = previous else {
        return done(None);
    };
    let kept_main = present(&prev);
    let kept_sidecars = SIDECARS.iter().any(|s| present(&with_suffix(&prev, s)));
    match (present(live), kept_main, kept_sidecars) {
        // The main file may already be back (or never left) while sidecars still sit at the kept
        // name: a stop between the renames of `put_back`. Finish the sidecars; the journal goes
        // only after every kept file is back.
        (false, true, _) | (_, false, true) => match put_back(&prev, live) {
            Ok(()) => done(Some(InterruptedRestore::PutBack { from: prev })),
            Err(e) => Err(failed(
                format!("the previous store could not be put back: {e}"),
                Some(prev),
                None,
                false,
            )),
        },
        (true, true, _) => done(Some(InterruptedRestore::KeptPlaced { previous: prev })),
        (true, false, false) => done(None),
        (false, false, false) => Err(failed(
            format!(
                "the journal names {} but neither it nor the live store exists",
                prev.display()
            ),
            None,
            None,
            false,
        )),
    }
}

/// Undo after a failure past the point of moving: set a placed copy aside, move the previous store
/// back, drop the journal. Returns the error to report.
fn rollback(
    live: &Path,
    previous: Option<&Path>,
    restore: &RestoreOptions<'_>,
    message: String,
) -> DbError {
    let mut failed_copy = None;
    if present(live) {
        match free_name(live, restore.failed_infix).and_then(|dest| {
            move_store(live, &dest)
                .map(|()| dest)
                .map_err(|e| failed(format!("{e}"), None, None, false))
        }) {
            Ok(dest) => failed_copy = Some(dest),
            Err(e) => {
                return failed(
                    format!(
                        "{message}; rollback incomplete: the restored copy could not be moved aside ({e})"
                    ),
                    previous.filter(|p| present(p)).map(Path::to_path_buf),
                    None,
                    false,
                )
            }
        }
    }
    if let Some(prev) = previous {
        if let Err(e) = put_back(prev, live) {
            return failed(
                format!(
                    "{message}; rollback incomplete: the previous store could not be put back ({e})"
                ),
                Some(prev.to_path_buf()),
                failed_copy,
                false,
            );
        }
    }
    let _ = std::fs::remove_file(journal_path(live));
    rightkit_fs::atomic::sync_dir(parent_of(live));
    failed(message, None, failed_copy, true)
}

fn failed(
    message: String,
    previous: Option<PathBuf>,
    failed_copy: Option<PathBuf>,
    rolled_back: bool,
) -> DbError {
    DbError::RestoreFailed {
        message,
        previous,
        failed_copy,
        rolled_back,
    }
}

fn journal_path(live: &Path) -> PathBuf {
    with_suffix(live, ".restore-journal")
}

fn write_journal(path: &Path, state: &str, previous: Option<&Path>) -> Result<()> {
    let name = previous
        .and_then(Path::file_name)
        .map_or_else(|| "none".to_string(), |n| n.to_string_lossy().into_owned());
    let body = format!("{JOURNAL_MAGIC}\nstate={state}\nprevious={name}\n");
    rightkit_fs::atomic::atomic_write_private(path, body.as_bytes())?;
    Ok(())
}

/// The previous store's path (`None` for `previous=none`). The name must be a plain file name that
/// starts with the live file's name, so a journal cannot point recovery elsewhere.
fn parse_journal(live: &Path, text: &str) -> std::result::Result<Option<PathBuf>, String> {
    let mut lines = text.lines();
    if lines.next() != Some(JOURNAL_MAGIC) {
        return Err("not a restore journal".into());
    }
    let (mut state, mut previous) = (None, None);
    for line in lines {
        if let Some(v) = line.strip_prefix("state=") {
            state = Some(v);
        } else if let Some(v) = line.strip_prefix("previous=") {
            previous = Some(v);
        }
    }
    if !matches!(state, Some("quarantining" | "placed")) {
        return Err("unknown state".into());
    }
    let name = previous.ok_or("no previous= line")?;
    if name == "none" {
        return Ok(None);
    }
    let live_name = live
        .file_name()
        .ok_or("live path has no file name")?
        .to_string_lossy();
    if name.is_empty()
        || name.contains(['/', '\\', '\0'])
        || !name.starts_with(&*live_name)
        || name == live_name
    {
        return Err(format!(
            "previous= {name:?} is not a name beside the live store"
        ));
    }
    Ok(Some(live.with_file_name(name)))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `<live><infix><unix-ms>`, then `-1`, `-2`, ...: a name where neither the main file nor a
/// sidecar exists.
fn free_name(live: &Path, infix: &str) -> Result<PathBuf> {
    let name = live
        .file_name()
        .ok_or(DbError::InvalidDatabasePath)?
        .to_string_lossy()
        .into_owned();
    if infix.is_empty() || infix.contains(['/', '\\', '\0']) {
        return Err(DbError::InvalidQuarantineSuffix(infix.to_string()));
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    for attempt in 0..NAME_ATTEMPTS {
        let candidate = if attempt == 0 {
            format!("{name}{infix}{ms}")
        } else {
            format!("{name}{infix}{ms}-{attempt}")
        };
        let dest = live.with_file_name(candidate);
        if !present(&dest) && !SIDECARS.iter().any(|s| present(&with_suffix(&dest, s))) {
            return Ok(dest);
        }
    }
    Err(DbError::Fs(rightkit_fs::FsError::io(
        live,
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no free name for the kept store",
        ),
    )))
}

/// Move the main file, then each sidecar that exists, to `to` and its sidecar names. Every move is
/// create-new: nothing at the destination is overwritten.
fn move_store(from: &Path, to: &Path) -> std::io::Result<()> {
    rightkit_fs::rename_exclusive(from, to).map_err(std::io::Error::from)?;
    for suffix in SIDECARS {
        let (src, dst) = (with_suffix(from, suffix), with_suffix(to, suffix));
        match rightkit_fs::rename_exclusive(&src, &dst) {
            Ok(()) => {}
            Err(e) => {
                let e = std::io::Error::from(e);
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

/// Move a kept store back to `live`, which must be free. Tolerates a half-done earlier move: the
/// main file moves if it is still at `kept`, and each sidecar still at `kept` moves with it.
fn put_back(kept: &Path, live: &Path) -> std::io::Result<()> {
    if present(kept) {
        rightkit_fs::rename_exclusive(kept, live).map_err(std::io::Error::from)?;
    }
    for suffix in SIDECARS {
        let (src, dst) = (with_suffix(kept, suffix), with_suffix(live, suffix));
        if present(&src) {
            rightkit_fs::rename_exclusive(&src, &dst).map_err(std::io::Error::from)?;
        }
    }
    Ok(())
}
