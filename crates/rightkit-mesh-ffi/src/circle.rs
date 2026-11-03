//! The phone's side of the device circle (`core/src/devicelink/circle.rs`, `docs/device-link.md`
//! "Circle"). The phone accepts no inbound stream, so it PULLS: it asks a computer it paired with
//! for `device.circle.members` on `pulse/link/1`, verifies the Ed25519 signature of that computer
//! over the list, and records the members the list proves (no grants: a phone holds only public
//! keys). A list never removes a member it omits; only a valid signed revoke in the same reply does.
//!
//! Trust is an owner-check key and nothing else. The phone trusts exactly (a) the owner-check key
//! it verified itself when it paired (the host's transcript-hash signature on this phone, pinned as
//! `Host::admit_check`), and (b) recursively, an owner-check key carried with `can_admit` in an
//! endorsement batch whose signer it already trusts. A member is added only when such a batch
//! carries a body for exactly that device and key. The computer's device-key signature on the list
//! authenticates the courier; it never creates trust.
//!
//! The signed bytes mirror `MemberList::signing_input`, `CircleRevoke::signing_input` and
//! `wire::Enc` field for field.

use rightkit_crypto::sha256;
use rightkit_fs::digest::hex;
use ring::signature::{
    ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_FIXED, ED25519, RSA_PKCS1_2048_8192_SHA256,
    UnparsedPublicKey,
};
use serde::{Deserialize, Serialize};

use crate::pairing::{
    CLOCK_SKEW_MS, DEVICE_REF_SCHEMA_VERSION, Enc, PmError, SCHEMA_VERSION, device_id_for_key,
    now_ms, unhex,
};
use crate::store::Host;

const MEMBERS_DOMAIN: &str = "pulse-link/circle-members/v1";
const REVOKE_DOMAIN: &str = "pulse-link/circle-revoke/v1";
const PAIRING_DOMAIN: &str = "pulse-link/circle-pairing/v1";
const GRANT_DOMAIN: &str = "pulse-circle-grant-v2";
/// `circle::REDEEM_DOMAIN`, version 2 (R06): the member class and the guest lease are signed. A
/// signature over the version 1 bytes does not verify under this domain.
const REDEEM_DOMAIN: &str = "pulse-circle-redeem-v2";
/// `circle::MEMBER_CLASS_MEMBER` / `MEMBER_CLASS_GUEST`.
const MEMBER_CLASS_MEMBER: &str = "member";
const MEMBER_CLASS_GUEST: &str = "guest";
/// `registry::GUEST_LEASE_MAX_MS`: the longest lease a guest redemption may carry.
const GUEST_LEASE_MAX_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// `circle::REDEMPTION_DELIVERY_ALLOWANCE_MS` (R07): how long after a grant's own expiry (by this
/// phone's clock) a redemption of it may still count.
const REDEMPTION_DELIVERY_ALLOWANCE_MS: u64 = 0;
/// `PAIRING_TTL_MS`: a token grant lives exactly this long.
const PAIRING_TTL_MS: u64 = 600_000;
/// `CIRCLE_MAX_PAIRS`.
const MAX_PAIRS: usize = 32;
/// `CIRCLE_MESSAGE_TTL_MS`.
const TTL_MS: u64 = 10 * 60 * 1000;
/// `CIRCLE_REVOKE_TTL_MS`.
const REVOKE_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// `CIRCLE_MAX_MEMBERS`.
const MAX_MEMBERS: usize = 32;
/// `CIRCLE_MAX_BATCHES`.
const MAX_BATCHES: usize = 32;
const PLATFORMS: [&str; 5] = ["mac", "windows", "linux", "ios", "android"];

#[derive(Deserialize)]
pub struct DeviceRef {
    schema_version: u32,
    device_id: String,
    platform: String,
    display_name: Option<String>,
}

#[derive(Deserialize)]
pub struct Member {
    device: DeviceRef,
    public_key: String,
    relay: Option<String>,
    /// `circle::CircleMember::cloud_lease_ms`: set only for one of the owner's cloud agents (#492),
    /// whose lease end (epoch ms) is part of the signed bytes. It must be read to verify the list;
    /// this phone then never adds such a member (it holds no lease and a guest has no phone role).
    #[serde(default)]
    cloud_lease_ms: Option<u64>,
}

/// `circle::CheckKey`: an owner-check public key (`mac-owner-check` P-256, `windows-hello` RSA),
/// with its binding to the device it is attached to. Two keys are equal when `device` and
/// `public_key` are; the binding is carried evidence, never identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckKey {
    pub device: String,
    pub public_key: String,
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

/// `approval_sig::KeyBinding`: the check key's own signature (kind `bind`) over a body naming one
/// device id, that device's identity key and the check key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub device_id: String,
    pub device_key: String,
    pub nonce: String,
    pub issued: u64,
    pub expires: u64,
    pub signature: String,
}

