//! Per-field redaction, applied before any event is persisted. A field is one
//! of the text-bearing parts of a record-event. Its mode is `full`, `hash`
//! (keyed digest) or `omit`. A field the policy does not name is `omit`, so the
//! default fails closed.
//!
//! A `hash` field stores HMAC-SHA256 under a per-installation key that lives
//! outside the bundle (`rightkit-credentials`: OS credential store). The bundle
//! records only the key id. Equal values hash equal within one installation, and
//! a reader without the key cannot confirm a guess. Bundles written before keyed
//! hashing carry label-salted digests (`saltId` `label:<field>`); they are still
//! read and `bundle_verify` reports them as weak.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use rightkit_contracts::types::record_event::{PolicyRef, RecordEvent, Redacted};
use serde_json::{json, Value};

use crate::error::{Code, RecordError, Result};
use crate::hash::{hex_lower, hmac_sha256, keyed_digest};

pub const FIELD_TARGET_IDENTITY: &str = "target.identity";
pub const FIELD_TARGET_LABEL: &str = "target.label";
pub const FIELD_TEXT: &str = "text";
/// Prefix of the `saltId` a keyed digest carries; the key id follows.
pub const KEYED_SALT_PREFIX: &str = "hmac-sha256:";
/// Prefix of the `saltId` of the weak, label-salted digests older bundles carry.
pub const LEGACY_SALT_PREFIX: &str = "label:";
/// The credential-store account and service of the per-installation key.
pub const KEY_ACCOUNT: &str = "record.redaction-key.v1";
pub const KEY_SERVICE: &str = "dev.rightkit.record";
const KEY_ID_CONTEXT: &[u8] = b"rightkit-record.redaction-key-id.v1";
const MIN_KEY_BYTES: usize = 16;

pub const FIELDS: [&str; 3] = [FIELD_TARGET_IDENTITY, FIELD_TARGET_LABEL, FIELD_TEXT];

/// The key a `hash` field is computed under, and its public id. The id is derived
/// through a one-way MAC, so it names the key without revealing it.
pub struct HashKey {
    id: String,
    bytes: Vec<u8>,
}

impl HashKey {
    /// Fails for a key shorter than 16 bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<HashKey> {
        let bytes = bytes.into();
        if bytes.len() < MIN_KEY_BYTES {
            return Err(RecordError::new(
                Code::ArgInvalid,
                format!("the redaction key must be at least {MIN_KEY_BYTES} bytes"),
            ));
        }
        let id = format!(
            "k1-{}",
            hex_lower(&hmac_sha256(&bytes, KEY_ID_CONTEXT)[..8])
        );
        Ok(HashKey { id, bytes })
    }

    /// The public key id the bundle records.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The `saltId` an event carries for a digest under this key.
    pub fn salt_id(&self) -> String {
        format!("{KEYED_SALT_PREFIX}{}", self.id)
    }

    pub fn digest(&self, field: &str, value: &str) -> String {
        keyed_digest(&self.bytes, field, value)
    }
}

impl fmt::Debug for HashKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HashKey({})", self.id)
    }
}

impl Drop for HashKey {
    fn drop(&mut self) {
        self.bytes.iter_mut().for_each(|b| *b = 0);
    }
}

/// Where the per-installation redaction key comes from. Production uses
/// [`SecretsKeySource`]; tests inject [`FixedKeySource`].
pub trait KeySource: Send + Sync + fmt::Debug {
    fn key(&self) -> Result<HashKey>;
}

/// A fixed key, for tests and for callers that manage the key themselves.
#[derive(Clone)]
pub struct FixedKeySource(Vec<u8>);

impl FixedKeySource {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        FixedKeySource(bytes.into())
    }
}

impl fmt::Debug for FixedKeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FixedKeySource(..)")
    }
}

impl KeySource for FixedKeySource {
    fn key(&self) -> Result<HashKey> {
        HashKey::new(self.0.clone())
    }
}

/// The installation's key in the OS credential store (`rightkit-credentials`), minted
/// on first use. Fails closed: if the store is unavailable the take is refused
/// rather than hashed under a weaker key.
#[derive(Debug, Clone, Default)]
pub struct SecretsKeySource;

