//! Paired-device records: what a device-link registry file holds.
//!
//! The registry holds public keys, grants and state only; there is no secret in it. These are the
//! record types and their pure rules; the file, its lock and every mutation live in
//! `rightkit-mesh` (`devicelink::registry`).

use serde::{Deserialize, Serialize};

use crate::device::DeviceRef;
use crate::effect::EffectClass;

/// Version of the registry file.
pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum PairedState {
    Active,
    /// Terminal for this pairing. A new pairing with the same device is a new record
    /// generation with a new `pairing_id`, never a reactivation.
    Revoked {
        at_ms: u64,
        /// Device id of the revoker, or `local` for a revoke made on this device.
        by: String,
    },
}

/// One paired device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub schema_version: u32,
    /// Names this pairing. Resume tickets and revoke messages bind to it, so none of them
    /// survives a revoke and re-pair.
    pub pairing_id: String,
    pub device: DeviceRef,
    /// Peer identity public key, 32 bytes as hex.
    pub public_key: String,
    pub paired_at_ms: u64,
    /// Increases on every change to this record (grants, state, re-pairing). Mutations name
    /// the revision they read and fail if the record moved.
    pub revision: u64,
    pub state: PairedState,
    /// Effect classes this device may REQUEST. Empty means none: pairing grants nothing.
    /// A grant is an upper bound; an irreversible class still needs the owner's approval.
    pub grants: Vec<EffectClass>,
    /// Epoch milliseconds after which the grants are void; `None` means no expiry.
    pub grants_expire_at_ms: Option<u64>,
    /// May this peer revoke OTHER paired devices by a signed message. Set only locally.
    /// No command or UI sets it (always false); kept until a later cleanup.
    pub manage_peers: bool,
    /// The id the transport (`rightkit-daemon`) gave this device when it enrolled, or `None`
    /// until it has. It is not derived from `device.device_id` and never replaces it; it is
    /// only what the host app hands the daemon to revoke or lease to this device. Set with
    /// `DeviceLink::set_transport_id`; read with `#[serde(default)]`, so a version 1
    /// record parses with `None`.
    #[serde(default)]
    pub transport_id: Option<String>,
    /// Joiner side only: the Host's TLS pin (SHA-256 of its CA certificate's SubjectPublicKeyInfo,
    /// hex) the typed code confirmed, kept so this device can dial the Host again after a restart.
    /// `None` on the Host side and on a version 1 record.
    #[serde(default, with = "opt_hex32")]
    pub peer_tls_pin: Option<[u8; 32]>,
    /// Joiner side only: the Host's link port from the confirmed offer. `None` on the Host side.
    #[serde(default)]
    pub peer_link_port: Option<u16>,
    /// When this pairing's first delivery was confirmed (Host: the pairing session ended Ok or a
    /// resume was served; Joiner: the outcome was recorded). `None` marks a record written by
    /// `complete_pairing` whose delivery was never confirmed: the engine's startup sweep revokes
    /// it after the resume window.
    #[serde(default)]
    pub confirmed_ms: Option<u64>,
    /// Circle: the device id of the member that introduced this device (`docs/device-link.md`,
    /// "Circle"). `None` for a device this one paired with directly. An introduced record starts
    /// with the same grants as a paired one: none.
    #[serde(default)]
    pub introduced_by: Option<String>,
    /// Circle: the admission id of this device, chosen by the member that admitted it (32 hex). It
    /// names the circle pairing every other member derives with this device, so it is the same on
    /// every computer. `None` on a record written before the circle existed.
    #[serde(default)]
    pub introduction_id: Option<String>,
    /// Circle: the relay URL (`https` only) this device was introduced with, a dial hint that is
    /// never trusted. `None` for a directly paired device.
    #[serde(default)]
    pub relay: Option<String>,
    /// Circle: may this device issue member lists, introductions and removals that this one
    /// accepts? Set only for a Mac or Windows computer whose owner-check key this device trusts:
    /// the one its own direct pairing verified (`DeviceLink::complete_pairing_with_host_proof`,
    /// `DeviceLink::record_endorsement`), or one a trusted endorsement carried with `can_admit`
    /// (`circle.rs`); never for a phone, a headless server or a PIN-only pairing. An owner's
    /// `DeviceLink::set_admit` can also change it. A record from before this field reads as
    /// `false`: re-grant it.
    #[serde(default)]
    pub admit: bool,
    /// Circle: the owner-check public key of this device's owner, trusted by the rule above. A
    /// device holding `admit` without one vouches for nobody: only a batch signed by a trusted key
    /// adds a member.
    #[serde(default)]
    pub admit_check_key: Option<crate::circle::CheckKey>,
    /// Circle: the endorsement batch (one owner-check signature over a list of bodies) that
    /// vouches for THIS device, kept to list it to others. For a device this computer admitted it
    /// is the batch its own prompt signed. `None` for a PIN admission, a join-token admission and
    /// the host this device joined (no batch endorses a joiner's host until this device signs one).
    #[serde(default)]
    pub endorsement: Option<crate::circle::EndorsementBatch>,
    /// Circle: the join-token endorsement of THIS device, kept to list it to others: the owner-check
    /// signed token grant and the admitting computer's device-key redemption (`circle::GrantPair`).
    /// Set for a device that joined by token or QR (never `admit`: a grant never confers it), on the
    /// admitting computer and on every member that accepted the pair. `None` otherwise.
    #[serde(default)]
    pub grant_pair: Option<crate::circle::GrantPair>,
    /// Circle: the owner took `admit` from this device (`DeviceLink::set_admit(false)`); a later
    /// bound-key update or endorsement never gives it back. Cleared by `set_admit(true)`.
    #[serde(default)]
    pub admit_denied: bool,
    /// Joiner side only: the owner-check key that signed this pairing's host approval, kept when
    /// it was not yet bound to the host device. The same key, once the host proves
    /// its binding in a signed `CheckKeyUpdate`, is trusted (`admit`); no other key is.
    #[serde(default)]
    pub pairing_check: Option<crate::circle::CheckKey>,
    /// Guest pass: one of the owner's cloud agents, joined by a one-time link and approved by
    /// him. It works like his other machines, for its lease, but never adds or manages devices and
    /// has no phone admin path. `None` for every other device.
    #[serde(default)]
    pub guest: Option<GuestPass>,
    /// What this member is to the owner: their own `Computer` (Mac, Windows, phone), the
    /// hosted `Server`, or a leased `Guest` cloud agent. Only the owner sets it, locally (the device role command); a record without it reads as `Unclassified`. A guest record is a guest whatever
    /// this says (`PairedDevice::member_role`). `Server` and `Guest` requests for a reserved action
    /// need the owner's phone (`chat::confirm`).
    #[serde(default)]
    pub role: MemberRole,
}

