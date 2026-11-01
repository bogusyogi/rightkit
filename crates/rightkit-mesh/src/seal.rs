//! End-to-end seal for mailbox items, and the per-pairing keys behind it.
//!
//! The mailbox server ends QUIC, so a sealed item is the only thing that keeps it blind.
//! Design (`docs/relay-hetzner.md`, "Seal"):
//!
//! * Each pairing holds two 32-byte keys, one per direction, exchanged over the first live
//!   iroh connection between the two devices (`keys.rs`, ALPN `right/keys/1`). That connection
//!   is authenticated by the devices' keys and gated by the paired registry, so the keys are
//!   exchanged only between paired devices and never touch a server.
//! * Per item: a random 16-byte salt, HKDF-SHA256 over the direction key, one ChaCha20-Poly1305
//!   key per item (so the all-zero nonce is never reused under a key), AAD binding sender,
//!   recipient, record id and sender sequence. The server cannot re-address or swap an item.
//! * The plaintext is length-prefixed and padded to 256 B, 2 KB, 16 KB, 64 KB steps (then
//!   64 KB multiples) so size reveals little.
//! * No forward secrecy: a stolen direction key opens that direction's stored items until the
//!   epoch is replaced. Items are deleted on ack and expire after 7 days, which bounds the
//!   exposure. A ratchet is a later version (the epoch field is there for it).
//!
//! Sender authenticity comes from the AEAD key shared only by the two paired devices. Replay
//! and reorder are the receiver's job: the sender sequence is in the AAD, and the engine's
//! request replay store stays the final guard.
//!
//! ring only: no second crypto family, no new crate.

use std::collections::HashMap;
use std::fmt::{self, Debug};
use std::sync::{Mutex, PoisonError};

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::hkdf;
use zeroize::Zeroize;

use crate::error::MeshError;
use crate::util::random;

const SEAL_VERSION: u8 = 1;
const INFO: &[u8] = b"rightkit-mesh/seal/v1";
const AAD_DOMAIN: &[u8] = b"rightkit-mesh/aad/v1";
const HEADER_LEN: usize = 1 + 4 + 8 + 16;
const TAG_LEN: usize = 16;
const BUCKETS: [usize; 4] = [256, 2048, 16 * 1024, 64 * 1024];

/// The two directional keys of one pairing at one epoch.
#[derive(Clone)]
pub struct PairKeys {
    /// The other device's key.
    pub peer: [u8; 32],
    /// Increases with every re-exchange; the newest epoch seals, any held epoch opens.
    pub epoch: u32,
    /// Seals items this device sends to `peer`.
    pub send: [u8; 32],
    /// Opens items `peer` sent to this device.
    pub recv: [u8; 32],
}

impl Drop for PairKeys {
    fn drop(&mut self) {
        self.send.zeroize();
        self.recv.zeroize();
    }
}

impl Debug for PairKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairKeys")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// Where pairing keys are kept. The app supplies a durable implementation (the OS credential
/// store); [`MemoryKeyStore`] loses them on restart, which only costs a fresh exchange on the
/// next live connection, never a wrong key.
pub trait KeyStore: Debug + Send + Sync + 'static {
    /// Held keys for `peer`, newest epoch first (at most two are kept).
    fn keys(&self, peer: &[u8; 32]) -> Vec<PairKeys>;
    fn save(&self, keys: PairKeys) -> Result<(), MeshError>;
    /// Drop every key held for `peer` (the pairing was revoked).
    fn forget(&self, peer: &[u8; 32]) -> Result<(), MeshError>;
}

#[derive(Debug, Default)]
pub struct MemoryKeyStore(Mutex<HashMap<[u8; 32], Vec<PairKeys>>>);

impl MemoryKeyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for MemoryKeyStore {
    fn keys(&self, peer: &[u8; 32]) -> Vec<PairKeys> {
        let map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(peer).cloned().unwrap_or_default()
    }

    fn save(&self, keys: PairKeys) -> Result<(), MeshError> {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let held = map.entry(keys.peer).or_default();
        held.retain(|k| k.epoch != keys.epoch);
        held.push(keys);
        held.sort_by_key(|k| std::cmp::Reverse(k.epoch));
        held.truncate(2);
        Ok(())
    }

    fn forget(&self, peer: &[u8; 32]) -> Result<(), MeshError> {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.remove(peer);
        Ok(())
    }
}

fn padded_len(content: usize) -> usize {
    for bucket in BUCKETS {
        if content <= bucket {
            return bucket;
        }
    }
    let step = BUCKETS[BUCKETS.len() - 1];
    content.div_ceil(step) * step
}

