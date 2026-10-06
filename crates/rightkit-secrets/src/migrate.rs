//! Migration and source resolution. Every report carries account names and counts only.

use crate::error::{Error, Result};
use crate::secret::Secret;
use crate::store::Store;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationReport {
    pub moved: Vec<String>,
    /// Destination already had a value: never overwritten, source left alone.
    pub skipped_present: Vec<String>,
    pub missing: Vec<String>,
    /// Copy or read-back verification failed, or the store was unreachable; source untouched.
    pub failed: Vec<String>,
}

/// Copy each account from `from` to `to` when `to` has none, verify the copy reads back
/// byte-identical, then delete the original. One account's failure never aborts the rest.
pub fn migrate_between(from: &Store, to: &Store, accounts: &[&str]) -> MigrationReport {
    let mut r = MigrationReport::default();
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
        // add_if_absent: a concurrent writer to the new namespace wins and is never clobbered.
        let stored = matches!(to.add_if_absent(acct, &value), Ok(true))
            && matches!(to.get(acct), Ok(Some(ref back)) if back.ct_eq(&value));
        if stored && from.delete(acct).is_ok() {
            r.moved.push(acct.into());
        } else {
            r.failed.push(acct.into());
        }
    }
    r
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
            total.skipped_present.extend(r.skipped_present);
            total.missing.extend(r.missing);
            total.failed.extend(r.failed);
        }
        if let (Some(m), true) = (marker, total.failed.is_empty()) {
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
    /// The store already had a value; the stale file was zeroed and deleted.
    AlreadyStored,
    /// The store could not take the value; the file is untouched (fail closed, no plaintext
    /// fallback in this crate).
    StoreUnavailable,
}

/// CodeRight's one-way plaintext-file migration (`~/.coderight/<provider>.key`).
pub fn migrate_plaintext_file(store: &Store, account: &str, path: &Path) -> Result<FileMigration> {
    let raw = match std::fs::read(path) {
        Ok(b) => Secret::from_bytes(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileMigration::NoFile),
        Err(e) => return Err(Error::Io(e.to_string())),
    };
    let trimmed = raw.expose_bytes().trim_ascii();
    let value = Secret::from_bytes(trimmed.to_vec());
    if value.is_empty() {
        wipe_and_remove(path)?;
        return Ok(FileMigration::NoFile);
    }
    match store.exists(account) {
        Ok(true) => {
            wipe_and_remove(path)?;
            return Ok(FileMigration::AlreadyStored);
        }
        Ok(false) => {}
        Err(_) => return Ok(FileMigration::StoreUnavailable),
    }
    let ok = matches!(store.add_if_absent(account, &value), Ok(true))
        && matches!(store.get(account), Ok(Some(ref b)) if b.ct_eq(&value));
    if !ok {
        return Ok(FileMigration::StoreUnavailable);
    }
    wipe_and_remove(path)?;
    Ok(FileMigration::Migrated)
}

fn wipe_and_remove(path: &Path) -> Result<()> {
    use std::io::Write;
    if let Ok(meta) = std::fs::metadata(path) {
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
            let _ = f.write_all(&vec![0u8; meta.len() as usize]);
            let _ = f.sync_all();
        }
    }
    std::fs::remove_file(path).map_err(|e| Error::Io(e.to_string()))
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
        .find_map(|v| env(v).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .map(Secret::from_string)
}

impl Store {
    /// Store first; the environment answers when the store has no value OR is unreachable
    /// (Windows over SSH has no logon session) so an env key is never hidden by a dead store.
    pub fn resolve(&self, account: &str, env_vars: &[&str], env: EnvReader) -> Result<Option<(Secret, Source)>> {
        match self.get(account) {
            Ok(Some(s)) => Ok(Some((s, Source::Store))),
            Ok(None) => Ok(env_value(env, env_vars).map(|s| (s, Source::Environment))),
            Err(e) => env_value(env, env_vars).map(|s| Some((s, Source::Environment))).ok_or(e),
        }
    }
    /// Passive status: store presence, else environment presence. Never reads a stored secret.
    pub fn status_source(&self, account: &str, env_vars: &[&str], env: EnvReader) -> Option<Source> {
        if matches!(self.exists(account), Ok(true)) {
            Some(Source::Store)
        } else if env_value(env, env_vars).is_some() {
            Some(Source::Environment)
        } else {
            None
        }
    }
    /// Copy environment keys into the store. Names only in the result.
    pub fn import_env(&self, map: EnvMap, overwrite: bool, env: EnvReader) -> Result<EnvImport> {
        let mut r = EnvImport::default();
        for (account, vars) in map {
            let Some(v) = env_value(env, vars) else {
                r.missing.push((*account).into());
                continue;
            };
            if !overwrite && self.exists(account)? {
                r.skipped_present.push((*account).into());
                continue;
            }
            self.set(account, &v)?;
            r.imported.push((*account).into());
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
