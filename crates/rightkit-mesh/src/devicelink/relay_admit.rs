//! Relay admission for a guest with no direct path (#492, review R04/R13/R20;
//! `docs/architecture/guest-relay-admission.md`).
//!
//! The relay admits only endpoint ids it was told about, and a new guest's key exists only after
//! it installs. Two keys bridge that gap, and neither needs a secret on the wire:
//!
//! * The **bootstrap key**: an Ed25519 key whose seed is HKDF-SHA256 of the join token's secret
//!   (salt `pulse-relay-bootstrap-v1`, info the 16-byte pairing id). The minting computer derives
//!   its public half when it makes a guest link and tells the computer that decides relay
//!   admission (the hosted relay) to admit it until the link expires. The public key is the
//!   commitment to the token secret: only a holder of the secret can complete an iroh handshake
//!   as that endpoint. The guest uses it for one thing: dialling the minting computer on
//!   [`ALPN`] to enrol its real key.
//! * The guest's **device key**: on that stream the minting computer sends a fresh nonce and the
//!   guest signs [`enrol_signing_input`] (domain `pulse-relay-enrol-v1`, pairing id, nonce,
//!   bootstrap key, minting computer key, guest key) with its device key. The minting computer
//!   verifies it and forwards the proof; the deciding computer verifies it again before it admits
//!   the guest key, for the pairing only, until the approval window or the lease ends.
//!
//! Admission to the relay is never trust: every the host app endpoint still authenticates each peer by
//! its key and refuses an unpaired or unapproved one on everything but its open pairing ALPN.

use rightkit_fs::digest::hex;
use ring::hkdf;
use ring::signature::{Ed25519KeyPair, KeyPair};

use super::LinkError;
use super::identity::{DeviceIdentity, verify};
use super::wire::{Enc, random, unhex};

/// The minting computer's enrolment protocol: dialled only by a bootstrap key it registered.
pub const ALPN: &[u8] = b"pulse/relay-enrol/1";
const BOOTSTRAP_SALT: &[u8] = b"pulse-relay-bootstrap-v1";
const ENROL_DOMAIN: &str = "pulse-relay-enrol-v1";
/// Longest relay admission file the engine writes (entries).
pub const ADMIT_FILE_MAX: usize = 4096;

struct Len32;

impl hkdf::KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

/// The bootstrap key's seed for the token with `secret` and `pairing_id`. Treat it as the secret
/// it derives from: wipe it after use, never log it.
pub fn bootstrap_seed(secret: &[u8; 32], pairing_id: &[u8; 16]) -> Result<[u8; 32], LinkError> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, BOOTSTRAP_SALT).extract(secret);
    let info: [&[u8]; 1] = [&pairing_id[..]];
    let okm = prk
        .expand(&info, Len32)
        .map_err(|_| LinkError::Malformed("bootstrap key"))?;
    let mut seed = [0u8; 32];
    okm.fill(&mut seed)
        .map_err(|_| LinkError::Malformed("bootstrap key"))?;
    Ok(seed)
}

/// The public half of the bootstrap key: the endpoint id the relay admits until the link expires.
pub fn bootstrap_key(secret: &[u8; 32], pairing_id: &[u8; 16]) -> Result<[u8; 32], LinkError> {
    let mut seed = bootstrap_seed(secret, pairing_id)?;
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed);
    seed.fill(0);
    let pair = pair.map_err(|_| LinkError::Malformed("bootstrap key"))?;
    let mut public = [0u8; 32];
    public.copy_from_slice(pair.public_key().as_ref());
    Ok(public)
}

/// 64 lowercase hex characters (an endpoint id or device public key).
pub fn is_key_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// 32 lowercase hex characters (a pairing id or an enrolment nonce).
pub fn is_id_hex(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A fresh enrolment nonce, 32 lowercase hex.
pub fn nonce() -> Result<String, LinkError> {
    Ok(hex(&random::<16>()?))
}

/// The bytes the guest's device key signs: domain, pairing id, nonce, bootstrap key, minting
/// computer key and guest key, each lowercase hex and length-prefixed.
pub fn enrol_signing_input(
    pairing_id: &str,
    nonce: &str,
    bootstrap: &str,
    host: &str,
    guest: &str,
) -> Vec<u8> {
    Enc::new(ENROL_DOMAIN)
        .str(pairing_id)
        .str(nonce)
        .str(bootstrap)
        .str(host)
        .str(guest)
        .finish()
}

/// The guest's proof, 128 hex, by its own device key.
pub fn sign_enrol(
    identity: &DeviceIdentity,
    pairing_id: &str,
    nonce: &str,
    bootstrap: &str,
    host: &str,
) -> String {
    identity.sign(&enrol_signing_input(
        pairing_id,
        nonce,
        bootstrap,
        host,
        &identity.public_key_hex(),
    ))
}

/// Checks a guest's proof: every field well formed, the guest key distinct from the bootstrap and
/// minting keys, and the signature valid under the guest key.
pub fn verify_enrol(
    guest: &str,
    pairing_id: &str,
    nonce: &str,
    bootstrap: &str,
    host: &str,
    signature: &str,
) -> Result<(), LinkError> {
    if !is_key_hex(guest)
        || !is_key_hex(bootstrap)
        || !is_key_hex(host)
        || !is_id_hex(pairing_id)
        || !is_id_hex(nonce)
    {
        return Err(LinkError::Malformed("relay enrolment"));
    }
    if guest == bootstrap || guest == host {
        return Err(LinkError::Malformed("relay enrolment key"));
    }
    let key = unhex::<32>(guest)?;
    let signature = unhex::<64>(signature)?;
    verify(
        &key,
        &enrol_signing_input(pairing_id, nonce, bootstrap, host, guest),
        &signature,
    )
}

/// The relay admission file the deciding computer writes for `rightkit-mesh-admission
/// --grant-file`: a comment header, then one line per admitted endpoint id, sorted, each either
/// `<64 hex>` (no expiry: a circle member) or `<64 hex> <epoch ms>` (admitted until then). A key
/// listed twice keeps the later expiry, and no expiry wins. Malformed keys and anything past
/// [`ADMIT_FILE_MAX`] entries are left out.
pub fn render_admit_file(entries: &[(String, Option<u64>)]) -> String {
    let mut merged: std::collections::BTreeMap<&str, Option<u64>> =
        std::collections::BTreeMap::new();
    for (key, until) in entries {
        if !is_key_hex(key) {
            continue;
        }
        let slot = merged.entry(key.as_str()).or_insert(*until);
        *slot = match (*slot, *until) {
            (None, _) | (_, None) => None,
            (Some(a), Some(b)) => Some(a.max(b)),
        };
    }
    let mut out = String::from(
        "# Written by the device engine. Do not edit:\n\
         # it is rewritten from the circle registry and the live guest enrolments.\n",
    );
    for (key, until) in merged.into_iter().take(ADMIT_FILE_MAX) {
        match until {
            None => out.push_str(&format!("{key}\n")),
            Some(ms) => out.push_str(&format!("{key} {ms}\n")),
        }
    }
    out
}
