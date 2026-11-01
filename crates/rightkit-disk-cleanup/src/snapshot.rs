//! Versioned snapshot files: `{"version": N, "data": ...}`, the one way an app keeps what it can
//! rebuild (the last Storage index and view, drive health history and alerts, app inventories).
//!
//! The file is read and written through the owner's flat-file store (`rightkit-app-settings`,
//! `FileFormat::Flat`), so a write is an atomic, owner-only replace, a file that does not parse is
//! treated as absent (the damaged bytes are kept beside it as a quarantined copy, never silently
//! replaced), and a file from a newer version is left as it
//! is. The bytes on disk keep the shape earlier builds wrote and read (`version`, `data`), so no
//! snapshot is invalidated and an older build still reads what a newer one saves at the same
//! version.

use rightkit_app_settings::{
    FileFormat, OnCorrupt, OnRefused, Schema, Store, StoreOptions, WriteSecurity,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::io;
use std::path::Path;

fn options(version: u32) -> StoreOptions {
    // On-disk schema id kept from the first release so existing snapshot files still read.
    StoreOptions::new(Schema::new("pulse-snapshot", version))
        .format(FileFormat::Flat)
        .flat_version_key("version")
        .defaults_provider(|| json!({}))
        .on_corrupt(OnCorrupt::Quarantine)
        .on_refused(OnRefused::ReadOnlyDefaults)
        .sidecars(false)
        .utf8_bom(false)
        .write_security(WriteSecurity::OwnerPrivate)
}

/// The data saved at `version` in `path`, or `None` when the file is missing, unreadable, does not
/// parse, is from another version, or its data is not a `T`.
pub fn load<T: DeserializeOwned>(path: &Path, version: u32) -> Option<T> {
    let store = Store::<Value>::open_read_only(path, options(version)).ok()?;
    if store.refusal().is_some() {
        return None;
    }
    let document = store.get();
    if document.get("version").and_then(Value::as_u64) != Some(u64::from(version)) {
        return None;
    }
    serde_json::from_value(document.get("data")?.clone()).ok()
}

/// [`load`], or `T::default()`.
pub fn load_or_default<T: DeserializeOwned + Default>(path: &Path, version: u32) -> T {
    load(path, version).unwrap_or_default()
}

/// Replace the snapshot in `path` with `data` at `version`, creating the folder. Atomic: a reader
/// never sees a partial file, and a failed write leaves the old file in place. A file from a newer
/// version is not overwritten (the write fails).
pub fn save<T: Serialize>(path: &Path, version: u32, data: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_value(data).map_err(io::Error::other)?;
    let store = Store::<Value>::open_path(path, options(version)).map_err(io::Error::other)?;
    store
        .update(|document| {
            *document = json!({ "version": version, "data": data });
            Ok(())
        })
        .map_err(io::Error::other)
}
