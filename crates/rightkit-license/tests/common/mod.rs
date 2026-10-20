#![allow(dead_code)]
//! Shared fixtures: a deterministic test signer and token builders.

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};

pub const APP: &str = "heardright";
pub const DEVICE: &str = "dev-1";

pub fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
}

pub fn pubkey() -> [u8; 32] {
    signing_key().verifying_key().to_bytes()
}

pub fn other_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[9_u8; 32])
}

pub fn other_pubkey() -> [u8; 32] {
    SigningKey::from_bytes(&[9_u8; 32])
        .verifying_key()
        .to_bytes()
}

pub fn sign_with(key: &SigningKey, payload: &str) -> String {
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let sig = key.sign(payload.as_bytes());
    format!(
        "{}.{}",
        engine.encode(payload.as_bytes()),
        engine.encode(sig.to_bytes())
    )
}

pub fn sign(payload: &str) -> String {
    sign_with(&signing_key(), payload)
}

/// Claims of a test token. Build with the shape constructors, adjust fields, call `token()`.
#[derive(Clone)]
pub struct Claims {
    pub id: &'static str,
    pub app: &'static str,
    pub tier: &'static str,
    pub device: &'static str,
    pub iat: i64,
    pub exp: i64,
    pub kind: Option<&'static str>,
    pub stage: Option<&'static str>,
    pub issued: Option<i64>,
    pub due: Option<i64>,
}

impl Claims {
    fn base(iat: i64, exp: i64) -> Self {
        Self {
            id: "lic_1",
            app: APP,
            tier: "pro",
            device: DEVICE,
            iat,
            exp,
            kind: None,
            stage: None,
            issued: None,
            due: None,
        }
    }

    /// Legacy v2: no lifecycle claims.
    pub fn legacy(iat: i64, exp: i64) -> Self {
        Self::base(iat, exp)
    }

    pub fn trial(iat: i64, exp: i64) -> Self {
        Self {
            kind: Some("trial"),
            stage: Some("time_bound"),
            issued: Some(iat),
            ..Self::base(iat, exp)
        }
    }

    pub fn term(iat: i64, exp: i64) -> Self {
        Self {
            kind: Some("term"),
            stage: Some("time_bound"),
            issued: Some(iat),
            ..Self::base(iat, exp)
        }
    }

    /// Provisional lifetime: `exp == confirmation_due_at`.
    pub fn provisional(iat: i64, due: i64) -> Self {
        Self {
            kind: Some("lifetime"),
            stage: Some("provisional"),
            issued: Some(iat),
            due: Some(due),
            ..Self::base(iat, due)
        }
    }

    /// Final lifetime: `issued <= due <= iat <= exp`.
    pub fn final_lifetime(issued: i64, due: i64, iat: i64, exp: i64) -> Self {
        Self {
            kind: Some("lifetime"),
            stage: Some("final"),
            issued: Some(issued),
            due: Some(due),
            ..Self::base(iat, exp)
        }
    }

    pub fn payload(&self) -> String {
        let mut json = format!(
            r#"{{"v":2,"id":"{}","app":"{}","tier":"{}","features":[],"device_id":"{}","iat":{},"exp":{}"#,
            self.id, self.app, self.tier, self.device, self.iat, self.exp
        );
        if let Some(kind) = self.kind {
            json.push_str(&format!(r#","license_kind":"{kind}""#));
        }
        if let Some(stage) = self.stage {
            json.push_str(&format!(r#","entitlement_stage":"{stage}""#));
        }
        if let Some(issued) = self.issued {
            json.push_str(&format!(r#","license_issued_at":{issued}"#));
        }
        if let Some(due) = self.due {
            json.push_str(&format!(r#","confirmation_due_at":{due}"#));
        }
        json.push('}');
        json
    }

    pub fn token(&self) -> String {
        sign(&self.payload())
    }

    pub fn token_signed_by(&self, key: &SigningKey) -> String {
        sign_with(key, &self.payload())
    }
}
