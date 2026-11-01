//! The device circle: a device is admitted once, by one computer's owner, and every member that
//! trusts that owner's computer trusts it (`docs/device-link.md`, "Circle").
//!
//! **Trust is an owner-check key, and only an owner-check signature mints it.** Every trust
//! statement is an [`EndorsementBatch`]: ONE owner-check signature (Touch ID or Windows Hello, never
//! a PIN, never a device key) over the canonical list of [`EndorsementBody`] it covers (domain
//! `pulse-circle-endorse-v1`; each body is a member's device id, device public key, owner-check
//! public key or none, `can_admit` and issued-at). Each body verifies against the batch signature.
//! A receiver trusts exactly these owner-check keys:
//!
//! * (a) the one it verified itself in its own direct pairing: the host's transcript-hash signature
//!   on this joiner (`DeviceLink::complete_pairing_with_host_proof`), or, on the host, the key of
//!   the member its own owner-check approval endorsed (`DeviceLink::record_endorsement`);
//! * (b) recursively, an owner-check key carried in an endorsement whose signer it already trusts
//!   and whose `can_admit` is true (`trust_chain`).
//!
//! Nothing else. An issuer's unsigned say-so about a check key, a self-attestation or a device key
//! alone adds nobody: a member is added only when a batch signed by a trusted key carries a body for
//! exactly that device and key.
//!
//! **Minting.** When a computer admits a device (`device.pair.approve` with an owner-check
//! signature) the same prompt signs one batch ([`DeviceLink::plan_endorsements`]): the new device,
//! plus every current member this computer holds only by what it verified itself (its host, with the
//! owner-check key it pinned) and has not endorsed yet. So the first time a joiner (the Dell) admits
//! a device, it endorses its own host (the Mac) in the same prompt. `can_admit` is true only for a
//! computer whose owner-check key the admitter knows (the joiner told the host its key in its
//! `PairAccept`; the owner's approval signs it), never for a PIN-only or join-token pairing, a phone
//! or a server (they are not endorsed at all: no owner check was present).
//!
//! **Join tokens and QR codes.** The joiner does not exist when the token is made, so the prompt that
//! makes the token (`device.join.create`, `device.pair.start`) signs, in the SAME batch as the
//! bodies above, a [`TokenGrant`]: domain `pulse-circle-grant-v2`, the admitter's device id, the
//! pairing id, SHA-256 of the token secret, `can_admit` (always false), `issued_at`/`expires_at`
//! (the pairing lifetime), `member_class` and `lease_expires_at_ms`. When the token is redeemed the admitter's DEVICE key signs a
//! [`Redemption`] (`pulse-circle-redeem-v2`: pairing id, joiner device id and public key,
//! redeemed-at, member class and lease end). A [`GrantPair`] (the batch carrying the grant, plus the redemption) is the joiner's
//! endorsement. A receiver accepts it only when the grant batch verifies under an owner-check key
//! it trusts with `can_admit`, the redemption is signed by the device key of the device that check
//! key belongs to, `redeemed_at` is no later than `expires_at`, and the pairing id has exactly one
//! joiner. Two different joiners for one pairing id are both rejected (an introduced record
//! already added is revoked) and the pairing id is logged ([`take_grant_conflicts`]). A device
//! admitted by a grant never gets `can_admit`.
//!
//! **Redemption line (v2, R06/R07).** The bytes the admitter's device key signs are `wire::Enc`
//! over, in order: domain `pulse-circle-redeem-v2`, pairing id, joiner device id, joiner public
//! key, redeemed-at (u64), `member_class` (str: `member` or `guest`), `lease_expires_at_ms` (u64:
//! 0 for `member`; for `guest` the absolute lease end, later than redeemed-at and at most
//! [`GUEST_LEASE_MAX_MS`](super::registry::GUEST_LEASE_MAX_MS) after it). A receiver that takes
//! a grant-pair introduction REQUIRES both fields, builds the member record from these signed
//! values only (a guest class makes a guest whatever the courier's unsigned `cloud_lease_ms`
//! says; that field may only shorten the lease, never lengthen it or remove it), and accepts the
//! pair only within [`REDEMPTION_DELIVERY_ALLOWANCE_MS`] after the grant's own expiry by its own
//! clock. The ffi mirror (`rightkit-mesh-ffi/src/circle.rs`) uses the same line and order. The
//! class and lease are ALSO bound by the owner-check signature (R06): the grant line
//! (`pulse-circle-grant-v2`, `class:` and `lease:`) is inside the signed batch, and a receiver
//! requires the redemption's class to equal the grant's and its lease to be no later than the
//! grant's (`redemption_fits`), so a relay or courier cannot turn a member into a guest or back.
//! Pairs travel beside endorsement batches in a
//! [`MemberList`] and an [`Introduction`], and in the phone's pull.
//!
//! When member A admits a new device N, A
//! 1. signs a [`MemberList`] for N: every other active member A holds an endorsement for (device id,
//!    public key (also its endpoint id), name and relay), with the endorsement batches themselves
//!    and the chain of batches that endorse their signers;
//! 2. signs an [`Introduction`] of N for each other member M, with N's batch and its chain. M checks
//!    that A is an ACTIVE paired member and verifies the chain;
//! 3. on removal, signs a [`CircleRevoke`] for each other member (revoke-only: it removes trust and
//!    never adds it). A phone, which accepts no inbound stream, receives the removals this computer
//!    holds in the reply to its pull (`device.circle.members`, `revokes`).
//!
//! Every message is signed with the sender's Ed25519 device key over `wire::Enc` bytes with its
//! own domain string, names its recipient (so it cannot be replayed to another device) and is
//! valid for [`CIRCLE_MESSAGE_TTL_MS`] (a removal: [`CIRCLE_REVOKE_TTL_MS`]). It travels over the existing authenticated link channel
//! (`pulse/link/1`), so the channel's peer is the signer too. The device-key signature authenticates
//! the courier only; it never creates trust. Replays are refused by the registry itself: a record
//! that already exists (same key) is never added twice, a revoked record is never reactivated, and
//! a revoke older than the record's pairing is ignored.
//!
//! **Who may issue.** Only a member holding the `admit` capability (`PairedDevice::admit`) may issue
//! a member list, an introduction or a removal; every receiver checks its own record of the issuer
//! (`check_issuer`). `admit` is never read from a device's self-declared platform: it is set only
//! for a computer whose owner-check key is trusted by (a) or (b) above (its record then also holds
//! that key as `admit_check_key`), or by an owner's `DeviceLink::set_admit` call. A record whose key
//! is revoked, or whose holder was revoked, stops being trusted.
//!
//! **Endorsements do not expire.** An admission lasts until a signed [`CircleRevoke`]; a list or an
//! introduction that omits a member never removes it. Replay of a new introduction is bounded by
//! its own ten-minute window and the registry (an existing record is never added twice, a revoked
//! one never returns).
//!
//! **Removals.** A [`CircleRevoke`] removes an INTRODUCED record only when its issuer introduced that
//! record, or is the member whose trusted owner-check key signed the record's endorsement batch (the
//! endorsement covers it), or removes itself; a directly paired device never changes this way.
//! The one exception is a cloud guest: any member holding `admit` may remove a guest record,
//! including the computer that minted it, so a revoke on any owner computer cuts the guest off
//! everywhere. No other record kind is widened.
//!
//! An introduced device starts exactly like a directly paired one: Active, NO grants, NO
//! `manage_peers`. Grants stay a local, per-device choice (`docs/device-link.md`).
//!
//! The pairing id of two circle members is derived, not exchanged: both sides compute
//! [`circle_pairing_id`] from the two device ids, so a request signed by one names the pairing id
//! the other stored. A revoked record is terminal and an introduction never reactivates it, so a
//! derived id cannot revive an old pairing's tickets; re-adding a device is a new direct pairing
//! with a random id.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use super::identity::{device_id_for_key, verify};
use super::link::{can_admit, revoke_record};
use super::registry::{PairedDevice, PairedState, RedeemedPairing, Registry};
use super::wire::{Enc, random, unhex};
use super::{CLOCK_SKEW_MS, DEVICE_LINK_SCHEMA_VERSION, DeviceLink, LinkError, PAIRING_TTL_MS};
use rightkit_crypto::sha256;
use rightkit_device_link_types::DeviceRef;
pub use rightkit_device_link_types::circle::{
    CheckKey, EndorsementBatch, EndorsementBody, GrantPair, KeyBinding, MEMBER_CLASS_GUEST,
    MEMBER_CLASS_MEMBER, OwnerSig, Redemption, TokenGrant, secret_hash,
};
use rightkit_fs::digest::hex;
use rightkit_owner_approvals::approval_sig::{self, OwnerCheckProof};

const MEMBERS_DOMAIN: &str = "pulse-link/circle-members/v1";
const INTRODUCTION_DOMAIN: &str = "pulse-link/circle-introduction/v1";
const REVOKE_DOMAIN: &str = "pulse-link/circle-revoke/v1";
const CHECKKEY_DOMAIN: &str = "pulse-link/circle-check-key/v1";
const PAIRING_DOMAIN: &str = "pulse-link/circle-pairing/v1";
/// Version 2 of the redemption line (R06): the member class and the guest lease are inside the
/// signed bytes. A signature over the version 1 bytes (`pulse-circle-redeem-v1`, no class, no
/// lease) does not verify under this domain, so an old redemption fails closed instead of being
/// read with a different meaning.
const REDEEM_DOMAIN: &str = "pulse-circle-redeem-v2";

/// No late first-seen redemptions: the admitter controls its timestamp, so only the receiver's
/// clock against the owner-signed grant expiry limits backdating. Offline newcomers need fresh
/// admission evidence; existing permanent members can receive refreshed owner endorsements.
pub const REDEMPTION_DELIVERY_ALLOWANCE_MS: u64 = 0;

