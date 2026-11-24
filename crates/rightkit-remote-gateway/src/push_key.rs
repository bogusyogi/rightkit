//! The APNs provider key the owner supplies for phone alerts.
//!
//! Apple issues an authentication key (a `.p8` file holding a P-256 private key in PEM, a 10-character key id) and
//! the account has a 10-character team id. The engine on the computer that sends alerts signs APNs requests with
//! them. They live in the OS credential store (`rightkit-credentials`, under the service the host app names in
//! [`KeyNames`]), never in a file of the repository, the settings or the logs, and no method returns the key.
//!
//! **Who may change it.** Only the owner, through the engine's `device.push.key.set` and `.clear` (owner UI
//! only: the hub or the notch, executable verified by the resident engine). Each carries the owner check
//! (`unlock::authorise_device_op`): a signature of the enrolled owner-check key (Touch ID on a Mac, Windows Hello)
//! over a challenge for exactly this operation, or the app PIN as the fallback. The operation names the key
//! itself: `push.key:<digest>`, where the digest is [`key_digest`], so a signature made for one key cannot store
//! another. This module only stores, reads and clears; the engine method does the check first.
//!
//! The host app names its credential service and its digest domain once in a [`KeyNames`]; the
//! values must not change once keys are stored or owners have signed digests.

use serde_json::{Value, json};

use super::{Refusal, Result};

/// The two names the host app owns: the credential-store service the key lives under, and the
/// domain string that opens the digest an owner-check signature commits to (for example
/// `app.push-key.v1`).
/// `is_engine` answers whether this process is the resident engine (the host app marks it; this
/// module never decides that for itself).
#[derive(Clone, Copy)]
pub struct KeyNames {
    pub service: &'static str,
    pub digest_domain: &'static str,
    pub is_engine: fn() -> bool,
}

const ACCOUNT: &str = "apns-provider-key";
/// The longest `.p8` text accepted (a real one is about 250 bytes).
const MAX_P8_BYTES: usize = 4096;

fn unavailable(why: impl std::fmt::Display) -> Refusal {
    Refusal::new(
        "push_key_unavailable",
        format!("the OS credential store cannot hold the alert key ({why})"),
    )
}

fn require_engine(names: &KeyNames) -> Result<()> {
    if (names.is_engine)() {
        Ok(())
    } else {
        Err(Refusal::new(
            "approval_unavailable",
            "the alert key is held by the engine, and this process is not the engine",
        ))
    }
}

fn open(names: &KeyNames) -> Result<rightkit_credentials::Store> {
    let namespace =
        rightkit_credentials::Namespace::new(names.service.to_string()).map_err(unavailable)?;
    let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
    if !store.is_durable() {
        return Err(unavailable("it is guarded or unavailable"));
    }
    Ok(store)
}

/// The key as the engine's sender needs it. `Debug` hides the key text.
pub struct ApnsKey {
    pub key_id: String,
    pub team_id: String,
    /// The `.p8` file's text (PEM).
    pub p8: String,
}

impl std::fmt::Debug for ApnsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApnsKey")
            .field("key_id", &self.key_id)
            .field("team_id", &self.team_id)
            .field("p8", &"<redacted>")
            .finish()
    }
}

/// An Apple key id or team id: 1 to 32 ASCII letters and digits (Apple issues 10; RightKit's rule).
pub fn valid_apple_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A `.p8` text: a PEM `PRIVATE KEY` block with a base64 body, nothing else that matters.
pub fn valid_p8(text: &str) -> bool {
    let text = text.trim();
    text.len() <= MAX_P8_BYTES
        && text.starts_with("-----BEGIN PRIVATE KEY-----")
        && text.ends_with("-----END PRIVATE KEY-----")
        && text.lines().filter(|l| !l.starts_with("-----")).all(|l| {
            l.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        })
}

/// What an owner-check signature commits to when it stores this key: SHA-256 hex of
/// the digest domain, the key id, the team id and the trimmed `.p8` text, one to a line. The signed operation is
/// `push.key:<this>` (`unlock::device_action`). The notch or hub computes the same digest to ask for the challenge.
pub fn key_digest(names: &KeyNames, key_id: &str, team_id: &str, p8: &str) -> String {
    rightkit_fs::digest::hex(&rightkit_crypto::sha256(
        format!(
            "{}\n{key_id}\n{team_id}\n{}",
            names.digest_domain,
            p8.trim()
        )
        .as_bytes(),
    ))
}

/// Stores the key, replacing any earlier one. The caller has made the owner check. Engine only.
pub fn set(names: &KeyNames, key_id: &str, team_id: &str, p8: &str) -> Result<()> {
    require_engine(names)?;
    if !valid_apple_id(key_id) || !valid_apple_id(team_id) {
        return Err(Refusal::new(
            "invalid_params",
            "the key id and the team id are letters and digits, as Apple shows them",
        ));
    }
    if !valid_p8(p8) {
        return Err(Refusal::new(
            "invalid_params",
            "the key is the text of the .p8 file Apple gave you, from BEGIN PRIVATE KEY to END PRIVATE KEY",
        ));
    }
    let value = json!({ "keyId": key_id, "teamId": team_id, "p8": p8.trim() });
    open(names)?
        .set_str(ACCOUNT, &value.to_string())
        .map_err(unavailable)
}

/// The stored key, or `None` when none was stored. Engine only.
pub fn load(names: &KeyNames) -> Result<Option<ApnsKey>> {
    require_engine(names)?;
    let store = open(names)?;
    let Some(secret) = store.get(ACCOUNT).map_err(unavailable)? else {
        return Ok(None);
    };
    let text = secret.expose_str().map_err(unavailable)?;
    let value: Value =
        serde_json::from_str(text).map_err(|_| unavailable("the stored key is damaged"))?;
    let field = |name: &str| value[name].as_str().map(str::to_string);
    match (field("keyId"), field("teamId"), field("p8")) {
        (Some(key_id), Some(team_id), Some(p8)) => Ok(Some(ApnsKey {
            key_id,
            team_id,
            p8,
        })),
        _ => Err(unavailable("the stored key is damaged")),
    }
}

/// Removes the stored key. True when one was there. The caller has made the owner check. Engine only.
pub fn clear(names: &KeyNames) -> Result<bool> {
    require_engine(names)?;
    open(names)?.delete(ACCOUNT).map_err(unavailable)
}

/// What the owner's page may show: whether a key is stored and its two ids, never the key. Engine only.
pub fn status(names: &KeyNames) -> Result<Value> {
    Ok(match load(names)? {
        Some(key) => json!({ "configured": true, "keyId": key.key_id, "teamId": key.team_id }),
        None => json!({ "configured": false }),
    })
}
