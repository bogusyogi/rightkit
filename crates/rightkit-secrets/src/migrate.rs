//! Migration and source resolution. Every report carries account names and counts only.

use crate::backend::ConditionalDelete;
use crate::error::{Error, Result};
use crate::secret::Secret;
use crate::store::{Store, WriteIntent};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationReport {
    /// Copied, verified, and the source removed because it still held exactly the copied value.
    pub moved: Vec<String>,
    /// Copied and verified, but the source was KEPT: its backend cannot delete conditionally
    /// ([`ConditionalDelete::Unsupported`], for example the macOS keychain), or a writer replaced
    /// the source after it was read ([`ConditionalDelete::Changed`]), so deleting it would have
    /// destroyed that newer value. The destination holds the copied value. Remove the source
    /// yourself, once nothing else writes it, if you want it gone.
    pub retained: Vec<String>,
    /// Destination already had a value: never overwritten, source left alone.
    pub skipped_present: Vec<String>,
    pub missing: Vec<String>,
    /// Copy or read-back verification failed, or the store was unreachable; source untouched.
    pub failed: Vec<String>,
    /// The destination is not a durable store ([`Store::is_durable`]: a guard put it in memory or
    /// deny mode, or its backend does not say it is durable). Nothing was copied and nothing was
    /// removed: deleting the persistent source after copying into process memory would lose the
    /// secret on exit.
    pub refused: Vec<String>,
}

/// Copy each account from `from` to `to` when `to` has none, verify the copy reads back
/// byte-identical, then delete the original **only if it still holds the value that was
/// copied** ([`Store::delete_if_equals`]). Where the source backend cannot do that atomically the
/// source is retained (`retained` in the report) rather than deleted unconditionally, because a
/// writer that replaced the source between the read and the delete would lose its value. One
/// account's failure never aborts the rest.
///
/// A destination that is not durable ([`Store::is_durable`]) takes nothing: every account is
/// `refused`, because this function removes the source and a copy that dies with the process
/// would be the only one left. The source is removed by [`remove_copied_source`], the one place
/// that does it.
pub fn migrate_between(from: &Store, to: &Store, accounts: &[&str]) -> MigrationReport {
    let mut r = MigrationReport::default();
    if !to.is_durable() {
        r.refused = accounts.iter().map(|a| (*a).to_string()).collect();
        return r;
    }
    for &acct in accounts {
        match to.exists(acct) {
            Ok(true) => {
                r.skipped_present.push(acct.into());
                continue;
            }
            Ok(false) => {}
            Err(_) => {
                r.failed.push(acct.into());
                continue;
            }
        }
        let value = match from.get(acct) {
            Ok(Some(v)) if !v.is_empty() => v,
            Ok(_) => {
                r.missing.push(acct.into());
                continue;
            }
            Err(_) => {
                r.failed.push(acct.into());
                continue;
            }
        };
        // CreateOnly: a concurrent writer to the new namespace wins and is never clobbered.
        let stored = matches!(to.put(acct, &value, WriteIntent::CreateOnly), Ok(true))
            && matches!(to.get(acct), Ok(Some(ref back)) if back.ct_eq(&value));
        if !stored {
            r.failed.push(acct.into());
            continue;
        }
        match remove_copied_source(from, to, acct, &value) {
            Ok(ConditionalDelete::Deleted | ConditionalDelete::Absent) => r.moved.push(acct.into()),
            Ok(ConditionalDelete::Changed | ConditionalDelete::Unsupported) => {
                r.retained.push(acct.into())
            }
            Err(_) => r.failed.push(acct.into()),
        }
    }
    r
}

/// The ONE place a migration removes a source credential. It removes nothing unless `to` is a
/// durable store and still holds exactly `copied` (re-read here, not trusted from the caller),
/// and then only if `from` still holds exactly `copied` ([`Store::delete_if_equals`]). A caller
/// that skips this function has no way to delete a migration source.
fn remove_copied_source(
    from: &Store,
    to: &Store,
    account: &str,
    copied: &Secret,
) -> Result<ConditionalDelete> {
    if !to.is_durable() {
        return Ok(ConditionalDelete::Unsupported);
    }
    match to.get(account)? {
        Some(held) if held.ct_eq(copied) => from.delete_if_equals(account, copied),
        _ => Ok(ConditionalDelete::Unsupported),
    }
}

