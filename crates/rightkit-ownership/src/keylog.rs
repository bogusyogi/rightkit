//! Ownership key log: the append-only, signed record of approval keys. Port of
//! `ownership-key-log.mjs`. Signatures are verified with RustCrypto: P-256 ECDSA/SHA-256
//! (`p256`) and RSA-2048 PSS or PKCS#1 v1.5 with SHA-256 (`rsa`).

use std::collections::HashMap;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::pkcs8::DecodePublicKey as _;
use rsa::pkcs8::DecodePublicKey as _;
use rsa::pkcs8::EncodePublicKey as _;
use rsa::traits::PublicKeyParts;
use serde_json::Value;
// rsa 0.9 hashes with digest 0.10, so RSA verification uses the sha2 0.10 alias (see Cargo.toml).
use sha2_rsa::{Digest, Sha256};
use spki::EncodePublicKey as _;

use crate::json::{canonical_json, field, is_hex64, items, safe_ordinal, sha256_hex, str_of};

const DOMAIN: &str = "rightkit.ownership-key-log.v1";

pub enum PublicKey {
    Ec(p256::PublicKey),
    Rsa(rsa::RsaPublicKey),
}

/// Parses an SPKI PEM public key.
pub fn parse_public_key(pem: &str) -> Option<PublicKey> {
    if let Ok(key) = p256::PublicKey::from_public_key_pem(pem) {
        return Some(PublicKey::Ec(key));
    }
    rsa::RsaPublicKey::from_public_key_pem(pem)
        .ok()
        .map(PublicKey::Rsa)
}

impl PublicKey {
    pub fn is_rsa(&self) -> bool {
        matches!(self, Self::Rsa(_))
    }

    /// SPKI DER bytes.
    pub fn der(&self) -> Option<Vec<u8>> {
        match self {
            Self::Ec(key) => key.to_public_key_der().ok().map(|d| d.as_bytes().to_vec()),
            Self::Rsa(key) => key.to_public_key_der().ok().map(|d| d.as_bytes().to_vec()),
        }
    }
}

/// SHA-256 of the key's SPKI DER, for P-256 or RSA-2048 with exponent 65537.
pub fn key_fingerprint(pem: &str) -> Result<String, String> {
    const MESSAGE: &str = "key must be P-256 ECDSA or RSA-2048 with exponent 65537";
    let key = parse_public_key(pem).ok_or_else(|| MESSAGE.to_string())?;
    if let PublicKey::Rsa(rsa) = &key {
        if rsa.size() * 8 != 2048 || rsa.e() != &rsa::BigUint::from(65537u32) {
            return Err(MESSAGE.into());
        }
    }
    key.der()
        .map(|der| sha256_hex(&der))
        .ok_or_else(|| MESSAGE.to_string())
}

/// Verifies `signature` over `payload`. Never panics; any malformed input is `false`.
pub fn verify_ownership_signature(public_key_pem: &str, payload: &[u8], signature: &[u8]) -> bool {
    if payload.len() > 1_000_000 || signature.len() > 256 {
        return false;
    }
    let Some(key) = parse_public_key(public_key_pem) else {
        return false;
    };
    if key_fingerprint(public_key_pem).is_err() {
        return false;
    }
    match key {
        PublicKey::Rsa(rsa) => {
            if signature.len() != 256 {
                return false;
            }
            let hashed = Sha256::digest(payload);
            rsa.verify(rsa::Pss::new_with_salt::<Sha256>(32), &hashed, signature)
                .is_ok()
                || rsa
                    .verify(rsa::Pkcs1v15Sign::new::<Sha256>(), &hashed, signature)
                    .is_ok()
        }
        PublicKey::Ec(ec) => {
            let verifying = p256::ecdsa::VerifyingKey::from(&ec);
            match p256::ecdsa::Signature::from_der(signature) {
                Ok(sig) => verifying.verify(payload, &sig).is_ok(),
                Err(_) => false,
            }
        }
    }
}

fn exact(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.len() == keys.len() && keys.iter().all(|k| map.contains_key(*k)))
}

fn valid_text(value: &Value, max: usize) -> bool {
    value.as_str().is_some_and(|s| {
        !s.trim().is_empty()
            && s.encode_utf16().count() <= max
            && !s.chars().any(|c| (c as u32) <= 0x1f || c as u32 == 0x7f)
    })
}

