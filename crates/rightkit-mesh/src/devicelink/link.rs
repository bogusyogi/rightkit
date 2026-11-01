//! The device-link service: registry changes, revocation, resume tickets and the
//! per-request capability check.
//!
//! Authority rules in one place:
//! * Pairing grants nothing. A new record has no grants.
//! * A grant names the effect classes a device may REQUEST, optionally until an expiry. It
//!   never replaces the owner's approval of an irreversible class (`ApprovalBinding`).
//! * Every request is signed by the device key and is checked against the registry file as
//!   it is now, not as it was at pairing or at resume. State, pairing id, expiry and grant
//!   are rechecked on each request.
//! * A resume ticket proves "this device has paired with me and has not been revoked"; it
//!   carries no grant.
//! * Grants and `manage_peers` change only through local calls here. No link message sets
//!   them.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use super::circle::{CheckKeyExt, EndorsementBatchExt};
use super::identity::{DeviceIdentity, device_id_for_key, verify};
use super::pairing::{HostPairing, PairingOutcome};
use super::registry::{PairedDevice, PairedState, Registry, RegistryStore};
use super::wire::{Enc, effect_tag, random, unhex};
use super::{
    CLOCK_SKEW_MS, DEVICE_LINK_SCHEMA_VERSION, LinkError, PAIRING_START_WINDOW_MS,
    PAIRING_STARTS_PER_WINDOW, PairOffer, REQUEST_MAX_WINDOW_MS, RESUME_CHALLENGE_TTL_MS,
    RESUME_TTL_DEFAULT_MS, RESUME_TTL_MAX_MS,
};
use rightkit_crypto::{ct_eq, sha256};
use rightkit_device_link_types::PRINCIPAL_SCHEMA_VERSION;
use rightkit_device_link_types::{DeviceRef, EffectClass, Principal, PrincipalKind};
use rightkit_fs::digest::hex;

const TICKET_DOMAIN: &str = "pulse-link/resume-ticket/v1";
const PROOF_DOMAIN: &str = "pulse-link/resume-proof/v1";
const REVOKE_DOMAIN: &str = "pulse-link/revoke/v1";
const REQUEST_DOMAIN: &str = "pulse-link/request/v1";
const REQUEST_BODY_DOMAIN: &str = "pulse-link/request-body/v1";

/// The challenge table never grows past this many live entries.
const MAX_LIVE_ENTRIES: usize = 4096;

fn check_version(version: u32) -> Result<(), LinkError> {
    if version == DEVICE_LINK_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(LinkError::SchemaVersion(version))
    }
}

/// The C0 principal for a paired device: a grant holder, never the owner. `subject` is the
/// device id and `connection` the pairing id.
pub fn principal_for(record: &PairedDevice) -> Principal {
    Principal {
        schema_version: PRINCIPAL_SCHEMA_VERSION,
        kind: PrincipalKind::PairedDevice,
        subject: record.device.device_id.clone(),
        connection: Some(record.pairing_id.clone()),
    }
}

// ---- request replay ----------------------------------------------------------------------

/// What a request id left behind. The journal stores one row per accepted or refused request
/// (`pulse_link_requests`), written in the same transaction that commits the task's acceptance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayEntry {
    pub request_id: String,
    pub device_id: String,
    pub pairing_id: String,
    /// SHA-256 of the signed request: its canonical signing bytes and its signature.
    pub body_sha256: [u8; 32],
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

/// How a request ended, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayOutcome {
    Accepted {
        task_id: String,
    },
    /// Refused after authorization (for example a missing owner approval); `reason` is the
    /// stable refusal code, never request content.
    Refused {
        reason: String,
    },
}

/// A stored row, as the store returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayRecord {
    pub entry: ReplayEntry,
    pub outcome: ReplayOutcome,
}

/// Persistent request-id memory, supplied by the engine (journal-backed). Replaces the
/// in-memory table and the start-time floor: a lease is at-least-once, so the same signed
/// request arrives again after a reconnect or a restart, and the duplicate must get the
/// original answer, not a refusal.
///
/// This trait is READ ONLY on purpose. A row is inserted by the caller that commits the
/// acceptance, in the same transaction as the task, from [`ReplayDisposition::Fresh`]. Two
/// concurrent deliveries can both see `Fresh`; the store's unique key on `request_id` makes the
/// second insert fail, and that caller must then return the row the first one wrote.
///
/// An error fails the request closed ([`LinkError::ReplayStore`]); never answer "not seen"
/// when the store could not be read. A store should drop rows once
/// `expires_at_ms + CLOCK_SKEW_MS` has passed; `authorize` refuses an expired request anyway.
pub trait ReplayStore: Send + Sync {
    fn lookup(&self, request_id: &str) -> Result<Option<ReplayRecord>, LinkError>;
}

/// What `authorize` concluded about the request id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayDisposition {
    /// First sight. Commit the task and insert this entry in one transaction.
    Fresh(ReplayEntry),
    /// The same signed request was already handled. Answer with this outcome; do not run it
    /// again.
    Duplicate(ReplayOutcome),
}

/// Resume ticket, issued by this device to a paired peer and signed with this device's key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeTicket {
    pub schema_version: u32,
    /// 16 random bytes, hex.
    pub ticket_id: String,
    pub pairing_id: String,
    pub holder_device_id: String,
    pub issuer_device_id: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub signature: String,
}

impl ResumeTicket {
    fn signing_input(&self) -> Vec<u8> {
        Enc::new(TICKET_DOMAIN)
            .str(&self.ticket_id)
            .str(&self.pairing_id)
            .str(&self.holder_device_id)
            .str(&self.issuer_device_id)
            .u64(self.issued_at_ms)
            .u64(self.expires_at_ms)
            .finish()
    }
}

/// Fresh, single-use challenge the verifier sends to a device that wants to resume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeChallenge {
    pub schema_version: u32,
    pub challenge_id: String,
    pub nonce: String,
    pub expires_at_ms: u64,
}