impl Store {
    /// One-time migration from legacy namespaces (a renamed bundle id). `marker` is a JSON file
    /// written only when nothing failed, so later launches skip the work entirely. All accesses
    /// are non-interactive unless this store was opened interactive.
    pub fn migrate_legacy_namespaces(
        &self,
        legacy: &[crate::Namespace],
        accounts: &[&str],
        marker: Option<&Path>,
    ) -> Result<Option<MigrationReport>> {
        if self.guard_active() || marker.is_some_and(|m| m.exists()) {
            return Ok(None);
        }
        let mut total = MigrationReport::default();
        for ns in legacy {
            let r = migrate_between(&self.with_namespace(ns.clone()), self, accounts);
            total.moved.extend(r.moved);
            total.retained.extend(r.retained);
            total.skipped_present.extend(r.skipped_present);
            total.missing.extend(r.missing);
            total.failed.extend(r.failed);
            total.refused.extend(r.refused);
        }
        if let (Some(m), true) = (marker, total.failed.is_empty() && total.refused.is_empty()) {
            if let Some(dir) = m.parent() {
                std::fs::create_dir_all(dir).map_err(|e| Error::Io(e.to_string()))?;
            }
            let body = serde_json::to_vec(&total).map_err(|e| Error::Io(e.to_string()))?;
            std::fs::write(m, body).map_err(|e| Error::Io(e.to_string()))?;
        }
        Ok(Some(total))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMigration {
    /// Stored, read back identical, file overwritten with zeros and deleted.
    Migrated,
    /// No legacy file.
    NoFile,
    /// The store already held exactly the file's value; the file was zeroed and deleted.
    AlreadyStored,
    /// The store could not take the value, or it is not a durable store (a guard put this
    /// `Store` in memory or deny mode); the file is untouched (fail closed, no plaintext
    /// fallback in this crate).
    StoreUnavailable,
    /// Stored (or already stored identically) and, as [`LegacyFile::Retain`] asked, the file was
    /// left exactly as it was. Remove it yourself once nothing else writes it.
    Retained,
    /// The file was replaced or rewritten while it was being migrated, or the store already holds
    /// a value that differs from the file (the stored copy is older than the file). It is
    /// untouched, because deleting it would have destroyed content that was never stored.
    SourceChanged,
}

/// What to do with the legacy plaintext file once its value is safely stored. Chosen by the
/// caller; there is no default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyFile {
    /// Leave the file untouched. The only choice that cannot destroy a writer's data: this crate
    /// shares no lock with the file's writers, so no wipe-and-delete of it is race-free.
    Retain,
    /// Zero and delete the file, if it is still exactly the file that was read. The caller
    /// asserts that nothing else writes the file (for example the app that owned it is not
    /// running). A writer that replaces the path is never touched, but one that already holds
    /// the file open and writes in place in the instant before the wipe cannot be excluded.
    RetireIfUnchanged,
}

/// The legacy file, opened ONCE: the bytes migrated and the handle they came from.
struct Legacy {
    file: std::fs::File,
    bytes: Secret,
}

fn io_err(e: std::io::Error) -> Error {
    Error::Io(e.to_string())
}

fn read_all(file: &mut std::fs::File, hint: usize) -> Result<Secret> {
    use std::io::Read;
    let mut buf = zeroize::Zeroizing::new(Vec::with_capacity(hint.saturating_add(1)));
    file.read_to_end(&mut buf).map_err(io_err)?;
    Ok(Secret::from_bytes(std::mem::take(&mut *buf)))
}

fn open_legacy(path: &Path, writable: bool) -> Result<Option<Legacy>> {
    // Read and write when the file may be retired, so the same handle can wipe the content it
    // read; a file we may not write is still read (and removed without a wipe, as before). A
    // file that is only to be retained is opened read-only and is never written through us.
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(writable)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(e)),
        },
    };
    let hint = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
    let bytes = read_all(&mut file, hint)?;
    Ok(Some(Legacy { file, bytes }))
}