/// A circle message is valid for this long after it is issued. The sender retries inside it.
pub const CIRCLE_MESSAGE_TTL_MS: u64 = 10 * 60 * 1000;
/// A removal is valid this long: a member that is offline for weeks still gets it from the sender's
/// outbox. It only removes trust, and a replay of one changes nothing (a revoked record is terminal).
pub const CIRCLE_REVOKE_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// Most members one list may carry, and most bodies one endorsement batch may carry.
pub const CIRCLE_MAX_MEMBERS: usize = 32;
/// Most endorsement batches one list or introduction may carry.
pub const CIRCLE_MAX_BATCHES: usize = 32;
/// Most grant pairs one list or introduction may carry.
pub const CIRCLE_MAX_PAIRS: usize = 32;
const MAX_NAME_CHARS: usize = 64;
const MAX_RELAY_CHARS: usize = 256;

/// Methods of [`CheckKey`] that need the owner-check signature check.
pub trait CheckKeyExt {
    /// Whether this key carries a binding that the key itself signed for exactly this device id and
    /// device identity key (hex). The only way a check key is attached to a device.
    fn bound_to(&self, device_id: &str, device_key: &str) -> bool;

    /// [`Self::bound_to`], logging a mismatch (a key carried without a binding for this device).
    fn bound_or_log(&self, device_id: &str, device_key: &str) -> bool;
}

impl CheckKeyExt for CheckKey {
    fn bound_to(&self, device_id: &str, device_key: &str) -> bool {
        self.binding.as_ref().is_some_and(|b| {
            b.device_id == device_id
                && b.device_key == device_key
                && approval_sig::check_key_binding(&self.device, &self.public_key, b).is_ok()
        })
    }

    fn bound_or_log(&self, device_id: &str, device_key: &str) -> bool {
        let ok = self.bound_to(device_id, device_key);
        if !ok {
            eprintln!("circle: owner-check key is not bound to {device_id}; ignored");
        }
        ok
    }
}

fn owner_sig_from_check(proof: OwnerCheckProof) -> OwnerSig {
    OwnerSig {
        check: CheckKey {
            device: proof.device,
            public_key: proof.public_key,
            binding: proof.binding,
        },
        challenge: proof.challenge,
        client: proof.client,
        key_id: proof.key_id,
        nonce: proof.nonce,
        issued: proof.issued,
        expires: proof.expires,
        signature: proof.signature,
    }
}

fn owner_sig_to_check(sig: &OwnerSig) -> OwnerCheckProof {
    OwnerCheckProof {
        device: sig.check.device.clone(),
        public_key: sig.check.public_key.clone(),
        challenge: sig.challenge.clone(),
        client: sig.client.clone(),
        key_id: sig.key_id.clone(),
        nonce: sig.nonce.clone(),
        issued: sig.issued,
        expires: sig.expires,
        signature: sig.signature.clone(),
        binding: sig.check.binding.clone(),
    }
}

fn lower_hex<const N: usize>(text: &str) -> Result<[u8; N], LinkError> {
    if text.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(LinkError::Malformed("hex must be lowercase"));
    }
    unhex::<N>(text)
}

fn clean_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The shape checks of a record that arrived from outside, before any signature is verified.
trait CheckShape {
    fn check_shape(&self, now_ms: u64) -> Result<(), LinkError>;
}

impl CheckShape for TokenGrant {
    fn check_shape(&self, now_ms: u64) -> Result<(), LinkError> {
        lower_hex::<16>(&self.pairing_id)?;
        lower_hex::<32>(&self.secret_hash)?;
        if !clean_id(&self.admitter_device_id) {
            return Err(LinkError::Malformed("grant admitter"));
        }
        let scoped = match self.member_class.as_str() {
            MEMBER_CLASS_MEMBER => self.lease_expires_at_ms == 0,
            MEMBER_CLASS_GUEST => {
                self.lease_expires_at_ms > self.issued_at_ms
                    && self.lease_expires_at_ms - self.issued_at_ms
                        <= super::registry::GUEST_LEASE_MAX_MS
            }
            _ => false,
        };
        if !scoped {
            return Err(LinkError::Malformed("owner grant scope"));
        }
        if self.can_admit {
            return Err(LinkError::Malformed("a grant never admits"));
        }
        if self.expires_at_ms.checked_sub(self.issued_at_ms) != Some(PAIRING_TTL_MS) {
            return Err(LinkError::Malformed("grant window"));
        }
        if self.issued_at_ms > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(LinkError::Malformed("grant issued in the future"));
        }
        Ok(())
    }
}

impl CheckShape for Redemption {
    fn check_shape(&self, now_ms: u64) -> Result<(), LinkError> {
        lower_hex::<16>(&self.pairing_id)?;
        let key = lower_hex::<32>(&self.joiner_public_key)?;
        lower_hex::<64>(&self.signature)?;
        if self.joiner_device_id != device_id_for_key(&key) {
            return Err(LinkError::Malformed(
                "redemption id does not derive from its key",
            ));
        }
        if self.redeemed_at_ms > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(LinkError::Malformed("redemption in the future"));
        }
        match self.member_class.as_str() {
            MEMBER_CLASS_MEMBER if self.lease_expires_at_ms == 0 => {}
            MEMBER_CLASS_GUEST
                if self.lease_expires_at_ms > self.redeemed_at_ms
                    && self.lease_expires_at_ms - self.redeemed_at_ms
                        <= super::registry::GUEST_LEASE_MAX_MS => {}
            _ => return Err(LinkError::Malformed("redemption member class or lease")),
        }
        Ok(())
    }
}

impl CheckShape for EndorsementBody {
    fn check_shape(&self, now_ms: u64) -> Result<(), LinkError> {
        let key = unhex::<32>(&self.public_key)?;
        if self.device_id != device_id_for_key(&key) {
            return Err(LinkError::Malformed(
                "endorsement id does not derive from its key",
            ));
        }
        match (&self.member_check, self.can_admit) {
            (Some(c), true) => check_key_shape(c)?,
            (None, false) => {}
            _ => return Err(LinkError::Malformed("endorsement check key")),
        }
        if self.issued_at_ms > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(LinkError::Malformed("endorsement issued in the future"));
        }
        Ok(())
    }
}

/// The signed bytes of a [`Redemption`].
pub trait RedemptionExt {
    /// The signed bytes: `pulse-circle-redeem-v2`, pairing id, joiner id, joiner key, redeemed-at,
    /// member class, lease end.
    fn signing_input(&self) -> Vec<u8>;
}

impl RedemptionExt for Redemption {
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
}

/// Which member a [`GrantPair`] vouches for.
trait PairCovers {
    fn covers(&self, device_id: &str, public_key: &str) -> bool;
    fn covers_member(&self, member: &CircleMember) -> bool;
}

impl PairCovers for GrantPair {
    fn covers(&self, device_id: &str, public_key: &str) -> bool {
        self.redemption.joiner_device_id == device_id
            && self.redemption.joiner_public_key == public_key
    }

    fn covers_member(&self, member: &CircleMember) -> bool {
        self.covers(&member.device.device_id, &member.public_key)
            && match self.redemption.member_class.as_str() {
                MEMBER_CLASS_GUEST => member
                    .cloud_lease_ms
                    .is_none_or(|lease| lease <= self.redemption.lease_expires_at_ms),
                _ => member.cloud_lease_ms.is_none(),
            }
    }
}

/// The redemption's checks that need no key: the grant is the batch's only grant, the redemption
/// names the same pairing, is not before the grant and is no later than its expiry.
fn redemption_fits(grant: &TokenGrant, redemption: &Redemption, now_ms: u64) -> bool {
    redemption.check_shape(now_ms).is_ok()
        && redemption.member_class == grant.member_class
        && redemption.lease_expires_at_ms <= grant.lease_expires_at_ms
        && redemption.pairing_id == grant.pairing_id
        && redemption.joiner_device_id != grant.admitter_device_id
        && redemption.redeemed_at_ms <= grant.expires_at_ms
        && redemption.redeemed_at_ms >= grant.issued_at_ms
}

/// `approval_sig::endorse_action` over these bodies, then these grants, in order: one batch hash.
pub fn endorse_action(bodies: &[EndorsementBody], grants: &[TokenGrant]) -> String {
    let mut lines: Vec<String> = bodies.iter().map(EndorsementBody::line).collect();
    lines.extend(grants.iter().map(TokenGrant::line));
    approval_sig::endorse_action(&lines)
}

/// Verification and construction of an [`EndorsementBatch`].
pub trait EndorsementBatchExt {
    /// The batch an engine's owner check left over `bodies`.
    fn from_check(proof: OwnerCheckProof, bodies: Vec<EndorsementBody>) -> EndorsementBatch;

    /// [`Self::from_check`] over `bodies` and `grants` (a join token's prompt).
    fn from_check_grants(
        proof: OwnerCheckProof,
        bodies: Vec<EndorsementBody>,
        grants: Vec<TokenGrant>,
    ) -> EndorsementBatch;

    /// Whether the signature is the owner-check key's over exactly these bodies, and the bodies
    /// are well formed, sorted and unique. A batch never expires; only one dated in the future is
    /// refused.
    fn verify(&self, now_ms: u64) -> Result<(), LinkError>;
}

impl EndorsementBatchExt for EndorsementBatch {
    fn from_check(proof: OwnerCheckProof, bodies: Vec<EndorsementBody>) -> EndorsementBatch {
        Self::from_check_grants(proof, bodies, Vec::new())
    }

    fn from_check_grants(
        proof: OwnerCheckProof,
        bodies: Vec<EndorsementBody>,
        grants: Vec<TokenGrant>,
    ) -> EndorsementBatch {
        EndorsementBatch {
            bodies,
            grants,
            sig: owner_sig_from_check(proof),
        }
    }

