//! One-shot removal of the files the retired remote-telemetry pipeline left in the app data
//! folder. Builds up to 0.1.213 queued usage events and kept a persistent random id there; nothing
//! reads or writes either any more, so an upgraded install would otherwise keep them indefinitely.
//! This file is the only place that may name them (`scripts/test-telemetry-exception-contract.mjs`).

use std::path::Path;

const OUTBOX_FILE: &str = "telemetry-outbox.json";
const INSTALL_ID_FILE: &str = "telemetry-install-id";
/// The outbox's atomic-replace temp files (`.<name>.tmp-<pid>-<n>`) and quarantined copies
/// (`<name>.corrupt-<pid>-<n>`).
const OUTBOX_TEMP_PREFIX: &str = ".telemetry-outbox.json.tmp-";
const OUTBOX_QUARANTINE_PREFIX: &str = "telemetry-outbox.json.corrupt-";

fn is_legacy_name(name: &str) -> bool {
    name == OUTBOX_FILE
        || name == INSTALL_ID_FILE
        || name.starts_with(OUTBOX_TEMP_PREFIX)
        || name.starts_with(OUTBOX_QUARANTINE_PREFIX)
}

/// Remove the legacy telemetry files from the app data folder. Best effort and idempotent: a
/// folder with none of them is `Ok(0)`. Returns how many files were removed.
pub fn remove_legacy_telemetry_files() -> Result<usize, String> {
    remove_legacy_telemetry_files_in(&crate::app_paths::app_data_root())
}

pub(crate) fn remove_legacy_telemetry_files_in(dir: &Path) -> Result<usize, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("read app data folder: {error}")),
    };
    let mut removed = 0;
    let mut first_error = None;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        if !file_name.to_str().is_some_and(is_legacy_name) {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                first_error.get_or_insert(format!("remove legacy telemetry file: {error}"));
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(removed),
    }
}
