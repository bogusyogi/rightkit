//! The engine's own copy of the owner-only security settings (`settings::PROTECTED_KEYS`).
//!
//! `owner app-state.json` is a file any process of the same user can write, so a gate read from it
//! proves nothing. The engine therefore keeps the protected values itself and ignores the file for
//! them (`Settings::load` calls `apply`):
//!
//! * **Who writes.** Only this module, through `set`, which `owner_settings::request` calls after
//!   the owner path (`remote.settings.set`: the hub's or owner app's executable verified, and the
//!   The host app PIN for a change that loosens a gate). The owner app still receives each change through
//!   `remote.settings.take` and applies it to its own preferences and to the file it publishes, but
//!   that file and those preferences are a display of the value, not the gate.
//! * **Where it lives.** One item in the OS credential store (`rightkit-credentials`, service
//!   `pulse.protected-settings`), created by the engine with the store's default access control.
//!   A same-user process can still delete it or write one of its own, so the engine does not trust
//!   an item by name (`protected_guard.rs`): on macOS it refuses an item whose keychain partition
//!   list is not the one the host app's signed code gets, and on Windows the blob carries an HMAC whose key
//!   sits in a DPAPI-protected file (wrapped with PIN-derived entropy once a PIN is set). macOS
//!   carries the same HMAC seal (key in `protected-settings.seal`, wrapped with a PBKDF2 key from the
//!   PIN once one is set), because a same-user process can make a keychain of its own with the right
//!   partition list and move it onto the login keychain's path. A blob whose seal is not proven is
//!   read through `tighten_only`: closed defaults, plus only the values that are safer than the
//!   defaults. With a PIN set, loosened gates read off after every engine restart until the PIN has
//!   been verified once (`pin_verified`); Touch ID or Windows Hello alone does not reopen them.
//! * **First run.** When there is no item and no marker file, the store starts at the closed
//!   defaults, stores them once, and writes the marker file `protected-settings.marker` beside the
//!   owner app's data. A missing item with the marker present is a deleted item: nothing is stored
//!   again and the engine fails closed until the owner recovers it (`recover`, after Touch ID or
//!   Windows Hello in owner app).
//! * **Fail closed.** When the item cannot be read, is not trusted or is not valid, `values` is
//!   `None` and `apply` leaves every protected key out: switches are off, lists are empty,
//!   `chatUnlock` is on, and `phoneChatReadAll` is on (the owner's own paired phone reads every
//!   chat; only that phone gains, #579 D9).
//! * **Other processes.** The gateway and the CLI run tools in their own process and ask the
//!   resident engine (`remote.settings.protected`), after the OS has said the answering process runs
//!   the engine's executable (`approvals::ask_engine_verified`); an engine that is not running, or
//!   not the engine, is a fail closed.
//! * **The PIN marker.** The item also records that a PIN was set, so deleting the PIN's own
//!   credential item does not let the PIN be reset without the old one (`unlock.rs`). This holds
//!   only while the engine's item survives: deleting the item, the marker file and the PIN item together is a first run with
//!   `pin_set` false, so the PIN reads as unset and can be set again. Nothing loosens (every gate is
//!   back at its default, off), but the PIN is then unset, not lost.

use super::protected_guard::{self, Seal};
use super::{Refusal, Result, approvals, owner_settings, settings};
use serde_json::{Map, Value, json};
use std::sync::Mutex;

const SERVICE: &str = "pulse.protected-settings";
const ACCOUNT: &str = "values-v2";
const MARKER_FILE: &str = "protected-settings.marker";
/// The most the item may hold; a larger or malformed item is not trusted.
const MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Default)]
struct Stored {
    values: Map<String, Value>,
    pin_set: bool,
    /// The blob's seal was not proven, so `values` went through `tighten_only` (macOS, Windows).
    unproven: bool,
}

/// The engine's copy, once loaded. Only the engine process fills it.
static CACHE: Mutex<Option<Stored>> = Mutex::new(None);

fn unavailable(why: impl std::fmt::Display) -> Refusal {
    Refusal::new(
        "settings_unavailable",
        format!(
            "the app's own copy of the security settings cannot be read ({why}), so every protected gate stays off until it can"
        ),
    )
}

fn open() -> std::result::Result<rightkit_credentials::Store, String> {
    let namespace =
        rightkit_credentials::Namespace::new(SERVICE.to_string()).map_err(|e| e.to_string())?;
    let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
    if !store.is_durable() {
        return Err("the OS credential store is guarded or unavailable".into());
    }
    Ok(store)
}

fn marker_path() -> Option<std::path::PathBuf> {
    rightkit_app_host::state_directory()
        .ok()
        .map(|dir| dir.join(MARKER_FILE))
}

/// Keeps only the values that are no looser than the defaults (gates off, `chatUnlock` on, no
/// roots, no agent path). What a tampered, recreated or migrated blob can say is bounded by this.
fn tighten_only(values: Map<String, Value>) -> Map<String, Value> {
    values
        .into_iter()
        .filter(|(key, value)| !owner_settings::loosens(key, value, None))
        .collect()
}