impl CheckKey {
    /// `circle::CheckKey::bound_to` (`approval_sig::check_key_binding`): whether the key carries a
    /// binding it signed itself for exactly this device id and device key (hex). The only way a
    /// check key is attached to a device; an unbound or mismatched key is ignored, never trusted.
    pub fn bound_to(&self, device_id: &str, device_key: &str) -> bool {
        let ok = self.binding.as_ref().is_some_and(|b| {
            b.device_id == device_id
                && b.device_key == device_key
                && binding_signature_ok(&self.device, &self.public_key, b)
        });
        if !ok {
            eprintln!("circle: owner-check key is not bound to {device_id}; ignored");
        }
        ok
    }
}

/// `approval_sig::check_key_binding`: the binding is the check key's signature over
/// `bind_action` (SHA-256 hex of `pulse-check-bind-v1`, device id, device key and the check key),
/// in the v1 approval envelope (kind `bind`, request `bind`, no client).
fn binding_signature_ok(check_device: &str, check_public: &str, b: &KeyBinding) -> bool {
    let (Some(public), Some(signature)) = (unhex_vec(check_public), unhex_vec(&b.signature)) else {
        return false;
    };
    let key_shape = match check_device {
        OWNER_DEVICE => is_p256(&public),
        HELLO_DEVICE => is_hello_rsa(&public),
        _ => false,
    };
    let ids_ok = b.device_id.len() <= 64
        && b.device_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && b.device_key.len() == 64
        && b.device_key.bytes().all(|c| c.is_ascii_hexdigit());
    let nonce_ok =
        (32..=64).contains(&b.nonce.len()) && b.nonce.chars().all(|c| c.is_ascii_hexdigit());
    if !key_shape
        || !ids_ok
        || !nonce_ok
        || b.expires < b.issued
        || b.expires - b.issued > PROOF_MAX_LIFETIME
    {
        return false;
    }
    let action = hex(&sha256(
        format!(
            "pulse-check-bind-v1\ndevice:{}\nkey:{}\ncheck:{check_device}:{check_public}\n",
            b.device_id, b.device_key
        )
        .as_bytes(),
    ));
    let message = format!(
        "pulse.approval.v1\nkind:bind\nrequest:bind\naction:{action}\nclient:\ndevice:{check_device}\nkey:{}\nnonce:{}\nissued:{}\nexpires:{}",
        hex(&sha256(&public)[..16]),
        b.nonce,
        b.issued,
        b.expires
    );
    if is_hello_rsa(&public) {
        return signature.len() == 256
            && UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &public)
                .verify(message.as_bytes(), &signature)
                .is_ok();
    }
    let algorithm = if signature.len() == 64 {
        &ECDSA_P256_SHA256_FIXED
    } else {
        &ECDSA_P256_SHA256_ASN1
    };
    UnparsedPublicKey::new(algorithm, &public)
        .verify(message.as_bytes(), &signature)
        .is_ok()
}

const OWNER_DEVICE: &str = "mac-owner-check";
const HELLO_DEVICE: &str = "windows-hello";
/// `approval_sig::MAX_LIFETIME`, seconds.
const PROOF_MAX_LIFETIME: u64 = 120;

/// `circle::OwnerSig`: an owner-check signature with everything needed to verify it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerSig {
    pub check: CheckKey,
    pub challenge: String,
    pub client: String,
    pub key_id: String,
    pub nonce: String,
    pub issued: u64,
    pub expires: u64,
    pub signature: String,
}

/// `circle::EndorsementBody`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndorsementBody {
    pub device_id: String,
    pub public_key: String,
    #[serde(default)]
    pub member_check: Option<CheckKey>,
    pub can_admit: bool,
    pub issued_at_ms: u64,
}

/// `circle::TokenGrant`: what the owner check signed about a join token before anyone redeemed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenGrant {
    pub admitter_device_id: String,
    pub pairing_id: String,
    pub secret_hash: String,
    pub can_admit: bool,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    #[serde(default)]
    pub member_class: String,
    #[serde(default)]
    pub lease_expires_at_ms: u64,
}

fn lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl TokenGrant {
    /// `circle::TokenGrant::line`.
    fn line(&self) -> String {
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

    /// `circle::TokenGrant::check_shape`.
    fn shape_ok(&self, now: u64) -> bool {
        lower_hex(&self.pairing_id, 32)
            && lower_hex(&self.secret_hash, 64)
            && !self.admitter_device_id.is_empty()
            && self.admitter_device_id.len() <= 64
            && self
                .admitter_device_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !self.can_admit
            && match self.member_class.as_str() {
                MEMBER_CLASS_MEMBER => self.lease_expires_at_ms == 0,
                MEMBER_CLASS_GUEST => {
                    self.lease_expires_at_ms > self.issued_at_ms
                        && self.lease_expires_at_ms - self.issued_at_ms <= GUEST_LEASE_MAX_MS
                }
                _ => false,
            }
            && self.expires_at_ms.checked_sub(self.issued_at_ms) == Some(PAIRING_TTL_MS)
            && self.issued_at_ms <= now.saturating_add(CLOCK_SKEW_MS)
    }
}

/// `circle::Redemption`: the admitting computer's device-key statement that the token was
/// redeemed by this joiner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redemption {
    pub pairing_id: String,
    pub joiner_device_id: String,
    pub joiner_public_key: String,
    pub redeemed_at_ms: u64,
    /// `member` or `guest`; empty on a redemption made before this field existed (never verifies).
    #[serde(default)]
    pub member_class: String,
    /// For `guest`: the absolute lease end in epoch ms; 0 for `member`.
    #[serde(default)]
    pub lease_expires_at_ms: u64,
    pub signature: String,
}

