//! Wipe OAuth-owned transport/JSON copies on success, early return & unwind.
//! Transport-library copies and rightkit-http's opaque Secret are separate boundaries.
use rightkit_http::{Request, Response};
use serde_json::Value;
use std::ops::Deref;
use zeroize::Zeroize;

pub(super) struct SensitiveRequest(pub Request);
impl Drop for SensitiveRequest {
    fn drop(&mut self) {
        self.0.body.zeroize();
        for (_, value) in &mut self.0.headers {
            value.zeroize();
        }
    }
}
pub(super) struct SensitiveResponse(pub Response);
impl Drop for SensitiveResponse {
    fn drop(&mut self) {
        self.0.body.zeroize();
        for (_, value) in &mut self.0.headers {
            value.zeroize();
        }
    }
}

pub(crate) struct SensitiveJson(Value);
pub(crate) fn from_value(value: Value) -> SensitiveJson {
    SensitiveJson(value)
}
impl SensitiveJson {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes).map(Self)
    }
}
impl Deref for SensitiveJson {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.0
    }
}
impl Drop for SensitiveJson {
    fn drop(&mut self) {
        wipe_json(&mut self.0);
    }
}
fn wipe_json(value: &mut Value) {
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(wipe_json),
        Value::Object(values) => {
            for (mut key, mut value) in std::mem::take(values) {
                key.zeroize();
                wipe_json(&mut value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wipes_nested_json_strings_and_keys() {
        let mut value = serde_json::json!({"echoed-secret-key": ["access-secret", {"refresh": "refresh-secret"}]});
        wipe_json(&mut value);
        assert_eq!(value, serde_json::json!({}));
        let mut value = serde_json::json!(["access-secret", ["refresh-secret"]]);
        wipe_json(&mut value);
        assert_eq!(value, serde_json::json!(["", [""]]));
    }
}
