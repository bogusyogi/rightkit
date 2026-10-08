//! Entitled promoted-artifact delivery. Blocking API: call from a worker thread.
//! Trust comes from an app-pinned public key, never the resolve response.

use crate::{Event, FileSpec, InstallOptions, InstallOutcome, ModelSpec, RevisionSpec, Store};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

pub const RUNTIME_ARTIFACT_TRUSTED_KEY_ID: &str = "rightkit-runtime-artifacts-2026-07";
const APPS: &[&str] = &[
    "viewright",
    "heardright",
    "mailright",
    "scraperight",
    "coderight",
    "screenright",
    "genright",
    "voiceright",
];
const KINDS: &[&str] = &[
    "asr-model",
    "tts-model",
    "ocr-model",
    "ort-runtime",
    "media-runtime",
    "tokenizer",
    "preprocessing",
];

#[derive(Debug)]
pub enum RuntimeArtifactError {
    NotEntitled,
    ExpiredToken,
    SignatureMismatch,
    DigestMismatch,
    SizeMismatch,
    Network,
    InvalidManifest,
    NotFound,
    Store(crate::Error),
}
impl fmt::Display for RuntimeArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(e) => write!(f, "model store: {e}"),
            _ => write!(f, "{self:?}"),
        }
    }
}
impl std::error::Error for RuntimeArtifactError {}
pub type Result<T> = std::result::Result<T, RuntimeArtifactError>;

macro_rules! wire_type {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $(pub $field: $ty),* }
    };
}
wire_type!(RuntimeArtifactTarget {
    os: String,
    arch: String
});
wire_type!(ArtifactEntitlement {
    app_key: String,
    tier: String
});
wire_type!(ArtifactDistribution { delivery: String, bucket: Option<String> });
wire_type!(ArtifactObject { r2_key: Option<String>, filename: String, sha256: String, size_bytes: u64 });
wire_type!(ArtifactVersions {
    runtime: String,
    model: String,
    tokenizer: String,
    preprocessing: String,
    license: String,
    provenance: String
});
wire_type!(ArtifactProvenance {
    source: String,
    source_revision: String,
    license_id: String,
    notice_sha256: String
});
wire_type!(ArtifactPromotion {
    authority_app_key: String,
    promotion_id: String,
    promoted_at: String,
    evidence_sha256: String
});
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeArtifactManifest {
    pub schema: u32,
    pub artifact_kind: String,
    pub entitlement: ArtifactEntitlement,
    pub distribution: ArtifactDistribution,
    pub target: RuntimeArtifactTarget,
    #[serde(
        default,
        deserialize_with = "deserialize_artifact_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub artifact_id: Option<String>,
    pub object: ArtifactObject,
    pub pointer_key: Option<String>,
    pub versions: ArtifactVersions,
    pub provenance: ArtifactProvenance,
    pub promotion: ArtifactPromotion,
}
wire_type!(ArtifactSignature {
    algorithm: String,
    key_id: String,
    value: String
});
wire_type!(RuntimeArtifactEnvelope {
    manifest: RuntimeArtifactManifest,
    signature: ArtifactSignature
});

impl RuntimeArtifactTarget {
    pub fn new(os: &str, arch: &str) -> Self {
        Self {
            os: os.into(),
            arch: arch.into(),
        }
    }
    pub fn current() -> Self {
        Self::new(
            if cfg!(target_os = "macos") {
                "darwin"
            } else {
                std::env::consts::OS
            },
            std::env::consts::ARCH,
        )
    }
    fn valid(&self) -> bool {
        ["darwin", "windows", "linux", "ios", "any"].contains(&self.os.as_str())
            && ["x86_64", "aarch64", "universal", "any"].contains(&self.arch.as_str())
            && (self.os == "any") == (self.arch == "any")
    }
    fn matches(&self, requested: &Self) -> bool {
        (self.os == "any" && self.arch == "any")
            || (self.os == requested.os && self.arch == requested.arch)
    }
}

/// Private grant fields prevent substitution after verification. Debug omits URL.
pub struct ResolvedRuntimeArtifact {
    envelope: RuntimeArtifactEnvelope,
    download_url: String,
    expires_at: u64,
}
impl fmt::Debug for ResolvedRuntimeArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedRuntimeArtifact")
            .field("envelope", &self.envelope)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
impl ResolvedRuntimeArtifact {
    pub fn envelope(&self) -> &RuntimeArtifactEnvelope {
        &self.envelope
    }
    /// Sensitive short-lived capability; do not log or persist.
    pub fn download_url(&self) -> &str {
        &self.download_url
    }
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
}