/// The holder's answer: the ticket plus a signature over the challenge. A stolen ticket
/// alone cannot resume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeProof {
    pub schema_version: u32,
    pub ticket: ResumeTicket,
    pub challenge_id: String,
    pub signature: String,
}

impl ResumeProof {
    /// Holder side: answer `challenge` for `ticket`.
    pub fn answer(
        identity: &DeviceIdentity,
        ticket: &ResumeTicket,
        challenge: &ResumeChallenge,
    ) -> Self {
        Self {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            ticket: ticket.clone(),
            challenge_id: challenge.challenge_id.clone(),
            signature: identity.sign(&proof_input(
                &ticket.ticket_id,
                &challenge.challenge_id,
                &challenge.nonce,
                &ticket.issuer_device_id,
            )),
        }
    }
}

fn proof_input(ticket_id: &str, challenge_id: &str, nonce: &str, issuer: &str) -> Vec<u8> {
    Enc::new(PROOF_DOMAIN)
        .str(ticket_id)
        .str(challenge_id)
        .str(nonce)
        .str(issuer)
        .finish()
}

/// A successful resume. It carries no grant; each request is still checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumedSession {
    pub device: DeviceRef,
    pub principal: Principal,
    /// A new ticket with a fresh expiry, so a device that keeps connecting never re-pairs.
    pub next_ticket: ResumeTicket,
}

/// Signed revoke message. Propagates a revocation to a device that was offline or is not the
/// one the owner used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeMessage {
    pub schema_version: u32,
    pub issuer_device_id: String,
    pub target_device_id: String,
    /// A message issued before the receiver's current pairing with the target is ignored, so
    /// an old revoke cannot be replayed against a device that has since been paired again.
    pub issued_at_ms: u64,
    pub signature: String,
}

impl RevokeMessage {
    fn signing_input(&self) -> Vec<u8> {
        Enc::new(REVOKE_DOMAIN)
            .str(&self.issuer_device_id)
            .str(&self.target_device_id)
            .u64(self.issued_at_ms)
            .finish()
    }
}

/// One signed request for an effect on this device. Binds the same fields as
/// `ApprovalBinding` (task, exact-arguments hash, destination, effect), so the check here and
/// the owner's approval refer to the same act.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRequest {
    pub schema_version: u32,
    pub device_id: String,
    pub pairing_id: String,
    /// 16 random bytes, hex; each id is accepted once.
    pub request_id: String,
    pub effect: EffectClass,
    pub task_id: String,
    /// `sha256:<64 lowercase hex>`, as in `ApprovalBinding.args_hash`.
    pub args_hash: String,
    pub destination: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub signature: String,
}

impl LinkRequest {
    /// Requesting device: build and sign a request valid for `ttl_ms` (capped by
    /// [`REQUEST_MAX_WINDOW_MS`] on the receiving side).
    #[allow(clippy::too_many_arguments)]
    pub fn sign(
        identity: &DeviceIdentity,
        pairing_id: &str,
        effect: EffectClass,
        task_id: &str,
        args_hash: &str,
        destination: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<Self, LinkError> {
        let mut request = Self {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            device_id: identity.device_id().to_string(),
            pairing_id: pairing_id.to_string(),
            request_id: hex(&random::<16>()?),
            effect,
            task_id: task_id.to_string(),
            args_hash: args_hash.to_string(),
            destination: destination.to_string(),
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
            signature: String::new(),
        };
        request.signature = identity.sign(&request.signing_input());
        Ok(request)
    }

    /// SHA-256 over the canonical signing bytes and the signature: two requests share it only
    /// if every signed field and the signature match.
    fn body_sha256(&self) -> [u8; 32] {
        sha256(
            &Enc::new(REQUEST_BODY_DOMAIN)
                .bytes(&self.signing_input())
                .str(&self.signature)
                .finish(),
        )
    }

    fn signing_input(&self) -> Vec<u8> {
        Enc::new(REQUEST_DOMAIN)
            .str(&self.device_id)
            .str(&self.pairing_id)
            .str(&self.request_id)
            .str(effect_tag(self.effect))
            .str(&self.task_id)
            .str(&self.args_hash)
            .str(&self.destination)
            .u64(self.issued_at_ms)
            .u64(self.expires_at_ms)
            .finish()
    }
}

/// A request that passed every check on this call. It is permission to ASK the engine, not
/// permission to act: an irreversible class still goes to the owner's approval check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedRequest {
    pub device: DeviceRef,
    pub principal: Principal,
    pub pairing_id: String,
    pub device_revision: u64,
    pub request_id: String,
    pub effect: EffectClass,
    pub task_id: String,
    pub args_hash: String,
    pub destination: String,
    /// `effect.is_irreversible()`: true means the owner's approval is still required.
    pub requires_owner_approval: bool,
    /// Fresh or duplicate. On `Duplicate` the other fields describe the same request again;
    /// act only on the stored outcome.
    pub replay: ReplayDisposition,
}

/// Registry owner on this device.
pub struct DeviceLink {
    identity: DeviceIdentity,
    store: RegistryStore,
    write: Mutex<()>,
    replay: Arc<dyn ReplayStore>,
    challenges: Mutex<BTreeMap<String, (String, u64)>>,
    pairing_starts: Mutex<Vec<u64>>,
    /// Newest `now_ms` any check has been given; see [`DeviceLink::tick`].
    clock_high_ms: AtomicU64,
}

impl DeviceLink {
    /// `replay` is the persistent request-id memory (the journal's table in the engine).
    pub fn new(
        identity: DeviceIdentity,
        store: RegistryStore,
        replay: Arc<dyn ReplayStore>,
    ) -> Self {
        Self {
            identity,
            store,
            write: Mutex::new(()),
            replay,
            challenges: Mutex::new(BTreeMap::new()),
            pairing_starts: Mutex::new(Vec::new()),
            clock_high_ms: AtomicU64::new(0),
        }
    }

