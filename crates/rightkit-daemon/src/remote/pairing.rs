//! Short-code pairing (`rk-pair/1`), the shared half: the commitment, the code, and the device
//! side's request bodies. No transport, no state. The host side is `Daemon::open_pairing` and
//! friends (feature `remote`).
//!
//! The protocol is a short authenticated string (SAS) exchange with a commit-reveal step:
//!
//! 1. The joining device D commits: `commit = SHA-256(frame("rk-pair/1 commit", key_D, nonce_D,
//!    label))`, and sends `{publicKey, commit, label}` to the host H.
//! 2. H answers with a fresh random `nonce_H`. D has committed to its key and nonce before it
//!    sees `nonce_H`, and H picked `nonce_H` before it saw `nonce_D`.
//! 3. D reveals `nonce_D`; H checks it against the commitment.
//! 4. Both compute the same six-digit code from the whole transcript:
//!    `SHA-256(frame("rk-pair/1 sas", key_D, nonce_D, nonce_H, label, host_binding))`, first four
//!    bytes as a big-endian number, modulo 10^6. H shows it only to its owner (in-process);
//!    D shows its own. The owner reads D's code off D's screen and types it into H.
//! 5. H enrols `key_D` only if the typed code equals the code H computed.
//!
//! Why this resists a man in the middle: a party that sits between D and H and swaps in its own
//! key must commit to `(key_M, nonce_M)` toward H before it learns `nonce_H`, and must pick the
//! `nonce_H'` it shows D before it learns `nonce_D` (D's commitment hides it). Neither code can
//! therefore be steered, and the two codes agree only by chance, 1 in 10^6 per attempt. The host
//! caps attempts (see `PairingOptions`), so the odds stay about that for the whole window.
//! `host_binding` (for example a hash of the TLS certificate both sides see) additionally makes
//! the code differ when the TLS channel was terminated by someone else.
//!
//! The code is 20 bits. It is safe only because it is checked online, by a person, under a
//! small attempt budget; it is not a secret to offline-guess against and must never be logged,
//! put in an event or sent over the channel.

use base64::Engine;
use serde_json::{json, Value};

use crate::remote::util::{hex, random_bytes, unhex};

// The commitment, the code and their constants are `rightkit-identity`'s (it owns the wire
// bytes); re-exported so these paths stay the daemon's public ones.
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
pub use rightkit_identity::{
    pairing_code, pairing_commit, CODE_DIGITS, MAX_LABEL_CHARS, NONCE_BYTES,
};

/// The device side of a pairing, with no transport: build the three request bodies and compute
/// the code to display. The caller sends them (`POST /v1/pair`, `POST /v1/pair/{id}/reveal`,
/// `GET /v1/pair/{id}`) over the pinned TLS channel it already uses for the host.
pub struct PairingInitiator {
    public_key: Vec<u8>,
    label: Option<String>,
    nonce: [u8; NONCE_BYTES],
    host_binding: Vec<u8>,
}

impl PairingInitiator {
    /// `public_key` is the 32-byte Ed25519 key the device will sign with afterwards.
    /// `host_binding` must equal what the host passed to `PairingOptions::host_binding`.
    pub fn new(public_key: &[u8], label: Option<&str>, host_binding: &[u8]) -> Self {
        Self {
            public_key: public_key.to_vec(),
            label: label.map(str::to_string),
            nonce: random_bytes::<NONCE_BYTES>(),
            host_binding: host_binding.to_vec(),
        }
    }

    /// Body of `POST /v1/pair`.
    pub fn begin_body(&self) -> Value {
        let mut body = json!({
            "publicKey": URL_SAFE_NO_PAD.encode(&self.public_key),
            "commit": pairing_commit(&self.public_key, &self.nonce, self.label.as_deref()),
        });
        if let Some(label) = &self.label {
            body["label"] = Value::String(label.clone());
        }
        body
    }

    /// Body of `POST /v1/pair/{id}/reveal`. Send it only after the host's `nonce` arrived: the
    /// commitment must reach the host before the nonce does.
    pub fn reveal_body(&self) -> Value {
        json!({ "nonce": hex(&self.nonce) })
    }

    /// The code to show the owner, from the host's `nonce` (hex, from the begin response).
    /// `None` when that is not a well-formed nonce.
    pub fn code(&self, nonce_h_hex: &str) -> Option<String> {
        let nonce_h = unhex(nonce_h_hex).filter(|n| n.len() == NONCE_BYTES)?;
        Some(pairing_code(
            &self.public_key,
            &self.nonce,
            &nonce_h,
            self.label.as_deref(),
            &self.host_binding,
        ))
    }
}
