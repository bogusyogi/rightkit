//! Records of the device circle that a paired-device record embeds: owner-check keys and their
//! bindings, signed endorsement batches, join-token grants and their redemptions.
//!
//! Only data and pure rules live here. Verifying a signature or a shape needs the crypto and the
//! error type of `rightkit-mesh` (`devicelink::circle`), which adds that behaviour to these types
//! through extension traits.

use serde::{Deserialize, Serialize};

use crate::PAIRING_TTL_MS;

/// Signed-action domain of a token grant line (`TokenGrant::line`). A wire id: it never changes.
const GRANT_DOMAIN: &str = "pulse-circle-grant-v2";

/// A redemption's `member_class` for one of the owner's own computers (no lease).
pub const MEMBER_CLASS_MEMBER: &str = "member";
/// A redemption's `member_class` for a leased cloud agent: `lease_expires_at_ms` is its
/// absolute lease end.
pub const MEMBER_CLASS_GUEST: &str = "guest";

/// Proof of possession of an owner-check key for ONE device: the check key itself signed (kind
/// `bind`, request `bind`, in the same owner-presence prompt that enrolled it or the next owner
/// check) a body naming this device id, this device's identity public key and the check key.
/// Anyone can verify it with only the check public key, so a joiner that merely CLAIMS another
/// computer's check key cannot produce one: a check key counts for a device only through a
/// binding that names that device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    /// The device id the check key is bound to.
    pub device_id: String,
    /// That device's identity public key, 64 hex.
    pub device_key: String,
    pub nonce: String,
    pub issued: u64,
    pub expires: u64,
    /// The check key's signature, hex.
    pub signature: String,
}

/// An owner-check public key: `device` is `mac-owner-check` (P-256) or `windows-hello` (RSA-2048).
///
/// A check key counts for a device only through its [`KeyBinding`]: the key's own signature naming
/// that device id and device key, made once when the key was enrolled. Two keys are the same key
/// when `device` and `public_key` are equal; `binding` is carried evidence, never identity. An
/// unbound key, or one bound to another device, is ignored for admit and revoke.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckKey {
    pub device: String,
    /// The public key, hex.
    pub public_key: String,
    /// The key's binding to the device it is attached to. Absent on a key no device has proved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<KeyBinding>,
}

impl PartialEq for CheckKey {
    fn eq(&self, other: &Self) -> bool {
        self.device == other.device && self.public_key == other.public_key
    }
}

impl Eq for CheckKey {}

impl std::hash::Hash for CheckKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.device.hash(state);
        self.public_key.hash(state);
    }
}

/// An owner-check signature with everything a third device needs to verify it with only the public
/// key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerSig {
    /// The owner-check key that signed.
    pub check: CheckKey,
    pub challenge: String,
    pub client: String,
    pub key_id: String,
    pub nonce: String,
    pub issued: u64,
    pub expires: u64,
    pub signature: String,
}

/// What an owner-check signature says about a join token before anyone has redeemed it: this
/// computer (`admitter_device_id`) opened pairing `pairing_id` whose token secret hashes to
/// `secret_hash`, for the pairing lifetime. It never grants `can_admit`. It is signed in the same
/// batch as the endorsement bodies (`EndorsementBatch::grants`), so one prompt covers both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenGrant {
    pub admitter_device_id: String,
    /// The pairing session the token opens, 32 hex.
    pub pairing_id: String,
    /// SHA-256 of the token secret, 64 hex.
    pub secret_hash: String,
    /// Always false: a device admitted through a grant never gets `can_admit`.
    pub can_admit: bool,
    pub issued_at_ms: u64,
    /// `issued_at_ms` plus the pairing lifetime ([`PAIRING_TTL_MS`]).
    pub expires_at_ms: u64,
    #[serde(default)]
    pub member_class: String,
    #[serde(default)]
    pub lease_expires_at_ms: u64,
}

/// SHA-256 of a token secret, 64 hex: what a [`TokenGrant`] carries.
pub fn secret_hash(secret: &[u8; 32]) -> String {
    rightkit_crypto::hex_encode(&rightkit_crypto::sha256(secret))
}

impl TokenGrant {
    /// The grant for a token whose secret is `secret`, opened by `admitter_device_id` at `now_ms`.
    pub fn new(
        admitter_device_id: &str,
        pairing_id: &str,
        secret: &[u8; 32],
        now_ms: u64,
    ) -> TokenGrant {
        TokenGrant {
            admitter_device_id: admitter_device_id.to_string(),
            pairing_id: pairing_id.to_string(),
            secret_hash: secret_hash(secret),
            can_admit: false,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(PAIRING_TTL_MS),
            member_class: MEMBER_CLASS_MEMBER.to_string(),
            lease_expires_at_ms: 0,
        }
    }

