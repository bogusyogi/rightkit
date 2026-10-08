//! Per-field redaction, applied before any event is persisted. A field is one
//! of the text-bearing parts of a record-event. Its mode is `full`, `hash`
//! (label-salted digest) or `omit`. A field the policy does not name is `omit`,
//! so the default fails closed.

use std::collections::BTreeMap;

use rightkit_contracts::types::record_event::{PolicyRef, RecordEvent, Redacted};
use serde_json::{json, Value};

use crate::error::{Code, RecordError, Result};
use crate::hash::salted_digest;

pub const FIELD_TARGET_IDENTITY: &str = "target.identity";
pub const FIELD_TARGET_LABEL: &str = "target.label";
pub const FIELD_TEXT: &str = "text";
pub const FIELDS: [&str; 3] = [FIELD_TARGET_IDENTITY, FIELD_TARGET_LABEL, FIELD_TEXT];

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

    pub fn mode(&self, field: &str) -> FieldMode {
        self.fields.get(field).copied().unwrap_or(FieldMode::Omit)
    }

    pub fn to_json(&self) -> Value {
        let fields: serde_json::Map<String, Value> = FIELDS
            .iter()
            .map(|f| (f.to_string(), json!(self.mode(f).as_str())))
            .collect();
        json!({
            "policyId": self.policy_id,
            "policyVersion": self.policy_version,
            "fields": fields,
        })
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
    pub fn apply(&self, event: &mut RecordEvent) {
        if let Some(target) = event.target.as_mut() {
            if let Some(identity) = target.identity.take() {
                target.identity = Some(self.redact(FIELD_TARGET_IDENTITY, identity));
            }
            if let Some(label) = target.label.take() {
                target.label = Some(self.redact(FIELD_TARGET_LABEL, label));
            }
        }
        if let Some(text) = event.text.take() {
            event.text = Some(self.redact(FIELD_TEXT, text));
        }
        event.redaction_policy = PolicyRef {
            policy_id: self.policy_id.clone(),
            policy_version: self.policy_version.clone(),
        };
    }

    fn redact(&self, field: &str, value: Redacted) -> Redacted {
        match (self.mode(field), value) {
            (FieldMode::Full, value) => value,
            (FieldMode::Omit, _) => Redacted::Omit,
            (FieldMode::Hash, Redacted::Full { value }) => Redacted::Hash {
                digest: salted_digest(&salt_id(field), &value),
                salt_id: salt_id(field),
            },
            // Already hashed or omitted by the caller: keep it.
            (FieldMode::Hash, value) => value,
        }
    }
}

fn salt_id(field: &str) -> String {
    format!("label:{field}")
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
    fn policy_round_trips_through_json() {
        let strict = RedactionPolicy::strict();
        assert_eq!(RedactionPolicy::from_json(&strict.to_json()), Some(strict));
    }
}