/// The bytes the seal covers: the version, the PIN flag and every value, keys in sorted order.
fn seal_message(pin_set: bool, values: &Map<String, Value>) -> String {
    let mut keys: Vec<&String> = values.keys().collect();
    keys.sort();
    let mut out = format!("pulse.protected.v2|{pin_set}\n");
    for key in keys {
        out.push_str(&format!("{key}={}\n", values[key]));
    }
    out
}

fn parse(text: &str) -> std::result::Result<Stored, String> {
    if text.len() > MAX_BYTES {
        return Err("the stored settings are too large".into());
    }
    let damaged = || "the stored settings are damaged".to_string();
    let value: Value = serde_json::from_str(text).map_err(|_| damaged())?;
    if value["v"] != 2 {
        return Err("the stored settings are a version this the app does not know".into());
    }
    // The whole set is shape-checked: one key that is not protected or not well formed drops it all.
    let mut values = Map::new();
    for (key, raw) in value["values"].as_object().ok_or_else(damaged)? {
        if !settings::is_protected(key) {
            return Err(damaged());
        }
        let checked = owner_settings::check_shape(key, raw).map_err(|_| damaged())?;
        values.insert(key.clone(), checked);
    }
    let pin_set = value["pinSet"].as_bool() == Some(true);
    let message = seal_message(pin_set, &values);
    // A loosened gate only ever exists under a PIN (`owner_settings::request`), so the seal counts
    // only when its key came from the PIN in this process. A clear-key seal file planted by another
    // process of the user (with `pinSet` false and the PIN item deleted) never does.
    let proven = matches!(
        protected_guard::check_seal(message.as_bytes(), value["mac"].as_str()),
        Seal::Valid
    ) && protected_guard::key_from_pin();
    Ok(Stored {
        values: if proven { values } else { tighten_only(values) },
        // A PIN-bound seal also proves a PIN was set, whatever an unsealed blob says.
        pin_set: pin_set || protected_guard::pin_bound(),
        unproven: !proven,
    })
}

fn persist(
    store: &rightkit_credentials::Store,
    stored: &Stored,
) -> std::result::Result<(), String> {
    let message = seal_message(stored.pin_set, &stored.values);
    let mut blob = json!({ "v": 2, "values": stored.values, "pinSet": stored.pin_set });
    if let Some(mac) = protected_guard::make_seal(message.as_bytes(), stored.pin_set) {
        blob["mac"] = Value::from(mac);
    }
    let text = blob.to_string();
    if text.len() > MAX_BYTES {
        return Err("the settings are too large to store".into());
    }
    store.set_str(ACCOUNT, &text).map_err(|e| e.to_string())
}

/// Reads the item; a first run stores the closed defaults. An error is a fail closed.
fn load_stored() -> std::result::Result<Stored, String> {
    let store = open()?;
    // On macOS the read itself is pinned to the login keychain and the item is checked before and
    // after it (`protected_guard::read_item`); elsewhere it goes through the credential store.
    let found = match protected_guard::read_item(SERVICE, ACCOUNT) {
        Some(read) => read?,
        None => match store.get(ACCOUNT) {
            Ok(Some(secret)) => Some(secret.expose_str().map_err(|e| e.to_string())?.to_owned()),
            Ok(None) => None,
            Err(e) => return Err(e.to_string()),
        },
    };
    match found {
        Some(text) => parse(&text),
        None => {
            // No item. With the marker present it was deleted, which is not a first run.
            let marker = marker_path().ok_or("the data folder is unknown")?;
            if marker.exists() {
                return Err("the engine's stored settings were deleted".into());
            }
            let stored = Stored {
                values: Map::new(),
                pin_set: super::unlock::pin_item_present(),
                unproven: false,
            };
            persist(&store, &stored)?;
            protected_guard::check_item(SERVICE, ACCOUNT)?;
            std::fs::write(&marker, b"1")
                .map_err(|e| format!("the marker was not written: {e}"))?;
            Ok(stored)
        }
    }
}