    /// The canonical text of this grant inside the signed action; the owner-check signer rebuilds
    /// exactly this.
    pub fn line(&self) -> String {
        format!(
            "{GRANT_DOMAIN}\nadmitter:{}\npairing:{}\nsecret:{}\nadmit:{}\nissued:{}\nexpires:{}\nclass:{}\nlease:{}\n",
            self.admitter_device_id,
            self.pairing_id,
            self.secret_hash,
            u8::from(self.can_admit),
            self.issued_at_ms,
            self.expires_at_ms,
            self.member_class,
            self.lease_expires_at_ms
        )
    }
}

/// The admitting computer's DEVICE-key statement that the token of `pairing_id` was redeemed by
/// this joiner. It makes no trust by itself: only together with an owner-check signed grant for the
/// same pairing id ([`GrantPair`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redemption {
    pub pairing_id: String,
    pub joiner_device_id: String,
    /// The joiner's device identity public key, 64 hex.
    pub joiner_public_key: String,
    pub redeemed_at_ms: u64,
    /// `member` or `guest` ([`MEMBER_CLASS_MEMBER`], [`MEMBER_CLASS_GUEST`]). Empty only on a
    /// redemption made before this field existed, which never verifies.
    #[serde(default)]
    pub member_class: String,
    /// For `guest`: the absolute lease end in epoch ms. 0 for `member`.
    #[serde(default)]
    pub lease_expires_at_ms: u64,
    /// The admitter's device-key signature over the redemption's signing input, 128 hex.
    pub signature: String,
}

impl Redemption {
    /// Whether this redemption makes a leased cloud agent.
    pub fn is_guest(&self) -> bool {
        self.member_class == MEMBER_CLASS_GUEST
    }
}

/// A joiner's endorsement when it joined by token or QR: the batch whose single grant the owner
/// check signed, and the admitter's redemption of that grant by this joiner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantPair {
    pub batch: EndorsementBatch,
    pub redemption: Redemption,
}

impl GrantPair {
    /// The one grant the batch carries.
    pub fn grant(&self) -> Option<&TokenGrant> {
        match self.batch.grants.as_slice() {
            [grant] => Some(grant),
            _ => None,
        }
    }
}

/// One endorsement body: the owner-check signer vouches that `device_id` with `public_key` is a
/// member of the owner's circle, and, when `can_admit`, that `member_check` is that member's own
/// owner-check key and the member may endorse others in turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndorsementBody {
    pub device_id: String,
    /// The member's device identity public key, 64 hex (its endpoint id).
    pub public_key: String,
    /// The member's own owner-check key. Present exactly when `can_admit`.
    #[serde(default)]
    pub member_check: Option<CheckKey>,
    /// Whether the member is a computer whose owner-check key `member_check` may endorse others.
    pub can_admit: bool,
    pub issued_at_ms: u64,
}

impl EndorsementBody {
    /// The canonical text of this body inside the signed action; the owner-check signer rebuilds
    /// exactly this.
    pub fn line(&self) -> String {
        let check = match &self.member_check {
            Some(c) => format!("{}:{}", c.device, c.public_key),
            None => "none".to_string(),
        };
        format!(
            "id:{}\nkey:{}\ncheck:{}\nadmit:{}\nat:{}\n",
            self.device_id,
            self.public_key,
            check,
            u8::from(self.can_admit),
            self.issued_at_ms
        )
    }
}

/// One owner-check signature over a canonical list of [`EndorsementBody`] (one prompt), kept with
/// the bodies so each one verifies against it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndorsementBatch {
    /// Sorted by device id, no id twice.
    pub bodies: Vec<EndorsementBody>,
    /// The token grant the same signature covers (at most one): empty for an approval.
    #[serde(default)]
    pub grants: Vec<TokenGrant>,
    pub sig: OwnerSig,
}

impl EndorsementBatch {
    /// The owner-check key that signed.
    pub fn signer(&self) -> &CheckKey {
        &self.sig.check
    }

    pub fn body_for(&self, device_id: &str) -> Option<&EndorsementBody> {
        self.bodies.iter().find(|b| b.device_id == device_id)
    }

    /// Whether the batch carries a body for exactly this device and key (not yet verified).
    pub fn endorses(&self, device_id: &str, public_key: &str) -> bool {
        self.body_for(device_id)
            .is_some_and(|b| b.public_key == public_key)
    }
}