impl KeySource for SecretsKeySource {
    fn key(&self) -> Result<HashKey> {
        let unavailable = |what: &str, error: &dyn fmt::Display| {
            RecordError::new(
                Code::PermissionDenied,
                format!(
                    "the redaction key is unavailable ({what}: {error}); a hash field cannot be written"
                ),
            )
        };
        let namespace = rightkit_credentials::Namespace::new(KEY_SERVICE)
            .map_err(|e| unavailable("namespace", &e))?;
        let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
        let (secret, _) = store
            .get_or_create_key(KEY_ACCOUNT, 32)
            .map_err(|e| unavailable("credential store", &e))?;
        HashKey::new(secret.expose_bytes())
    }
}

/// A shared key source that compares by identity, so a request that carries one
/// can still derive `PartialEq`.
#[derive(Clone)]
pub struct KeyHandle(pub Arc<dyn KeySource>);

impl KeyHandle {
    pub fn new(source: impl KeySource + 'static) -> Self {
        KeyHandle(Arc::new(source))
    }

    /// The production default: the OS credential store.
    pub fn system() -> Self {
        KeyHandle::new(SecretsKeySource)
    }
}

impl fmt::Debug for KeyHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq for KeyHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldMode {
    Full,
    Hash,
    Omit,
}