    pub fn verify(&self, now_ms: u64) -> Result<(), LinkError> {
        if (self.bodies.is_empty() && self.grants.is_empty())
            || self.bodies.len() > CIRCLE_MAX_MEMBERS
            || self.grants.len() > 1
        {
            return Err(LinkError::Malformed("endorsement batch size"));
        }
        check_key_shape(&self.sig.check)?;
        for grant in &self.grants {
            grant.check_shape(now_ms)?;
        }
        let mut previous: Option<&str> = None;
        for body in &self.bodies {
            body.check_shape(now_ms)?;
            if previous.is_some_and(|p| p >= body.device_id.as_str()) {
                return Err(LinkError::Malformed("endorsement bodies not canonical"));
            }
            previous = Some(&body.device_id);
        }
        approval_sig::check_owner_proof(
            &owner_sig_to_check(&self.sig),
            &endorse_action(&self.bodies, &self.grants),
        )
        .map_err(|_| LinkError::NotAuthorized)
    }
}

fn encode_check(enc: Enc, check: Option<&CheckKey>) -> Enc {
    match check {
        Some(c) => enc.u64(1).str(&c.device).str(&c.public_key),
        None => enc.u64(0),
    }
}

/// A batch's signed bytes in a message that carries one (an introduction, a member list).
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
    let mut enc = enc.u64(batch.grants.len() as u64);
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

/// Grant pairs in a message that carries them: each pair's batch, then its redemption.
fn encode_pairs(enc: Enc, pairs: &[GrantPair]) -> Enc {
    let mut enc = enc.u64(pairs.len() as u64);
    for pair in pairs {
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
    enc
}

fn encode_batches(enc: Enc, batches: &[EndorsementBatch]) -> Enc {
    let mut enc = enc.u64(batches.len() as u64);
    for batch in batches {
        enc = encode_batch(enc, batch);
    }
    enc
}

pub(super) fn check_key_shape(check: &CheckKey) -> Result<(), LinkError> {
    let ok = matches!(
        check.device.as_str(),
        approval_sig::OWNER_DEVICE | approval_sig::HELLO_DEVICE
    ) && check.public_key.len() <= 600
        && !check.public_key.is_empty()
        && check.public_key.bytes().all(|b| b.is_ascii_hexdigit())
        && check.binding.as_ref().is_none_or(|b| {
            b.device_id.len() <= 64
                && b.device_key.len() == 64
                && b.nonce.len() <= 64
                && b.signature.len() <= 600
        });
    if ok {
        Ok(())
    } else {
        Err(LinkError::Malformed("circle owner-check key"))
    }
}

/// One member as another member describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CircleMember {
    /// Id, platform and name. The id must derive from `public_key`.
    pub device: DeviceRef,
    /// Identity public key, 64 hex. It is also the device's mesh endpoint id.
    pub public_key: String,
    /// The relay URL this member is reached by, `https` only; a dial hint, never trusted.
    pub relay: Option<String>,
    /// A cloud agent of the owner: when its lease ends, in epoch ms. Every member that
    /// records it from a signed list or introduction holds it to the same lease and the same
    /// refusals as the computer that minted its link. Signed only when present, so the bytes of
    /// every other member are unchanged (a build without it rejects a list naming an agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_lease_ms: Option<u64>,
}

fn encode_member(enc: Enc, member: &CircleMember) -> Enc {
    let enc = enc.device(&member.device).str(&member.public_key);
    let enc = match &member.relay {
        Some(relay) => enc.u64(1).str(relay),
        None => enc.u64(0),
    };
    match member.cloud_lease_ms {
        Some(lease) => enc.str("cloud").u64(lease),
        None => enc,
    }
}

/// A signed list of every member the issuer holds an endorsement for, for a device it just admitted
/// (or a phone that asks for it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberList {
    pub schema_version: u32,
    pub issuer_device_id: String,
    /// The device the list is for; a list names its reader.
    pub recipient_device_id: String,
    /// The recipient's admission id (32 hex), chosen by its admitter.
    pub introduction_id: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub members: Vec<CircleMember>,
    /// The endorsement batches that vouch for the listed members, and the chain of batches that
    /// endorse their signers. A listed member without a verifiable chain is not added.
    #[serde(default)]
    pub endorsements: Vec<EndorsementBatch>,
    /// The join-token endorsements (grant and redemption) of listed members that joined by token
    /// or QR, with no batch of their own.
    #[serde(default)]
    pub grant_pairs: Vec<GrantPair>,
    pub signature: String,
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
            enc = encode_member(enc, member);
        }
        encode_pairs(encode_batches(enc, &self.endorsements), &self.grant_pairs).finish()
    }
}

/// A signed introduction of one new device to one existing member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Introduction {
    pub schema_version: u32,
    pub issuer_device_id: String,
    pub recipient_device_id: String,
    /// The new device's admission id (32 hex), the same in every message about it.
    pub introduction_id: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub member: CircleMember,
    /// The batch that endorses `member`, and the chain of batches that endorse its signer.
    #[serde(default)]
    pub endorsements: Vec<EndorsementBatch>,
    /// Instead of a batch for `member`: the grant and redemption of a device that joined by token
    /// or QR (at most one).
    #[serde(default)]
    pub grant_pairs: Vec<GrantPair>,
    pub signature: String,
}

impl Introduction {
    fn signing_input(&self) -> Vec<u8> {
        let enc = encode_member(
            Enc::new(INTRODUCTION_DOMAIN)
                .str(&self.issuer_device_id)
                .str(&self.recipient_device_id)
                .str(&self.introduction_id)
                .u64(self.issued_at_ms)
                .u64(self.expires_at_ms),
            &self.member,
        );
        encode_pairs(encode_batches(enc, &self.endorsements), &self.grant_pairs).finish()
    }
}

/// A signed removal of one circle member, sent to each other member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CircleRevoke {
    pub schema_version: u32,
    pub issuer_device_id: String,
    pub recipient_device_id: String,
    pub target_device_id: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub signature: String,
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

/// A computer's owner-check key binding, signed by its device key and sent over an authenticated
/// direct link to a peer that holds the computer's record without a bound key (the key was enrolled
/// or bound after the pairing). The peer stores the key on that record only when the binding names
/// that device id and key; it never grants `admit` by itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckKeyUpdate {
    pub schema_version: u32,
    pub device_id: String,
    pub recipient_device_id: String,
    pub check: CheckKey,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub signature: String,
}

impl CheckKeyUpdate {
    fn signing_input(&self) -> Vec<u8> {
        let enc = Enc::new(CHECKKEY_DOMAIN)
            .str(&self.device_id)
            .str(&self.recipient_device_id)
            .str(&self.check.device)
            .str(&self.check.public_key);
        let enc = match &self.check.binding {
            Some(b) => enc
                .u64(1)
                .str(&b.device_id)
                .str(&b.device_key)
                .str(&b.nonce)
                .u64(b.issued)
                .u64(b.expires)
                .str(&b.signature),
            None => enc.u64(0),
        };
        enc.u64(self.issued_at_ms).u64(self.expires_at_ms).finish()
    }
}

/// The pairing id two circle members both store for each other (32 hex): SHA-256 over the two
/// device ids in sorted order.
pub fn circle_pairing_id(a: &str, b: &str) -> String {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    let digest = sha256(&Enc::new(PAIRING_DOMAIN).str(low).str(high).finish());
    hex(&digest[..16])
}

fn clean_name(name: &str) -> bool {
    name.chars().count() <= MAX_NAME_CHARS
        && !name.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
}

/// A member's shape: the key is 32 bytes, the id derives from it, the name and relay are bounded.
fn check_member(member: &CircleMember) -> Result<(), LinkError> {
    let key = unhex::<32>(&member.public_key)?;
    if member.device.device_id != device_id_for_key(&key) {
        return Err(LinkError::Malformed(
            "circle member id does not derive from its key",
        ));
    }
    if member.device.schema_version != rightkit_device_link_types::DEVICE_REF_SCHEMA_VERSION {
        return Err(LinkError::Malformed("circle member version"));
    }
    if let Some(name) = &member.device.display_name
        && !clean_name(name)
    {
        return Err(LinkError::Malformed("circle member name"));
    }
    if let Some(relay) = &member.relay {
        let https = relay
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"));
        if !https || relay.len() > MAX_RELAY_CHARS || relay.chars().any(char::is_control) {
            return Err(LinkError::Malformed("circle member relay"));
        }
    }
    Ok(())
}

fn check_version(version: u32) -> Result<(), LinkError> {
    if version == DEVICE_LINK_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(LinkError::SchemaVersion(version))
    }
}

/// A message's window: not from the future, not expired, not longer-lived than the longest any
/// circle message may live; members lists and introductions are held to the short one by
/// [`check_short_window`].
fn check_window(issued: u64, expires: u64, now_ms: u64) -> Result<(), LinkError> {
    if expires <= now_ms {
        return Err(LinkError::Expired);
    }
    if issued > now_ms.saturating_add(CLOCK_SKEW_MS) {
        return Err(LinkError::Malformed("circle message issued in the future"));
    }
    if expires < issued || expires - issued > CIRCLE_REVOKE_TTL_MS {
        return Err(LinkError::Malformed("circle message window"));
    }
    Ok(())
}

/// A member list or an introduction lives ten minutes at most.
fn check_short_window(issued: u64, expires: u64) -> Result<(), LinkError> {
    if expires < issued || expires - issued > CIRCLE_MESSAGE_TTL_MS {
        return Err(LinkError::Malformed("circle message window"));
    }
    Ok(())
}

fn member_of(record: &PairedDevice, relay: Option<String>) -> CircleMember {
    CircleMember {
        device: record.device.clone(),
        public_key: record.public_key.clone(),
        relay,
        // Only an approved cloud agent is listed at all (`is_active`); its lease travels with it.
        // Never later than the lease the admitter signed into the redemption (`covers_member`).
        cloud_lease_ms: record
            .guest
            .as_ref()
            .filter(|g| g.approved_ms.is_some())
            .map(|g| match record.grant_pair.as_ref() {
                Some(pair) if pair.redemption.is_guest() => {
                    g.lease_expires_ms.min(pair.redemption.lease_expires_at_ms)
                }
                _ => g.lease_expires_ms,
            }),
    }
}