impl Redemption {
    /// `circle::Redemption::signing_input`.
    fn signing_input(&self) -> Vec<u8> {
        Enc::new(REDEEM_DOMAIN)
            .str(&self.pairing_id)
            .str(&self.joiner_device_id)
            .str(&self.joiner_public_key)
            .u64(self.redeemed_at_ms)
            .str(&self.member_class)
            .u64(self.lease_expires_at_ms)
            .finish()
    }

    /// `circle::Redemption::is_guest`.
    fn is_guest(&self) -> bool {
        self.member_class == MEMBER_CLASS_GUEST
    }

    /// `circle::Redemption::check_shape`.
    fn shape_ok(&self, now: u64) -> bool {
        let Ok(key) = unhex::<32>(&self.joiner_public_key) else {
            return false;
        };
        lower_hex(&self.pairing_id, 32)
            && lower_hex(&self.joiner_public_key, 64)
            && lower_hex(&self.signature, 128)
            && self.joiner_device_id == device_id_for_key(&key)
            && self.redeemed_at_ms <= now.saturating_add(CLOCK_SKEW_MS)
            && match self.member_class.as_str() {
                MEMBER_CLASS_MEMBER => self.lease_expires_at_ms == 0,
                MEMBER_CLASS_GUEST => {
                    self.lease_expires_at_ms > self.redeemed_at_ms
                        && self.lease_expires_at_ms - self.redeemed_at_ms <= GUEST_LEASE_MAX_MS
                }
                _ => false,
            }
    }
}

/// `circle::redemption_fits`.
fn redemption_fits(grant: &TokenGrant, r: &Redemption, now: u64) -> bool {
    r.shape_ok(now)
        && r.member_class == grant.member_class
        && r.lease_expires_at_ms <= grant.lease_expires_at_ms
        && r.pairing_id == grant.pairing_id
        && r.joiner_device_id != grant.admitter_device_id
        && r.redeemed_at_ms <= grant.expires_at_ms
        && r.redeemed_at_ms >= grant.issued_at_ms
}

/// `circle::GrantPair`: a joiner's endorsement when it joined by token or QR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantPair {
    pub batch: EndorsementBatch,
    pub redemption: Redemption,
}

impl GrantPair {
    fn grant(&self) -> Option<&TokenGrant> {
        match self.batch.grants.as_slice() {
            [grant] => Some(grant),
            _ => None,
        }
    }
}

/// `circle::EndorsementBatch`: one owner-check signature over a canonical list of bodies and at
/// most one token grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndorsementBatch {
    pub bodies: Vec<EndorsementBody>,
    #[serde(default)]
    pub grants: Vec<TokenGrant>,
    pub sig: OwnerSig,
}

