//! Validation of a QA fixture home: a folder a test or QA build names (through an environment
//! variable of the app's choosing) to run on instead of the account's real folders.
//!
//! A fixture home must be an absolute existing directory that, after resolving symlinks, is
//! neither the account's own home (the password database entry on Unix, or the launching
//! environment's `HOME`/`USERPROFILE`), nor inside its `Library` or `AppData` folder, nor the
//! filesystem root. The app decides what a rejection does (a QA build exits before touching any
//! state); this module only judges.

use std::path::PathBuf;

/// The fixture home `variable` names, `Ok(None)` when it is unset or empty (the app runs on the
/// real folders), or `Err` with a message when it is set but invalid.
pub fn validate_fixture_home(variable: &str) -> Result<Option<PathBuf>, String> {
    let Some(raw) = std::env::var_os(variable).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(format!(
            "{variable} {} is not an absolute path",
            path.display()
        ));
    }
    let resolved = std::fs::canonicalize(&path)
        .map_err(|error| format!("{variable} {} cannot be resolved: {error}", path.display()))?;
    if !resolved.is_dir() {
        return Err(format!("{variable} {} is not a folder", resolved.display()));
    }
    if resolved.parent().is_none() {
        return Err(format!("{variable} is the filesystem root"));
    }
    let mut real: Vec<PathBuf> = Vec::new();
    #[cfg(unix)]
    real.extend(rightkit_fs::app_dirs::account_home().ok());
    for name in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(name) {
            let value = PathBuf::from(value);
            // A launcher that rewrites HOME to the fixture itself is not naming a real home.
            if value != path && value != resolved {
                real.push(value);
            }
        }
    }
    for account in real {
        let account = std::fs::canonicalize(&account).unwrap_or(account);
        if resolved == account
            || resolved.starts_with(account.join("Library"))
            || resolved.starts_with(account.join("AppData"))
        {
            return Err(format!(
                "{variable} {} is the account's own home or inside its application folders",
                resolved.display()
            ));
        }
    }
    Ok(Some(path))
}
