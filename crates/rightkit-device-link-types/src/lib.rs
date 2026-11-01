//! Record types shared by the RightKit device-link crates.
//!
//! * [`device`]: the portable reference to a paired device.
//! * [`effect`]: the classes of effect a device may request.
//! * [`principal`]: who submitted or authorized an act.
//! * [`registry`]: paired-device records and the registry document that holds them.
//! * [`circle`]: the signed device-circle records a paired-device record embeds.
//!
//! Everything here is serde data and pure rules: no file, no socket, no signature check.
//! `rightkit-owner-approvals` and `rightkit-mesh` build on it.

pub mod circle;
pub mod device;
pub mod effect;
pub mod principal;
pub mod registry;

pub use circle::{
    secret_hash, CheckKey, EndorsementBatch, EndorsementBody, GrantPair, KeyBinding, OwnerSig,
    Redemption, TokenGrant, MEMBER_CLASS_GUEST, MEMBER_CLASS_MEMBER,
};
pub use device::{
    DevicePlatform, DeviceRef, DEVICE_PLATFORM_SCHEMA_VERSION, DEVICE_REF_SCHEMA_VERSION,
};
pub use effect::{EffectClass, EFFECT_CLASS_SCHEMA_VERSION};
pub use principal::{
    Principal, PrincipalKind, PRINCIPAL_KIND_SCHEMA_VERSION, PRINCIPAL_SCHEMA_VERSION,
};
pub use registry::{
    GuestPass, MemberRole, PairedDevice, PairedState, RedeemedPairing, Registry, GUEST_IDLE_MS,
    GUEST_LEASE_DEFAULT_MS, GUEST_LEASE_MAX_MS, GUEST_REFUSED, GUEST_SEEN_WRITE_MS,
    REGISTRY_SCHEMA_VERSION,
};

/// A pairing session (offer to final confirmation) lives this long, then it is void.
pub const PAIRING_TTL_MS: u64 = 600_000;