fn pem_shape(pem: &str) -> bool {
    // ^-----BEGIN PUBLIC KEY-----\r?\n[\s\S]+\r?\n-----END PUBLIC KEY-----\r?\n?$
    let Some(rest) = pem.strip_prefix("-----BEGIN PUBLIC KEY-----") else {
        return false;
    };
    let rest = rest.strip_prefix('\r').unwrap_or(rest);
    let Some(rest) = rest.strip_prefix('\n') else {
        return false;
    };
    let rest = rest
        .strip_suffix('\n')
        .map(|r| r.strip_suffix('\r').map_or(r, |_| r))
        .unwrap_or(rest);
    let Some(body) = rest.strip_suffix("-----END PUBLIC KEY-----") else {
        return false;
    };
    let body = body
        .strip_suffix('\n')
        .map(|b| b.strip_suffix('\r').unwrap_or(b));
    body.is_some_and(|b| !b.is_empty())
}

pub fn validate_key_record(record: &Value) -> Vec<String> {
    if !exact(record, &["keyId", "publicKeyPem", "platform", "deviceName"]) {
        return vec!["key record fields are malformed".into()];
    }
    let mut problems = Vec::new();
    if !is_hex64(str_of(field(record, "keyId"))) {
        problems.push("key id must be a lowercase SHA-256 fingerprint".into());
    }
    let platform = str_of(field(record, "platform"));
    if platform != "windows" && platform != "macos" {
        problems.push("key platform is invalid".into());
    }
    if !valid_text(field(record, "deviceName"), 128) {
        problems.push("device name must be nonempty and at most 128 characters".into());
    }
    match field(record, "publicKeyPem").as_str() {
        Some(pem) if pem.encode_utf16().count() <= 4096 && pem_shape(pem) => {
            match parse_public_key(pem) {
                Some(key) => {
                    if key_fingerprint(pem).ok().as_deref() != field(record, "keyId").as_str() {
                        problems.push("key id does not match public key".into());
                    }
                    if key.is_rsa() && platform != "windows" {
                        problems.push("RSA keys are permitted only for Windows devices".into());
                    }
                }
                None => {
                    problems.push("public key must be P-256 SPKI or Windows RSA-2048 SPKI".into())
                }
            }
        }
        _ => problems.push("public key PEM must be a bounded SPKI public key".into()),
    }
    problems
}

/// SHA-256 of the canonical event payload.
pub fn event_hash(event: &Value) -> String {
    sha256_hex(canonical_json(field(event, "payload")).as_bytes())
}

const PAYLOAD_KEYS: [&str; 7] = [
    "domain",
    "rootFingerprint",
    "sequence",
    "previousHash",
    "operation",
    "subject",
    "signerKeyId",
];

fn validate_payload(payload: &Value) -> Vec<String> {
    if !exact(payload, &PAYLOAD_KEYS) {
        return vec!["event payload fields are malformed".into()];
    }
    let mut problems = Vec::new();
    if str_of(field(payload, "domain")) != DOMAIN {
        problems.push("event domain is invalid".into());
    }
    if !is_hex64(str_of(field(payload, "rootFingerprint"))) {
        problems.push("root fingerprint is invalid".into());
    }
    if !safe_ordinal(field(payload, "sequence")) {
        problems.push("event sequence is invalid".into());
    }
    let first = field(payload, "sequence").as_f64() == Some(0.0);
    let previous = field(payload, "previousHash");
    if if first {
        !previous.is_null()
    } else {
        !is_hex64(str_of(previous))
    } {
        problems.push("event previous hash is invalid".into());
    }
    let operation = str_of(field(payload, "operation"));
    if !matches!(operation, "root" | "add" | "revoke") {
        problems.push("event operation is invalid".into());
    }
    if !is_hex64(str_of(field(payload, "signerKeyId"))) {
        problems.push("event signer key id is invalid".into());
    }
    let subject = field(payload, "subject");
    if operation == "revoke" {
        if !exact(subject, &["keyId"]) || !is_hex64(str_of(field(subject, "keyId"))) {
            problems.push("revocation subject is malformed".into());
        }
    } else {
        problems.extend(
            validate_key_record(subject)
                .into_iter()
                .map(|p| format!("subject {p}")),
        );
    }
    problems
}

