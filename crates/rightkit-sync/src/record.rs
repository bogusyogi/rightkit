//! Transport-neutral record model. Field keys `id`, `createdAt`, `updatedAt`
//! and `deletedAt` are the shared wire contract (see API.md).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod field {
    pub const ID: &str = "id";
    pub const CREATED_AT: &str = "createdAt";
    pub const UPDATED_AT: &str = "updatedAt";
    pub const DELETED_AT: &str = "deletedAt";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldValue {
    Text(String),
    I64(i64),
    F64(f64),
    /// Unix epoch milliseconds.
    Date(i64),
    /// Out-of-line body (CKAsset on Apple platforms).
    Bytes(Vec<u8>),
}

impl FieldValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_date_ms(&self) -> Option<i64> {
        match self {
            Self::Date(ms) => Some(*ms),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::F64(v) => Some(*v),
            Self::I64(v) => Some(*v as f64),
            _ => None,
        }
    }
    /// Inline text or UTF-8 asset bytes.
    pub fn as_text_or_bytes(&self) -> Option<String> {
        match self {
            Self::Text(s) => Some(s.clone()),
            Self::Bytes(b) => String::from_utf8(b.clone()).ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRecord {
    pub record_type: String,
    pub record_name: String,
    pub fields: BTreeMap<String, FieldValue>,
    /// Server modification date, when the transport knows it (decode fallback).
    pub modified_ms: Option<i64>,
    /// Server creation date, when the transport knows it (decode fallback).
    pub created_ms: Option<i64>,
    /// Opaque server change tag. Reusing a fetched record keeps it, so
    /// updates modify instead of colliding as false inserts.
    pub change_tag: Option<String>,
}

impl RawRecord {
    pub fn new(record_type: &str, record_name: &str) -> Self {
        Self {
            record_type: record_type.to_string(),
            record_name: record_name.to_string(),
            fields: BTreeMap::new(),
            modified_ms: None,
            created_ms: None,
            change_tag: None,
        }
    }
    pub fn get(&self, key: &str) -> Option<&FieldValue> {
        self.fields.get(key)
    }
    pub fn text(&self, key: &str) -> Option<String> {
        self.fields.get(key)?.as_str().map(str::to_string)
    }
    pub fn date_ms(&self, key: &str) -> Option<i64> {
        self.fields.get(key)?.as_date_ms()
    }
    pub fn set(&mut self, key: &str, value: FieldValue) {
        self.fields.insert(key.to_string(), value);
    }
    /// Set when `Some`, clear when `None` (`.allKeys` semantics).
    pub fn set_opt(&mut self, key: &str, value: Option<FieldValue>) {
        match value {
            Some(v) => self.set(key, v),
            None => {
                self.fields.remove(key);
            }
        }
    }
}

/// Tombstone convention shared with HeardRight iOS: keep `id`, `updatedAt`
/// (= deletion time) and `deletedAt`; clear every other field except
/// `keep`. Decoders must not require any other field on a tombstone.
pub fn write_tombstone(record: &mut RawRecord, id: &str, deleted_at_ms: i64, keep: &[&str]) {
    record.fields.retain(|k, _| keep.contains(&k.as_str()));
    record.set(field::ID, FieldValue::Text(id.to_string()));
    record.set(field::UPDATED_AT, FieldValue::Date(deleted_at_ms));
    record.set(field::DELETED_AT, FieldValue::Date(deleted_at_ms));
}

/// `(id, deletedAt)` when the record is a tombstone. The id falls back to
/// the record name for legacy tombstones that lost their `id` field.
pub fn read_tombstone(record: &RawRecord) -> Option<(String, i64)> {
    let deleted_at = record.date_ms(field::DELETED_AT)?;
    let id = record.text(field::ID).unwrap_or_else(|| record.record_name.clone());
    Some((id, deleted_at))
}
