//! rightkit-sync: end-to-end record encryption: a record is sealed (its plaintext no longer appears in the sealed bytes), opened
//! back to the same fields, and refused under a different key or after one byte of the ciphertext changes.
use std::sync::Arc;

use rightkit_sync::cipher::{CipherKey, RecordCipher, StaticKeys, ENC_FIELD};
use rightkit_sync::{FieldValue, RawRecord};
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let mut record = RawRecord::new("Note", "n1");
    record.set("id", FieldValue::Text("n1".into()));
    record.set("text", FieldValue::Text("secret transcript".into()));
    record.set("count", FieldValue::I64(-7));
    let cipher = RecordCipher::new("zone", Arc::new(StaticKeys::new(1, CipherKey::from_bytes([7; 32]))));
    let sealed = cipher.seal(&record).map_err(|e| format!("seal: {e}"))?;
    let bytes = match sealed.get(ENC_FIELD) {
        Some(FieldValue::Bytes(b)) => b.clone(),
        other => return Err(format!("no sealed field: {other:?}")),
    };
    if bytes.windows(b"secret transcript".len()).any(|w| w == b"secret transcript") {
        return Err("the plaintext appears in the sealed bytes".into());
    }
    let opened = cipher.open(&sealed).map_err(|e| format!("open: {e:?}"))?;
    if opened.get("text") != Some(&FieldValue::Text("secret transcript".into())) || opened.get("count") != Some(&FieldValue::I64(-7)) {
        return Err("the opened record differs from the original".into());
    }
    let other = RecordCipher::new("zone", Arc::new(StaticKeys::new(1, CipherKey::from_bytes([8; 32]))));
    let wrong_key_refused = other.open(&sealed).is_err();
    let mut tampered = sealed.clone();
    let mut t = bytes.clone();
    let last = t.len() - 1;
    t[last] ^= 1;
    tampered.set(ENC_FIELD, FieldValue::Bytes(t));
    let tamper_refused = cipher.open(&tampered).is_err();
    if !wrong_key_refused || !tamper_refused {
        return Err(format!("wrong key refused {wrong_key_refused}, tampered ciphertext refused {tamper_refused}"));
    }
    Ok(json!({ "sealed_bytes": bytes.len(), "wrong_key_refused": true, "tamper_refused": true }))
}
