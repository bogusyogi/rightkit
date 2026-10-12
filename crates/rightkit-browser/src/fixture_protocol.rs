//! Closed, source-only protocol for browser fixtures.
//!
//! Fixture source digests hash compact JSON for `{"files":[...]}`. Files are
//! sorted by their validated UTF-8 path bytes; each object contains `body` as
//! an array of byte integers, `contentType`, and `path`. `serde_json::Value`
//! Objects are recursively rebuilt in lexicographic key order before compact
//! serialization, so this stays stable even when serde_json's preserve_order
//! feature is enabled. The resulting SHA-256 is lowercase hex prefixed by
//! `sha256:`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use thiserror::Error;
use url::Url;

pub const FIXTURE_SCHEMA: &str = "rightkit.browser.fixture-intent.v1";
pub(crate) const MAX_FIXTURE_FILES: usize = 64;
pub(crate) const MAX_FIXTURE_BYTES: usize = 16 * 1024 * 1024;

const MAX_STRING_BYTES: usize = 2048;
const MAX_ACTIONS: usize = 8;
const MAX_EXPIRY_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureFile {
    pub path: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct FixtureBundle {
    fixture_id: String,
    source_digest: String,
    files: Vec<FixtureFile>,
}

impl FixtureBundle {
    pub fn verify(
        fixture_id: impl Into<String>,
        expected_source_digest: &str,
        mut files: Vec<FixtureFile>,
    ) -> Result<Self, FixtureError> {
        let fixture_id = fixture_id.into();
        validate_id("fixture_id", &fixture_id)?;
        validate_digest("fixture_source_digest", expected_source_digest)?;
        validate_files(&files)?;
        files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let source_digest = Self::source_digest(&files)?;
        if source_digest != expected_source_digest {
            return Err(FixtureError::Invalid(
                "fixture source digest does not match files".into(),
            ));
        }
        Ok(Self {
            fixture_id,
            source_digest,
            files,
        })
    }

    pub fn source_digest(files: &[FixtureFile]) -> Result<String, FixtureError> {
        validate_files(files)?;
        let mut ordered: Vec<&FixtureFile> = files.iter().collect();
        ordered.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let value = Value::Object(
            [(
                "files".to_owned(),
                Value::Array(
                    ordered
                        .into_iter()
                        .map(|file| {
                            serde_json::json!({
                            "body": &file.body,
                            "contentType": &file.content_type,
                            "path": &file.path,
                            })
                        })
                        .collect(),
                ),
            )]
            .into_iter()
            .collect(),
        );
        let bytes = canonical_json_bytes(value)?;
        Ok(format!("sha256:{}", hex(&Sha256::digest(bytes))))
    }

    pub fn id(&self) -> &str {
        &self.fixture_id
    }

    pub fn digest(&self) -> &str {
        &self.source_digest
    }

    pub(crate) fn files(&self) -> &[FixtureFile] {
        &self.files
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureIntent {
    pub schema: String,
    pub fixture_id: String,
    pub fixture_source_digest: String,
    pub probe_id: String,
    pub actions_digest: String,
    pub exact_url: String,
    pub actions: Vec<FixtureAction>,
    pub expires_after_ms: u64,
}

impl FixtureIntent {
    pub fn validate(&self, bundle: &FixtureBundle) -> Result<(), FixtureError> {
        if self.schema != FIXTURE_SCHEMA {
            return Err(FixtureError::Invalid("unsupported fixture schema".into()));
        }
        validate_id("fixture_id", &self.fixture_id)?;
        validate_id("probe_id", &self.probe_id)?;
        validate_digest("fixture_source_digest", &self.fixture_source_digest)?;
        validate_digest("actions_digest", &self.actions_digest)?;
        validate_text("exact_url", &self.exact_url)?;
        if self.actions.len() > MAX_ACTIONS {
            return Err(FixtureError::Invalid("too many fixture actions".into()));
        }
        if self.expires_after_ms == 0 || self.expires_after_ms > MAX_EXPIRY_MS {
            return Err(FixtureError::Invalid(
                "expires_after_ms must be between 1 and 30000".into(),
            ));
        }
        validate_actions(&self.actions)?;
        let actual_actions_digest = actions_digest(&self.actions)?;
        if actual_actions_digest != self.actions_digest {
            return Err(FixtureError::Invalid(
                "actions digest does not match actions".into(),
            ));
        }
        if self.fixture_id != bundle.id() || self.fixture_source_digest != bundle.digest() {
            return Err(FixtureError::Invalid(
                "intent does not reference the owned fixture bundle".into(),
            ));
        }
        let url = parse_fixture_url(&self.exact_url)?;
        let path = url.path();
        if !bundle.files().iter().any(|file| file.path == path) {
            return Err(FixtureError::Invalid(
                "exact_url does not point to a fixture manifest resource".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum FixtureAction {
    Click {
        selector: String,
    },
    Type {
        text: String,
    },
    Press {
        key: FixtureKey,
    },
    AssertVisible {
        selector: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    AssertText {
        selector: String,
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FixtureKey {
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
}

pub fn actions_digest(actions: &[FixtureAction]) -> Result<String, FixtureError> {
    if actions.len() > MAX_ACTIONS {
        return Err(FixtureError::Invalid("too many fixture actions".into()));
    }
    validate_actions(actions)?;
    // Match the request producer's Value representation, with explicit key sorting.
    let value = serde_json::to_value(actions).map_err(|error| {
        FixtureError::Invalid(format!("cannot encode fixture actions: {error}"))
    })?;
    let bytes = canonical_json_bytes(value)?;
    Ok(format!("sha256:{}", hex(&Sha256::digest(bytes))))
}

fn canonical_json_bytes(value: Value) -> Result<Vec<u8>, FixtureError> {
    serde_json::to_vec(&sort_json_objects(value))
        .map_err(|error| FixtureError::Invalid(format!("cannot encode canonical JSON: {error}")))
}

fn sort_json_objects(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json_objects).collect()),
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = Map::new();
            for (key, value) in entries {
                sorted.insert(key, sort_json_objects(value));
            }
            Value::Object(sorted)
        }
        scalar => scalar,
    }
}

#[derive(Debug, Clone, Error, Serialize, Deserialize)]
#[serde(tag = "code", content = "message", rename_all = "camelCase")]
pub enum FixtureError {
    #[error("invalid fixture request: {0}")]
    Invalid(String),
    #[error("fixture server is not owned: {0}")]
    FixtureServerNotOwned(String),
    #[error("fixture confinement is unsupported: {0}")]
    ConfinementUnsupported(String),
    #[error("fixture confinement is unavailable: {0}")]
    ConfinementUnavailable(String),
    #[error("fixture request was revoked: {0}")]
    Revoked(String),
    #[error("fixture execution was cancelled: {0}")]
    Cancelled(String),
    #[error("fixture execution expired: {0}")]
    Expired(String),
    #[error("browser operation failed: {0}")]
    Browser(String),
    #[error("fixture cleanup failed: {0}")]
    Cleanup(String),
}

pub(crate) fn validate_fixture_path(path: &str) -> Result<(), FixtureError> {
    if path.is_empty() || !path.starts_with('/') || path.len() > MAX_STRING_BYTES {
        return Err(FixtureError::Invalid(
            "fixture paths must be absolute URL paths".into(),
        ));
    }
    if path
        .chars()
        .any(|character| matches!(character, '\\' | '?' | '#' | '%') || character.is_control())
    {
        return Err(FixtureError::Invalid(
            "fixture path contains forbidden characters".into(),
        ));
    }
    if path
        .split('/')
        .skip(1)
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(FixtureError::Invalid(
            "fixture path contains an empty or dot segment".into(),
        ));
    }
    if !path.is_ascii() {
        return Err(FixtureError::Invalid(
            "fixture paths must use ASCII URL characters".into(),
        ));
    }
    if !path.bytes().all(|byte| {
        byte == b'/' || byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=@:".contains(&byte)
    }) {
        return Err(FixtureError::Invalid(
            "fixture path contains an unsupported character".into(),
        ));
    }
    Ok(())
}

pub(crate) fn parse_fixture_url(value: &str) -> Result<Url, FixtureError> {
    let remainder = value
        .strip_prefix("http://")
        .ok_or_else(|| FixtureError::Invalid("exact_url must use lowercase http://".into()))?;
    let authority_end = remainder.find('/').unwrap_or(remainder.len());
    let raw_path = if authority_end == remainder.len() {
        "/"
    } else {
        remainder[authority_end..]
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
    };
    validate_fixture_path(raw_path)?;
    let url = Url::parse(value)
        .map_err(|_| FixtureError::Invalid("exact_url is not a valid URL".into()))?;
    explicit_fixture_port(value)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(FixtureError::Invalid(
            "exact_url must be explicit-port HTTP on 127.0.0.1 without credentials, query, or fragment".into(),
        ));
    }
    if url.path() != raw_path {
        return Err(FixtureError::Invalid(
            "exact_url path was normalized during parsing".into(),
        ));
    }
    Ok(url)
}

pub(crate) fn explicit_fixture_port(value: &str) -> Result<u16, FixtureError> {
    let remainder = value
        .strip_prefix("http://")
        .ok_or_else(|| FixtureError::Invalid("exact_url must use lowercase http://".into()))?;
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    let port = authority
        .strip_prefix("127.0.0.1:")
        .ok_or_else(|| FixtureError::Invalid("exact_url must name 127.0.0.1 with a port".into()))?;
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(FixtureError::Invalid(
            "exact_url must contain an explicit numeric port".into(),
        ));
    }
    let parsed = port
        .parse::<u16>()
        .map_err(|_| FixtureError::Invalid("exact_url port is outside the valid range".into()))?;
    if parsed == 0 {
        return Err(FixtureError::Invalid(
            "exact_url port must be nonzero".into(),
        ));
    }
    Ok(parsed)
}

fn validate_files(files: &[FixtureFile]) -> Result<(), FixtureError> {
    if files.is_empty() || files.len() > MAX_FIXTURE_FILES {
        return Err(FixtureError::Invalid(
            "fixture file count is outside allowed bounds".into(),
        ));
    }
    let mut paths = BTreeSet::new();
    let mut total = 0usize;
    for file in files {
        validate_fixture_path(&file.path)?;
        if !paths.insert(file.path.as_str()) {
            return Err(FixtureError::Invalid("duplicate fixture path".into()));
        }
        validate_text("content_type", &file.content_type)?;
        if file
            .content_type
            .chars()
            .any(|character| matches!(character, '\r' | '\n'))
            || !file.content_type.is_ascii()
        {
            return Err(FixtureError::Invalid("invalid fixture content type".into()));
        }
        total = total
            .checked_add(file.body.len())
            .ok_or_else(|| FixtureError::Invalid("fixture byte count overflow".into()))?;
    }
    if total > MAX_FIXTURE_BYTES {
        return Err(FixtureError::Invalid(
            "fixture exceeds total byte limit".into(),
        ));
    }
    Ok(())
}

fn validate_actions(actions: &[FixtureAction]) -> Result<(), FixtureError> {
    if actions.is_empty() {
        return Err(FixtureError::Invalid(
            "fixture actions must contain at least one action".into(),
        ));
    }
    for action in actions {
        match action {
            FixtureAction::Click { selector } | FixtureAction::AssertVisible { selector, .. } => {
                validate_text("selector", selector)?;
            }
            FixtureAction::Type { text } => validate_text("text", text)?,
            FixtureAction::Press { .. } => {}
            FixtureAction::AssertText { selector, text } => {
                validate_text("selector", selector)?;
                validate_text("text", text)?;
            }
        }
        if let FixtureAction::AssertVisible {
            timeout: Some(timeout),
            ..
        } = action
        {
            if !(1..=5000).contains(timeout) {
                return Err(FixtureError::Invalid(
                    "assertVisible timeout must be between 1 and 5000".into(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_id(name: &str, value: &str) -> Result<(), FixtureError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
    {
        return Err(FixtureError::Invalid(format!(
            "{name} must be 1..128 ASCII identifier bytes"
        )));
    }
    Ok(())
}

fn validate_digest(name: &str, value: &str) -> Result<(), FixtureError> {
    let Some(hex_value) = value.strip_prefix("sha256:") else {
        return Err(FixtureError::Invalid(format!(
            "{name} must be a sha256 digest"
        )));
    };
    if hex_value.len() != 64
        || !hex_value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(FixtureError::Invalid(format!(
            "{name} must use lowercase SHA-256 hex"
        )));
    }
    Ok(())
}

fn validate_text(name: &str, value: &str) -> Result<(), FixtureError> {
    if value.trim().is_empty() || value.len() > MAX_STRING_BYTES {
        return Err(FixtureError::Invalid(format!(
            "{name} must be nonempty and at most 2048 bytes"
        )));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{actions_digest, canonical_json_bytes, FixtureAction, FixtureBundle, FixtureFile};
    use serde_json::{json, Map, Value};
    use sha2::{Digest, Sha256};

    #[test]
    fn canonical_json_recursively_sorts_object_keys() {
        let mut nested = Map::new();
        nested.insert("b".into(), json!(1));
        nested.insert("a".into(), json!(2));
        let mut object = Map::new();
        object.insert("z".into(), Value::Object(nested));
        object.insert("a".into(), json!(0));

        let bytes = canonical_json_bytes(Value::Object(object)).unwrap();
        assert_eq!(bytes, br#"{"a":0,"z":{"a":2,"b":1}}"#);
    }

    #[test]
    fn action_digest_matches_known_canonical_bytes_without_null_timeout() {
        let action = FixtureAction::AssertVisible {
            selector: "#ready".into(),
            timeout: None,
        };
        let value = serde_json::to_value([action.clone()]).unwrap();
        let bytes = canonical_json_bytes(value).unwrap();
        let expected_bytes = br##"[{"selector":"#ready","type":"assertVisible"}]"##;
        assert_eq!(bytes, expected_bytes);
        let expected_digest = format!("sha256:{}", super::hex(&Sha256::digest(expected_bytes)));
        assert_eq!(actions_digest(&[action]).unwrap(), expected_digest);
    }

    #[test]
    fn source_digest_matches_known_canonical_bytes() {
        let files = [FixtureFile {
            path: "/index.html".into(),
            content_type: "text/html".into(),
            body: vec![1, 2],
        }];
        let expected_bytes =
            br#"{"files":[{"body":[1,2],"contentType":"text/html","path":"/index.html"}]}"#;
        let expected_digest = format!("sha256:{}", super::hex(&Sha256::digest(expected_bytes)));
        assert_eq!(
            FixtureBundle::source_digest(&files).unwrap(),
            expected_digest
        );
    }

    #[test]
    fn empty_actions_are_rejected() {
        assert!(actions_digest(&[]).is_err());
    }

    #[test]
    fn fixture_error_uses_stable_code_and_message_fields() {
        let value = serde_json::to_value(super::FixtureError::Expired("deadline".into()))
            .expect("fixture error should serialize");
        assert_eq!(value["code"], "expired");
        assert_eq!(value["message"], "deadline");
        let decoded: super::FixtureError =
            serde_json::from_value(value).expect("fixture error should deserialize");
        assert!(matches!(decoded, super::FixtureError::Expired(message) if message == "deadline"));
    }
}