pub struct RuntimeArtifactClient {
    base: Url,
    key: VerifyingKey,
    agent: ureq::Agent,
}
impl RuntimeArtifactClient {
    /// Pin the public key corresponding to RUNTIME_ARTIFACT_TRUSTED_KEY_ID.
    /// HTTPS required except loopback HTTP for deterministic tests.
    pub fn new(base: &str, public_key: [u8; 32]) -> Result<Self> {
        let mut base = secure_url(base)?;
        if base.query().is_some() || base.fragment().is_some() {
            return Err(RuntimeArtifactError::InvalidManifest);
        }
        base.set_path(&format!("{}/", base.path().trim_end_matches('/')));
        let key = VerifyingKey::from_bytes(&public_key)
            .map_err(|_| RuntimeArtifactError::SignatureMismatch)?;
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self { base, key, agent })
    }

    /// Resolve an artifact ID, current promotion row ID or exact manifest pointer key.
    /// Pass `signedToken` from activate/refresh, not `activationToken`.
    pub fn resolve(
        &self,
        app_key: &str,
        artifact: &str,
        license_token: &str,
        target: &RuntimeArtifactTarget,
    ) -> Result<ResolvedRuntimeArtifact> {
        self.resolve_inner(app_key, artifact, license_token, target, false)
    }

    fn resolve_inner(
        &self,
        app_key: &str,
        artifact: &str,
        license_token: &str,
        target: &RuntimeArtifactTarget,
        explicit_artifact_id: bool,
    ) -> Result<ResolvedRuntimeArtifact> {
        if !APPS.contains(&app_key)
            || artifact.trim().is_empty()
            || !target.valid()
            || target.os == "any"
        {
            return Err(RuntimeArtifactError::InvalidManifest);
        }
        let endpoint = self
            .base
            .join(&format!("v1/apps/{app_key}/runtime-artifacts/resolve"))
            .map_err(|_| RuntimeArtifactError::InvalidManifest)?;
        let body_value = if explicit_artifact_id {
            serde_json::json!({"artifactId": artifact, "target": target})
        } else {
            serde_json::json!({"artifact": artifact, "target": target})
        };
        let body = serde_json::to_string(&body_value)
            .map_err(|_| RuntimeArtifactError::InvalidManifest)?;
        let mut response = self
            .agent
            .post(endpoint.as_str())
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {license_token}"))
            .send(body.as_bytes())
            .map_err(|_| RuntimeArtifactError::Network)?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(1_048_576)
            .read_to_string()
            .map_err(|_| RuntimeArtifactError::Network)?;
        let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        if status != 200 {
            return Err(match value.pointer("/error/code").and_then(Value::as_str) {
                Some("TOKEN_EXPIRED") => RuntimeArtifactError::ExpiredToken,
                _ if status == 401 || status == 403 => RuntimeArtifactError::NotEntitled,
                _ if status == 404 => RuntimeArtifactError::NotFound,
                _ => RuntimeArtifactError::Network,
            });
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Resolution {
            envelope: Value,
            download_url: String,
            expires_at: u64,
        }
        let result: Resolution =
            serde_json::from_value(value).map_err(|_| RuntimeArtifactError::InvalidManifest)?;
        let envelope = self.verify_envelope(&result.envelope)?;
        let m = &envelope.manifest;
        let selector_is_artifact_id = explicit_artifact_id
            || (!artifact.contains('/')
                && artifact_id_valid(artifact)
                && !looks_like_uuid(artifact));
        if m.entitlement.app_key != app_key
            || !m.target.matches(target)
            || m.distribution.delivery != "private-r2"
            || m.entitlement.tier != "pro"
            || m.promotion.authority_app_key != app_key
            || (artifact.contains('/') && m.pointer_key.as_deref() != Some(artifact))
            || (selector_is_artifact_id && m.artifact_id.as_deref() != Some(artifact))
        {
            return Err(RuntimeArtifactError::InvalidManifest);
        }
        secure_url(&result.download_url)?;
        let now = unix_now();
        if result.expires_at <= now {
            return Err(RuntimeArtifactError::ExpiredToken);
        }
        if result.expires_at > now + 300 {
            return Err(RuntimeArtifactError::InvalidManifest);
        }
        Ok(ResolvedRuntimeArtifact {
            envelope,
            download_url: result.download_url,
            expires_at: result.expires_at,
        })
    }

    /// Resolve a safe artifact ID for an app and concrete target.
    pub fn resolve_by_artifact_id(
        &self,
        app_key: &str,
        artifact_id: &str,
        license_token: &str,
        target: &RuntimeArtifactTarget,
    ) -> Result<ResolvedRuntimeArtifact> {
        if !artifact_id_valid(artifact_id) {
            return Err(RuntimeArtifactError::InvalidManifest);
        }
        self.resolve_inner(app_key, artifact_id, license_token, target, true)
    }

    /// Mirror JS canonical sorted-key Ed25519 envelope verification.
    pub fn verify_envelope(&self, value: &Value) -> Result<RuntimeArtifactEnvelope> {
        match value.pointer("/manifest/artifactId") {
            Some(Value::String(id)) if artifact_id_valid(id) => {}
            Some(_) => return Err(RuntimeArtifactError::InvalidManifest),
            None => {}
        }
        if value.pointer("/manifest/pointerKey").is_none()
            || value.pointer("/manifest/object/r2Key").is_none()
            || value.pointer("/manifest/distribution/bucket").is_none()
        {
            return Err(RuntimeArtifactError::InvalidManifest);
        }
        let envelope: RuntimeArtifactEnvelope = serde_json::from_value(value.clone())
            .map_err(|_| RuntimeArtifactError::InvalidManifest)?;
        validate_manifest(&envelope.manifest)?;
        let s = &envelope.signature;
        if s.algorithm != "Ed25519" || s.key_id != RUNTIME_ARTIFACT_TRUSTED_KEY_ID {
            return Err(RuntimeArtifactError::SignatureMismatch);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(&s.value)
            .map_err(|_| RuntimeArtifactError::SignatureMismatch)?;
        let signature =
            Signature::from_slice(&bytes).map_err(|_| RuntimeArtifactError::SignatureMismatch)?;
        // Verify original JSON values: absent fields & numeric coercions cannot
        // be silently rewritten into a different signed manifest.
        let canonical = canonical_json(&value["manifest"])?;
        self.key
            .verify_strict(canonical.as_bytes(), &signature)
            .map_err(|_| RuntimeArtifactError::SignatureMismatch)?;
        Ok(envelope)
    }

    /// Download pinned raw bytes into a revisioned model store. Reuses its
    /// cross-process lock, Range recovery, size/hash checks & atomic activation.
    /// Resolve again after grant expiry; same digest resumes existing partials.
    pub fn install(
        &self,
        resolved: &ResolvedRuntimeArtifact,
        store: &Store,
        model_id: &str,
        options: &InstallOptions,
        sink: impl FnMut(Event),
    ) -> Result<InstallOutcome> {
        if resolved.expires_at <= unix_now() {
            return Err(RuntimeArtifactError::ExpiredToken);
        }
        let verified = self.verify_envelope(
            &serde_json::to_value(&resolved.envelope)
                .map_err(|_| RuntimeArtifactError::InvalidManifest)?,
        )?;
        let m = &verified.manifest;
        let spec = ModelSpec {
            id: model_id.into(),
            label: String::new(),
            revisions: vec![RevisionSpec {
                revision: m.object.sha256.clone(),
                files: vec![FileSpec {
                    path: m.object.filename.clone(),
                    url: resolved.download_url.clone(),
                    bytes: Some(m.object.size_bytes),
                    sha256: Some(m.object.sha256.clone()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let mut options = options.clone();
        options.revision = Some(m.object.sha256.clone());
        options.quant = None;
        options.authorization = None;
        options.max_download_redirects = 0;
        options.require_pinned = true;
        options.resume_identity = m.object.r2_key.clone();
        store
            .install(&spec, &options, sink)
            .map_err(|e| match e.code {
                crate::ErrorCode::HashMismatch => RuntimeArtifactError::DigestMismatch,
                crate::ErrorCode::SizeMismatch => RuntimeArtifactError::SizeMismatch,
                crate::ErrorCode::Download if resolved.expires_at <= unix_now() => {
                    RuntimeArtifactError::ExpiredToken
                }
                crate::ErrorCode::Download if e.message.contains("authorization rejected") => {
                    RuntimeArtifactError::NotEntitled
                }
                crate::ErrorCode::Download => RuntimeArtifactError::Network,
                _ => RuntimeArtifactError::Store(e),
            })
    }
}

fn validate_manifest(m: &RuntimeArtifactManifest) -> Result<()> {
    let fail = || RuntimeArtifactError::InvalidManifest;
    if m.schema != 1
        || !APPS.contains(&m.entitlement.app_key.as_str())
        || !APPS.contains(&m.promotion.authority_app_key.as_str())
        || !KINDS.contains(&m.artifact_kind.as_str())
        || !m.target.valid()
    {
        return Err(fail());
    }
    let lane = match m.distribution.delivery.as_str() {
        "private-r2" => ("pro", Some("rightapps-updates")),
        "public-r2" => ("public", Some("rightapps-downloads")),
        "bundled" => ("bundled", None),
        _ => return Err(fail()),
    };
    if m.entitlement.tier != lane.0 || m.distribution.bucket.as_deref() != lane.1 {
        return Err(fail());
    }
    if ["asr-model", "tts-model", "ocr-model", "tokenizer"].contains(&m.artifact_kind.as_str())
        && lane.0 != "pro"
    {
        return Err(fail());
    }
    let filename = &m.object.filename;
    if !exact(filename)
        || filename.contains("..")
        || filename.contains(['/', '\\', ':', '\0'])
        || filename == "."
        || !digest(&m.object.sha256)
        || m.object.size_bytes == 0
        || m.object.size_bytes > 9_007_199_254_740_991
    {
        return Err(fail());
    }
    let key = format!(
        "{}/runtime-artifacts/objects/sha256/{}/{}",
        m.entitlement.app_key, m.object.sha256, filename
    );
    let pointer = match &m.artifact_id {
        Some(artifact_id) => format!(
            "{}/runtime-artifacts/{}/{}/{}/{}/current/manifest.json",
            m.entitlement.app_key, m.artifact_kind, artifact_id, m.target.os, m.target.arch
        ),
        None => format!(
            "{}/runtime-artifacts/{}/{}/{}/current/manifest.json",
            m.entitlement.app_key, m.artifact_kind, m.target.os, m.target.arch
        ),
    };
    if lane.0 == "bundled" {
        if m.object.r2_key.is_some() || m.pointer_key.is_some() {
            return Err(fail());
        }
    } else if m.object.r2_key.as_deref() != Some(&key) || m.pointer_key.as_deref() != Some(&pointer)
    {
        return Err(fail());
    }
    for v in [
        &m.versions.runtime,
        &m.versions.model,
        &m.versions.tokenizer,
        &m.versions.preprocessing,
        &m.versions.license,
        &m.versions.provenance,
        &m.provenance.source_revision,
        &m.provenance.license_id,
        &m.promotion.promotion_id,
    ] {
        if !exact(v) {
            return Err(fail());
        }
    }
    let source = Url::parse(&m.provenance.source).map_err(|_| fail())?;
    if !exact(&m.provenance.source)
        || source.scheme() != "https"
        || normalize_license(&m.versions.license) != normalize_license(&m.provenance.license_id)
        || !digest(&m.provenance.notice_sha256)
        || !digest(&m.promotion.evidence_sha256)
    {
        return Err(fail());
    }
    let date =
        chrono::DateTime::parse_from_rfc3339(&m.promotion.promoted_at).map_err(|_| fail())?;
    if date
        .to_utc()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        != m.promotion.promoted_at
    {
        return Err(fail());
    }
    Ok(())
}
fn artifact_id_valid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return false;
    }
    let mut previous_hyphen = false;
    for &byte in bytes {
        if byte == b'-' {
            if previous_hyphen {
                return false;
            }
            previous_hyphen = true;
        } else if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            previous_hyphen = false;
        } else {
            return false;
        }
    }
    true
}
fn deserialize_artifact_id<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::String(value) => Ok(Some(value)),
        _ => Err(serde::de::Error::custom("artifactId must be a string")),
    }
}
fn looks_like_uuid(s: &str) -> bool {
    let parts: Vec<_> = s.split('-').collect();
    [8, 4, 4, 4, 12]
        .iter()
        .zip(parts.iter())
        .all(|(len, part)| part.len() == *len && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && parts.len() == 5
}
fn exact(s: &str) -> bool {
    !s.is_empty() && s.trim() == s
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn normalize_license(s: &str) -> String {
    s.to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn secure_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| RuntimeArtifactError::InvalidManifest)?;
    let loopback = url
        .host_str()
        .is_some_and(|host| host == "localhost" || host == "127.0.0.1" || host == "[::1]");
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(RuntimeArtifactError::InvalidManifest);
    }
    Ok(url)
}
fn canonical_json(value: &Value) -> Result<String> {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by_key(|(a, _)| *a);
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k.clone(), sorted(v)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.iter().map(sorted).collect()),
            _ => v.clone(),
        }
    }
    serde_json::to_string(&sorted(value)).map_err(|_| RuntimeArtifactError::InvalidManifest)
}