/// Runs `f` on the engine's copy, loading it first when needed. Engine only.
fn with_loaded<T>(f: impl FnOnce(&mut Option<Stored>) -> Result<T>) -> Result<T> {
    if !approvals::is_engine_process() {
        return Err(Refusal::new(
            "approval_unavailable",
            "the security settings are held by the engine, and this process is not the engine",
        ));
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.is_none() {
        *cache = Some(load_stored().map_err(unavailable)?);
    }
    f(&mut cache)
}

/// The protected values, as the engine holds them. `None`: they cannot be read (fail closed). In
/// the engine this is its own copy; in any other process it asks the engine, which must be the
/// engine's own executable (`approvals::ask_engine_verified`).
pub fn values() -> Option<Map<String, Value>> {
    if approvals::is_engine_process() {
        return with_loaded(|cache| {
            Ok(cache.as_ref().map(|s| s.values.clone()).unwrap_or_default())
        })
        .ok();
    }
    let answer = approvals::ask_engine_verified("remote.settings.protected", json!({})).ok()?;
    answer["values"].as_object().cloned()
}

/// The engine's answer to `remote.settings.protected`: `{"values": {...}}`, or the refusal.
pub fn serve() -> Result<Value> {
    with_loaded(|cache| {
        let values = cache.as_ref().map(|s| s.values.clone()).unwrap_or_default();
        Ok(json!({ "values": values }))
    })
}

/// Replaces the protected keys of `published` (a `settings` object) with the engine's values.
/// Anything the file said about a protected key is dropped first, so an unreadable engine copy
/// leaves them at their defaults (off).
pub fn apply(published: &mut Value) {
    if !published.is_object() {
        *published = Value::Object(Map::new());
    }
    let Some(object) = published.as_object_mut() else {
        return;
    };
    object.retain(|key, _| !settings::is_protected(key));
    if let Some(values) = values() {
        for (key, value) in values {
            if settings::is_protected(&key) {
                object.insert(key, value);
            }
        }
    }
}

/// Stores one protected value. Engine only, after the owner path has accepted it
/// (`owner_settings::request`). The credential store is written first; the engine's copy changes
/// only when that succeeded.
pub fn set(key: &str, value: &Value) -> Result<()> {
    if !settings::is_protected(key) {
        return Err(Refusal::new(
            "invalid_request",
            "that setting is not an owner-only setting",
        ));
    }
    with_loaded(|cache| {
        let mut next = cache.clone().unwrap_or_default();
        next.values.insert(key.to_string(), value.clone());
        let store = open().map_err(unavailable)?;
        persist(&store, &next).map_err(unavailable)?;
        *cache = Some(next);
        Ok(())
    })
}

/// Every protected value the engine holds, for `owner_settings::seed_queue`. Engine only.
pub fn all() -> Result<Vec<(String, Value)>> {
    with_loaded(|cache| {
        Ok(cache
            .as_ref()
            .map(|s| {
                s.values
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default())
    })
}

/// Whether the host app PIN was ever set, as the engine recorded it. Engine only.
pub fn pin_was_set() -> Result<bool> {
    with_loaded(|cache| Ok(cache.as_ref().is_some_and(|s| s.pin_set)))
}

/// Records that a PIN is set or changed, and re-seals the item under the new PIN (macOS, Windows).
/// Engine only.
pub fn mark_pin_set(pin: &str) -> Result<()> {
    with_loaded(|cache| {
        let mut next = cache.clone().unwrap_or_default();
        next.pin_set = true;
        protected_guard::pin_changed(pin);
        let store = open().map_err(unavailable)?;
        persist(&store, &next).map_err(unavailable)?;
        *cache = Some(next);
        Ok(())
    })
}

/// The owner's PIN was just verified. On macOS and Windows that unwraps the seal key, and a copy that was read
/// as closed defaults because its seal could not be checked is read again. Engine only; a failure
/// leaves the copy as it was.
pub fn pin_verified(pin: &str) {
    if !approvals::is_engine_process() {
        return;
    }
    protected_guard::pin_entered(pin);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.as_ref().is_some_and(|s| s.unproven)
        && let Ok(reloaded) = load_stored()
        && !reloaded.unproven
    {
        *cache = Some(reloaded);
    }
}

/// The owner recovers an unreadable or untrusted item (deleted, replaced from outside, or a marker
/// without an item): the item and the marker are removed and the store starts again from the
/// closed defaults, and every loosened gate is lost, so this can only make things safer. The caller must have proved the owner's presence to the system
/// (Touch ID, Windows Hello) in owner app. Engine only.
pub fn recover() -> Result<()> {
    if !approvals::is_engine_process() {
        return Err(Refusal::new(
            "approval_unavailable",
            "the security settings are held by the engine, and this process is not the engine",
        ));
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let store = open().map_err(unavailable)?;
    store
        .delete(ACCOUNT)
        .map_err(|e| unavailable(format!("the stored item could not be removed: {e}; remove the keychain or credential item {SERVICE} by hand, then try again")))?;
    if let Some(marker) = marker_path() {
        let _ = std::fs::remove_file(marker);
    }
    protected_guard::reset_seal();
    let fresh = Stored {
        values: Map::new(),
        pin_set: super::unlock::pin_item_present(),
        unproven: false,
    };
    persist(&store, &fresh).map_err(unavailable)?;
    protected_guard::check_item(SERVICE, ACCOUNT).map_err(unavailable)?;
    let marker = marker_path().ok_or_else(|| unavailable("the data folder is unknown"))?;
    std::fs::write(marker, b"1")
        .map_err(|e| unavailable(format!("the marker was not written: {e}")))?;
    *cache = Some(fresh);
    Ok(())
}
