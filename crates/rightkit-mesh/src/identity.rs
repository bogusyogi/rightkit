//! Device identity and trust, as the mesh sees them.
//!
//! A device IS its Ed25519 key. Pulse already holds that key as a 32-byte seed in the OS credential
//! store (`core/src/devicelink/identity.rs`); the same seed becomes the iroh `SecretKey`, so the
//! iroh `EndpointId` equals the device-link public key and no device is re-keyed or re-paired.
//! Both ring and iroh derive the public key from the seed by RFC 8032, so the two agree.
//!
//! Trust is NOT forked here. The mesh asks a [`PeerTrust`] "is this key an active paired device?";
//! Pulse answers from its device-link registry (implemented by the host app, e.g. Pulse). The mesh never stores a second
//! list of devices.

use std::fmt::Debug;

use iroh::SecretKey;

use crate::circle::Introduction;
use crate::error::MeshError;

/// The iroh secret key for an identity seed.
pub fn secret_key_from_seed(seed: &[u8; 32]) -> SecretKey {
    SecretKey::from_bytes(seed)
}

/// The public key (iroh `EndpointId` bytes, device-link public key) for an identity seed.
pub fn public_key_for_seed(seed: &[u8; 32]) -> [u8; 32] {
    *secret_key_from_seed(seed).public().as_bytes()
}

/// An active paired device, as the trust source reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerInfo {
    /// The device's public key; also its iroh endpoint id.
    pub endpoint: [u8; 32],
    /// The device-link id (`dev_` plus 32 hex), derived from the key.
    pub device_id: String,
    /// `mac`, `windows`, `linux`, `ios`, `android` ...
    pub platform: String,
    pub display_name: Option<String>,
    /// A guest (Pulse #492): reached and served only on the mesh's `guest_alpns`.
    pub guest: bool,
}

/// What became of an introduction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntroOutcome {
    /// The device was added with no grants.
    Added,
    /// The device was already an active member; nothing changed.
    AlreadyMember,
    /// The device was revoked here before; a new pairing, not an introduction, is needed.
    Revoked,
}

/// The device-link trust the mesh depends on. Implementations answer from the registry file on
/// every call, so a revocation made by another process takes effect on the next connection.
pub trait PeerTrust: Debug + Send + Sync + 'static {
    /// The active paired device with this key, or `None` (unknown, revoked or unreadable registry).
    fn peer(&self, endpoint: &[u8; 32]) -> Option<PeerInfo>;

    /// The active paired device with this id.
    fn peer_by_device(&self, device_id: &str) -> Option<PeerInfo>;

    /// Every active paired device.
    fn members(&self) -> Vec<PeerInfo>;

    /// Add a device a circle member introduced (D4): active, NO grants. `from` is the verified
    /// introducer. Must never grant anything and must not reactivate a revoked record.
    fn apply_introduction(
        &self,
        from: &PeerInfo,
        introduction: &Introduction,
    ) -> Result<IntroOutcome, MeshError>;

    /// Revoke `target_device_id` because circle member `from` asked (D4, revoke-only: removes trust,
    /// never adds it). Idempotent. Revoking `from` itself is allowed ("stop trusting me").
    fn apply_revoke(
        &self,
        from: &PeerInfo,
        target_device_id: &str,
        now_ms: u64,
    ) -> Result<(), MeshError>;
}
