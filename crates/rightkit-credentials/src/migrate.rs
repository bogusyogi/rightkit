//! Migration and source resolution. Every report carries account names and counts only.

use crate::backend::ConditionalDelete;
use crate::error::{Error, Result};
use crate::secret::Secret;
use crate::store::{Store, WriteIntent};
use serde::Serialize;
use std::path::Path;

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
/// would be the only one left. The source is removed by `remove_copied_source`, the one place
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

/// What [`migrate_plaintext_file`] found and did. Whatever it is, the legacy file was NOT
/// changed, wiped, moved or removed: removing it is the caller's separate step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMigration {
    /// No legacy file, or it holds only whitespace. Nothing was stored.
    NoFile,
    /// The store holds exactly the file's value: it was stored now (and read back identical), or
    /// the store already held exactly it. The file was left exactly as it was. Remove it yourself,
    /// once you have verified the stored value and nothing else writes the file.
    Retained,
    /// The store already holds a value that differs from the file's. Neither was changed. The
    /// file holds something the store does not, so deleting it would destroy content that was
    /// never stored: the caller decides which of the two is current.
    StoredValueDiffers,
    /// The store could not take the value or could not be read back, or it is not a durable
    /// store ([`Store::is_durable`]: a guard put this `Store` in memory or deny mode, or its
    /// backend does not report itself durable); the file is untouched (fail closed, no plaintext
    /// fallback in this crate).
    StoreUnavailable,
}

/// Read the legacy file once, read-only. The buffer is sized for the whole file up front so it
/// never grows (a regrown buffer would leave an unwiped copy of the secret behind).
fn read_legacy(path: &Path) -> Result<Option<Secret>> {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e.to_string())),
    };
    let hint = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
    let mut buf = zeroize::Zeroizing::new(Vec::with_capacity(hint.saturating_add(1)));
    file.read_to_end(&mut buf)
        .map_err(|e| Error::Io(e.to_string()))?;
    Ok(Some(Secret::from_bytes(std::mem::take(&mut *buf))))
}

/// One-way COPY of a legacy plaintext key file (e.g. `~/.myapp/<provider>.key`) into the store.
///
/// The file is opened read-only, once. Its content, trimmed of ASCII whitespace, is stored with
/// [`WriteIntent::CreateOnly`] (an existing credential is never replaced) and read back from the
/// store to confirm it is identical.
///
/// **This function never modifies, wipes, moves or deletes the file, whatever it returns.**
/// [`FileMigration::Retained`] says the store holds exactly the file's value. Removing the
/// now-redundant plaintext file is the app's explicit, separate step, taken after it has verified
/// the stored value (for example by reading it back with [`Store::get`]) and when nothing else
/// writes the file. This crate shares no lock with the file's writers, so no deletion it made could
/// be safe against them; that is why it makes none.
///
/// Nothing is stored when `store` is not durable ([`Store::is_durable`]): a store guarded into
/// memory or deny mode, or one whose backend does not report itself durable, returns
/// [`FileMigration::StoreUnavailable`], because a value copied into process memory is lost on exit
/// and the caller must not be led to delete the only persistent copy.
pub fn migrate_plaintext_file(store: &Store, account: &str, path: &Path) -> Result<FileMigration> {
    if !store.is_durable() {
        return Ok(FileMigration::StoreUnavailable);
    }
    let Some(raw) = read_legacy(path)? else {
        return Ok(FileMigration::NoFile);
    };
    let value = Secret::from_bytes(raw.expose_bytes().trim_ascii().to_vec());
    if value.is_empty() {
        return Ok(FileMigration::NoFile);
    }
    match store.exists(account) {
        Ok(true) => {
            // The store already has a credential. It is the file's own value, or it is not.
            return match store.get(account) {
                Ok(Some(stored)) if stored.ct_eq(&value) => Ok(FileMigration::Retained),
                Ok(Some(_)) => Ok(FileMigration::StoredValueDiffers),
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
    Ok(FileMigration::Retained)
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
