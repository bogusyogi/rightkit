//! A small trust-on-first-use digest pin store, shared by the MCP tool-definition pin and the
//! project hook-set pin.
//!
//! A pin is recorded the first time a definition is accepted. A later, different digest is
//! reported as [`PinCheck::Changed`] and the caller must refuse. The store is the file named by
//! the caller inside the `pins` folder of the state root ([`pin_store_path`]); it never falls
//! back to another location. A store that exists but cannot be read is reported as Changed too
//! (fail closed) and is never rewritten.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The `schema` string of a pin store: `<host slug>.definition-pins.v1` (`myapp.definition-pins.v1`
/// for a host whose env prefix is `MYAPP`; see [`crate::host_slug`]).
fn schema_name() -> String {
    format!("{}.definition-pins.v1", crate::host_slug())
}

/// The digest of a definition checked against its pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinCheck {
    /// No pin existed; the digest is now pinned.
    FirstSeen,
    /// The digest equals the pinned one.
    Match,
    /// The digest differs from the pinned one (or the store is unreadable): refuse.
    Changed { pinned: String },
    /// The pin could not be written; nothing is pinned and the caller proceeds.
    Unavailable,
}

#[derive(Serialize, Deserialize)]
struct Store {
    schema: String,
    pins: BTreeMap<String, BTreeMap<String, String>>,
}

/// One process-wide lock for every read-modify-write of a pin store.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The store file `file_name` inside the `pins` folder of the state root.
pub fn pin_store_path(file_name: &str) -> Option<PathBuf> {
    crate::state_root().map(|root| root.join("pins").join(file_name))
}

/// Lowercase hex SHA-256 over each part's bytes followed by one 0 byte.
pub fn digest_of(parts: &[&str]) -> String {
    let mut buffer: Vec<u8> = Vec::new();
    for part in parts {
        buffer.extend_from_slice(part.as_bytes());
        buffer.push(0);
    }
    rightkit_fs::digest::sha256_bytes(&buffer)
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn digest_of_bytes(bytes: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(bytes)
}

/// Read the store. A missing file is an empty store; `None` means it exists but cannot be read,
/// is not valid JSON or has a different schema.
fn load(store: &Path) -> Option<Store> {
    match std::fs::read(store) {
        Ok(bytes) => {
            let parsed = serde_json::from_slice::<Store>(&bytes).ok()?;
            (parsed.schema == schema_name()).then_some(parsed)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Store {
            schema: schema_name(),
            pins: BTreeMap::new(),
        }),
        Err(_) => None,
    }
}

/// Write the store atomically: a temp file in the same folder, then a rename over the store.
fn write(store: &Path, data: &Store) -> bool {
    let Ok(text) = serde_json::to_string_pretty(data) else {
        return false;
    };
    if let Some(parent) = store.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let file_name = store
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = store.with_file_name(format!("{file_name}.{}.tmp", std::process::id()));
    if std::fs::write(&temp, text).is_err() {
        let _ = std::fs::remove_file(&temp);
        return false;
    }
    if std::fs::rename(&temp, store).is_err() {
        let _ = std::fs::remove_file(&temp);
        return false;
    }
    true
}

/// Check `digest` against the pin of `key` in `scope`, pinning it when none exists.
pub fn check_or_pin(store: &Path, scope: &str, key: &str, digest: &str) -> PinCheck {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut data) = load(store) else {
        return PinCheck::Changed {
            pinned: "pin store unreadable".to_string(),
        };
    };
    match data.pins.get(scope).and_then(|keys| keys.get(key)) {
        Some(pinned) if pinned == digest => PinCheck::Match,
        Some(pinned) => PinCheck::Changed {
            pinned: pinned.clone(),
        },
        None => {
            data.pins
                .entry(scope.to_string())
                .or_default()
                .insert(key.to_string(), digest.to_string());
            if write(store, &data) {
                PinCheck::FirstSeen
            } else {
                PinCheck::Unavailable
            }
        }
    }
}

/// Remove every pin of `scope`; returns how many pins were removed.
pub fn clear_scope(store: &Path, scope: &str) -> usize {
    clear_where(store, |name| name == scope)
}

/// Remove the scope equal to `prefix` and every scope that starts with `prefix` followed by `/`;
/// returns how many pins were removed.
pub fn clear_scopes_under(store: &Path, prefix: &str) -> usize {
    let nested = format!("{prefix}/");
    clear_where(store, |name| name == prefix || name.starts_with(&nested))
}

fn clear_where(store: &Path, matches: impl Fn(&str) -> bool) -> usize {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut data) = load(store) else {
        return 0;
    };
    let doomed: Vec<String> = data
        .pins
        .keys()
        .filter(|name| matches(name))
        .cloned()
        .collect();
    let mut removed = 0;
    for name in doomed {
        if let Some(keys) = data.pins.remove(&name) {
            removed += keys.len();
        }
    }
    if removed > 0 && !write(store, &data) {
        return 0;
    }
    removed
}
