use crate::digest_str;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
pub const COMPRESSION_RECEIPT_SCHEMA_VERSION: u32 = 1;
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct NonEmptyString(String);

impl NonEmptyString {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty() {
            Err("value must not be empty")
        } else {
            Ok(Self(value))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for NonEmptyString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpanV1 {
    pub start: u32,
    pub end: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DroppedSpanV1 {
    pub span: SpanV1,
    pub reason: NonEmptyString,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompressionReceiptV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    #[serde(deserialize_with = "deserialize_sha256")]
    pub original_hash: String,
    pub transform: NonEmptyString,
    pub transform_version: NonEmptyString,
    pub kept_spans: Vec<SpanV1>,
    pub protected_spans: Vec<SpanV1>,
    pub dropped: Vec<DroppedSpanV1>,
    pub resolver: NonEmptyString,
    pub risk: NonEmptyString,
    pub token_delta: i64,
}

fn deserialize_schema_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let value = u32::deserialize(deserializer)?;
    if value == COMPRESSION_RECEIPT_SCHEMA_VERSION {
        Ok(value)
    } else {
        Err(D::Error::custom("schemaVersion must equal 1"))
    }
}

fn deserialize_sha256<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    let valid = value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        Ok(value)
    } else {
        Err(D::Error::custom(
            "originalHash must be sha256:<64 lowercase hex>",
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImmutableSourceReason {
    #[error("resolver_unavailable")]
    ResolverUnavailable,
    #[error("original_hash_mismatch")]
    OriginalHashMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("immutable source unavailable for {resolver} ({original_hash}): {reason}")]
pub struct ImmutableSourceError {
    pub resolver: String,
    pub original_hash: String,
    pub reason: ImmutableSourceReason,
}

impl CompressionReceiptV1 {
    pub const SCHEMA_VERSION: u32 = COMPRESSION_RECEIPT_SCHEMA_VERSION;

    pub fn is_lossy(&self) -> bool {
        !self.dropped.is_empty()
    }

    pub fn recover_if_lossy<F>(&self, resolve: F) -> Result<Option<String>, ImmutableSourceError>
    where
        F: FnOnce(&str) -> Option<String>,
    {
        if !self.is_lossy() {
            return Ok(None);
        }
        let original = resolve(self.resolver.as_str()).ok_or_else(|| ImmutableSourceError {
            resolver: self.resolver.as_str().to_string(),
            original_hash: self.original_hash.clone(),
            reason: ImmutableSourceReason::ResolverUnavailable,
        })?;
        if digest_str(&original) != self.original_hash {
            return Err(ImmutableSourceError {
                resolver: self.resolver.as_str().to_string(),
                original_hash: self.original_hash.clone(),
                reason: ImmutableSourceReason::OriginalHashMismatch,
            });
        }
        Ok(Some(original))
    }
}