/// True when `path` still names the file `legacy` was read from.
#[cfg(unix)]
fn path_is_legacy(path: &Path, legacy: &Legacy) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(path), legacy.file.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// True when `path` still names the file `legacy` was read from. Without a portable file
/// identity the content is compared instead.
#[cfg(not(unix))]
fn path_is_legacy(path: &Path, legacy: &Legacy) -> bool {
    match std::fs::File::open(path)
        .map_err(io_err)
        .and_then(|mut f| read_all(&mut f, legacy.bytes.len()))
    {
        Ok(now) => now.ct_eq(&legacy.bytes),
        Err(_) => false,
    }
}

/// True when `path` is still the file `legacy` was read from and the handle still reads the
/// bytes that were read.
fn still_legacy(path: &Path, legacy: &mut Legacy) -> Result<bool> {
    use std::io::{Seek, SeekFrom};
    legacy.file.seek(SeekFrom::Start(0)).map_err(io_err)?;
    let now = read_all(&mut legacy.file, legacy.bytes.len())?;
    Ok(now.ct_eq(&legacy.bytes) && path_is_legacy(path, legacy))
}

/// A hidden sibling of `path` to move the file to before it is wiped.
fn aside_name(path: &Path) -> Result<PathBuf> {
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(|_| Error::Unavailable("OS CSPRNG failed".into()))?;
    let hex: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let name = path.file_name().ok_or_else(|| Error::Invalid {
        what: "path",
        reason: "has no file name".into(),
    })?;
    let mut aside = std::ffi::OsString::from(".");
    aside.push(name);
    aside.push(format!(".rk-retiring-{hex}"));
    Ok(path.with_file_name(aside))
}

/// Zero and delete the legacy file, but only if it is still the file that was read and still
/// holds exactly the bytes that were read. `Ok(false)` means it changed and nothing was lost.
///
/// The file is first MOVED ASIDE (a rename, so the path is free again at once) and verified
/// there, and only the moved file is wiped and removed. A writer that replaces the path after
/// the first check therefore never has its new file wiped or deleted: the rename carries the
/// replacement aside, the identity check fails, and the replacement is put back (without
/// overwriting anything newer; if the name was taken again it is left at its hidden name, never
/// deleted). The one case this cannot exclude is a writer that already holds the file open and
/// writes in place between the final verification and the wipe, because no lock is shared with
/// the file's writers; hence [`LegacyFile::RetireIfUnchanged`] is the caller's assertion that
/// there is no such writer.
///
/// `before_move` runs after the first check and before the rename (a seam for tests).
fn retire(path: &Path, mut legacy: Legacy, before_move: &mut dyn FnMut()) -> Result<bool> {
    use std::io::{Seek, SeekFrom, Write};
    if !still_legacy(path, &mut legacy)? {
        return Ok(false);
    }
    before_move();
    let aside = aside_name(path)?;
    std::fs::rename(path, &aside).map_err(io_err)?;
    if !still_legacy(&aside, &mut legacy)? {
        // Not the file that was read: give it back, never overwriting a newer file.
        if std::fs::hard_link(&aside, path).is_ok() {
            let _ = std::fs::remove_file(&aside);
        }
        return Ok(false);
    }
    // Wipe through the handle that was read, so a file that replaced it is never written to.
    if legacy.file.seek(SeekFrom::Start(0)).is_ok() {
        let _ = legacy.file.write_all(&vec![0u8; legacy.bytes.len()]);
        let _ = legacy.file.sync_all();
    }
    drop(legacy);
    std::fs::remove_file(&aside).map_err(io_err)?;
    Ok(true)
}

/// One-way migration of a legacy plaintext key file (e.g. `~/.myapp/<provider>.key`).
///
/// The file is opened once; the bytes stored are the bytes of that open file, stored with
/// [`WriteIntent::CreateOnly`] (an existing credential is never replaced). What happens to the
/// file afterwards is the caller's typed choice, `file`: [`LegacyFile::Retain`] leaves it alone
/// ([`FileMigration::Retained`]); [`LegacyFile::RetireIfUnchanged`] zeroes and deletes it only if
/// it is still that file with the same content ([`FileMigration::SourceChanged`] otherwise).
/// Nothing is removed when `store` is not durable ([`Store::is_durable`]): a store guarded into
/// memory or deny mode, or one whose backend does not report itself durable, returns
/// [`FileMigration::StoreUnavailable`] with the file untouched, because copying into process
/// memory and deleting the only persistent copy would lose the secret on exit.
pub fn migrate_plaintext_file(
    store: &Store,
    account: &str,
    path: &Path,
    file: LegacyFile,
) -> Result<FileMigration> {
    migrate_plaintext_file_with(store, account, path, file, &mut || {})
}