/// Whether a joiner's wire platform is a computer that can hold an owner-check key.
pub fn wire_platform_is_computer(platform: &str) -> bool {
    matches!(platform, "macos" | "windows")
}

/// The bodies a verified pool of batches vouches for, from the owner-check keys a device trusts.
struct Chain {
    /// Device id to the body and the index of the trusted batch that carries it.
    bodies: HashMap<String, (EndorsementBody, usize)>,
    /// Every owner-check key this device trusts to admit, with the device (id and public key) it
    /// belongs to: the record that holds it, or the body that carried it.
    admitters: HashMap<CheckKey, (String, String)>,
    /// Device ids whose records are revoked here.
    revoked: HashSet<String>,
}

/// Resolve `batches` against the keys this device trusts: the owner-check key of every active
/// member holding `admit` (what its own direct pairing verified, or what a trusted endorsement
/// carried), then, repeatedly, every owner-check key carried with `can_admit` in a batch whose
/// signer is already trusted. A batch is used only when its signature and bodies verify and its
/// signer is trusted; the bodies of a batch signed by any other key are ignored whole. A key of a
/// revoked device (its record's key, or a body for a revoked device id) is never trusted.
fn trust_chain(registry: &Registry, batches: &[EndorsementBatch], now_ms: u64) -> Chain {
    let mut keys: Vec<CheckKey> = Vec::new();
    let mut blocked: Vec<CheckKey> = Vec::new();
    let mut revoked: HashSet<&str> = HashSet::new();
    let mut admitters: HashMap<CheckKey, (String, String)> = HashMap::new();
    for record in &registry.devices {
        if record.is_active() {
            if record.admit
                && let Some(key) = &record.admit_check_key
                && !keys.contains(key)
                && key.bound_or_log(&record.device.device_id, &record.public_key)
            {
                keys.push(key.clone());
                admitters.insert(
                    key.clone(),
                    (record.device.device_id.clone(), record.public_key.clone()),
                );
            }
        } else {
            blocked.extend(record.admit_check_key.clone());
            revoked.insert(record.device.device_id.as_str());
        }
    }
    keys.retain(|k| !blocked.contains(k));
    admitters.retain(|k, _| keys.contains(k));
    let valid: Vec<bool> = batches.iter().map(|b| b.verify(now_ms).is_ok()).collect();
    let mut used = vec![false; batches.len()];
    let mut bodies: HashMap<String, (EndorsementBody, usize)> = HashMap::new();
    loop {
        let mut progress = false;
        for (i, batch) in batches.iter().enumerate() {
            if used[i] || !valid[i] || !keys.contains(&batch.sig.check) {
                continue;
            }
            used[i] = true;
            progress = true;
            for body in &batch.bodies {
                if revoked.contains(body.device_id.as_str()) {
                    continue;
                }
                // A check key is trusted for the device its own binding names, no other.
                if body.can_admit
                    && let Some(key) = &body.member_check
                    && !blocked.contains(key)
                    && !keys.contains(key)
                    && key.bound_or_log(&body.device_id, &body.public_key)
                {
                    keys.push(key.clone());
                    admitters.insert(
                        key.clone(),
                        (body.device_id.clone(), body.public_key.clone()),
                    );
                }
                // A device endorsed twice keeps the endorsement that lets it admit.
                let better = bodies
                    .get(&body.device_id)
                    .is_none_or(|(have, _)| !have.can_admit && body.can_admit);
                if better {
                    bodies.insert(body.device_id.clone(), (body.clone(), i));
                }
            }
        }
        if !progress {
            break;
        }
    }
    Chain {
        bodies,
        admitters,
        revoked: revoked.into_iter().map(str::to_string).collect(),
    }
}

/// An introduced record keeps `admit` only when the trust chain maps its check key to exactly this
/// device (a key the chain trusts only through a binding that names this device).
fn strip_untrusted_admit(chain: &Chain, record: &mut PairedDevice) {
    let Some(key) = &record.admit_check_key else {
        return;
    };
    let id = &record.device.device_id;
    if !chain.admitters.get(key).is_some_and(|(d, _)| d == id) {
        eprintln!("circle: introduced device's owner-check key not bound to it; no admit");
        record.admit = false;
        record.admit_check_key = None;
    }
}

/// Every batch a message carries: its endorsement batches, then the batch of each grant pair.
fn with_pair_batches(
    endorsements: &[EndorsementBatch],
    pairs: &[GrantPair],
) -> Vec<EndorsementBatch> {
    let mut all = endorsements.to_vec();
    all.extend(pairs.iter().map(|p| p.batch.clone()));
    all
}

/// Pairing ids that two different joiners claimed, kept for the engine to log
/// ([`take_grant_conflicts`]). A pairing id is not a secret.
static GRANT_CONFLICTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The pairing ids this process rejected because two different joiners redeemed them, drained.
pub fn take_grant_conflicts() -> Vec<String> {
    std::mem::take(
        &mut *GRANT_CONFLICTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    )
}

/// Whether `pair` stands on its own: one grant, a batch that verifies and is signed by an
/// owner-check key this device trusts to admit (`chain`), a redemption signed by the DEVICE key of
/// the device that check key belongs to (and that is the grant's admitter), made no later than the
/// grant's expiry.
fn pair_valid(chain: &Chain, pair: &GrantPair, now_ms: u64) -> bool {
    let Some(grant) = pair.grant() else {
        return false;
    };
    let check = &pair.batch.sig.check;
    let Some((admitter_id, admitter_key)) = chain.admitters.get(check) else {
        return false;
    };
    if *admitter_id != grant.admitter_device_id
        || chain.revoked.contains(admitter_id.as_str())
        || pair.batch.verify(now_ms).is_err()
        || !redemption_fits(grant, &pair.redemption, now_ms)
    {
        return false;
    }
    // The redemption's date is the admitter's own word (R07): hold it to this device's clock. A
    // grant whose window closed more than the delivery allowance ago is no evidence, and neither
    // is a guest whose signed lease already ended.
    if now_ms
        > grant
            .expires_at_ms
            .saturating_add(REDEMPTION_DELIVERY_ALLOWANCE_MS)
        || (pair.redemption.is_guest() && now_ms >= pair.redemption.lease_expires_at_ms)
    {
        return false;
    }
    let (Ok(key), Ok(signature)) = (
        unhex::<32>(admitter_key),
        unhex::<64>(&pair.redemption.signature),
    ) else {
        return false;
    };
    verify(&key, &pair.redemption.signing_input(), &signature).is_ok()
}

/// Which of `pairs` (by index) this device accepts, and the pairing ids two joiners claimed. A
/// pairing id with more than one joiner, counting the pairs held in `registry` and the valid ones
/// here, accepts none of them.
fn accept_pairs(
    registry: &Registry,
    chain: &Chain,
    pairs: &[GrantPair],
    now_ms: u64,
) -> (Vec<usize>, Vec<String>) {
    let valid: Vec<usize> = (0..pairs.len())
        .filter(|&i| pair_valid(chain, &pairs[i], now_ms))
        .collect();
    let mut joiners: HashMap<&str, HashSet<(&str, &str)>> = HashMap::new();
    for pair in registry
        .devices
        .iter()
        .filter_map(|d| d.grant_pair.as_ref())
    {
        joiners
            .entry(pair.redemption.pairing_id.as_str())
            .or_default()
            .insert((
                pair.redemption.joiner_device_id.as_str(),
                pair.redemption.joiner_public_key.as_str(),
            ));
    }
    // The durable ledger: a pairing id redeemed once stays claimed after its record is gone.
    for seen in &registry.redeemed_pairings {
        joiners
            .entry(seen.pairing_id.as_str())
            .or_default()
            .insert((
                seen.joiner_device_id.as_str(),
                seen.joiner_public_key.as_str(),
            ));
    }
    for &i in &valid {
        let r = &pairs[i].redemption;
        joiners
            .entry(r.pairing_id.as_str())
            .or_default()
            .insert((r.joiner_device_id.as_str(), r.joiner_public_key.as_str()));
    }
    let mut conflicts: Vec<String> = joiners
        .iter()
        .filter(|(_, who)| who.len() > 1)
        .map(|(id, _)| (*id).to_string())
        .collect();
    conflicts.sort();
    let accepted = valid
        .into_iter()
        .filter(|&i| !conflicts.contains(&pairs[i].redemption.pairing_id))
        .collect();
    (accepted, conflicts)
}

/// Record the redemptions of the accepted `pairs` in the registry's durable ledger (first seen
/// now), and drop entries older than the grant window plus the delivery allowance (R07).
fn note_redemptions(registry: &mut Registry, pairs: &[GrantPair], accepted: &[usize], now_ms: u64) {
    let keep_ms = PAIRING_TTL_MS.saturating_add(REDEMPTION_DELIVERY_ALLOWANCE_MS);
    registry
        .redeemed_pairings
        .retain(|e| now_ms.saturating_sub(e.first_seen_ms) <= keep_ms);
    for &i in accepted {
        let r = &pairs[i].redemption;
        if !registry.redeemed_pairings.iter().any(|e| {
            e.pairing_id == r.pairing_id
                && e.joiner_device_id == r.joiner_device_id
                && e.joiner_public_key == r.joiner_public_key
        }) {
            registry.redeemed_pairings.push(RedeemedPairing {
                pairing_id: r.pairing_id.clone(),
                joiner_device_id: r.joiner_device_id.clone(),
                joiner_public_key: r.joiner_public_key.clone(),
                first_seen_ms: now_ms,
            });
        }
    }
}