/// A member's role. The sender's own word never sets it: the receiving computer reads it
/// from its registry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemberRole {
    /// One of the owner's computers or his phone: its `said` answers are enough.
    Computer,
    /// No local owner classification yet: reserved actions require the phone.
    #[default]
    Unclassified,
    /// The Hetzner server: a full chat member whose reserved actions need the phone.
    Server,
    /// A leased cloud agent (`<name> (cloud)`): the same.
    Guest,
}

impl MemberRole {
    pub fn as_str(self) -> &'static str {
        match self {
            MemberRole::Computer => "computer",
            MemberRole::Unclassified => "unclassified",
            MemberRole::Server => "server",
            MemberRole::Guest => "guest",
        }
    }

    /// Whether a reserved action requested by this role needs the phone's confirmation.
    pub fn needs_phone(self) -> bool {
        !matches!(self, MemberRole::Computer)
    }
}

/// What a cloud agent may not do on any computer: add or manage devices (circle and revoke
/// messages, `admit`) and the phone admin path (`admin.*`).
pub const GUEST_REFUSED: [&str; 2] = ["devices", "admin"];

/// A guest's admission: minted behind the owner check, approved by the owner,
/// leased. A guest that is not approved, or whose lease ran out, is not active anywhere.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestPass {
    /// The device id of the computer whose owner minted the link.
    pub minted_by: String,
    /// When the owner approved this guest; `None` while it waits in the guests list.
    #[serde(default)]
    pub approved_ms: Option<u64>,
    /// The lease end: the guest is dropped at this time, approved or not.
    pub lease_expires_ms: u64,
    /// Dropped after this long with no successful call (two hours by default).
    pub idle_ms: u64,
    /// The last successful call from the guest; renews the idle window.
    #[serde(default)]
    pub last_seen_ms: Option<u64>,
    /// The check code the guest printed, shown beside it for the owner to compare.
    pub check_code: String,
    /// The cloud session id the guest reported about itself. Unverified: shown as "reported".
    #[serde(default)]
    pub reported_session: Option<String>,
}

/// A guest's idle window: dropped after two hours with no successful call.
pub const GUEST_IDLE_MS: u64 = 2 * 60 * 60 * 1000;
/// "Last seen" is written at most this often (five minutes).
pub const GUEST_SEEN_WRITE_MS: u64 = 5 * 60 * 1000;
/// The default and the longest guest lease.
pub const GUEST_LEASE_DEFAULT_MS: u64 = 24 * 60 * 60 * 1000;
pub const GUEST_LEASE_MAX_MS: u64 = 7 * 24 * 60 * 60 * 1000;