fn migrate_plaintext_file_with(
    store: &Store,
    account: &str,
    path: &Path,
    file: LegacyFile,
    before_move: &mut dyn FnMut(),
) -> Result<FileMigration> {
    if !store.is_durable() {
        return Ok(FileMigration::StoreUnavailable);
    }
    let retire_requested = file == LegacyFile::RetireIfUnchanged;
    let Some(legacy) = open_legacy(path, retire_requested)? else {
        return Ok(FileMigration::NoFile);
    };
    let value = Secret::from_bytes(legacy.bytes.expose_bytes().trim_ascii().to_vec());
    let mut finish = |legacy: Legacy, done: FileMigration| -> Result<FileMigration> {
        if !retire_requested {
            return Ok(FileMigration::Retained);
        }
        retire(path, legacy, before_move).map(|removed| {
            if removed {
                done
            } else {
                FileMigration::SourceChanged
            }
        })
    };
    if value.is_empty() {
        return if retire_requested {
            finish(legacy, FileMigration::NoFile)
        } else {
            Ok(FileMigration::NoFile)
        };
    }
    match store.exists(account) {
        Ok(true) => {
            // Delete the file only if the store already holds exactly its content; a file that
            // differs holds something never stored, so it is left untouched.
            return match store.get(account) {
                Ok(Some(stored)) if stored.ct_eq(&value) => {
                    finish(legacy, FileMigration::AlreadyStored)
                }
                Ok(Some(_)) => Ok(FileMigration::SourceChanged),
                Ok(None) | Err(_) => Ok(FileMigration::StoreUnavailable),
            };
        }
        Ok(false) => {}
        Err(_) => return Ok(FileMigration::StoreUnavailable),
    }
    let ok = matches!(
        store.put(account, &value, WriteIntent::CreateOnly),
        Ok(true)
    ) && matches!(store.get(account), Ok(Some(ref b)) if b.ct_eq(&value));
    if !ok {
        return Ok(FileMigration::StoreUnavailable);
    }
    finish(legacy, FileMigration::Migrated)
}