/// Act on `conflicts`: log each pairing id, and revoke every INTRODUCED record that joined under
/// it (a directly paired device only changes by its own owner).
fn reject_conflicts(registry: &mut Registry, conflicts: &[String], now_ms: u64, by: &str) {
    if conflicts.is_empty() {
        return;
    }
    {
        let mut log = GRANT_CONFLICTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for id in conflicts {
            if log.len() < 64 && !log.contains(id) {
                log.push(id.clone());
            }
        }
    }
    for record in registry.devices.iter_mut() {
        if record.introduced_by.is_some()
            && record
                .grant_pair
                .as_ref()
                .is_some_and(|p| conflicts.contains(&p.redemption.pairing_id))
        {
            revoke_record(record, now_ms, by);
        }
    }
}

/// The joiner's check on the grant and redemption its host sent in the final message of a token or
/// QR pairing, as the host's owner-check key to pin (`admit`), or `None`. Everything must hold: the
/// batch verifies and carries exactly one grant, naming this host and this pairing id and the hash
/// of the secret THIS device holds; the redemption names this device and the same pairing id, is no
/// later than the grant's expiry and is signed by the host's device key.
#[allow(clippy::too_many_arguments)]
pub fn host_grant_check(
    batch: &EndorsementBatch,
    redemption: &Redemption,
    me_id: &str,
    me_key_hex: &str,
    host_id: &str,
    host_key_hex: &str,
    pairing_id: &str,
    secret_hash: &str,
    now_ms: u64,
) -> Option<CheckKey> {
    let [grant] = batch.grants.as_slice() else {
        return None;
    };
    let host_key = unhex::<32>(host_key_hex).ok()?;
    let signature = unhex::<64>(&redemption.signature).ok()?;
    (grant.admitter_device_id == host_id
        && grant.pairing_id == pairing_id
        && grant.secret_hash == secret_hash
        && redemption.joiner_device_id == me_id
        && redemption.joiner_public_key == me_key_hex
        && batch.verify(now_ms).is_ok()
        && redemption_fits(grant, redemption, now_ms)
        && batch.sig.check.bound_or_log(host_id, host_key_hex)
        && verify(&host_key, &redemption.signing_input(), &signature).is_ok())
    .then(|| batch.sig.check.clone())
}

/// Add to `batches` the batch that endorses each batch's signer, as far as this device holds them
/// (the signer is a member whose record keeps its own endorsement), up to the limit.
fn add_chain(registry: &Registry, batches: &mut Vec<EndorsementBatch>) {
    let mut i = 0;
    while i < batches.len() {
        let signer = batches[i].sig.check.clone();
        for record in registry
            .devices
            .iter()
            .filter(|r| r.is_active() && r.admit && r.admit_check_key.as_ref() == Some(&signer))
        {
            let Some(batch) = &record.endorsement else {
                continue;
            };
            if batches.len() >= CIRCLE_MAX_BATCHES
                || batches
                    .iter()
                    .any(|b| b.sig.signature == batch.sig.signature)
            {
                continue;
            }
            batches.push(batch.clone());
        }
        i += 1;
    }
}

fn push_batch(batches: &mut Vec<EndorsementBatch>, batch: &EndorsementBatch) {
    if batches.len() < CIRCLE_MAX_BATCHES
        && !batches
            .iter()
            .any(|b| b.sig.signature == batch.sig.signature)
    {
        batches.push(batch.clone());
    }
}

fn push_pair(pairs: &mut Vec<GrantPair>, pair: &GrantPair) {
    if pairs.len() < CIRCLE_MAX_PAIRS
        && !pairs
            .iter()
            .any(|p| p.redemption.signature == pair.redemption.signature)
    {
        pairs.push(pair.clone());
    }
}

/// [`add_chain`] for the signers of `batches` and of the batches inside `pairs`: adds the batches
/// that endorse them to `batches`.
fn add_chain_for(registry: &Registry, batches: &mut Vec<EndorsementBatch>, pairs: &[GrantPair]) {
    let held = batches.len();
    let mut probe = with_pair_batches(batches, pairs);
    add_chain(registry, &mut probe);
    for batch in probe.iter().skip(held + pairs.len()) {
        push_batch(batches, batch);
    }
}

impl DeviceLink {
    /// This device as a circle member, with the relay it is reached by (`own_relay`, if any).
    fn own_member(&self, own_relay: Option<&str>) -> CircleMember {
        CircleMember {
            device: self.identity().device().clone(),
            public_key: self.identity().public_key_hex(),
            relay: own_relay.map(str::to_string),
            cloud_lease_ms: None,
        }
    }