impl FieldMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldMode::Full => "full",
            FieldMode::Hash => "hash",
            FieldMode::Omit => "omit",
        }
    }

    fn parse(text: &str) -> Option<FieldMode> {
        match text {
            "full" => Some(FieldMode::Full),
            "hash" => Some(FieldMode::Hash),
            "omit" => Some(FieldMode::Omit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionPolicy {
    policy_id: String,
    policy_version: String,
    fields: BTreeMap<String, FieldMode>,
}

impl RedactionPolicy {
    /// An empty policy: every field is `omit` until set.
    pub fn new(policy_id: impl Into<String>, policy_version: impl Into<String>) -> Self {
        RedactionPolicy {
            policy_id: policy_id.into(),
            policy_version: policy_version.into(),
            fields: BTreeMap::new(),
        }
    }

    pub fn with_field(mut self, field: &str, mode: FieldMode) -> Result<Self> {
        if !FIELDS.contains(&field) {
            return Err(RecordError::new(
                Code::ArgInvalid,
                format!("unknown redaction field '{field}'"),
            ));
        }
        self.fields.insert(field.to_string(), mode);
        Ok(self)
    }

    /// The default for recording: target identity and label are hashed, typed
    /// text is omitted.
    pub fn strict() -> Self {
        RedactionPolicy::new("redaction.rk3", "1.0.0")
            .with_field(FIELD_TARGET_IDENTITY, FieldMode::Hash)
            .and_then(|p| p.with_field(FIELD_TARGET_LABEL, FieldMode::Hash))
            .and_then(|p| p.with_field(FIELD_TEXT, FieldMode::Omit))
            .expect("the built-in field names are valid")
    }

    pub fn policy_id(&self) -> &str {
        &self.policy_id
    }

    pub fn policy_version(&self) -> &str {
        &self.policy_version
    }

    /// Whether any field is hashed, which is when a take needs the key.
    pub fn needs_key(&self) -> bool {
        FIELDS.iter().any(|f| self.mode(f) == FieldMode::Hash)
    }

    pub fn mode(&self, field: &str) -> FieldMode {
        self.fields.get(field).copied().unwrap_or(FieldMode::Omit)
    }

    pub fn to_json(&self) -> Value {
        self.to_json_with_key(None)
    }

    /// As [`RedactionPolicy::to_json`], recording the id (never the bytes) of the key
    /// the take's hashes use.
    pub fn to_json_with_key(&self, key: Option<&HashKey>) -> Value {
        let fields: serde_json::Map<String, Value> = FIELDS
            .iter()
            .map(|f| (f.to_string(), json!(self.mode(f).as_str())))
            .collect();
        let mut doc = json!({
            "policyId": self.policy_id,
            "policyVersion": self.policy_version,
            "fields": fields,
        });
        if let Some(key) = key {
            doc["hashAlgorithm"] = json!("hmac-sha256");
            doc["keyId"] = json!(key.id());
        }
        doc
    }

    /// Reads the policy back from scenario.json. Returns `None` for a malformed
    /// policy.
    pub fn from_json(value: &Value) -> Option<RedactionPolicy> {
        let mut policy = RedactionPolicy::new(
            value.get("policyId")?.as_str()?,
            value.get("policyVersion")?.as_str()?,
        );
        let fields = value.get("fields")?.as_object()?;
        for field in FIELDS {
            let mode = FieldMode::parse(fields.get(field)?.as_str()?)?;
            policy.fields.insert(field.to_string(), mode);
        }
        Some(policy)
    }

    /// Rewrites the redacted fields of an event in place and stamps the policy
    /// reference. Runs before the event is validated and written.
    pub fn apply(&self, event: &mut RecordEvent, key: Option<&HashKey>) {
        if let Some(target) = event.target.as_mut() {
            if let Some(identity) = target.identity.take() {
                target.identity = Some(self.redact(FIELD_TARGET_IDENTITY, identity, key));
            }
            if let Some(label) = target.label.take() {
                target.label = Some(self.redact(FIELD_TARGET_LABEL, label, key));
            }
        }
        if let Some(text) = event.text.take() {
            event.text = Some(self.redact(FIELD_TEXT, text, key));
        }
        event.redaction_policy = PolicyRef {
            policy_id: self.policy_id.clone(),
            policy_version: self.policy_version.clone(),
        };
    }

    fn redact(&self, field: &str, value: Redacted, key: Option<&HashKey>) -> Redacted {
        match (self.mode(field), value) {
            (FieldMode::Full, value) => value,
            (FieldMode::Omit, _) => Redacted::Omit,
            (FieldMode::Hash, Redacted::Full { value }) => match key {
                Some(key) => Redacted::Hash {
                    digest: key.digest(field, &value),
                    salt_id: key.salt_id(),
                },
                // No key: fail closed rather than store a guessable digest.
                None => Redacted::Omit,
            },
            // Already hashed or omitted by the caller: keep it.
            (FieldMode::Hash, value) => value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unnamed_fields_fail_closed() {
        assert_eq!(
            RedactionPolicy::new("p", "1").mode(FIELD_TEXT),
            FieldMode::Omit
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(RedactionPolicy::new("p", "1")
            .with_field("pixels", FieldMode::Full)
            .is_err());
    }

    #[test]
    fn keyed_hash_is_stable_per_key_and_never_plaintext() {
        let key = HashKey::new(vec![7u8; 32]).unwrap();
        let other = HashKey::new(vec![8u8; 32]).unwrap();
        assert_ne!(key.id(), other.id());
        assert!(key.id().starts_with("k1-"));
        let policy = RedactionPolicy::strict();
        let full = |v: &str| Redacted::Full { value: v.into() };
        let a = policy.redact(FIELD_TARGET_LABEL, full("Export"), Some(&key));
        let b = policy.redact(FIELD_TARGET_LABEL, full("Export"), Some(&key));
        let c = policy.redact(FIELD_TARGET_LABEL, full("Export"), Some(&other));
        assert_eq!(a, b);
        assert_ne!(a, c);
        match a {
            Redacted::Hash { digest, salt_id } => {
                assert!(!digest.contains("Export"));
                assert_eq!(salt_id, format!("hmac-sha256:{}", key.id()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_hash_field_without_a_key_is_omitted() {
        let full = Redacted::Full { value: "x".into() };
        assert_eq!(
            RedactionPolicy::strict().redact(FIELD_TARGET_LABEL, full, None),
            Redacted::Omit
        );
    }

    #[test]
    fn short_keys_are_refused() {
        assert!(HashKey::new(vec![1u8; 8]).is_err());
    }

    #[test]
    fn policy_round_trips_through_json() {
        let strict = RedactionPolicy::strict();
        assert_eq!(RedactionPolicy::from_json(&strict.to_json()), Some(strict));
    }
}