#[derive(Debug, Default, Clone)]
pub struct KeyLogResult {
    pub ok: bool,
    pub problems: Vec<String>,
    pub keys: Vec<Value>,
    pub all_keys: Vec<Value>,
    pub revoked_key_ids: Vec<String>,
    /// `(sequence, head)` of the verified log.
    pub checkpoint: Option<(i64, Option<String>)>,
    pub root_fingerprint: Option<String>,
}

fn base64_canonical(value: &str) -> Option<Vec<u8>> {
    // ^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$ and round-trip equal.
    let bytes = BASE64.decode(value).ok()?;
    (BASE64.encode(&bytes) == value).then_some(bytes)
}

/// Verifies the whole log against a pinned root fingerprint and optional checkpoint.
pub fn verify_key_log(
    log: &Value,
    root_fingerprint: &Value,
    checkpoint: Option<&Value>,
) -> KeyLogResult {
    let malformed = |problems: Vec<String>, root: Option<String>| KeyLogResult {
        ok: false,
        problems,
        root_fingerprint: root,
        ..Default::default()
    };
    if !exact(log, &["schema", "events"])
        || field(log, "schema").as_f64() != Some(1.0)
        || !field(log, "events").is_array()
    {
        return malformed(vec!["key log schema or events are malformed".into()], None);
    }
    let events = items(field(log, "events"));
    let pin = root_fingerprint.as_str().map(String::from);
    let mut problems = Vec::new();
    if !is_hex64(str_of(root_fingerprint)) {
        problems.push("pinned root fingerprint is required".into());
    }
    if events.is_empty() {
        problems.push("key log must contain a signed root event".into());
    }
    if events.len() > 10_000 {
        problems.push("key log exceeds 10000 events".into());
    }
    if !problems.is_empty() {
        return malformed(problems, pin);
    }
    let pin_text = pin.clone().unwrap_or_default();
    let mut keys: Vec<Value> = Vec::new();
    let mut all_keys: Vec<Value> = Vec::new();
    let mut revoked: Vec<String> = Vec::new();
    let mut previous_hash: Option<String> = None;
    let mut fail_at: Option<String> = None;
    'events: for (i, event) in events.iter().enumerate() {
        let fail = |message: String| -> Option<String> { Some(message) };
        let signature = field(event, "signature");
        if !exact(event, &["schema", "payload", "signature"])
            || field(event, "schema").as_f64() != Some(1.0)
            || (!signature.is_null() && !exact(signature, &["keyId", "host", "value"]))
        {
            fail_at = fail(format!("event {i} envelope is malformed or unsigned"));
            break;
        }
        let payload = field(event, "payload");
        let payload_problems = validate_payload(payload);
        if !payload_problems.is_empty() {
            let message = payload_problems
                .into_iter()
                .map(|p| format!("event {i}: {p}"))
                .collect::<Vec<_>>()
                .join("\u{0}");
            fail_at = Some(message);
            break;
        }
        if field(payload, "sequence").as_u64() != Some(i as u64)
            && field(payload, "sequence").as_f64() != Some(i as f64)
        {
            fail_at = fail(format!("event {i} sequence is not contiguous"));
            break;
        }
        if field(payload, "previousHash").as_str() != previous_hash.as_deref() {
            fail_at = fail(format!("event {i} previous hash does not match log head"));
            break;
        }
        if str_of(field(payload, "rootFingerprint")) != pin_text {
            fail_at = fail(format!("event {i} root fingerprint does not match pin"));
            break;
        }
        let operation = str_of(field(payload, "operation"));
        if i == 0 && operation != "root" {
            fail_at = fail("first event must establish root".into());
            break;
        }
        if i > 0 && operation == "root" {
            fail_at = fail(format!("event {i} cannot replace root"));
            break;
        }
        let subject = field(payload, "subject");
        let signer_key_id = str_of(field(payload, "signerKeyId"));
        let signer: Option<Value> = if i == 0 {
            Some(subject.clone())
        } else {
            keys.iter()
                .find(|k| str_of(field(k, "keyId")) == signer_key_id)
                .cloned()
        };
        let Some(signer) = signer.filter(|s| {
            str_of(field(s, "keyId")) == signer_key_id
                && !revoked.iter().any(|r| r == signer_key_id)
        }) else {
            fail_at = fail(format!("event {i} signer is not active"));
            break;
        };
        let signer_pem = str_of(field(&signer, "publicKeyPem"));
        let Some(signer_key) = parse_public_key(signer_pem) else {
            fail_at = fail(format!("event {i} signer is not active"));
            break;
        };
        let rsa_signer = signer_key.is_rsa();
        if signature.is_null() {
            let anchor = i == 0
                && operation == "root"
                && str_of(field(&signer, "platform")) == "windows"
                && rsa_signer
                && str_of(field(payload, "rootFingerprint")) == pin_text
                && str_of(field(payload, "rootFingerprint")) == str_of(field(subject, "keyId"));
            if !anchor {
                fail_at = fail(format!(
                    "event {i} unsigned root is not a pinned Windows RSA genesis checkpoint"
                ));
                break;
            }
        } else {
            let max_length = if rsa_signer { 344 } else { 128 };
            let value = field(signature, "value");
            let value_text = value.as_str();
            let sig_bytes = value_text.and_then(base64_canonical);
            let length_ok = value_text.is_some_and(|v| {
                v.len() <= max_length
                    && if rsa_signer {
                        v.len() == 344 && sig_bytes.as_ref().is_some_and(|b| b.len() == 256)
                    } else {
                        v.len() <= 128 && sig_bytes.as_ref().is_some_and(|b| !b.is_empty())
                    }
            });
            if str_of(field(signature, "keyId")) != signer_key_id
                || str_of(field(signature, "host")) != str_of(field(&signer, "platform"))
                || !length_ok
                || sig_bytes.is_none()
            {
                fail_at = fail(format!(
                    "event {i} signature envelope does not match signer"
                ));
                break;
            }
            let ok = verify_ownership_signature(
                signer_pem,
                canonical_json(payload).as_bytes(),
                &sig_bytes.expect("checked"),
            );
            if !ok {
                fail_at = fail(format!("event {i} signature is invalid"));
                break;
            }
        }
        if i == 0 {
            let fingerprint_ok = key_fingerprint(str_of(field(subject, "publicKeyPem")))
                .ok()
                .as_deref()
                == Some(pin_text.as_str());
            if signer_key_id != str_of(field(subject, "keyId")) || !fingerprint_ok {
                fail_at = fail("root event is not self-signed or pin does not match".into());
                break;
            }
            keys.push(signer.clone());
            all_keys.push(signer);
        } else if operation == "add" {
            let id = str_of(field(subject, "keyId"));
            if all_keys.iter().any(|k| str_of(field(k, "keyId")) == id) {
                fail_at = fail(format!(
                    "event {i} adds a duplicate or previously revoked key"
                ));
                break 'events;
            }
            keys.push(subject.clone());
            all_keys.push(subject.clone());
        } else if operation == "revoke" {
            let id = str_of(field(subject, "keyId"));
            let Some(at) = keys.iter().position(|k| str_of(field(k, "keyId")) == id) else {
                fail_at = fail(format!("event {i} revokes a key that is not active"));
                break 'events;
            };
            keys.remove(at);
            revoked.push(id.to_string());
        }
        previous_hash = Some(event_hash(event));
    }
    let mut problems: Vec<String> = fail_at
        .map(|m| m.split('\u{0}').map(String::from).collect())
        .unwrap_or_default();
    let current = (events.len() as i64 - 1, previous_hash.clone());
    if problems.is_empty() {
        if let Some(checkpoint) = checkpoint {
            let valid = exact(checkpoint, &["sequence", "head"])
                && safe_ordinal(field(checkpoint, "sequence"))
                && is_hex64(str_of(field(checkpoint, "head")));
            if !valid {
                problems.push("protected checkpoint is malformed".into());
            } else {
                let sequence = field(checkpoint, "sequence").as_f64().unwrap_or(0.0) as i64;
                if current.0 < sequence {
                    problems.push("key log is shorter than protected checkpoint".into());
                } else if events.get(sequence as usize).is_some_and(|e| {
                    Some(event_hash(e)).as_deref() != field(checkpoint, "head").as_str()
                }) {
                    problems.push("key log forks before or at protected checkpoint".into());
                }
            }
        }
    }
    let _ = HashMap::<(), ()>::new();
    KeyLogResult {
        ok: problems.is_empty(),
        problems,
        keys,
        all_keys,
        revoked_key_ids: revoked,
        checkpoint: Some(current),
        root_fingerprint: pin,
    }
}