    /// The admission id of `device_id` (32 hex): the one this record already has, or a fresh one
    /// stored now. Every list and introduction about the device carries it.
    pub fn circle_admission(&self, device_id: &str) -> Result<String, LinkError> {
        self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !record.is_active() {
                return Err(LinkError::Revoked);
            }
            if let Some(id) = &record.introduction_id {
                return Ok(id.clone());
            }
            let id = hex(&random::<16>()?);
            record.introduction_id = Some(id.clone());
            record.revision += 1;
            Ok(id)
        })
    }

    /// A fresh join token's pairing id and secret, with the [`TokenGrant`] the owner-check prompt
    /// that makes the token signs (`device.owner.challenge`, `join`). The secret stays with the
    /// caller until the owner check passes; only its hash is signed.
    pub fn prepare_token_grant(&self, now_ms: u64) -> Result<(TokenGrant, [u8; 32]), LinkError> {
        let pairing_id = hex(&random::<16>()?);
        let secret = random::<32>()?;
        let grant = TokenGrant::new(self.identity().device_id(), &pairing_id, &secret, now_ms);
        Ok((grant, secret))
    }

    /// The bodies ONE owner-check signature covers when this computer admits `new_device_id`:
    /// the new device (`can_admit` only for a computer that told this one its owner-check key), and
    /// every active member this computer holds an owner-check key for (verified by its own pairing)
    /// and has not endorsed yet. Sorted by device id, as the batch is signed.
    pub fn plan_endorsements(
        &self,
        new_device_id: &str,
        new_public_key: &str,
        new_is_computer: bool,
        new_check: Option<CheckKey>,
        now_ms: u64,
    ) -> Result<Vec<EndorsementBody>, LinkError> {
        // A claimed check key is endorsed only with its binding to this joiner's device id and
        // key: the joiner is otherwise admitted as a device without `admit`.
        let new_check = new_check.filter(|k| k.bound_or_log(new_device_id, new_public_key));
        let can = new_is_computer && new_check.is_some();
        let mut bodies = vec![EndorsementBody {
            device_id: new_device_id.to_string(),
            public_key: new_public_key.to_string(),
            member_check: new_check.filter(|_| can),
            can_admit: can,
            issued_at_ms: now_ms,
        }];
        self.push_unendorsed(&mut bodies, new_device_id, now_ms)?;
        Ok(bodies)
    }

    /// The bodies the prompt that makes a join token or QR signs beside its [`TokenGrant`]: every
    /// active member this computer holds an owner-check key for and has not endorsed yet (there is
    /// no new device yet). So an admitter's first grant also endorses its own host. May be empty.
    pub fn plan_grant_endorsements(&self, now_ms: u64) -> Result<Vec<EndorsementBody>, LinkError> {
        let mut bodies = Vec::new();
        self.push_unendorsed(&mut bodies, "", now_ms)?;
        Ok(bodies)
    }

    /// Append a body for each active member (not `skip`) this computer verified an owner-check key
    /// for and holds no endorsement of, then sort by device id.
    fn push_unendorsed(
        &self,
        bodies: &mut Vec<EndorsementBody>,
        skip: &str,
        now_ms: u64,
    ) -> Result<(), LinkError> {
        let registry = self.registry()?;
        for record in registry.devices.iter().filter(|r| {
            r.is_active()
                && r.device.device_id != skip
                && ((r
                    .endorsement
                    .as_ref()
                    .is_none_or(|b| b.verify(now_ms).is_err())
                    && !r.is_guest()
                    && ((r.admit && r.admit_check_key.is_some()) || r.grant_pair.is_some()))
                    || awaiting_admit(r))
        }) {
            if bodies.len() >= CIRCLE_MAX_MEMBERS {
                break;
            }
            bodies.push(EndorsementBody {
                device_id: record.device.device_id.clone(),
                public_key: record.public_key.clone(),
                member_check: record.admit_check_key.clone().filter(|_| record.admit),
                can_admit: record.admit && record.admit_check_key.is_some(),
                issued_at_ms: now_ms,
            });
        }
        bodies.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        Ok(())
    }

    /// Keep the batch this computer's owner-check signature made when it admitted
    /// `new_device_id` (a direct pairing's record only): the record holds it for its own lists and
    /// introductions and, when the batch made it a computer with an owner-check key, trusts that
    /// key (`admit`). Every other member the batch endorsed and this computer holds no endorsement
    /// for keeps it too. Refused unless the batch verifies and carries a body for exactly this
    /// device and key.
    pub fn record_endorsement(
        &self,
        batch: &EndorsementBatch,
        new_device_id: &str,
        now_ms: u64,
    ) -> Result<(), LinkError> {
        batch.verify(now_ms)?;
        self.mutate(|registry| {
            let new_body = batch
                .body_for(new_device_id)
                .ok_or(LinkError::NotAuthorized)?;
            let key_bound = new_body
                .member_check
                .as_ref()
                .is_some_and(|k| k.bound_or_log(new_device_id, &new_body.public_key));
            {
                let record = registry
                    .get_mut(new_device_id)
                    .ok_or(LinkError::UnknownDevice)?;
                if !record.is_active()
                    || record.introduced_by.is_some()
                    || record.public_key != new_body.public_key
                {
                    return Err(LinkError::NotAuthorized);
                }
                let admit = new_body.can_admit && key_bound && can_admit(record.device.platform);
                record.admit = admit;
                record.admit_check_key = new_body.member_check.clone().filter(|_| admit);
                record.endorsement = Some(batch.clone());
                record.revision += 1;
            }
            for body in batch.bodies.iter().filter(|b| b.device_id != new_device_id) {
                if let Some(record) = registry.get_mut(&body.device_id)
                    && record.is_active()
                    && record.public_key == body.public_key
                {
                    if record
                        .endorsement
                        .as_ref()
                        .is_none_or(|b| b.verify(now_ms).is_err())
                    {
                        record.endorsement = Some(batch.clone());
                        record.revision += 1;
                    } else {
                        upgrade_endorsed(record, batch, body);
                    }
                }
            }
            Ok(())
        })
    }

    /// Redeem the join token of `batch`'s grant for `joiner_device_id`, whose record (a direct
    /// pairing, `pairing_id` equal to the grant's) was just written: sign the [`Redemption`] with
    /// this device's key and keep the pair on the joiner's record. The joiner never gets `admit`.
    /// Every other member the batch endorsed and this computer holds no endorsement for keeps it
    /// too. Refused unless the batch verifies and carries exactly one grant of THIS device, `now_ms`
    /// is inside the grant's window, and no other joiner redeemed this pairing id. Returns the
    /// redemption to send to the joiner.
    pub fn record_grant_redemption(
        &self,
        batch: &EndorsementBatch,
        joiner_device_id: &str,
        now_ms: u64,
    ) -> Result<Redemption, LinkError> {
        self.tick(now_ms)?;
        batch.verify(now_ms)?;
        let [grant] = batch.grants.as_slice() else {
            return Err(LinkError::NotAuthorized);
        };
        if grant.admitter_device_id != self.identity().device_id() {
            return Err(LinkError::NotAuthorized);
        }
        if now_ms > grant.expires_at_ms {
            return Err(LinkError::Expired);
        }
        self.mutate(|registry| {
            let held = registry.devices.iter().any(|d| {
                d.grant_pair
                    .as_ref()
                    .is_some_and(|p| p.redemption.pairing_id == grant.pairing_id)
            }) || registry
                .redeemed_pairings
                .iter()
                .any(|e| e.pairing_id == grant.pairing_id);
            let record = registry
                .get_mut(joiner_device_id)
                .ok_or(LinkError::UnknownDevice)?;
            // A cloud agent is redeemed while it waits for approval; it is introduced to the
            // circle only once approved (`make_introduction` asks `is_active`).
            if !record.is_unrevoked()
                || record.introduced_by.is_some()
                || record.pairing_id != grant.pairing_id
                || held
            {
                return Err(LinkError::NotAuthorized);
            }
            // The class and lease are signed in: a guest record makes a guest redemption
            // for its pass's lease, every other joiner a member one.
            let (member_class, lease_expires_at_ms) = match record.guest.as_mut() {
                Some(pass) if grant.member_class == MEMBER_CLASS_GUEST => {
                    pass.lease_expires_ms = pass.lease_expires_ms.min(grant.lease_expires_at_ms);
                    (MEMBER_CLASS_GUEST, pass.lease_expires_ms)
                }
                None if grant.member_class == MEMBER_CLASS_MEMBER => (MEMBER_CLASS_MEMBER, 0),
                _ => return Err(LinkError::NotAuthorized),
            };
            let mut redemption = Redemption {
                pairing_id: grant.pairing_id.clone(),
                joiner_device_id: record.device.device_id.clone(),
                joiner_public_key: record.public_key.clone(),
                redeemed_at_ms: now_ms,
                member_class: member_class.to_string(),
                lease_expires_at_ms,
                signature: String::new(),
            };
            redemption.signature = self.identity().sign(&redemption.signing_input());
            record.admit = false;
            record.admit_check_key = None;
            record.grant_pair = Some(GrantPair {
                batch: batch.clone(),
                redemption: redemption.clone(),
            });
            record.revision += 1;
            registry.redeemed_pairings.push(RedeemedPairing {
                pairing_id: redemption.pairing_id.clone(),
                joiner_device_id: redemption.joiner_device_id.clone(),
                joiner_public_key: redemption.joiner_public_key.clone(),
                first_seen_ms: now_ms,
            });
            for body in &batch.bodies {
                if let Some(other) = registry.get_mut(&body.device_id)
                    && other.is_active()
                    && other.device.device_id != joiner_device_id
                    && other.public_key == body.public_key
                {
                    if other
                        .endorsement
                        .as_ref()
                        .is_none_or(|b| b.verify(now_ms).is_err())
                    {
                        other.endorsement = Some(batch.clone());
                        other.revision += 1;
                    } else {
                        upgrade_endorsed(other, batch, body);
                    }
                }
            }
            Ok(redemption)
        })
    }

    /// Sign the list of every other active member this device holds an endorsement for, for
    /// `recipient_device_id`. The recipient's own record and any revoked record are left out; this
    /// device is included. The list carries the endorsement batches (and the chain that endorses
    /// their signers), so the reader can verify each member itself.
    pub fn make_member_list(
        &self,
        recipient_device_id: &str,
        own_relay: Option<&str>,
        now_ms: u64,
    ) -> Result<MemberList, LinkError> {
        self.tick(now_ms)?;
        let introduction_id = self.circle_admission(recipient_device_id)?;
        let registry = self.registry()?;
        let mut members = vec![self.own_member(own_relay)];
        let mut batches: Vec<EndorsementBatch> = Vec::new();
        let mut pairs: Vec<GrantPair> = Vec::new();
        for d in registry
            .devices
            .iter()
            .filter(|d| d.is_active() && d.device.device_id != recipient_device_id)
        {
            let batch = d
                .endorsement
                .as_ref()
                .filter(|b| b.endorses(&d.device.device_id, &d.public_key));
            let pair = d
                .grant_pair
                .as_ref()
                .filter(|p| p.covers(&d.device.device_id, &d.public_key));
            if batch.is_none() && pair.is_none() {
                continue;
            }
            if members.len() >= CIRCLE_MAX_MEMBERS {
                break;
            }
            members.push(member_of(d, d.relay.clone()));
            match (batch, pair) {
                (Some(batch), _) => push_batch(&mut batches, batch),
                (None, Some(pair)) => push_pair(&mut pairs, pair),
                (None, None) => {}
            }
        }
        add_chain_for(&registry, &mut batches, &pairs);
        let mut list = MemberList {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            issuer_device_id: self.identity().device_id().to_string(),
            recipient_device_id: recipient_device_id.to_string(),
            introduction_id,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(CIRCLE_MESSAGE_TTL_MS),
            members,
            endorsements: batches,
            grant_pairs: pairs,
            signature: String::new(),
        };
        list.signature = self.identity().sign(&list.signing_input());
        Ok(list)
    }

    /// Sign the introduction of `new_device_id` for `recipient_device_id`.
    pub fn make_introduction(
        &self,
        new_device_id: &str,
        recipient_device_id: &str,
        now_ms: u64,
    ) -> Result<Introduction, LinkError> {
        self.tick(now_ms)?;
        let introduction_id = self.circle_admission(new_device_id)?;
        let registry = self.registry()?;
        let record = registry
            .get(new_device_id)
            .ok_or(LinkError::UnknownDevice)?;
        if !record.is_active() {
            return Err(LinkError::Revoked);
        }
        // No endorsement and no redeemed grant for this device (a PIN): this device cannot vouch
        // for it.
        let batch = record
            .endorsement
            .as_ref()
            .filter(|b| b.endorses(new_device_id, &record.public_key));
        let pair = record
            .grant_pair
            .as_ref()
            .filter(|p| p.covers(new_device_id, &record.public_key));
        let mut batches = Vec::new();
        let mut pairs = Vec::new();
        match (batch, pair) {
            (Some(batch), _) => push_batch(&mut batches, batch),
            (None, Some(pair)) => push_pair(&mut pairs, pair),
            (None, None) => return Err(LinkError::NotAuthorized),
        }
        add_chain_for(&registry, &mut batches, &pairs);
        let mut intro = Introduction {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            issuer_device_id: self.identity().device_id().to_string(),
            recipient_device_id: recipient_device_id.to_string(),
            introduction_id,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(CIRCLE_MESSAGE_TTL_MS),
            member: member_of(record, record.relay.clone()),
            endorsements: batches,
            grant_pairs: pairs,
            signature: String::new(),
        };
        intro.signature = self.identity().sign(&intro.signing_input());
        Ok(intro)
    }

    fn sign_revoke(
        &self,
        target_device_id: &str,
        recipient_device_id: &str,
        issued_at_ms: u64,
    ) -> CircleRevoke {
        let mut message = CircleRevoke {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            issuer_device_id: self.identity().device_id().to_string(),
            recipient_device_id: recipient_device_id.to_string(),
            target_device_id: target_device_id.to_string(),
            issued_at_ms,
            expires_at_ms: issued_at_ms.saturating_add(CIRCLE_REVOKE_TTL_MS),
            signature: String::new(),
        };
        message.signature = self.identity().sign(&message.signing_input());
        message
    }

    /// Sign the removal of `target_device_id` for `recipient_device_id`.
    pub fn make_circle_revoke(
        &self,
        target_device_id: &str,
        recipient_device_id: &str,
        now_ms: u64,
    ) -> CircleRevoke {
        self.sign_revoke(target_device_id, recipient_device_id, now_ms)
    }

    /// The removals this computer holds for `recipient_device_id`, signed fresh and dated at each
    /// revocation (so a record the recipient added later is not touched): one per device this
    /// computer revoked within the removal window, newest first. A phone, which accepts no inbound
    /// stream, receives these in the reply to its pull and applies them under the same scoping as
    /// [`Self::apply_circle_revoke`].
    pub fn make_circle_revokes_for(
        &self,
        recipient_device_id: &str,
        now_ms: u64,
    ) -> Result<Vec<CircleRevoke>, LinkError> {
        self.tick(now_ms)?;
        let registry = self.registry()?;
        let mut revoked: Vec<(u64, &str)> = registry
            .devices
            .iter()
            .filter(|d| d.device.device_id != recipient_device_id)
            .filter_map(|d| match &d.state {
                PairedState::Revoked { at_ms, .. }
                    if at_ms.saturating_add(CIRCLE_REVOKE_TTL_MS) > now_ms =>
                {
                    Some((*at_ms, d.device.device_id.as_str()))
                }
                _ => None,
            })
            .collect();
        revoked.sort_by_key(|a| std::cmp::Reverse(a.0));
        revoked.truncate(CIRCLE_MAX_MEMBERS);
        Ok(revoked
            .into_iter()
            .map(|(at_ms, id)| self.sign_revoke(id, recipient_device_id, at_ms))
            .collect())
    }

    /// The common checks of every circle message: this device is the recipient, the issuer is the
    /// peer the channel authenticated and an ACTIVE paired member holding `admit`, the window holds
    /// and the signature verifies under the issuer's registered key.
    #[allow(clippy::too_many_arguments)]
    fn check_issuer(
        &self,
        channel_peer_device_id: &str,
        issuer: &str,
        recipient: &str,
        window: (u64, u64),
        signing_input: &[u8],
        signature: &str,
        now_ms: u64,
    ) -> Result<(), LinkError> {
        self.tick(now_ms)?;
        if recipient != self.identity().device_id() || issuer != channel_peer_device_id {
            return Err(LinkError::NotAuthorized);
        }
        check_window(window.0, window.1, now_ms)?;
        let registry = self.registry()?;
        let record = registry.get(issuer).ok_or(LinkError::UnknownDevice)?;
        if !record.is_active() {
            return Err(LinkError::Revoked);
        }
        // Only a member the owner admitted with an owner check may speak for the circle: not a
        // phone, a headless server or an introduced device.
        if !record.admit {
            return Err(LinkError::NotAuthorized);
        }
        let key = unhex::<32>(&record.public_key)?;
        verify(&key, signing_input, &unhex::<64>(signature)?)
    }

    /// Apply a member list received from `channel_peer_device_id`. Returns the device ids newly
    /// recorded. A member that is already known (active or revoked) is left alone: a list never
    /// reactivates a revoked record, never changes an existing one and never removes one (only a
    /// signed [`CircleRevoke`] does: a list that omits a member says nothing about it).
    ///
    /// A listed member is added only when an endorsement batch signed by an owner-check key this
    /// device trusts (`trust_chain`) carries a body for exactly that device and key. A computer
    /// holds `admit` here, with its own owner-check key, only when that body says `can_admit`. The
    /// issuer's device key and its own say-so add nobody.
    pub fn apply_member_list(
        &self,
        channel_peer_device_id: &str,
        list: &MemberList,
        now_ms: u64,
    ) -> Result<Vec<String>, LinkError> {
        check_version(list.schema_version)?;
        if list.members.len() > CIRCLE_MAX_MEMBERS
            || list.endorsements.len() > CIRCLE_MAX_BATCHES
            || list.grant_pairs.len() > CIRCLE_MAX_PAIRS
        {
            return Err(LinkError::Malformed("circle member list too long"));
        }
        unhex::<16>(&list.introduction_id)?;
        check_short_window(list.issued_at_ms, list.expires_at_ms)?;
        self.check_issuer(
            channel_peer_device_id,
            &list.issuer_device_id,
            &list.recipient_device_id,
            (list.issued_at_ms, list.expires_at_ms),
            &list.signing_input(),
            &list.signature,
            now_ms,
        )?;
        let me = self.identity().device_id().to_string();
        for member in &list.members {
            check_member(member)?;
        }
        self.mutate(|registry| {
            let all = with_pair_batches(&list.endorsements, &list.grant_pairs);
            let chain = trust_chain(registry, &all, now_ms);
            let (accepted, conflicts) = accept_pairs(registry, &chain, &list.grant_pairs, now_ms);
            reject_conflicts(registry, &conflicts, now_ms, &list.issuer_device_id);
            note_redemptions(registry, &list.grant_pairs, &accepted, now_ms);
            let mut added = Vec::new();
            for member in &list.members {
                let id = &member.device.device_id;
                if *id == me || *id == list.issuer_device_id || registry.get(id).is_some() {
                    continue;
                }
                if let Some((body, at)) = chain.bodies.get(id.as_str()) {
                    if body.public_key == member.public_key {
                        let mut record = introduced_record(
                            member,
                            &list.issuer_device_id,
                            &list.introduction_id,
                            &me,
                            now_ms,
                            &all[*at],
                            body,
                        );
                        strip_untrusted_admit(&chain, &mut record);
                        registry.devices.push(record);
                        added.push(id.clone());
                    }
                    continue;
                }
                if let Some(&at) = accepted
                    .iter()
                    .find(|&&i| list.grant_pairs[i].covers_member(member))
                {
                    registry.devices.push(introduced_record_from_pair(
                        member,
                        &list.issuer_device_id,
                        &list.introduction_id,
                        &me,
                        now_ms,
                        &list.grant_pairs[at],
                    ));
                    added.push(id.clone());
                }
            }
            Ok(added)
        })
    }

    /// Apply an introduction received from `channel_peer_device_id`. Returns the new record.
    ///
    /// Refused: an issuer that is not an active paired member (`UnknownDevice`, `Revoked`), a
    /// message for another recipient or relayed by another peer (`NotAuthorized`), a bad
    /// signature, an expired or future one, the same admission seen again (`Replay`), and a device
    /// this one has revoked (`Revoked`: a re-admission needs a new pairing). The member is added
    /// only when a batch signed by a trusted owner-check key carries a body for exactly that device
    /// and key (`NotAuthorized` otherwise), as for a member list.
    pub fn apply_introduction(
        &self,
        channel_peer_device_id: &str,
        intro: &Introduction,
        now_ms: u64,
    ) -> Result<PairedDevice, LinkError> {
        check_version(intro.schema_version)?;
        unhex::<16>(&intro.introduction_id)?;
        check_member(&intro.member)?;
        check_short_window(intro.issued_at_ms, intro.expires_at_ms)?;
        if intro.endorsements.len() > CIRCLE_MAX_BATCHES
            || intro.grant_pairs.len() > CIRCLE_MAX_PAIRS
        {
            return Err(LinkError::Malformed("circle introduction too long"));
        }
        self.check_issuer(
            channel_peer_device_id,
            &intro.issuer_device_id,
            &intro.recipient_device_id,
            (intro.issued_at_ms, intro.expires_at_ms),
            &intro.signing_input(),
            &intro.signature,
            now_ms,
        )?;
        let me = self.identity().device_id().to_string();
        let id = intro.member.device.device_id.clone();
        if id == me {
            return Err(LinkError::Malformed("introduced to itself"));
        }
        self.mutate(|registry| {
            if let Some(existing) = registry.get(&id) {
                // A revoked record is terminal. An active one means the same admission arrived
                // again (a retry or a replay) or the device is already a member another way.
                return Ok(Err(if existing.is_unrevoked() {
                    LinkError::Replay
                } else {
                    LinkError::Revoked
                }));
            }
            let all = with_pair_batches(&intro.endorsements, &intro.grant_pairs);
            let chain = trust_chain(registry, &all, now_ms);
            let (accepted, conflicts) = accept_pairs(registry, &chain, &intro.grant_pairs, now_ms);
            reject_conflicts(registry, &conflicts, now_ms, &intro.issuer_device_id);
            note_redemptions(registry, &intro.grant_pairs, &accepted, now_ms);
            let record = if let Some((body, at)) = chain
                .bodies
                .get(id.as_str())
                .filter(|(body, _)| body.public_key == intro.member.public_key)
            {
                let mut record = introduced_record(
                    &intro.member,
                    &intro.issuer_device_id,
                    &intro.introduction_id,
                    &me,
                    now_ms,
                    &all[*at],
                    body,
                );
                strip_untrusted_admit(&chain, &mut record);
                record
            } else if let Some(&at) = accepted
                .iter()
                .find(|&&i| intro.grant_pairs[i].covers_member(&intro.member))
            {
                introduced_record_from_pair(
                    &intro.member,
                    &intro.issuer_device_id,
                    &intro.introduction_id,
                    &me,
                    now_ms,
                    &intro.grant_pairs[at],
                )
            } else {
                // Persist a conflict's revocations; the introduction itself adds nobody.
                return Ok(Err(LinkError::NotAuthorized));
            };
            registry.devices.push(record.clone());
            Ok(Ok(record))
        })?
    }

    /// Apply a circle removal received from `channel_peer_device_id`. The issuer must be an active
    /// member. Only a circle member (one that was introduced) can be removed this way: a device
    /// this computer paired with directly changes only by its own owner here, or by the existing
    /// signed [`super::RevokeMessage`]. Even then the issuer must be the member that introduced the
    /// record, or the one whose trusted owner-check key signed the record's endorsement batch, or
    /// the target itself; another member's removal is ignored. Returns the revoked record, or
    /// `None` when there was nothing to do (unknown, already revoked, not covered by the issuer, or
    /// the message predates the record).
    pub fn apply_circle_revoke(
        &self,
        channel_peer_device_id: &str,
        message: &CircleRevoke,
        now_ms: u64,
    ) -> Result<Option<PairedDevice>, LinkError> {
        check_version(message.schema_version)?;
        self.check_issuer(
            channel_peer_device_id,
            &message.issuer_device_id,
            &message.recipient_device_id,
            (message.issued_at_ms, message.expires_at_ms),
            &message.signing_input(),
            &message.signature,
            now_ms,
        )?;
        self.mutate(|registry| {
            // Revoke authority comes only from a check key bound to the issuer's own device.
            let issuer_key = registry
                .get(&message.issuer_device_id)
                .filter(|r| r.admit)
                .and_then(|r| {
                    r.admit_check_key
                        .clone()
                        .filter(|k| k.bound_or_log(&r.device.device_id, &r.public_key))
                });
            let Some(target) = registry.get_mut(&message.target_device_id) else {
                return Ok(None);
            };
            let self_removal = message.issuer_device_id == message.target_device_id;
            // The issuer introduced this record, or its trusted owner-check key signed the
            // batch that endorsed it (the endorsement covers it). Anyone else's removal is void.
            let endorsed_by = target
                .endorsement
                .as_ref()
                .map(|b| &b.sig.check)
                .or_else(|| target.grant_pair.as_ref().map(|p| &p.batch.sig.check));
            let covered = target.introduced_by.as_deref()
                == Some(message.issuer_device_id.as_str())
                || issuer_key.is_some() && endorsed_by == issuer_key.as_ref();
            // A cloud guest is the one record any `admit` member may remove, whether this
            // computer minted it (a direct record) or learned of it by introduction: the issuer
            // already passed `check_issuer` (active, `admit`, signed, inside the window), and the
            // pairing-time check below stops a replay of an older removal. Computers and phones
            // keep the narrower rule above.
            let guest = target.guest.is_some();
            if !target.is_unrevoked()
                || (target.introduced_by.is_none() && !self_removal && !guest)
                || (!self_removal && !covered && !guest)
                || message.issued_at_ms.saturating_add(CLOCK_SKEW_MS) < target.paired_at_ms
            {
                return Ok(None);
            }
            revoke_record(target, now_ms, &message.issuer_device_id);
            Ok(Some(target.clone()))
        })
    }
}

