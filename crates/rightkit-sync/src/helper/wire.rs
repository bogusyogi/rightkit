//! Line-JSON wire format of the `rightkit-sync-helper/1` protocol. One JSON
//! object per line, UTF-8, no other framing. See `API.md` section 4.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::SyncError;
use crate::record::{FieldValue, RawRecord};

pub const PROTOCOL_VERSION: u32 = 1;

/// Standard base64 (RFC 4648, padded). Tokens and byte fields cross the wire
/// as base64 text. Kept in-crate on purpose: no workspace crate owns base64, and `rightkit-secrets`
/// and `rightkit-shell` carry their own small copies; the next owner of a shared helper replaces all three.
pub fn b64_encode(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn b64_decode(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for ch in text.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' | b' ' => continue,
            _ => return Err(format!("invalid base64 byte 0x{ch:02x}")),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
pub enum WireValue {
    Text(String),
    I64(i64),
    F64(f64),
    /// Unix epoch milliseconds.
    Date(i64),
    /// Base64 bytes; a CKAsset on the CloudKit side.
    Bytes(String),
    Strings(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRecord {
    #[serde(rename = "type")]
    pub record_type: String,
    pub name: String,
    #[serde(default)]
    pub fields: BTreeMap<String, WireValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_tag: Option<String>,
}

impl From<&RawRecord> for WireRecord {
    fn from(r: &RawRecord) -> Self {
        let fields = r
            .fields
            .iter()
            .map(|(k, v)| {
                let w = match v {
                    FieldValue::Text(s) => WireValue::Text(s.clone()),
                    FieldValue::I64(n) => WireValue::I64(*n),
                    FieldValue::F64(n) => WireValue::F64(*n),
                    FieldValue::Date(n) => WireValue::Date(*n),
                    FieldValue::Bytes(b) => WireValue::Bytes(b64_encode(b)),
                    FieldValue::Strings(s) => WireValue::Strings(s.clone()),
                };
                (k.clone(), w)
            })
            .collect();
        Self {
            record_type: r.record_type.clone(),
            name: r.record_name.clone(),
            fields,
            modified_ms: r.modified_ms,
            created_ms: r.created_ms,
            change_tag: r.change_tag.clone(),
        }
    }
}

impl WireRecord {
    pub fn into_raw(self) -> Result<RawRecord, String> {
        let mut fields = BTreeMap::new();
        for (k, v) in self.fields {
            let f = match v {
                WireValue::Text(s) => FieldValue::Text(s),
                WireValue::I64(n) => FieldValue::I64(n),
                WireValue::F64(n) => FieldValue::F64(n),
                WireValue::Date(n) => FieldValue::Date(n),
                WireValue::Bytes(b) => {
                    FieldValue::Bytes(b64_decode(&b).map_err(|e| format!("field {k}: {e}"))?)
                }
                WireValue::Strings(s) => FieldValue::Strings(s),
            };
            fields.insert(k, f);
        }
        Ok(RawRecord {
            record_type: self.record_type,
            record_name: self.name,
            fields,
            modified_ms: self.modified_ms,
            created_ms: self.created_ms,
            change_tag: self.change_tag,
        })
    }
}

/// Error kinds a helper reports. Anything unknown is treated as transient.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireError {
    pub kind: String,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<f64>,
}

impl WireError {
    pub fn new(kind: &str, message: impl Into<String>, retry_after_secs: Option<f64>) -> Self {
        Self {
            kind: kind.to_string(),
            message: message.into(),
            retry_after_secs,
        }
    }

    pub fn into_sync_error(self) -> SyncError {
        let hint = self.retry_after_secs.filter(|s| s.is_finite() && *s > 0.0);
        match self.kind.as_str() {
            "unavailable" | "not_authenticated" | "permission_denied" | "quota_exceeded" => {
                SyncError::Unavailable(self.message)
            }
            "token_expired" => SyncError::TokenExpired,
            "zone_missing" => SyncError::ZoneMissing,
            "scope_changed" => SyncError::Cancelled,
            "conflict" => SyncError::Conflict,
            "retry_after" | "network" | "busy" => SyncError::RetryAfter {
                secs: hint.unwrap_or(if self.kind == "network" { 15.0 } else { 30.0 }),
                detail: self.message,
            },
            _ => match hint {
                Some(secs) => SyncError::RetryAfter {
                    secs,
                    detail: self.message,
                },
                None => SyncError::Transient(self.message),
            },
        }
    }

    pub fn from_sync_error(e: &SyncError) -> Self {
        match e {
            SyncError::Unavailable(m) => Self::new("unavailable", m.clone(), None),
            SyncError::RetryAfter { secs, detail } => {
                Self::new("retry_after", detail.clone(), Some(*secs))
            }
            SyncError::TokenExpired => Self::new("token_expired", "change token expired", None),
            SyncError::ZoneMissing => Self::new("zone_missing", "zone missing", None),
            SyncError::Cancelled => Self::new("scope_changed", "account scope changed", None),
            SyncError::Conflict => Self::new("conflict", "server record changed", None),
            other => Self::new("transient", other.to_string(), None),
        }
    }
}

/// One response line. `rest` holds the command-specific members.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<WireError>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// One request line as the helper sees it.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub v: u32,
    pub id: u64,
    pub cmd: String,
    #[serde(flatten)]
    pub params: Map<String, Value>,
}

pub fn ok_line(id: u64, mut body: Map<String, Value>) -> String {
    body.insert("id".into(), id.into());
    body.insert("ok".into(), true.into());
    Value::Object(body).to_string()
}

pub fn err_line(id: u64, error: &WireError) -> String {
    let mut m = Map::new();
    m.insert("id".into(), id.into());
    m.insert("ok".into(), false.into());
    m.insert(
        "error".into(),
        serde_json::to_value(error).unwrap_or(Value::Null),
    );
    Value::Object(m).to_string()
}