    /// Refuse a `now_ms` that is more than [`CLOCK_SKEW_MS`] older than the newest one this
    /// service has seen: a rolled-back clock cannot revive an expired challenge, ticket or
    /// request. Records the new high-water mark otherwise.
    pub(super) fn tick(&self, now_ms: u64) -> Result<(), LinkError> {
        let seen = self.clock_high_ms.fetch_max(now_ms, Ordering::SeqCst);
        if now_ms.saturating_add(CLOCK_SKEW_MS) < seen {
            return Err(LinkError::ClockRolledBack);
        }
        Ok(())
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    pub fn registry(&self) -> Result<Registry, LinkError> {
        self.store.load()
    }

    pub fn device(&self, device_id: &str) -> Result<Option<PairedDevice>, LinkError> {
        Ok(self.store.load()?.get(device_id).cloned())
    }

    pub(super) fn mutate<R>(
        &self,
        change: impl FnOnce(&mut Registry) -> Result<R, LinkError>,
    ) -> Result<R, LinkError> {
        let _guard = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        self.store.update(change)
    }

    // ---- pairing -------------------------------------------------------------------------

    /// Start a pairing session as the host. Refused when too many sessions started recently:
    /// each session is one guess for an active attacker.
    ///
    /// `host_tls_pin` and `host_link_port` go into the offer and the transcript; see
    /// [`PairOffer`].
    pub fn begin_pairing(
        &self,
        host_tls_pin: [u8; 32],
        host_link_port: u16,
        now_ms: u64,
    ) -> Result<(HostPairing, PairOffer), LinkError> {
        self.tick(now_ms)?;
        {
            let mut starts = self
                .pairing_starts
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            starts.retain(|at| now_ms.saturating_sub(*at) < PAIRING_START_WINDOW_MS);
            if starts.len() >= PAIRING_STARTS_PER_WINDOW {
                return Err(LinkError::PairingRateLimited);
            }
            starts.push(now_ms);
        }
        HostPairing::start(&self.identity, host_tls_pin, host_link_port, now_ms, None)
    }

    /// [`Self::begin_pairing`] for a join token or QR whose owner check already signed its grant
    /// ([`super::circle::TokenGrant`]): the session takes the grant's pairing id, and its lifetime
    /// starts at the grant's `issued_at_ms`, so it ends exactly when the grant expires.
    pub fn begin_pairing_granted(
        &self,
        host_tls_pin: [u8; 32],
        host_link_port: u16,
        grant: &super::circle::TokenGrant,
        now_ms: u64,
    ) -> Result<(HostPairing, PairOffer), LinkError> {
        let id = unhex::<16>(&grant.pairing_id)?;
        if grant.admitter_device_id != self.identity.device_id() {
            return Err(LinkError::NotAuthorized);
        }
        if now_ms >= grant.expires_at_ms {
            return Err(LinkError::Expired);
        }
        self.tick(now_ms)?;
        {
            let mut starts = self
                .pairing_starts
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            starts.retain(|at| now_ms.saturating_sub(*at) < PAIRING_START_WINDOW_MS);
            if starts.len() >= PAIRING_STARTS_PER_WINDOW {
                return Err(LinkError::PairingRateLimited);
            }
            starts.push(now_ms);
        }
        HostPairing::start(
            &self.identity,
            host_tls_pin,
            host_link_port,
            grant.issued_at_ms,
            Some(id),
        )
    }

    /// Record a finished pairing (either side). Starts the record with NO grants. Pairing a
    /// device that is already in the registry (active or revoked) replaces its record with a
    /// new pairing, a higher revision and no grants.
    ///
    /// A [`PairingOutcome`] can only be built inside the pairing module: on the host side by
    /// [`HostPairing::approve`] (the owner's approval of the code, name and fingerprint), on
    /// the joiner side by `JoinerPairing::finish` (the user typed the code). So nothing is
    /// recorded Active without the approval of the owner of the recording device.
    pub fn complete_pairing(&self, outcome: &PairingOutcome) -> Result<PairedDevice, LinkError> {
        self.complete_pairing_with_host_proof(outcome, None)
    }

    /// [`Self::complete_pairing`] on the joiner side with what the host sent: the endorsement batch
    /// the host owner's owner-check signature made when it admitted THIS device. `admit` (and the
    /// trusted owner-check key) is set for the host only when the batch verifies, its request is
    /// this pairing's transcript hash, it carries a body for this device's own id and key, and the
    /// host is a computer. That key is the one thing this device verified itself: every later
    /// endorsement chains to it. No batch, or one that does not verify, still records the pairing,
    /// without `admit`.
    pub fn complete_pairing_with_host_proof(
        &self,
        outcome: &PairingOutcome,
        host_batch: Option<&super::circle::EndorsementBatch>,
    ) -> Result<PairedDevice, LinkError> {
        let key = unhex::<32>(&outcome.peer_key)?;
        if outcome.peer.device_id != device_id_for_key(&key)
            || outcome.peer.device_id == self.identity.device_id()
        {
            return Err(LinkError::Malformed("pairing outcome"));
        }
        unhex::<16>(&outcome.pairing_id)?;
        let host_check = host_batch.and_then(|batch| {
            (can_admit(outcome.peer.platform)
                && batch.sig.challenge == outcome.transcript_hash
                && batch.endorses(self.identity.device_id(), &self.identity.public_key_hex())
                && batch.verify(outcome.paired_at_ms).is_ok()
                && batch
                    .sig
                    .check
                    .bound_or_log(&outcome.peer.device_id, &outcome.peer_key))
            .then(|| batch.sig.check.clone())
        });
        // The host's approval verified for this pairing but its key carries no binding yet (a key
        // enrolled before bindings existed): remember the key, so it is trusted only if the host
        // later proves the binding (`DeviceLink::apply_check_key_update`).
        let pending_check = host_check.is_none().then(|| {
            host_batch.and_then(|batch| {
                (can_admit(outcome.peer.platform)
                    && batch.sig.challenge == outcome.transcript_hash
                    && batch.endorses(self.identity.device_id(), &self.identity.public_key_hex())
                    && batch.verify(outcome.paired_at_ms).is_ok())
                .then(|| batch.sig.check.clone())
            })
        });
        self.record_pairing(outcome, host_check, pending_check.flatten())
    }

    /// [`Self::complete_pairing`] on the joiner side for a join token or QR: the host sent the
    /// owner-check signed token grant (`batch`) and its device-key redemption of THIS device. The
    /// host's owner-check key is pinned (`admit`) only when [`super::circle::host_grant_check`]
    /// holds: the grant names this host and this pairing and the hash of the secret this device
    /// holds (`secret_hash`), the redemption names this device and is signed by the host's device
    /// key inside the grant's window, and the host is a computer. Otherwise the pairing is still
    /// recorded, without `admit`. `relay` is the token's relay (`https`, at most 256 characters,
    /// else dropped): kept on the host's record in the same write, so a joiner that loses
    /// `relay-learned.json` can still dial it after a restart. A dial hint, never trusted.
    pub fn complete_pairing_with_grant(
        &self,
        outcome: &PairingOutcome,
        batch: Option<&super::circle::EndorsementBatch>,
        redemption: Option<&super::circle::Redemption>,
        secret_hash: &str,
        relay: Option<&str>,
    ) -> Result<PairedDevice, LinkError> {
        let key = unhex::<32>(&outcome.peer_key)?;
        if outcome.peer.device_id != device_id_for_key(&key)
            || outcome.peer.device_id == self.identity.device_id()
        {
            return Err(LinkError::Malformed("pairing outcome"));
        }
        unhex::<16>(&outcome.pairing_id)?;
        let host_check = match (batch, redemption) {
            (Some(batch), Some(redemption)) if can_admit(outcome.peer.platform) => {
                super::circle::host_grant_check(
                    batch,
                    redemption,
                    self.identity.device_id(),
                    &self.identity.public_key_hex(),
                    &outcome.peer.device_id,
                    &outcome.peer_key,
                    &outcome.pairing_id,
                    secret_hash,
                    outcome.paired_at_ms,
                )
            }
            _ => None,
        };
        let relay = relay
            .map(str::trim)
            .filter(|r| {
                r.len() <= 256
                    && r.get(..8)
                        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
            })
            .map(str::to_string);
        self.record_pairing_as(outcome, host_check, None, None, relay)
    }

    /// The host side of a guest link: the redeeming device is recorded a pending guest in
    /// the same registry write that records it, so it never exists as a full member, even for a
    /// moment or after a crash.
    pub fn complete_pairing_guest(
        &self,
        outcome: &PairingOutcome,
        lease_ms: u64,
        now_ms: u64,
    ) -> Result<PairedDevice, LinkError> {
        // A cloud agent is a Linux machine whatever it reported (never a phone or an admitter), and
        // named `<name> (cloud)` on every computer that learns of it.
        let mut outcome = outcome.clone();
        outcome.peer.platform = rightkit_device_link_types::DevicePlatform::Linux;
        let name = outcome
            .peer
            .display_name
            .clone()
            .unwrap_or_else(|| "agent".to_string());
        if !name.ends_with(" (cloud)") {
            outcome.peer.display_name = Some(format!("{name} (cloud)"));
        }
        let outcome = &outcome;
        let pass = super::registry::GuestPass {
            minted_by: self.identity.device_id().to_string(),
            approved_ms: None,
            lease_expires_ms: now_ms
                .saturating_add(lease_ms.min(super::registry::GUEST_LEASE_MAX_MS)),
            idle_ms: super::registry::GUEST_IDLE_MS,
            last_seen_ms: None,
            check_code: super::registry::GuestPass::check_code(
                &outcome.pairing_id,
                &outcome.peer_key,
            ),
            reported_session: Some(name),
        };
        self.record_pairing_as(outcome, None, None, Some(pass), None)
    }

    fn record_pairing(
        &self,
        outcome: &PairingOutcome,
        host_check: Option<super::circle::CheckKey>,
        pairing_check: Option<super::circle::CheckKey>,
    ) -> Result<PairedDevice, LinkError> {
        self.record_pairing_as(outcome, host_check, pairing_check, None, None)
    }

    fn record_pairing_as(
        &self,
        outcome: &PairingOutcome,
        host_check: Option<super::circle::CheckKey>,
        pairing_check: Option<super::circle::CheckKey>,
        guest: Option<super::registry::GuestPass>,
        relay: Option<String>,
    ) -> Result<PairedDevice, LinkError> {
        let is_guest = guest.is_some();
        let _guard = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = self.store.load()?.get(&outcome.peer.device_id) {
            // The same pairing completed again (a retried confirmation): nothing changes and no
            // revision is spent. A revoked record of this pairing is never revived by replaying
            // its approval. A different pairing id is a new pairing and replaces the record.
            if ct_eq(
                existing.pairing_id.as_bytes(),
                outcome.pairing_id.as_bytes(),
            ) && ct_eq(existing.public_key.as_bytes(), outcome.peer_key.as_bytes())
            {
                return if existing.is_unrevoked() {
                    Ok(existing.clone())
                } else {
                    Err(LinkError::Revoked)
                };
            }
        }
        let recorded = self.store.update(|registry| {
            let prior = registry
                .get(&outcome.peer.device_id)
                .map_or(0, |record| record.revision);
            let record = PairedDevice {
                schema_version: DEVICE_LINK_SCHEMA_VERSION,
                pairing_id: outcome.pairing_id.clone(),
                device: outcome.peer.clone(),
                public_key: outcome.peer_key.clone(),
                paired_at_ms: outcome.paired_at_ms,
                revision: prior + 1,
                state: PairedState::Active,
                grants: Vec::new(),
                grants_expire_at_ms: None,
                manage_peers: false,
                transport_id: None,
                // Set on the Joiner side only, so it can dial the Host again after a restart.
                peer_tls_pin: outcome.peer_tls_pin,
                peer_link_port: outcome.peer_link_port,
                // Unconfirmed until the first delivery or resume completes (`confirm_pairing`).
                confirmed_ms: None,
                introduced_by: None,
                introduction_id: None,
                // Joiner side of a token join only: the token's relay (`complete_pairing_with_grant`).
                relay,
                // Never from the peer's declared platform: only a host whose owner-check signature
                // over this pairing verified (`host_check`), pinned to that key, or a host-side
                // record whose owner signed (`record_endorsement`), or an owner's `set_admit`.
                admit: host_check.is_some(),
                admit_check_key: host_check.clone(),
                // No batch endorses the host itself; this device signs one when it first admits a
                // device (`DeviceLink::plan_endorsements`).
                endorsement: None,
                grant_pair: None,
                admit_denied: is_guest,
                pairing_check,
                guest,
                role: Default::default(),
            };
            registry
                .devices
                .retain(|d| d.device.device_id != outcome.peer.device_id);
            registry.devices.push(record.clone());
            Ok(record)
        })?;
        audit(
            "link.paired",
            &recorded.device.device_id,
            serde_json::json!({ "guest": is_guest }),
        );
        Ok(recorded)
    }

    // ---- grants (local owner calls only) ---------------------------------------------------

    /// Replace a device's grants. `expected_revision` is the revision the caller read.
    pub fn set_grants(
        &self,
        device_id: &str,
        effects: &[EffectClass],
        expires_at_ms: Option<u64>,
        expected_revision: u64,
    ) -> Result<PairedDevice, LinkError> {
        let record = self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !record.is_active() {
                return Err(LinkError::Revoked);
            }
            if record.revision != expected_revision {
                return Err(LinkError::RevisionConflict {
                    have: expected_revision,
                    now: record.revision,
                });
            }
            let mut grants: Vec<EffectClass> = Vec::new();
            for effect in effects {
                if !grants.contains(effect) {
                    grants.push(*effect);
                }
            }
            record.grants = grants;
            record.grants_expire_at_ms = expires_at_ms;
            record.revision += 1;
            Ok(record.clone())
        })?;
        audit(
            "link.grants_set",
            device_id,
            serde_json::json!({ "count": record.grants.len(), "expires": expires_at_ms.is_some() }),
        );
        Ok(record)
    }

    /// Record the id the transport gave this device at enrolment (see
    /// [`PairedDevice::transport_id`]). Allowed on an active record whose `transport_id` is
    /// unset or equal; setting a different id is refused, so one pairing cannot be repointed at
    /// another daemon device.
    pub fn set_transport_id(
        &self,
        device_id: &str,
        transport_id: &str,
        expected_revision: u64,
    ) -> Result<PairedDevice, LinkError> {
        let valid = !transport_id.is_empty()
            && transport_id.len() <= 64
            && transport_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !valid {
            return Err(LinkError::Malformed("transport id"));
        }
        self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !record.is_active() {
                return Err(LinkError::Revoked);
            }
            if record.revision != expected_revision {
                return Err(LinkError::RevisionConflict {
                    have: expected_revision,
                    now: record.revision,
                });
            }
            match record.transport_id.as_deref() {
                Some(existing) if existing != transport_id => {
                    return Err(LinkError::Malformed("transport id already set"));
                }
                Some(_) => {}
                None => {
                    record.transport_id = Some(transport_id.to_string());
                    record.revision += 1;
                }
            }
            Ok(record.clone())
        })
    }

    /// Let a peer revoke other devices by signed message, or take that away.
    pub fn set_manage_peers(
        &self,
        device_id: &str,
        allowed: bool,
        expected_revision: u64,
    ) -> Result<PairedDevice, LinkError> {
        self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !record.is_active() {
                return Err(LinkError::Revoked);
            }
            if record.revision != expected_revision {
                return Err(LinkError::RevisionConflict {
                    have: expected_revision,
                    now: record.revision,
                });
            }
            record.manage_peers = allowed;
            record.revision += 1;
            Ok(record.clone())
        })
    }

    /// Grant or take away the circle's `admit` capability of `device_id` (an owner-checked call:
    /// the engine runs a signed owner check first, never a PIN). Only a Mac or Windows computer can hold it. The
    /// owner-check key this device verified for it stays: a grant without one vouches for nobody
    /// (only a batch signed by a trusted key adds a member), and taking `admit` away stops the key
    /// from being trusted.
    pub fn set_admit(&self, device_id: &str, allowed: bool) -> Result<PairedDevice, LinkError> {
        let record = self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !record.is_active() {
                return Err(LinkError::Revoked);
            }
            if allowed && (record.is_guest() || !can_admit(record.device.platform)) {
                return Err(LinkError::NotAuthorized);
            }
            record.admit = allowed;
            record.admit_denied = !allowed;
            record.revision += 1;
            Ok(record.clone())
        })?;
        audit(
            "link.admit_set",
            device_id,
            serde_json::json!({ "allowed": allowed }),
        );
        Ok(record)
    }

    /// Set what an active, non-guest member is: `Computer` or `Server`. A guest stays a guest
    /// and a phone is never a server. Local to this computer; no message carries it.
    pub fn set_role(
        &self,
        device_id: &str,
        role: super::registry::MemberRole,
    ) -> Result<PairedDevice, LinkError> {
        let record = self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !record.is_active() {
                return Err(LinkError::Revoked);
            }
            if record.is_guest()
                || role == super::registry::MemberRole::Guest
                || (role == super::registry::MemberRole::Server
                    && matches!(
                        record.device.platform,
                        rightkit_device_link_types::DevicePlatform::Ios
                            | rightkit_device_link_types::DevicePlatform::Android
                    ))
            {
                return Err(LinkError::NotAuthorized);
            }
            record.role = role;
            record.revision += 1;
            Ok(record.clone())
        })?;
        audit(
            "link.role_set",
            device_id,
            serde_json::json!({ "role": role.as_str() }),
        );
        Ok(record)
    }

    // ---- guests --------------------------------------------------------------------

    /// The owner approved the pending guest `device_id`: from now it may make the chat calls until
    /// its lease ends or it goes idle.
    pub fn approve_guest(&self, device_id: &str, now_ms: u64) -> Result<PairedDevice, LinkError> {
        let record = self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !matches!(record.state, PairedState::Active) {
                return Err(LinkError::Revoked);
            }
            let pass = record.guest.as_mut().ok_or(LinkError::NotAuthorized)?;
            // Only a pending guest inside its lease; a lapsed one needs a new link.
            if pass.approved_ms.is_some() || now_ms >= pass.lease_expires_ms {
                return Err(LinkError::NotAuthorized);
            }
            pass.approved_ms = Some(now_ms.max(1));
            record.revision += 1;
            Ok(record.clone())
        })?;
        audit("guest.approved", device_id, serde_json::json!({}));
        Ok(record)
    }

    /// Revoke every cloud agent whose lease ended or that has waited unapproved longer than
    /// `pending_ms`: a stale one is removed, never kept listed.
    /// Returns the revoked device ids.
    pub fn sweep_guests(&self, now_ms: u64, pending_ms: u64) -> Result<Vec<String>, LinkError> {
        // Nothing to look at: no write, no registry lock, no file created.
        if !self
            .registry()?
            .devices
            .iter()
            .any(|d| d.guest.is_some() && d.is_unrevoked())
        {
            return Ok(Vec::new());
        }
        let gone = self.mutate(|registry| {
            let mut gone = Vec::new();
            let mut why = Vec::new();
            for record in registry.devices.iter_mut() {
                let Some(pass) = record.guest.as_ref() else {
                    continue;
                };
                let stale = now_ms >= pass.lease_expires_ms
                    || (pass.approved_ms.is_none()
                        && now_ms.saturating_sub(record.paired_at_ms) > pending_ms);
                if stale && record.is_unrevoked() {
                    let unapproved = pass.approved_ms.is_none();
                    revoke_record(record, now_ms, "guest-lapsed");
                    gone.push(record.device.device_id.clone());
                    why.push(unapproved);
                }
            }
            Ok((gone, why))
        })?;
        for (id, unapproved) in gone.0.iter().zip(gone.1.iter()) {
            audit(
                "guest.lapsed",
                id,
                serde_json::json!({ "reason": if *unapproved { "unapproved" } else { "lease" } }),
            );
        }
        Ok(gone.0)
    }

    /// Every guest this computer holds that is not revoked: pending, approved or lapsed.
    pub fn guests(&self) -> Result<Vec<PairedDevice>, LinkError> {
        Ok(self
            .registry()?
            .devices
            .into_iter()
            .filter(|d| d.guest.is_some() && matches!(d.state, PairedState::Active))
            .collect())
    }

    // ---- revocation ----------------------------------------------------------------------

    /// Revoke a device on this computer. Terminal, idempotent, and effective on the next
    /// request: grants are cleared, resume tickets stop working, requests are refused.
    pub fn revoke_local(&self, device_id: &str, now_ms: u64) -> Result<PairedDevice, LinkError> {
        let (record, was_live) = self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            let was_live = record.is_unrevoked();
            revoke_record(record, now_ms, "local");
            Ok((record.clone(), was_live))
        })?;
        if was_live {
            audit(
                "link.revoked",
                device_id,
                serde_json::json!({ "by": "local" }),
            );
        }
        Ok(record)
    }

    /// Revoke `device_id` only while its record still belongs to the pairing `pairing_id` with
    /// key `public_key_hex` (decided under the registry lock). A record that now belongs to a
    /// newer pairing is left untouched and `Ok(None)` is returned, with no revision spent.
    pub fn revoke_local_if_pairing(
        &self,
        device_id: &str,
        pairing_id: &str,
        public_key_hex: &str,
        now_ms: u64,
    ) -> Result<Option<PairedDevice>, LinkError> {
        let result = self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !(ct_eq(record.pairing_id.as_bytes(), pairing_id.as_bytes())
                && ct_eq(record.public_key.as_bytes(), public_key_hex.as_bytes()))
            {
                // Aborts the update: nothing is saved.
                return Err(LinkError::UnknownDevice);
            }
            let was_live = record.is_unrevoked();
            revoke_record(record, now_ms, "local");
            Ok((record.clone(), was_live))
        });
        match result {
            Ok((record, was_live)) => {
                if was_live {
                    audit(
                        "link.revoked",
                        device_id,
                        serde_json::json!({ "by": "local", "pairing": "scoped" }),
                    );
                }
                Ok(Some(record))
            }
            Err(LinkError::UnknownDevice) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Mark the pairing `pairing_id` of `device_id` as delivered (see `PairedDevice::confirmed_ms`).
    /// A record of another pairing, or one already confirmed, is left untouched (no save).
    pub fn confirm_pairing(
        &self,
        device_id: &str,
        pairing_id: &str,
        now_ms: u64,
    ) -> Result<(), LinkError> {
        let result = self.mutate(|registry| {
            let record = registry
                .get_mut(device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if record.confirmed_ms.is_some()
                || !ct_eq(record.pairing_id.as_bytes(), pairing_id.as_bytes())
            {
                // Aborts the update: nothing is saved.
                return Err(LinkError::UnknownDevice);
            }
            record.confirmed_ms = Some(now_ms.max(1));
            Ok(())
        });
        match result {
            Err(LinkError::UnknownDevice) => Ok(()),
            other => other,
        }
    }

    /// Revoke every Active record whose pairing was never confirmed and is older than
    /// `window_ms` (its delivery never completed and its revoke timer is gone, for example
    /// after an engine restart). Each goes through `revoke_local_if_pairing` with its own
    /// pairing id and key. Returns how many were revoked.
    pub fn sweep_unconfirmed(&self, now_ms: u64, window_ms: u64) -> Result<usize, LinkError> {
        let stale: Vec<(String, String, String)> = self
            .registry()?
            .devices
            .iter()
            .filter(|d| {
                d.is_unrevoked()
                    && d.confirmed_ms.is_none()
                    && now_ms.saturating_sub(d.paired_at_ms) > window_ms
            })
            .map(|d| {
                (
                    d.device.device_id.clone(),
                    d.pairing_id.clone(),
                    d.public_key.clone(),
                )
            })
            .collect();
        let mut revoked = 0;
        for (device_id, pairing_id, key) in stale {
            if self
                .revoke_local_if_pairing(&device_id, &pairing_id, &key, now_ms)?
                .is_some()
            {
                revoked += 1;
            }
        }
        Ok(revoked)
    }

    /// Sign a revoke for `target_device_id` to send to other devices (or, with this device's
    /// own id as the target, to tell peers "stop trusting me").
    pub fn make_revoke(&self, target_device_id: &str, now_ms: u64) -> RevokeMessage {
        let mut message = RevokeMessage {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            issuer_device_id: self.identity.device_id().to_string(),
            target_device_id: target_device_id.to_string(),
            issued_at_ms: now_ms,
            signature: String::new(),
        };
        message.signature = self.identity.sign(&message.signing_input());
        message
    }

    /// Apply a signed revoke received from a peer. The issuer must be an active paired device
    /// and either the target itself or a peer with `manage_peers`. Returns the revoked record,
    /// or `None` when there was nothing to do (unknown target, already revoked, or the message
    /// predates the current pairing with the target).
    pub fn apply_revoke(
        &self,
        message: &RevokeMessage,
        now_ms: u64,
    ) -> Result<Option<PairedDevice>, LinkError> {
        check_version(message.schema_version)?;
        let revoked = self.mutate(|registry| {
            let issuer = registry
                .get(&message.issuer_device_id)
                .ok_or(LinkError::UnknownDevice)?;
            if !issuer.is_active() {
                return Err(LinkError::Revoked);
            }
            let issuer_key = unhex::<32>(&issuer.public_key)?;
            let signature = unhex::<64>(&message.signature)?;
            verify(&issuer_key, &message.signing_input(), &signature)?;
            let self_revoke = message.issuer_device_id == message.target_device_id;
            if !self_revoke && !issuer.manage_peers {
                return Err(LinkError::NotAuthorized);
            }
            if message.issued_at_ms > now_ms.saturating_add(CLOCK_SKEW_MS) {
                return Err(LinkError::Malformed("revoke issued in the future"));
            }
            let Some(target) = registry.get_mut(&message.target_device_id) else {
                return Ok(None);
            };
            if !target.is_unrevoked() || message.issued_at_ms < target.paired_at_ms {
                return Ok(None);
            }
            revoke_record(target, now_ms, &message.issuer_device_id);
            Ok(Some(target.clone()))
        })?;
        if let Some(record) = &revoked {
            audit(
                "link.revoked",
                &record.device.device_id,
                serde_json::json!({
                    "by": "peer",
                    "self": message.issuer_device_id == message.target_device_id,
                }),
            );
        }
        Ok(revoked)
    }

    // ---- resume --------------------------------------------------------------------------

    /// Issue a ticket to an active paired device. `ttl_ms` is clamped to
    /// [`RESUME_TTL_MAX_MS`]. Give the ticket to the peer over an authenticated session
    /// (right after pairing, or in [`ResumedSession::next_ticket`]).
    pub fn issue_resume_ticket(
        &self,
        device_id: &str,
        ttl_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<ResumeTicket, LinkError> {
        self.tick(now_ms)?;
        let registry = self.store.load()?;
        let record = registry.get(device_id).ok_or(LinkError::UnknownDevice)?;
        if !record.is_active() {
            return Err(LinkError::Revoked);
        }
        let ttl = ttl_ms
            .unwrap_or(RESUME_TTL_DEFAULT_MS)
            .clamp(1, RESUME_TTL_MAX_MS);
        let mut ticket = ResumeTicket {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            ticket_id: hex(&random::<16>()?),
            pairing_id: record.pairing_id.clone(),
            holder_device_id: record.device.device_id.clone(),
            issuer_device_id: self.identity.device_id().to_string(),
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(ttl),
            signature: String::new(),
        };
        ticket.signature = self.identity.sign(&ticket.signing_input());
        Ok(ticket)
    }

    /// A device asks to resume: send it a fresh single-use challenge.
    pub fn resume_challenge(&self, now_ms: u64) -> Result<ResumeChallenge, LinkError> {
        self.tick(now_ms)?;
        let challenge = ResumeChallenge {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            challenge_id: hex(&random::<16>()?),
            nonce: hex(&random::<32>()?),
            expires_at_ms: now_ms.saturating_add(RESUME_CHALLENGE_TTL_MS),
        };
        let mut table = self
            .challenges
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        table.retain(|_, (_, expires)| *expires > now_ms);
        if table.len() >= MAX_LIVE_ENTRIES {
            return Err(LinkError::Busy);
        }
        table.insert(
            challenge.challenge_id.clone(),
            (challenge.nonce.clone(), challenge.expires_at_ms),
        );
        Ok(challenge)
    }

    /// Check a resume proof. The challenge is consumed first, so a wrong answer burns it.
    pub fn verify_resume(
        &self,
        proof: &ResumeProof,
        now_ms: u64,
    ) -> Result<ResumedSession, LinkError> {
        check_version(proof.schema_version)?;
        self.tick(now_ms)?;
        let entry = self
            .challenges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&proof.challenge_id);
        let Some((nonce, challenge_expires)) = entry else {
            return Err(LinkError::Replay);
        };
        if now_ms >= challenge_expires {
            return Err(LinkError::Expired);
        }
        let ticket = &proof.ticket;
        check_version(ticket.schema_version)?;
        if ticket.issuer_device_id != self.identity.device_id() {
            return Err(LinkError::Malformed("ticket issued by another device"));
        }
        let ticket_sig = unhex::<64>(&ticket.signature)?;
        verify(
            &self.identity.public_key(),
            &ticket.signing_input(),
            &ticket_sig,
        )?;
        if now_ms >= ticket.expires_at_ms {
            return Err(LinkError::Expired);
        }
        let registry = self.store.load()?;
        let record = registry
            .get(&ticket.holder_device_id)
            .ok_or(LinkError::UnknownDevice)?;
        if !record.is_active() {
            return Err(LinkError::Revoked);
        }
        if record.pairing_id != ticket.pairing_id {
            return Err(LinkError::PairingMismatch);
        }
        let holder_key = unhex::<32>(&record.public_key)?;
        let holder_sig = unhex::<64>(&proof.signature)?;
        verify(
            &holder_key,
            &proof_input(
                &ticket.ticket_id,
                &proof.challenge_id,
                &nonce,
                &ticket.issuer_device_id,
            ),
            &holder_sig,
        )?;
        let next_ticket = self.issue_resume_ticket(&record.device.device_id, None, now_ms)?;
        Ok(ResumedSession {
            device: record.device.clone(),
            principal: principal_for(record),
            next_ticket,
        })
    }

    // ---- per-request check ---------------------------------------------------------------

    /// Check one signed request against the registry as it is now. Any failure refuses the
    /// request; success is permission to ask, not permission to act.
    ///
    /// Order: version, device state, pairing, signature, shape, then the replay store. A known
    /// request id with the same signed bytes returns `Ok` with
    /// [`ReplayDisposition::Duplicate`] before the time-window and grant checks, because the
    /// answer already exists and grants nothing new (a redelivery after the window or after
    /// grants changed still learns its original task id); a revoked or unknown device is
    /// refused before that. The same id with different bytes, or another device's id, is
    /// [`LinkError::Replay`].
    pub fn authorize(
        &self,
        request: &LinkRequest,
        now_ms: u64,
    ) -> Result<AuthorizedRequest, LinkError> {
        check_version(request.schema_version)?;
        self.tick(now_ms)?;
        let registry = self.store.load()?;
        let record = registry
            .get(&request.device_id)
            .ok_or(LinkError::UnknownDevice)?;
        if !record.is_active() {
            return Err(LinkError::Revoked);
        }
        if record.pairing_id != request.pairing_id {
            return Err(LinkError::PairingMismatch);
        }
        let key = unhex::<32>(&record.public_key)?;
        let signature = unhex::<64>(&request.signature)?;
        verify(&key, &request.signing_input(), &signature)?;

        unhex::<16>(&request.request_id)?;
        check_args_hash(&request.args_hash)?;
        let entry = ReplayEntry {
            request_id: request.request_id.clone(),
            device_id: request.device_id.clone(),
            pairing_id: request.pairing_id.clone(),
            body_sha256: request.body_sha256(),
            issued_at_ms: request.issued_at_ms,
            expires_at_ms: request.expires_at_ms,
        };
        if let Some(stored) = self.replay.lookup(&request.request_id)? {
            if stored.entry.device_id != entry.device_id
                || stored.entry.pairing_id != entry.pairing_id
                || !ct_eq(&stored.entry.body_sha256, &entry.body_sha256)
            {
                return Err(LinkError::Replay);
            }
            return Ok(authorized(
                record,
                request,
                ReplayDisposition::Duplicate(stored.outcome),
            ));
        }
        if request.expires_at_ms <= now_ms {
            return Err(LinkError::Expired);
        }
        if request.issued_at_ms > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(LinkError::Malformed("request issued in the future"));
        }
        if request.expires_at_ms < request.issued_at_ms
            || request.expires_at_ms - request.issued_at_ms > REQUEST_MAX_WINDOW_MS
        {
            return Err(LinkError::Malformed("request window"));
        }
        if let Some(expires) = record.grants_expire_at_ms
            && now_ms >= expires
        {
            return Err(LinkError::GrantExpired);
        }
        if !record.grants.contains(&request.effect) {
            return Err(LinkError::NotGranted);
        }
        // Only authenticated, authorized requests reach the caller as `Fresh`, so only they
        // can occupy the store.
        Ok(authorized(record, request, ReplayDisposition::Fresh(entry)))
    }
}