fn item_key(direction_key: &[u8; 32], salt: &[u8; 16]) -> Result<LessSafeKey, MeshError> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(direction_key);
    let okm = prk
        .expand(&[INFO], hkdf::HKDF_SHA256)
        .map_err(|_| MeshError::Crypto("key derivation"))?;
    let mut key = [0u8; 32];
    okm.fill(&mut key)
        .map_err(|_| MeshError::Crypto("key derivation"))?;
    let unbound =
        UnboundKey::new(&CHACHA20_POLY1305, &key).map_err(|_| MeshError::Crypto("cipher key"))?;
    key.zeroize();
    Ok(LessSafeKey::new(unbound))
}

fn aad(from: &[u8; 32], to: &[u8; 32], record_id: &[u8; 16], seq: u64, epoch: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_DOMAIN.len() + 32 + 32 + 16 + 8 + 4);
    out.extend_from_slice(AAD_DOMAIN);
    out.extend_from_slice(from);
    out.extend_from_slice(to);
    out.extend_from_slice(record_id);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&epoch.to_be_bytes());
    out
}

/// Seal `plaintext` from `from` to the paired device `keys.peer` (`to`). `seq` is the sender's
/// per-peer counter; the caller persists it so it only ever increases.
pub fn seal(
    keys: &PairKeys,
    from: &[u8; 32],
    record_id: &[u8; 16],
    seq: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>, MeshError> {
    let to = &keys.peer;
    let salt = random::<16>()?;
    let cipher = item_key(&keys.send, &salt)?;
    let mut buffer = Vec::with_capacity(padded_len(4 + plaintext.len()) + TAG_LEN);
    buffer.extend_from_slice(&(plaintext.len() as u32).to_be_bytes());
    buffer.extend_from_slice(plaintext);
    buffer.resize(padded_len(4 + plaintext.len()), 0);
    let aad = aad(from, to, record_id, seq, keys.epoch);
    cipher
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key([0u8; 12]),
            Aad::from(aad.as_slice()),
            &mut buffer,
        )
        .map_err(|_| MeshError::Crypto("seal"))?;
    let mut out = Vec::with_capacity(HEADER_LEN + buffer.len());
    out.push(SEAL_VERSION);
    out.extend_from_slice(&keys.epoch.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&buffer);
    Ok(out)
}

/// The sender sequence stored in a sealed item's clear header, without opening it. `None` for a
/// malformed item.
pub fn peek_seq(sealed: &[u8]) -> Option<u64> {
    let raw: [u8; 8] = sealed.get(5..13)?.try_into().ok()?;
    Some(u64::from_be_bytes(raw))
}

/// Open an item `from` sealed to `to` (this device). `candidates` are the keys held for `from`;
/// the one whose epoch matches the item opens it. Returns the sender sequence and the plaintext.
pub fn open(
    candidates: &[PairKeys],
    from: &[u8; 32],
    to: &[u8; 32],
    record_id: &[u8; 16],
    sealed: &[u8],
) -> Result<(u64, Vec<u8>), MeshError> {
    if sealed.len() < HEADER_LEN + TAG_LEN || sealed[0] != SEAL_VERSION {
        return Err(MeshError::Malformed("sealed item"));
    }
    let epoch = u32::from_be_bytes([sealed[1], sealed[2], sealed[3], sealed[4]]);
    let seq = peek_seq(sealed).ok_or(MeshError::Malformed("sealed item"))?;
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&sealed[13..HEADER_LEN]);
    let keys = candidates
        .iter()
        .find(|k| k.epoch == epoch && &k.peer == from)
        .ok_or(MeshError::NoKeys)?;
    let cipher = item_key(&keys.recv, &salt)?;
    let mut buffer = sealed[HEADER_LEN..].to_vec();
    let aad = aad(from, to, record_id, seq, epoch);
    let plain = cipher
        .open_in_place(
            Nonce::assume_unique_for_key([0u8; 12]),
            Aad::from(aad.as_slice()),
            &mut buffer,
        )
        .map_err(|_| MeshError::Crypto("open: wrong key, wrong addressee or tampered"))?;
    if plain.len() < 4 {
        return Err(MeshError::Malformed("sealed item"));
    }
    let len = u32::from_be_bytes([plain[0], plain[1], plain[2], plain[3]]) as usize;
    let body = plain
        .get(4..4 + len)
        .ok_or(MeshError::Malformed("sealed item length"))?
        .to_vec();
    Ok((seq, body))
}