/// A directly paired computer this owner approved with an owner check (its endorsement body said
/// `can_admit` false only because no bound key existed then) that now has a bound key on its record
/// and no `admit`: the next owner-check batch endorses it with `can_admit` true. A PIN-approved
/// record has no endorsement and never qualifies; neither does one the owner took `admit` from.
pub(super) fn awaiting_admit(record: &PairedDevice) -> bool {
    !record.admit
        && !record.admit_denied
        && record.introduced_by.is_none()
        && record.grant_pair.is_none()
        && can_admit(record.device.platform)
        && record
            .admit_check_key
            .as_ref()
            .is_some_and(|k| k.bound_to(&record.device.device_id, &record.public_key))
        && record
            .endorsement
            .as_ref()
            .and_then(|b| b.body_for(&record.device.device_id))
            .is_some_and(|b| !b.can_admit && b.public_key == record.public_key)
}

/// Apply this computer's own freshly signed `batch`, whose `body` re-endorses `record` with
/// `can_admit`: the record gets `admit` only when it was awaiting it with exactly that key.
fn upgrade_endorsed(record: &mut PairedDevice, batch: &EndorsementBatch, body: &EndorsementBody) {
    if body.can_admit
        && body.public_key == record.public_key
        && body.member_check.is_some()
        && body.member_check == record.admit_check_key
        && awaiting_admit(record)
    {
        record.admit = true;
        record.endorsement = Some(batch.clone());
        record.revision += 1;
    }
}