fn authorized(
    record: &PairedDevice,
    request: &LinkRequest,
    replay: ReplayDisposition,
) -> AuthorizedRequest {
    AuthorizedRequest {
        device: record.device.clone(),
        principal: principal_for(record),
        pairing_id: record.pairing_id.clone(),
        device_revision: record.revision,
        request_id: request.request_id.clone(),
        effect: request.effect,
        task_id: request.task_id.clone(),
        args_hash: request.args_hash.clone(),
        destination: request.destination.clone(),
        requires_owner_approval: request.effect.is_irreversible(),
        replay,
    }
}

/// Only a computer can run the owner check (Touch ID, Windows Hello), so only a Mac or Windows
/// computer may hold the circle's `admit` capability.
pub(super) fn can_admit(platform: rightkit_device_link_types::DevicePlatform) -> bool {
    matches!(
        platform,
        rightkit_device_link_types::DevicePlatform::Mac
            | rightkit_device_link_types::DevicePlatform::Windows
    )
}

pub(super) fn revoke_record(record: &mut PairedDevice, now_ms: u64, by: &str) {
    if record.is_unrevoked() {
        record.state = PairedState::Revoked {
            at_ms: now_ms,
            by: by.to_string(),
        };
        record.grants.clear();
        record.grants_expire_at_ms = None;
        record.manage_peers = false;
        record.revision += 1;
    }
}

/// One security event about a device: the event code, the device's first eight characters
/// and fixed codes only; never a name, key, pairing id or anything a peer sent.
fn audit(event: &str, device_id: &str, extra: serde_json::Value) {
    let mut fields =
        serde_json::json!({ "device": rightkit_approvals::audit::short_id(device_id) });
    if let (Some(out), Some(more)) = (fields.as_object_mut(), extra.as_object()) {
        out.extend(more.clone());
    }
    rightkit_approvals::audit::record_security(event, fields);
}

/// `sha256:` followed by 64 lowercase hex characters.
fn check_args_hash(value: &str) -> Result<(), LinkError> {
    let valid = value.strip_prefix("sha256:").is_some_and(|digits| {
        digits.len() == 64
            && digits
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    });
    if valid {
        Ok(())
    } else {
        Err(LinkError::Malformed("args_hash"))
    }
}