impl GuestPass {
    /// The check code the guest prints and the owner compares: three digits from SHA-256
    /// of the pairing id and the guest's public key, so a second box racing the same link shows a
    /// different code. Computed the same way on the guest.
    pub fn check_code(pairing_id: &str, public_key_hex: &str) -> String {
        let digest = rightkit_crypto::sha256(
            format!("pulse-guest-check-v1\n{pairing_id}\n{public_key_hex}").as_bytes(),
        );
        format!("{}-{}-{}", digest[0] % 10, digest[1] % 10, digest[2] % 10)
    }

    /// Approved, inside its lease and not idle at `now_ms`. Idleness is counted
    /// per computer: the window starts at the last successful call this computer served (or at the
    /// approval when there was none) and is `idle_ms` long, two hours by default. A guest dropped
    /// here for idleness is not revoked anywhere: other computers keep their own window. "Last seen"
    /// is written at most every `GUEST_SEEN_WRITE_MS`, far inside the idle window.
    pub fn admits(&self, now_ms: u64) -> bool {
        let Some(approved) = self.approved_ms else {
            return false;
        };
        if now_ms >= self.lease_expires_ms {
            return false;
        }
        let idle = if self.idle_ms == 0 {
            GUEST_IDLE_MS
        } else {
            self.idle_ms
        };
        let since = self
            .last_seen_ms
            .map_or(approved, |seen| seen.max(approved));
        now_ms.saturating_sub(since) < idle
    }

    /// True when "last seen" should be written again at `now_ms`.
    pub fn seen_is_stale(&self, now_ms: u64) -> bool {
        self.last_seen_ms
            .is_none_or(|at| now_ms.saturating_sub(at) > GUEST_SEEN_WRITE_MS)
    }
}

/// `Option<[u8; 32]>` as an optional lowercase hex string.
mod opt_hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<[u8; 32]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(bytes) => serializer.serialize_some(&rightkit_crypto::hex_encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<[u8; 32]>, D::Error> {
        let text = Option::<String>::deserialize(deserializer)?;
        text.map(|text| super::unhex32(&text).ok_or_else(|| serde::de::Error::custom("hex length")))
            .transpose()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl PairedDevice {
    /// Active, and for a guest also approved and inside its lease (`GuestPass::admits`): a pending
    /// or lapsed guest is refused everywhere this is asked, failing closed.
    pub fn is_active(&self) -> bool {
        matches!(self.state, PairedState::Active)
            && self.guest.as_ref().is_none_or(|g| g.admits(now_ms()))
    }

    /// Not revoked, whatever a guest pass says: the state a revoke, a re-pair or an introduction
    /// acts on. Trust decisions use `is_active`; a revoke must also reach a pending or lapsed guest.
    pub fn is_unrevoked(&self) -> bool {
        matches!(self.state, PairedState::Active)
    }

    /// A cloud agent: see `GUEST_REFUSED`.
    pub fn is_guest(&self) -> bool {
        self.guest.is_some()
    }

    /// The role: `Guest` for a guest record, else the stored one.
    pub fn member_role(&self) -> MemberRole {
        if self.is_guest() {
            MemberRole::Guest
        } else {
            self.role
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub schema_version: u32,
    /// Increases on every save.
    pub revision: u64,
    pub devices: Vec<PairedDevice>,
    /// Every join-token pairing id this device has seen redeemed, with the joiner key and when it
    /// was first seen (R07). It outlives the device's record, so a second redemption of the same
    /// pairing id for a different joiner key is refused even when the first joiner was removed.
    /// Entries are dropped once the grant window and the delivery allowance have passed
    /// (`circle::note_redemptions`).
    #[serde(default)]
    pub redeemed_pairings: Vec<RedeemedPairing>,
}

/// One redeemed join-token pairing id (`Registry::redeemed_pairings`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemedPairing {
    pub pairing_id: String,
    pub joiner_device_id: String,
    pub joiner_public_key: String,
    pub first_seen_ms: u64,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            revision: 0,
            devices: Vec::new(),
            redeemed_pairings: Vec::new(),
        }
    }
}

impl Registry {
    pub fn get(&self, device_id: &str) -> Option<&PairedDevice> {
        self.devices
            .iter()
            .find(|d| d.device.device_id == device_id)
    }

    pub fn get_mut(&mut self, device_id: &str) -> Option<&mut PairedDevice> {
        self.devices
            .iter_mut()
            .find(|d| d.device.device_id == device_id)
    }
}

/// Decodes exactly 32 bytes of lowercase or uppercase hex.
fn unhex32(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(bytes.chunks(2)) {
        *slot = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}
