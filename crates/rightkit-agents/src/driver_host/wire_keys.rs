//! The key of the host session id in the persisted and wire records of the driver host.
//!
//! [`DriverBinding`](crate::driver_host::DriverBinding),
//! [`ClaudeHarnessBindingRecord`](crate::driver_host::claude_harness::ClaudeHarnessBindingRecord)
//! and [`HarnessProgressEvent`](crate::driver_host::claude_harness::HarnessProgressEvent) carry the
//! id of the host session. The default host writes it under `hostSessionId`. A host that named
//! itself writes it under `<host slug>SessionId` (`coderightSessionId` for the host `CODERIGHT`),
//! which is the key its files and events carried before it used this crate, and it reads that key
//! as well as `hostSessionId`, so an existing record keeps loading. The key is worked out at every
//! read and write from `rightkit_agent_config::host_slug`, never cached, so a host that names
//! itself after an earlier call still gets its own key afterwards.
//!
//! A record that carries both keys with different values is refused when read (the error names
//! both keys); equal values are accepted and the record keeps one id. A record with neither key
//! is refused by the derive as before.
//!
//! A host that named itself serialises a record through a JSON value. The order of the keys then
//! depends on how `serde_json` is built in the final binary: with its `preserve_order` feature
//! (any crate in the build may enable it) they come out in the derived order with the renamed key
//! last, without it in alphabetical order. The content is the same either way and nothing here
//! reads or compares the order.

/// The key the default host reads and writes, and the key every record is derived with.
pub(crate) const DEFAULT_SESSION_ID_KEY: &str = "hostSessionId";

/// The key a host that named itself writes, `None` for the default host.
pub(crate) fn host_session_id_key() -> Option<String> {
    if crate::driver_host::host_names::is_default_host() {
        None
    } else {
        Some(format!("{}SessionId", rightkit_agent_config::host_slug()))
    }
}

/// `value` with the derived key moved to `key`: what a host that named itself writes.
pub(crate) fn rename_to_host_key(mut value: serde_json::Value, key: &str) -> serde_json::Value {
    if let serde_json::Value::Object(map) = &mut value {
        if let Some(session_id) = map.remove(DEFAULT_SESSION_ID_KEY) {
            map.insert(key.to_string(), session_id);
        }
    }
    value
}

/// `value` with the host's key moved to the derived key: what a host that named itself reads. A
/// record that carries both keys must carry the same id under each (the host's key is then
/// dropped); different ids are an error naming both keys.
pub(crate) fn rename_to_default_key(
    mut value: serde_json::Value,
    key: &str,
) -> Result<serde_json::Value, String> {
    if key == DEFAULT_SESSION_ID_KEY {
        return Ok(value);
    }
    if let serde_json::Value::Object(map) = &mut value {
        if let Some(host_id) = map.remove(key) {
            match map.get(DEFAULT_SESSION_ID_KEY) {
                None => {
                    map.insert(DEFAULT_SESSION_ID_KEY.to_string(), host_id);
                }
                Some(default_id) if *default_id == host_id => {}
                Some(_) => {
                    return Err(format!(
                        "the record carries both `{DEFAULT_SESSION_ID_KEY}` and `{key}` with different values"
                    ));
                }
            }
        }
    }
    Ok(value)
}

/// Implements `Serialize` and `Deserialize` for a record whose derive is
/// `#[serde(remote = "Self")]` (the derive then generates the inherent `serialize` and
/// `deserialize` functions, which this macro calls): the default host runs them as they are, a
/// host that named itself moves the host session id key as described in the module docs.
macro_rules! session_keyed_serde {
    ($record:ident) => {
        impl serde::Serialize for $record {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match $crate::driver_host::wire_keys::host_session_id_key() {
                    None => $record::serialize(self, serializer),
                    Some(key) => {
                        let value = $record::serialize(self, serde_json::value::Serializer)
                            .map_err(<S::Error as serde::ser::Error>::custom)?;
                        serde::Serialize::serialize(
                            &$crate::driver_host::wire_keys::rename_to_host_key(value, &key),
                            serializer,
                        )
                    }
                }
            }
        }

        impl<'de> serde::Deserialize<'de> for $record {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                match $crate::driver_host::wire_keys::host_session_id_key() {
                    None => $record::deserialize(deserializer),
                    Some(key) => {
                        let value =
                            <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
                        let value =
                            $crate::driver_host::wire_keys::rename_to_default_key(value, &key)
                                .map_err(<D::Error as serde::de::Error>::custom)?;
                        $record::deserialize(value).map_err(<D::Error as serde::de::Error>::custom)
                    }
                }
            }
        }
    };
}

pub(crate) use session_keyed_serde;