/// Account -> environment variable names; the first non-empty value wins.
pub type EnvMap<'a> = &'a [(&'a str, &'a [&'a str])];
/// Injectable environment reader so tests never touch the real environment.
pub type EnvReader<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Store,
    Environment,
}

fn env_value(env: EnvReader, vars: &[&str]) -> Option<Secret> {
    vars.iter()
        .find_map(|v| {
            env(v)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .map(Secret::from_string)
}

impl Store {
    /// Store first; the environment answers when the store has no value OR is unreachable
    /// (Windows over SSH has no logon session) so an env key is never hidden by a dead store.
    pub fn resolve(
        &self,
        account: &str,
        env_vars: &[&str],
        env: EnvReader,
    ) -> Result<Option<(Secret, Source)>> {
        match self.get(account) {
            Ok(Some(s)) => Ok(Some((s, Source::Store))),
            Ok(None) => Ok(env_value(env, env_vars).map(|s| (s, Source::Environment))),
            Err(e) => env_value(env, env_vars)
                .map(|s| Some((s, Source::Environment)))
                .ok_or(e),
        }
    }
    /// Passive status: store presence, else environment presence. Never reads a stored secret.
    pub fn status_source(
        &self,
        account: &str,
        env_vars: &[&str],
        env: EnvReader,
    ) -> Option<Source> {
        if matches!(self.exists(account), Ok(true)) {
            Some(Source::Store)
        } else if env_value(env, env_vars).is_some() {
            Some(Source::Environment)
        } else {
            None
        }
    }
    /// Copy environment keys into the store. Names only in the result. `intent` is the caller's
    /// typed choice: [`WriteIntent::CreateOnly`] imports only accounts that have no credential
    /// (the others are `skipped_present`) and cannot overwrite a credential saved concurrently,
    /// because the presence check and the write are one [`Store::put`]; [`WriteIntent::Replace`]
    /// overwrites.
    pub fn import_env(
        &self,
        map: EnvMap,
        intent: WriteIntent,
        env: EnvReader,
    ) -> Result<EnvImport> {
        let mut r = EnvImport::default();
        for (account, vars) in map {
            let Some(v) = env_value(env, vars) else {
                r.missing.push((*account).into());
                continue;
            };
            if self.put(account, &v, intent)? {
                r.imported.push((*account).into());
            } else {
                r.skipped_present.push((*account).into());
            }
        }
        Ok(r)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct EnvImport {
    pub imported: Vec<String>,
    pub skipped_present: Vec<String>,
    pub missing: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemoryBackend;
    use crate::store::{Config, GuardMode};
    use crate::Namespace;
    use std::sync::Arc;

    fn memory_store() -> Store {
        Store::open(
            Config::new(Namespace::new("dev.rightkit.secrets.test.migrate-file").unwrap())
                .backend(Arc::new(MemoryBackend::simulating_durable())),
        )
    }

    #[test]
    fn an_uncontended_migration_stores_zeroes_and_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b" legacy-secret \n").unwrap();
        let store = memory_store();
        assert_eq!(
            migrate_plaintext_file(&store, "acct", &path, LegacyFile::RetireIfUnchanged).unwrap(),
            FileMigration::Migrated
        );
        assert!(!path.exists());
        assert_eq!(
            store.get("acct").unwrap().unwrap().expose_str().unwrap(),
            "legacy-secret"
        );
    }

    #[test]
    fn a_guarded_store_never_consumes_the_only_persistent_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b"legacy-secret").unwrap();
        for mode in [GuardMode::Memory, GuardMode::Deny] {
            let store = Store::open(
                Config::new(Namespace::new("dev.rightkit.secrets.test.migrate-guard").unwrap())
                    .guard_when(true, mode),
            );
            assert_eq!(
                migrate_plaintext_file(&store, "acct", &path, LegacyFile::RetireIfUnchanged)
                    .unwrap(),
                FileMigration::StoreUnavailable
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"legacy-secret");
        }
    }

    #[test]
    fn a_file_rewritten_after_it_was_read_is_kept_not_wiped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b"content-A").unwrap();
        let legacy = open_legacy(&path, true).unwrap().unwrap();
        // Another writer rewrites the same file in place.
        std::fs::write(&path, b"content-B").unwrap();
        assert!(!retire(&path, legacy, &mut || {}).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"content-B");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_replaced_after_it_was_read_is_kept_and_never_written_to() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b"content-A").unwrap();
        let legacy = open_legacy(&path, true).unwrap().unwrap();
        // Another writer replaces the path with a different file.
        let other = dir.path().join("replacement");
        std::fs::write(&other, b"content-B").unwrap();
        std::fs::rename(&other, &path).unwrap();
        assert!(!retire(&path, legacy, &mut || {}).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"content-B");
    }

    #[test]
    fn an_unchanged_file_is_retired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b"content-A").unwrap();
        let legacy = open_legacy(&path, true).unwrap().unwrap();
        assert!(retire(&path, legacy, &mut || {}).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn a_file_rewritten_after_its_first_copy_was_stored_is_kept_on_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b"first-copy").unwrap();
        let store = memory_store();
        // The first copy is stored but the file survives (as after a crash before removal).
        store.set_str("acct", "first-copy").unwrap();
        std::fs::write(&path, b"rewritten-later").unwrap();
        assert_eq!(
            migrate_plaintext_file(&store, "acct", &path, LegacyFile::RetireIfUnchanged).unwrap(),
            FileMigration::SourceChanged
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"rewritten-later");
        assert_eq!(
            store.get("acct").unwrap().unwrap().expose_str().unwrap(),
            "first-copy"
        );
        // A file equal to the stored value is the stale copy and is retired.
        std::fs::write(&path, b"first-copy\n").unwrap();
        assert_eq!(
            migrate_plaintext_file(&store, "acct", &path, LegacyFile::RetireIfUnchanged).unwrap(),
            FileMigration::AlreadyStored
        );
        assert!(!path.exists());
    }

    #[test]
    fn a_missing_file_is_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            migrate_plaintext_file(
                &memory_store(),
                "acct",
                &dir.path().join("none"),
                LegacyFile::RetireIfUnchanged
            )
            .unwrap(),
            FileMigration::NoFile
        );
    }

    #[test]
    fn a_non_durable_store_takes_no_file_and_loses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b"legacy-secret").unwrap();
        // A plain memory backend does not report itself durable.
        let store = Store::open(
            Config::new(Namespace::new("dev.rightkit.secrets.test.migrate-nondurable").unwrap())
                .backend(Arc::new(MemoryBackend::new())),
        );
        assert!(!store.is_durable());
        for file in [LegacyFile::Retain, LegacyFile::RetireIfUnchanged] {
            assert_eq!(
                migrate_plaintext_file(&store, "acct", &path, file).unwrap(),
                FileMigration::StoreUnavailable
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"legacy-secret");
        assert!(!store.exists("acct").unwrap());
    }

    #[test]
    fn retain_stores_the_value_and_leaves_the_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b" legacy-secret \n").unwrap();
        let store = memory_store();
        assert_eq!(
            migrate_plaintext_file(&store, "acct", &path, LegacyFile::Retain).unwrap(),
            FileMigration::Retained
        );
        assert_eq!(std::fs::read(&path).unwrap(), b" legacy-secret \n");
        assert_eq!(
            store.get("acct").unwrap().unwrap().expose_str().unwrap(),
            "legacy-secret"
        );
        // Already stored and identical: still retained, never removed.
        assert_eq!(
            migrate_plaintext_file(&store, "acct", &path, LegacyFile::Retain).unwrap(),
            FileMigration::Retained
        );
        assert!(path.exists());
    }

    #[test]
    fn migration_never_replaces_a_credential_that_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b"from-file").unwrap();
        let store = memory_store();
        store.set_str("acct", "saved-by-user").unwrap();
        assert_eq!(
            migrate_plaintext_file(&store, "acct", &path, LegacyFile::RetireIfUnchanged).unwrap(),
            FileMigration::SourceChanged
        );
        assert_eq!(
            store.get("acct").unwrap().unwrap().expose_str().unwrap(),
            "saved-by-user"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"from-file");
    }

    /// Audit finding 4: a replacement that arrives AFTER the final comparison is not deleted.
    #[test]
    fn a_replacement_arriving_after_the_final_comparison_is_neither_wiped_nor_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.key");
        std::fs::write(&path, b"content-A").unwrap();
        let legacy = open_legacy(&path, true).unwrap().unwrap();
        let mut replace = || {
            let other = dir.path().join("replacement");
            std::fs::write(&other, b"content-B").unwrap();
            std::fs::rename(&other, &path).unwrap();
        };
        assert!(!retire(&path, legacy, &mut replace).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"content-B");
        // Nothing is left behind under a hidden name.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["legacy.key"], "{names:?}");
    }

    #[test]
    fn import_env_create_only_skips_existing_and_cannot_overwrite_a_concurrent_save() {
        let store = memory_store();
        let env = |name: &str| (name == "KEY").then(|| "stale-from-env".to_string());
        let map: EnvMap = &[("acct", &["KEY"]), ("fresh", &["KEY"])];
        // A user saves B just before the import runs; the import must not replace it.
        store.set_str("acct", "B-saved-by-user").unwrap();
        let report = store
            .import_env(map, WriteIntent::CreateOnly, &env)
            .unwrap();
        assert_eq!(report.skipped_present, vec!["acct"]);
        assert_eq!(report.imported, vec!["fresh"]);
        assert_eq!(
            store.get("acct").unwrap().unwrap().expose_str().unwrap(),
            "B-saved-by-user"
        );
        // Replace is the explicit opt-in.
        let report = store.import_env(map, WriteIntent::Replace, &env).unwrap();
        assert_eq!(report.imported, vec!["acct", "fresh"]);
        assert_eq!(
            store.get("acct").unwrap().unwrap().expose_str().unwrap(),
            "stale-from-env"
        );
    }
}
