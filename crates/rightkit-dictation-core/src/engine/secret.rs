//! Secret string wrapper for credentials that cross the shell -> sidecar IPC.
//!
//! `Debug` never prints the value, and the backing bytes are overwritten with
//! zeros on drop. `Serialize` emits the real value because the frame must
//! carry it on the private stdin pipe; callers must never route a serialized
//! frame to a log. There is deliberately no `Display` impl.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

/// Overwrite a string's bytes with zeros (volatile, so the stores are not
/// elided) and empty it. Zero bytes are valid UTF-8, so the `String`
/// invariant holds throughout.
pub fn wipe_string(value: &mut String) {
    // SAFETY: writing 0x00 to every byte keeps the buffer valid UTF-8.
    unsafe {
        for byte in value.as_bytes_mut() {
            std::ptr::write_volatile(byte, 0);
        }
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    value.clear();
}

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the secret. Use only at the point of use (an auth header).
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        wipe_string(&mut self.0);
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Serialize for SecretString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

/// Best-effort in-place wipe of every string inside a parsed JSON value.
pub fn wipe_json_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => wipe_string(text),
        serde_json::Value::Array(items) => items.iter_mut().for_each(wipe_json_strings),
        serde_json::Value::Object(map) => map.values_mut().for_each(wipe_json_strings),
        _ => {}
    }
}