/// Store `check` (already verified bound to this record's device) on `record`. `admit` is set only
/// when this computer's own pairing saw the very same key sign the owner-check approval
/// (`pairing_check`). Returns whether anything changed.
fn adopt_check_key(record: &mut PairedDevice, check: &CheckKey) -> bool {
    if record.admit_denied
        || !can_admit(record.device.platform)
        || !check.bound_to(&record.device.device_id, &record.public_key)
    {
        return false;
    }
    let mut changed = false;
    if record
        .admit_check_key
        .as_ref()
        .is_some_and(|have| have != check)
    {
        return false;
    }
    let held = record
        .admit_check_key
        .as_ref()
        .is_some_and(|k| k.bound_to(&record.device.device_id, &record.public_key));
    if !held {
        record.admit_check_key = Some(check.clone());
        changed = true;
    }
    if !record.admit && record.pairing_check.as_ref() == Some(check) {
        record.admit = true;
        changed = true;
    }
    if changed {
        record.revision += 1;
    }
    changed
}

impl DeviceLink {
    /// This computer's bound owner-check key, signed by its device key for `recipient_device_id`.
    /// `None` when this process holds no owner-check key bound to this device.
    pub fn make_check_key_update(
        &self,
        recipient_device_id: &str,
        now_ms: u64,
    ) -> Result<Option<CheckKeyUpdate>, LinkError> {
        self.tick(now_ms)?;
        let Some((device, public_key, binding)) = approval_sig::engine_owner_check_key() else {
            return Ok(None);
        };
        let check = CheckKey {
            device,
            public_key,
            binding,
        };
        let me = self.identity().device_id();
        if !check.bound_to(me, &self.identity().public_key_hex()) {
            return Ok(None);
        }
        let mut message = CheckKeyUpdate {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            device_id: me.to_string(),
            recipient_device_id: recipient_device_id.to_string(),
            check,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(CIRCLE_MESSAGE_TTL_MS),
            signature: String::new(),
        };
        message.signature = self.identity().sign(&message.signing_input());
        Ok(Some(message))
    }

    /// Apply a bound-key update received on the authenticated direct link from
    /// `channel_peer_device_id`. The sender must be that peer, an active paired computer; the
    /// message names this device, is inside its window and verifies under the peer's registered
    /// device key; the key's binding must name that peer's device id and key. The key is stored on
    /// the peer's record; `admit` follows only from an owner-check approval this device already
    /// holds for that very key. Returns whether the record changed.
    pub fn apply_check_key_update(
        &self,
        channel_peer_device_id: &str,
        message: &CheckKeyUpdate,
        now_ms: u64,
    ) -> Result<bool, LinkError> {
        check_version(message.schema_version)?;
        self.tick(now_ms)?;
        if message.recipient_device_id != self.identity().device_id()
            || message.device_id != channel_peer_device_id
        {
            return Err(LinkError::NotAuthorized);
        }
        check_window(message.issued_at_ms, message.expires_at_ms, now_ms)?;
        check_key_shape(&message.check)?;
        {
            let registry = self.registry()?;
            let record = registry
                .get(&message.device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !record.is_active() {
                return Err(LinkError::Revoked);
            }
            let key = unhex::<32>(&record.public_key)?;
            verify(
                &key,
                &message.signing_input(),
                &unhex::<64>(&message.signature)?,
            )?;
            if !can_admit(record.device.platform)
                || !message
                    .check
                    .bound_or_log(&record.device.device_id, &record.public_key)
            {
                return Err(LinkError::NotAuthorized);
            }
            if !adopt_check_key(&mut record.clone(), &message.check) {
                return Ok(false);
            }
        }
        self.mutate(|registry| {
            let record = registry
                .get_mut(&message.device_id)
                .ok_or(LinkError::UnknownDevice)?;
            Ok(adopt_check_key(record, &message.check))
        })
    }
}

/// The record of a device this one learned of through `introducer`, on the strength of `batch`
/// (the trusted batch that carried `body`). No grants, no peer management, confirmed (the sweep
/// never revokes it), and the derived circle pairing id. It may speak for the circle here only
/// when the body says `can_admit` and it is a computer.
fn introduced_record(
    member: &CircleMember,
    introducer: &str,
    introduction_id: &str,
    me: &str,
    now_ms: u64,
    batch: &EndorsementBatch,
    body: &EndorsementBody,
) -> PairedDevice {
    let admit = body.can_admit
        && can_admit(member.device.platform)
        && body
            .member_check
            .as_ref()
            .is_some_and(|k| k.bound_or_log(&member.device.device_id, &member.public_key));
    PairedDevice {
        schema_version: DEVICE_LINK_SCHEMA_VERSION,
        pairing_id: circle_pairing_id(me, &member.device.device_id),
        device: member.device.clone(),
        public_key: member.public_key.clone(),
        paired_at_ms: now_ms,
        revision: 1,
        state: PairedState::Active,
        grants: Vec::new(),
        grants_expire_at_ms: None,
        manage_peers: false,
        transport_id: None,
        peer_tls_pin: None,
        peer_link_port: None,
        confirmed_ms: Some(now_ms.max(1)),
        introduced_by: Some(introducer.to_string()),
        introduction_id: Some(introduction_id.to_string()),
        relay: member.relay.clone(),
        // Whether it may speak for the circle here: a computer endorsed with its own owner-check
        // key by a key this device trusts.
        admit,
        admit_check_key: body.member_check.clone().filter(|_| admit),
        // Kept so this device can list the member, with the batch that endorsed it.
        endorsement: Some(batch.clone()),
        grant_pair: None,
        admit_denied: false,
        pairing_check: None,
        guest: None,
        role: Default::default(),
    }
}

/// The record of a device this one learned of through `introducer` on the strength of an accepted
/// [`GrantPair`] (a join token or QR). Like [`introduced_record`], and never `admit`: a grant does
/// not confer `can_admit`.
fn introduced_record_from_pair(
    member: &CircleMember,
    introducer: &str,
    introduction_id: &str,
    me: &str,
    now_ms: u64,
    pair: &GrantPair,
) -> PairedDevice {
    let mut record = introduced_record(
        member,
        introducer,
        introduction_id,
        me,
        now_ms,
        &pair.batch,
        &EndorsementBody {
            device_id: member.device.device_id.clone(),
            public_key: member.public_key.clone(),
            member_check: None,
            can_admit: false,
            issued_at_ms: now_ms,
        },
    );
    record.endorsement = None;
    record.grant_pair = Some(pair.clone());
    // One of the owner's cloud agents: held to its lease here too, never `admit`, never a
    // phone, and refused device management like on the computer that minted its link.
    // The class and the lease come from the signed redemption, never from the courier's field.
    if pair.redemption.is_guest() {
        let signed = pair.redemption.lease_expires_at_ms;
        let lease = member.cloud_lease_ms.map_or(signed, |own| own.min(signed));
        record.device.platform = rightkit_device_link_types::DevicePlatform::Linux;
        record.admit = false;
        record.admit_check_key = None;
        record.admit_denied = true;
        record.guest = Some(super::registry::GuestPass {
            minted_by: introducer.to_string(),
            approved_ms: Some(now_ms.max(1)),
            // Never longer than the longest lease this computer would mint itself.
            lease_expires_ms: lease.min(now_ms.saturating_add(super::registry::GUEST_LEASE_MAX_MS)),
            idle_ms: super::registry::GUEST_IDLE_MS,
            last_seen_ms: None,
            check_code: String::new(),
            reported_session: None,
        });
    }
    record
}