fn unhex_vec(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || text.len() > 1200 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

fn is_p256(public: &[u8]) -> bool {
    public.len() == 65 && public[0] == 4
}

fn is_hello_rsa(public: &[u8]) -> bool {
    public.len() == 270
        && public.starts_with(&[0x30, 0x82, 0x01, 0x0a, 0x02, 0x82, 0x01, 0x01, 0x00])
        && public[9] & 0x80 != 0
        && public.ends_with(&[0x02, 0x03, 0x01, 0x00, 0x01])
}

/// `circle::check_key_shape`.
fn check_key_shape(check: &CheckKey) -> bool {
    matches!(check.device.as_str(), OWNER_DEVICE | HELLO_DEVICE)
        && check.public_key.len() <= 600
        && !check.public_key.is_empty()
        && check.public_key.bytes().all(|b| b.is_ascii_hexdigit())
        && check.binding.as_ref().is_none_or(|b| {
            b.device_id.len() <= 64
                && b.device_key.len() == 64
                && b.nonce.len() <= 64
                && b.signature.len() <= 600
        })
}

impl EndorsementBody {
    /// `circle::EndorsementBody::line`.
    fn line(&self) -> String {
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

    /// `circle::EndorsementBody::check_shape`.
    fn shape_ok(&self, now: u64) -> bool {
        let Ok(key) = unhex::<32>(&self.public_key) else {
            return false;
        };
        self.device_id == device_id_for_key(&key)
            && match (&self.member_check, self.can_admit) {
                (Some(c), true) => check_key_shape(c),
                (None, false) => true,
                _ => false,
            }
            && self.issued_at_ms <= now.saturating_add(CLOCK_SKEW_MS)
    }
}

/// `circle::endorse_action`: `approval_sig::endorse_action` over `bodies`, then `grants`, in order.
fn endorse_action(bodies: &[EndorsementBody], grants: &[TokenGrant]) -> String {
    let lines: String = bodies
        .iter()
        .map(EndorsementBody::line)
        .chain(grants.iter().map(TokenGrant::line))
        .collect();
    hex(&sha256(
        format!(
            "pulse-circle-endorse-v1\ncount:{}\n{lines}",
            bodies.len() + grants.len()
        )
        .as_bytes(),
    ))
}

impl EndorsementBatch {
    /// Whether the batch carries a body for exactly this device and key (not yet verified).
    pub fn endorses(&self, device_id: &str, key_hex: &str) -> bool {
        self.bodies
            .iter()
            .any(|b| b.device_id == device_id && b.public_key == key_hex)
    }

    /// `circle::EndorsementBatch::verify` and `approval_sig::check_owner_proof`: the bodies are
    /// well formed, sorted and unique, and the owner-check key signed exactly them. A batch never
    /// expires: an endorsement lasts until a signed revoke.
    pub fn verify(&self, now: u64) -> bool {
        if (self.bodies.is_empty() && self.grants.is_empty())
            || self.bodies.len() > MAX_MEMBERS
            || self.grants.len() > 1
            || !check_key_shape(&self.sig.check)
            || self.grants.iter().any(|g| !g.shape_ok(now))
        {
            return false;
        }
        let mut previous: Option<&str> = None;
        for body in &self.bodies {
            if !body.shape_ok(now) || previous.is_some_and(|p| p >= body.device_id.as_str()) {
                return false;
            }
            previous = Some(&body.device_id);
        }
        let sig = &self.sig;
        let action = endorse_action(&self.bodies, &self.grants);
        let Some(public) = unhex_vec(&sig.check.public_key) else {
            return false;
        };
        let Some(signature) = unhex_vec(&sig.signature) else {
            return false;
        };
        let shape_ok = match sig.check.device.as_str() {
            OWNER_DEVICE => is_p256(&public),
            HELLO_DEVICE => is_hello_rsa(&public),
            _ => false,
        };
        let nonce_ok = (32..=64).contains(&sig.nonce.len())
            && sig.nonce.chars().all(|c| c.is_ascii_hexdigit());
        let field_ok = ![sig.challenge.as_str(), action.as_str(), sig.client.as_str()]
            .iter()
            .any(|f| f.contains('\n'));
        if !shape_ok
            || !nonce_ok
            || !field_ok
            || sig.expires < sig.issued
            || sig.expires - sig.issued > PROOF_MAX_LIFETIME
            || hex(&sha256(&public)[..16]) != sig.key_id
        {
            return false;
        }
        let message = format!(
            "pulse.approval.v1\nkind:device\nrequest:{}\naction:{}\nclient:{}\ndevice:{}\nkey:{}\nnonce:{}\nissued:{}\nexpires:{}",
            sig.challenge,
            action,
            sig.client,
            sig.check.device,
            sig.key_id,
            sig.nonce,
            sig.issued,
            sig.expires
        );
        if is_hello_rsa(&public) {
            return signature.len() == 256
                && UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &public)
                    .verify(message.as_bytes(), &signature)
                    .is_ok();
        }
        let algorithm = if signature.len() == 64 {
            &ECDSA_P256_SHA256_FIXED
        } else {
            &ECDSA_P256_SHA256_ASN1
        };
        UnparsedPublicKey::new(algorithm, &public)
            .verify(message.as_bytes(), &signature)
            .is_ok()
    }
}

/// The host's owner-check key when `batch` is the host owner's signature that admitted THIS phone
/// in THIS pairing (`challenge` is the pairing's transcript hash and the batch carries a body for
/// this phone's id and key), or `None`. The phone pins it: this is the one key it verified itself,
/// and every later endorsement chains to it.
pub fn host_admit_check(
    batch: &EndorsementBatch,
    me: &str,
    my_key_hex: &str,
    host_id: &str,
    host_key_hex: &str,
    transcript_hash_hex: &str,
    now: u64,
) -> Option<CheckKey> {
    (batch.sig.challenge == transcript_hash_hex
        && batch.endorses(me, my_key_hex)
        && batch.verify(now)
        && batch.sig.check.bound_to(host_id, host_key_hex))
    .then(|| batch.sig.check.clone())
}

/// `circle::host_grant_check`: the host's owner-check key when `batch` carries exactly one grant
/// naming this host, this pairing and the hash of the secret this phone's QR carried, and
/// `redemption` is this phone's, no later than the grant's expiry and signed by the host's device
/// key. The phone pins it, as for [`host_admit_check`].
#[allow(clippy::too_many_arguments)]
pub fn host_grant_check(
    batch: &EndorsementBatch,
    redemption: &Redemption,
    me: &str,
    my_key_hex: &str,
    host_id: &str,
    host_key_hex: &str,
    pairing_id: &str,
    secret_hash: &str,
    now: u64,
) -> Option<CheckKey> {
    let [grant] = batch.grants.as_slice() else {
        return None;
    };
    let host_key = unhex::<32>(host_key_hex).ok()?;
    let signature = unhex::<64>(&redemption.signature).ok()?;
    (grant.admitter_device_id == host_id
        && grant.pairing_id == pairing_id
        && grant.secret_hash == secret_hash
        && redemption.joiner_device_id == me
        && redemption.joiner_public_key == my_key_hex
        && batch.verify(now)
        && redemption_fits(grant, redemption, now)
        && batch.sig.check.bound_to(host_id, host_key_hex)
        && UnparsedPublicKey::new(&ED25519, &host_key)
            .verify(&redemption.signing_input(), &signature)
            .is_ok())
    .then(|| batch.sig.check.clone())
}

fn encode_check(enc: Enc, check: Option<&CheckKey>) -> Enc {
    match check {
        Some(c) => enc.u64(1).str(&c.device).str(&c.public_key),
        None => enc.u64(0),
    }
}

#[derive(Deserialize)]
pub struct MemberList {
    schema_version: u32,
    issuer_device_id: String,
    recipient_device_id: String,
    introduction_id: String,
    issued_at_ms: u64,
    expires_at_ms: u64,
    members: Vec<Member>,
    #[serde(default)]
    endorsements: Vec<EndorsementBatch>,
    #[serde(default)]
    grant_pairs: Vec<GrantPair>,
    signature: String,
}

/// `circle::encode_batch`: a batch's signed bytes in a message that carries one.
fn encode_batch(enc: Enc, batch: &EndorsementBatch) -> Enc {
    let mut enc = enc.u64(batch.bodies.len() as u64);
    for body in &batch.bodies {
        enc = encode_check(
            enc.str(&body.device_id).str(&body.public_key),
            body.member_check.as_ref(),
        )
        .u64(u64::from(body.can_admit))
        .u64(body.issued_at_ms);
    }
    enc = enc.u64(batch.grants.len() as u64);
    for grant in &batch.grants {
        enc = enc
            .str(&grant.admitter_device_id)
            .str(&grant.pairing_id)
            .str(&grant.secret_hash)
            .u64(u64::from(grant.can_admit))
            .u64(grant.issued_at_ms)
            .u64(grant.expires_at_ms)
            .str(&grant.member_class)
            .u64(grant.lease_expires_at_ms);
    }
    let sig = &batch.sig;
    encode_check(enc, Some(&sig.check))
        .str(&sig.challenge)
        .str(&sig.client)
        .str(&sig.key_id)
        .str(&sig.nonce)
        .u64(sig.issued)
        .u64(sig.expires)
        .str(&sig.signature)
}

impl MemberList {
    fn signing_input(&self) -> Vec<u8> {
        let mut enc = Enc::new(MEMBERS_DOMAIN)
            .str(&self.issuer_device_id)
            .str(&self.recipient_device_id)
            .str(&self.introduction_id)
            .u64(self.issued_at_ms)
            .u64(self.expires_at_ms)
            .u64(self.members.len() as u64);
        for member in &self.members {
            let device = &member.device;
            enc = enc
                .u64(u64::from(device.schema_version))
                .str(&device.device_id)
                .str(&device.platform);
            enc = match &device.display_name {
                Some(name) => enc.u64(1).str(name),
                None => enc.u64(0),
            };
            enc = enc.str(&member.public_key);
            enc = match &member.relay {
                Some(relay) => enc.u64(1).str(relay),
                None => enc.u64(0),
            };
            // Signed only when present, exactly as `circle::encode_member` does.
            if let Some(lease) = member.cloud_lease_ms {
                enc = enc.str("cloud").u64(lease);
            }
        }
        enc = enc.u64(self.endorsements.len() as u64);
        for batch in &self.endorsements {
            enc = encode_batch(enc, batch);
        }
        enc = enc.u64(self.grant_pairs.len() as u64);
        for pair in &self.grant_pairs {
            let r = &pair.redemption;
            enc = encode_batch(enc, &pair.batch)
                .str(&r.pairing_id)
                .str(&r.joiner_device_id)
                .str(&r.joiner_public_key)
                .u64(r.redeemed_at_ms)
                .str(&r.member_class)
                .u64(r.lease_expires_at_ms)
                .str(&r.signature);
        }
        enc.finish()
    }
}

/// `circle::CircleRevoke`: a signed removal of one circle member.
#[derive(Deserialize)]
pub struct CircleRevoke {
    schema_version: u32,
    issuer_device_id: String,
    recipient_device_id: String,
    target_device_id: String,
    issued_at_ms: u64,
    expires_at_ms: u64,
    signature: String,
}

impl CircleRevoke {
    fn signing_input(&self) -> Vec<u8> {
        Enc::new(REVOKE_DOMAIN)
            .str(&self.issuer_device_id)
            .str(&self.recipient_device_id)
            .str(&self.target_device_id)
            .u64(self.issued_at_ms)
            .u64(self.expires_at_ms)
            .finish()
    }
}

/// `circle_pairing_id`: SHA-256 over the two device ids in sorted order, first 16 bytes as hex.
pub fn circle_pairing_id(a: &str, b: &str) -> String {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    let digest = sha256(&Enc::new(PAIRING_DOMAIN).str(low).str(high).finish());
    hex(&digest[..16])
}

fn clean_name(name: &str) -> bool {
    name.chars().count() <= 64
        && !name.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
}

/// The hosts a verified list adds, and the ids it removes: the joiners of a pairing id that two
/// different joiners claimed (both are rejected, and one this phone already holds is removed).
pub struct Verified {
    pub add: Vec<Host>,
    pub remove: Vec<String>,
}

fn is_computer(platform: &str) -> bool {
    matches!(platform, "mac" | "windows")
}

/// The computer this phone paired with and queried, and the key to verify its signatures with.
/// Only a computer whose owner approved this phone with an owner-check signature, which this phone
/// verified and pinned at pairing, may speak for the circle (`PairedDevice::admit`); the platform
/// the computer declared proves nothing on its own.
fn pinned_issuer(issuer: &Host) -> Result<&CheckKey, PmError> {
    issuer
        .admit_check
        .as_ref()
        .filter(|_| issuer.introduced_by.is_none() && is_computer(&issuer.platform))
        .ok_or_else(|| {
            PmError::new(
                "not_authorized",
                "only a computer whose owner added this phone with Touch ID or Windows Hello may share the circle",
            )
        })
}

/// Verify `list` as a list `issuer` (a computer this phone paired with, whose key is `issuer_key`)
/// signed for the phone `me`. Refuses a wrong recipient or issuer, a bad signature, an expired or
/// future message, and any member whose id does not derive from its key.
///
/// `known` are the hosts this phone already holds; their pinned owner-check keys (`Host::admit_check`)
/// are the trust roots (the issuer's included). `revoked` are the ids this phone has removed by a
/// signed revoke: they are never added again and carry no key. The endorsement batches resolve as
/// `circle::trust_chain` does: a batch counts only when it verifies and its signer is trusted, and
/// each `can_admit` body adds its owner-check key to the trusted set. A listed member is added only
/// when a counted batch carries a body for exactly that device and key; a computer so endorsed keeps
/// its own check key as `admit_check` only when the body says `can_admit`. Whatever does not
/// resolve is not added; the issuer's say-so alone adds nobody. A member the list omits is never
/// removed.
pub fn verify_list(
    list: &MemberList,
    issuer: &Host,
    issuer_key: &[u8; 32],
    me: &str,
    known: &[Host],
    revoked: &[String],
) -> Result<Verified, PmError> {
    let bad = |what: &str| PmError::new("malformed", what.to_string());
    if list.schema_version != SCHEMA_VERSION {
        return Err(bad("circle list version"));
    }
    if list.recipient_device_id != me || list.issuer_device_id != issuer.device_id {
        return Err(PmError::new(
            "not_authorized",
            "the member list is not for this phone from that computer",
        ));
    }
    pinned_issuer(issuer)?;
    let signature = unhex::<64>(&list.signature)?;
    UnparsedPublicKey::new(&ED25519, issuer_key)
        .verify(&list.signing_input(), &signature)
        .map_err(|_| PmError::new("signature_invalid", "the member list did not verify"))?;
    unhex::<16>(&list.introduction_id)?;
    let now = now_ms();
    if list.expires_at_ms <= now {
        return Err(PmError::retry("expired", "the member list expired"));
    }
    if list.issued_at_ms > now.saturating_add(CLOCK_SKEW_MS)
        || list.expires_at_ms < list.issued_at_ms
        || list.expires_at_ms - list.issued_at_ms > TTL_MS
        || list.members.len() > MAX_MEMBERS
        || list.endorsements.len() > MAX_BATCHES
        || list.grant_pairs.len() > MAX_PAIRS
    {
        return Err(bad("circle list window"));
    }
    for member in &list.members {
        let key = unhex::<32>(&member.public_key)?;
        let device = &member.device;
        let relay_ok = member.relay.as_deref().is_none_or(|r| {
            r.get(..8)
                .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
                && r.len() <= 256
                && !r.chars().any(char::is_control)
        });
        if device.device_id != device_id_for_key(&key)
            || device.schema_version != DEVICE_REF_SCHEMA_VERSION
            || !PLATFORMS.contains(&device.platform.as_str())
            || device
                .display_name
                .as_deref()
                .is_some_and(|n| !clean_name(n))
            || !relay_ok
        {
            return Err(bad("circle member"));
        }
    }
    // Trust roots: every pinned owner-check key (what this phone verified itself), each only
    // through its binding to the host that holds it.
    let mut keys: Vec<CheckKey> = Vec::new();
    // The owner-check keys that may admit, with the device each belongs to (its id and endpoint).
    let mut admitters: std::collections::HashMap<CheckKey, (String, String)> =
        std::collections::HashMap::new();
    for h in known.iter().chain(std::iter::once(issuer)) {
        let Some(key) = &h.admit_check else {
            continue;
        };
        if !keys.contains(key) && key.bound_to(&h.device_id, &h.endpoint_id) {
            keys.push(key.clone());
            admitters.insert(key.clone(), (h.device_id.clone(), h.endpoint_id.clone()));
        }
    }
    // Every batch the list carries: the endorsement batches, then the batch of each grant pair.
    let all: Vec<&EndorsementBatch> = list
        .endorsements
        .iter()
        .chain(list.grant_pairs.iter().map(|p| &p.batch))
        .collect();
    let valid: Vec<bool> = all.iter().map(|b| b.verify(now)).collect();
    let mut used = vec![false; all.len()];
    // Device id to the body and the batch that carries it.
    let mut bodies: std::collections::HashMap<&str, (&EndorsementBody, &EndorsementBatch)> =
        std::collections::HashMap::new();
    loop {
        let mut progress = false;
        for (i, batch) in all.iter().copied().enumerate() {
            if used[i] || !valid[i] || !keys.contains(&batch.sig.check) {
                continue;
            }
            used[i] = true;
            progress = true;
            for body in &batch.bodies {
                if revoked.contains(&body.device_id) {
                    continue;
                }
                // A check key is trusted for the device its own binding names, no other.
                if body.can_admit
                    && let Some(key) = &body.member_check
                    && !keys.contains(key)
                    && key.bound_to(&body.device_id, &body.public_key)
                {
                    keys.push(key.clone());
                    admitters.insert(
                        key.clone(),
                        (body.device_id.clone(), body.public_key.clone()),
                    );
                }
                let better = bodies
                    .get(body.device_id.as_str())
                    .is_none_or(|(have, _)| !have.can_admit && body.can_admit);
                if better {
                    bodies.insert(body.device_id.as_str(), (body, batch));
                }
            }
        }
        if !progress {
            break;
        }
    }
    // Grant pairs: valid when the grant batch's signer is trusted to admit, the redemption is signed
    // by the device key of the device that signer belongs to (and is the grant's admitter), and it
    // is no later than the grant's expiry (`circle::pair_valid`).
    let pair_ok: Vec<bool> = list
        .grant_pairs
        .iter()
        .enumerate()
        .map(|(n, pair)| {
            let batch_at = list.endorsements.len() + n;
            let Some(grant) = pair.grant() else {
                return false;
            };
            let check = &pair.batch.sig.check;
            let Some((admitter_id, admitter_key)) = admitters.get(check) else {
                return false;
            };
            let (Ok(key), Ok(signature)) = (
                unhex::<32>(admitter_key),
                unhex::<64>(&pair.redemption.signature),
            ) else {
                return false;
            };
            // `circle::pair_valid`: held to this phone's clock, a guest's lease not yet ended.
            now <= grant
                .expires_at_ms
                .saturating_add(REDEMPTION_DELIVERY_ALLOWANCE_MS)
                && !(pair.redemption.is_guest() && now >= pair.redemption.lease_expires_at_ms)
                && valid[batch_at]
                && keys.contains(check)
                && *admitter_id == grant.admitter_device_id
                && !revoked.contains(admitter_id)
                && redemption_fits(grant, &pair.redemption, now)
                && UnparsedPublicKey::new(&ED25519, &key)
                    .verify(&pair.redemption.signing_input(), &signature)
                    .is_ok()
        })
        .collect();
    // One joiner per pairing id, counting the hosts this phone already holds from a grant.
    let mut joiners: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
        std::collections::HashMap::new();
    for host in known {
        if let Some(pairing_id) = &host.grant_pairing_id {
            joiners
                .entry(pairing_id.as_str())
                .or_default()
                .insert(host.device_id.as_str());
        }
    }
    for (pair, ok) in list.grant_pairs.iter().zip(&pair_ok) {
        if *ok {
            joiners
                .entry(pair.redemption.pairing_id.as_str())
                .or_default()
                .insert(pair.redemption.joiner_device_id.as_str());
        }
    }
    let conflicts: Vec<&str> = joiners
        .iter()
        .filter(|(_, who)| who.len() > 1)
        .map(|(id, _)| *id)
        .collect();
    let mut remove: Vec<String> = Vec::new();
    for pairing_id in &conflicts {
        for id in joiners[pairing_id].iter() {
            if known
                .iter()
                .find(|h| h.device_id == *id)
                .is_none_or(|h| h.introduced_by.is_some())
            {
                remove.push((*id).to_string());
            }
        }
    }
    let mut add = Vec::new();
    for member in &list.members {
        let device = &member.device;
        let id = &device.device_id;
        if id == me
            || *id == issuer.device_id
            || known.iter().any(|h| h.device_id == *id)
            || revoked.contains(id)
            // A cloud guest (#492) is chat-only between the owner's computers: the phone learns
            // nothing from it and trusts it for nothing, so it is never recorded here.
            || member.cloud_lease_ms.is_some()
        {
            continue;
        }
        let Some((body, batch)) = bodies.get(id.as_str()) else {
            // No batch for this member: a token or QR join, vouched for by a valid grant pair.
            let Some((pair, _)) = list.grant_pairs.iter().zip(&pair_ok).find(|(p, ok)| {
                **ok
                    // A guest class (signed) is never recorded here, whatever `cloud_lease_ms` says.
                    && !p.redemption.is_guest()
                    && p.redemption.joiner_device_id == *id
                    && p.redemption.joiner_public_key == member.public_key
                    && !conflicts.contains(&p.redemption.pairing_id.as_str())
            }) else {
                continue;
            };
            add.push(Host {
                endpoint_id: member.public_key.clone(),
                device_id: id.clone(),
                name: device.display_name.clone(),
                platform: device.platform.clone(),
                relay: member.relay.clone(),
                paired_at_ms: now,
                pairing_id: circle_pairing_id(me, id),
                introduced_by: Some(issuer.device_id.clone()),
                // A grant never confers `can_admit`.
                admit_check: None,
                endorsed_by: Some(pair.batch.sig.check.clone()),
                grant_pairing_id: Some(pair.redemption.pairing_id.clone()),
            });
            continue;
        };
        if body.public_key != member.public_key {
            continue;
        }
        let admit = body.can_admit
            && is_computer(&device.platform)
            && body.member_check.as_ref().is_some_and(|k| {
                admitters.get(k).is_some_and(|(d, _)| d == id) && k.bound_to(id, &member.public_key)
            });
        add.push(Host {
            endpoint_id: member.public_key.clone(),
            device_id: id.clone(),
            name: device.display_name.clone(),
            platform: device.platform.clone(),
            relay: member.relay.clone(),
            paired_at_ms: now,
            pairing_id: circle_pairing_id(me, id),
            introduced_by: Some(issuer.device_id.clone()),
            admit_check: body.member_check.clone().filter(|_| admit),
            endorsed_by: Some(batch.sig.check.clone()),
            grant_pairing_id: None,
        });
    }
    // The joiners of a conflicted pairing id are never added (and a held one is removed), whichever
    // way a batch might have carried them.
    remove.sort();
    remove.dedup();
    add.retain(|h| !remove.contains(&h.device_id));
    Ok(Verified { add, remove })
}

/// The ids this phone must remove, from the signed removals in the reply to its pull. Each
/// `revoke` must be from `issuer` (the computer queried, pinned as a circle speaker) to this phone,
/// inside its window and signed with the issuer's device key, and is scoped as
/// `DeviceLink::apply_circle_revoke` scopes it: the target must be a host that was introduced (a
/// directly paired one changes only by its own removal), and the issuer must be the one that
/// introduced it, the owner of the owner-check key that endorsed it, or the target itself. A
/// removal older than the host's record is ignored. Anything else is skipped, never an error.
pub fn verify_revokes(
    revokes: &[CircleRevoke],
    issuer: &Host,
    issuer_key: &[u8; 32],
    me: &str,
    known: &[Host],
) -> Vec<String> {
    if pinned_issuer(issuer).is_err() {
        return Vec::new();
    }
    // Revoke authority comes only from a check key bound to the issuer's own device.
    let issuer_check = issuer
        .admit_check
        .as_ref()
        .filter(|k| k.bound_to(&issuer.device_id, &issuer.endpoint_id));
    let now = now_ms();
    let mut out: Vec<String> = Vec::new();
    for message in revokes.iter().take(MAX_MEMBERS) {
        let window_ok = message.expires_at_ms > now
            && message.issued_at_ms <= now.saturating_add(CLOCK_SKEW_MS)
            && message.expires_at_ms >= message.issued_at_ms
            && message.expires_at_ms - message.issued_at_ms <= REVOKE_TTL_MS;
        if message.schema_version != SCHEMA_VERSION
            || message.recipient_device_id != me
            || message.issuer_device_id != issuer.device_id
            || !window_ok
        {
            continue;
        }
        let Ok(signature) = unhex::<64>(&message.signature) else {
            continue;
        };
        if UnparsedPublicKey::new(&ED25519, issuer_key)
            .verify(&message.signing_input(), &signature)
            .is_err()
        {
            continue;
        }
        let Some(target) = known
            .iter()
            .find(|h| h.device_id == message.target_device_id)
        else {
            continue;
        };
        let self_removal = message.issuer_device_id == message.target_device_id;
        let covered = target.introduced_by.as_deref() == Some(message.issuer_device_id.as_str())
            || issuer_check.is_some() && target.endorsed_by.as_ref() == issuer_check;
        if (target.introduced_by.is_none() && !self_removal)
            || (!self_removal && !covered)
            || message.issued_at_ms.saturating_add(CLOCK_SKEW_MS) < target.paired_at_ms
        {
            continue;
        }
        if !out.contains(&target.device_id) {
            out.push(target.device_id.clone());
        }
    }
    out
}
