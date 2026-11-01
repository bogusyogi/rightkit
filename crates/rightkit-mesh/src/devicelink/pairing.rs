//! Pairing: a short code shown on one device and typed on the other, with no trust on first
//! use.
//!
//! The vetted PAKE crates (SPAKE2 and similar) are not in this workspace's lock file, so this
//! is a signed key exchange authenticated by a short authentication string, the construction
//! Bluetooth numeric comparison and ZRTP use:
//!
//! 1. Host sends [`PairOffer`] (its identity key, device reference, the SHA-256 pin of the TLS
//!    certificate authority it serves the link with, and its link port). The Joiner refuses an
//!    offer whose pin is not the certificate the offer actually arrived over.
//! 2. Joiner sends [`PairAccept`]: its key and a COMMITMENT to a random nonce `Nj`.
//! 3. Host sends [`PairHostNonce`] (`Nh`) only after it holds the commitment.
//! 4. Joiner sends [`PairReveal`] (`Nj`); Host checks it against the commitment.
//! 5. Both compute the transcript hash over both identities, both keys, the TLS pin and link
//!    port and both nonces, and from it the same eight-digit code. Host SHOWS it (as 4+4, see
//!    [`format_code`]). The user TYPES it on the Joiner. A man in the middle that put its own
//!    certificate in the offer changes the transcript, so the typed code fails: typing the
//!    right code also confirms the certificate.
//! 6. Joiner checks the typed code, then signs the transcript hash ([`PairConfirm`], role
//!    joiner). Host verifies the signature, which yields a [`PendingApproval`] and NOTHING
//!    else: no outcome, no reply.
//! 7. The Host's trusted UI shows the owner the joiner's name, device id and fingerprint and
//!    the same code. The owner confirms that they match the device in front of them. Only
//!    [`HostPairing::approve`], handed that exact pending value, yields the outcome and the
//!    Host's own confirmation (role host). Joiner verifies and records the device.
//! 8. Only after approval the Host calls [`HostPairing::enrol`] to build [`PairEnrol`], the
//!    single-use transport enrolment code, and sends it inside the same pinned channel. The
//!    Joiner takes it with [`JoinerPairing::receive_enrol`], which succeeds only after the
//!    Host's confirmation verified.
//!
//! The code is derived from the public transcript, so a device that is not the one the owner
//! holds can compute it and sign. The Joiner-side typed check therefore proves nothing to the
//! Host about WHO answered; step 7 does. Without it the Host would trust on first use
//! whichever device answered an open session.
//!
//! The nonce commitment makes the code unpredictable to a man in the middle at the moment it
//! must choose its keys, so one session gives it a single guess of 1 in 10^8. Any error voids
//! the session, and sessions are rate-limited by [`super::DeviceLink::begin_pairing`]. The
//! code is never sent over the link and is not a key: it only lets the user confirm that both
//! screens agree, and it means nothing once the session ends.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::circle::CheckKeyExt;
use super::identity::{DeviceIdentity, device_id_for_key, verify};
use super::wire::{Enc, random, unhex};
use super::{
    CLOCK_SKEW_MS, DEVICE_LINK_SCHEMA_VERSION, LinkError, PAIRING_CODE_DIGITS, PAIRING_TTL_MS,
};
use rightkit_crypto::{ct_eq, sha256};
use rightkit_device_link_types::{DevicePlatform, DeviceRef};
use rightkit_fs::digest::hex;

// v2: the host TLS pin, link port and the joiner's platform are part of both layouts.
const COMMIT_DOMAIN: &str = "pulse-link/pair-commit/v2";
const TRANSCRIPT_DOMAIN: &str = "pulse-link/pair-transcript/v2";
const SAS_DOMAIN: &str = "pulse-link/pair-code/v1";
const CONFIRM_DOMAIN: &str = "pulse-link/pair-confirm/v1";
const ROLE_HOST: &str = "host";
const ROLE_JOINER: &str = "joiner";
// The join token (`join_token`): the host keeps `join_verifier(secret)` only; the joiner proves it
// knows the secret with HMAC-SHA256 keyed by that verifier over the confirmed transcript hash.
const JOIN_VERIFIER_DOMAIN: &str = "pulse-link/join-verifier/v1";
const JOIN_PROOF_DOMAIN: &str = "pulse-link/join-proof/v1";

/// Step 1. Host to Joiner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairOffer {
    pub schema_version: u32,
    /// 16 random bytes as hex; names this session and this pairing for its whole life.
    pub pairing_id: String,
    pub host: DeviceRef,
    /// Host identity public key, 32 bytes as hex.
    pub host_key: String,
    /// SHA-256 of the SubjectPublicKeyInfo of the CA certificate the Host serves the link and
    /// this pairing channel with (hex on the wire). Bound into the commitment and the
    /// transcript, so the typed code confirms it.
    #[serde(with = "pin_hex")]
    pub host_tls_pin: [u8; 32],
    /// The Host's link port. Bound into the commitment and the transcript. The pairing channel
    /// listens on this port plus one.
    pub host_link_port: u16,
    pub expires_at_ms: u64,
}

mod pin_hex {
    use super::{Deserialize, Deserializer, Serializer, hex, unhex};

    pub fn serialize<S: Serializer>(pin: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex(pin))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(deserializer)?;
        unhex::<32>(&text).map_err(serde::de::Error::custom)
    }
}

/// Step 2. Joiner to Host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairAccept {
    pub schema_version: u32,
    pub pairing_id: String,
    pub joiner: DeviceRef,
    pub joiner_key: String,
    /// The joiner's operating system: `macos`, `windows`, `linux` or `ios` (see `PLATFORMS`). Bound
    /// into the commitment and the transcript, so the typed code confirms it, and shown to the
    /// owner at approval. It must agree with `joiner.platform`.
    pub platform: String,
    /// SHA-256 commitment to the joiner nonce, hex.
    pub commitment: String,
    /// The joiner's own owner-check public key, when it is a computer that has one, with its
    /// binding to this joiner's device (the key's own signature naming the joiner's id and key).
    /// It rides the authenticated stream of this pairing; the host ignores it without a matching
    /// binding, and the host owner's owner-check signature endorses it (`circle.rs`) only if the
    /// owner approves with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<super::circle::CheckKey>,
}

/// Step 3. Host to Joiner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairHostNonce {
    pub schema_version: u32,
    pub pairing_id: String,
    pub nonce: String,
}

/// Step 4. Joiner to Host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairReveal {
    pub schema_version: u32,
    pub pairing_id: String,
    pub nonce: String,
}

/// Step 6, both directions. A signature over the transcript hash by the sender's identity key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairConfirm {
    pub schema_version: u32,
    pub pairing_id: String,
    pub device_id: String,
    pub signature: String,
}

/// Final message, Host to Joiner, sent only after the owner approved. It carries the transport's
/// single-use enrolment code, so it is confidential: send it only inside the pinned TLS
/// channel, never log it (`Debug` hides the code).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairEnrol {
    pub schema_version: u32,
    pub pairing_id: String,
    /// The transport enrolment code. Bearer secret; single use; expires with the daemon's own
    /// lifetime for it.
    pub enrol_code: String,
    /// The port the Joiner dials the link on. Must equal the offer's `host_link_port`.
    pub link_port: u16,
    /// The endorsement batch the Host owner's owner-check signature made when it admitted the
    /// Joiner (Touch ID or Windows Hello, never a PIN): one signature over bodies that include the
    /// Joiner's id and key, with the transcript hash as its request. The Joiner verifies it and
    /// pins the key before it lets this Host speak for the circle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admit: Option<super::circle::EndorsementBatch>,
    /// A join token or QR pairing: `admit` is then the batch whose grant the owner check signed
    /// when the token was made, and this is the Host's device-key redemption of it by the Joiner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redemption: Option<super::circle::Redemption>,
}

impl fmt::Debug for PairEnrol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairEnrol")
            .field("pairing_id", &self.pairing_id)
            .field("enrol_code", &"<redacted>")
            .field("link_port", &self.link_port)
            .finish()
    }
}

/// What the Joiner takes from a verified [`PairEnrol`].
#[derive(Clone, PartialEq, Eq)]
pub struct EnrolGrant {
    pub pairing_id: String,
    pub enrol_code: String,
    pub link_port: u16,
    /// The Host owner's endorsement batch, as received; unverified until
    /// `DeviceLink::complete_pairing_with_host_proof`.
    pub admit: Option<super::circle::EndorsementBatch>,
    /// The Host's redemption of the token grant in `admit`, as received; unverified until
    /// `DeviceLink::complete_pairing_with_grant`.
    pub redemption: Option<super::circle::Redemption>,
}

impl fmt::Debug for EnrolGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrolGrant")
            .field("pairing_id", &self.pairing_id)
            .field("enrol_code", &"<redacted>")
            .field("link_port", &self.link_port)
            .finish()
    }
}

/// An enrolment code is printable ASCII with no whitespace and a sane length.
fn check_enrol_code(code: &str) -> Result<(), LinkError> {
    if code.is_empty() || code.len() > 128 || !code.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(LinkError::Malformed("enrolment code"));
    }
    Ok(())
}

/// What a finished pairing yields. The caller records it with
/// [`super::DeviceLink::complete_pairing`]. Only this module can build one (the private
/// field), and the Host side builds it only in [`HostPairing::approve`], so a recorded
/// pairing implies the host owner's approval.
// `#[non_exhaustive]` would still let the rest of the crate build one; the private field keeps
// construction inside this module, which is the point.
#[allow(clippy::manual_non_exhaustive)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingOutcome {
    pub pairing_id: String,
    pub peer: DeviceRef,
    /// Peer identity public key, hex.
    pub peer_key: String,
    pub paired_at_ms: u64,
    /// Hex SHA-256 of the confirmed transcript, for audit.
    pub transcript_hash: String,
    /// Joiner side only: the Host's TLS pin and link port, as confirmed by the typed code.
    /// `None` on the Host side. The transport must persist them to dial again after a restart.
    pub peer_tls_pin: Option<[u8; 32]>,
    pub peer_link_port: Option<u16>,
    /// Keeps construction inside this module.
    sealed: (),
}

/// A joiner whose signature verified and who now waits for the host owner. It is data to
/// SHOW; holding it grants nothing. Fields are read through accessors so only this module
/// can make one, and [`HostPairing::approve`] accepts only the value its own session made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingApproval {
    pairing_id: String,
    code: String,
    joiner_name: String,
    joiner_device_id: String,
    /// The joiner's identity public key, hex (the circle's admission names it).
    joiner_key: String,
    joiner_platform: &'static str,
    /// The joiner's own owner-check key as it declared it: shown to nobody as fact, endorsed only
    /// by the owner's owner-check signature at approval.
    joiner_check: Option<super::circle::CheckKey>,
    fingerprint: String,
    transcript_hash: [u8; 32],
}

impl PendingApproval {
    /// The eight digits to display; the owner checks they match what the joiner device shows
    /// or was typed on. Display them with [`format_code`].
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The joiner's self-chosen label, with control and bidirectional-override characters
    /// removed and the length capped. Attacker-controlled: identification comes from the
    /// device id and fingerprint, never from this.
    pub fn joiner_name(&self) -> &str {
        &self.joiner_name
    }

    pub fn joiner_device_id(&self) -> &str {
        &self.joiner_device_id
    }

    /// The owner-check key the joiner declared for itself, if any (a computer's). It is a claim:
    /// it becomes trusted only through the host owner's owner-check signature over it.
    pub fn joiner_check(&self) -> Option<&super::circle::CheckKey> {
        self.joiner_check.as_ref()
    }

    /// The joiner's identity public key, 64 hex; the device id derives from it.
    pub fn joiner_key(&self) -> &str {
        &self.joiner_key
    }

    /// The joiner's platform: `macos`, `windows`, `linux` or `ios`. Bound into the transcript the
    /// owner's code confirms, and checked against an allowlist, so it is never free text.
    pub fn joiner_platform(&self) -> &str {
        self.joiner_platform
    }

    /// Short grouped fingerprint of the joiner's key ([`key_fingerprint`]); the joiner device
    /// shows the same string for its own key.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn pairing_id(&self) -> &str {
        &self.pairing_id
    }

    /// The confirmed transcript hash, 64 hex: the request an admitting owner-check signature
    /// commits to, so the joiner can tell the signature is for exactly this pairing.
    pub fn transcript_hash_hex(&self) -> String {
        hex(&self.transcript_hash)
    }
}

/// What a host keeps of a join token's secret: SHA-256 over a domain and the secret. The secret
/// itself is never stored on the host.
pub fn join_verifier(secret: &[u8; 32]) -> [u8; 32] {
    sha256(&Enc::new(JOIN_VERIFIER_DOMAIN).bytes(secret).finish())
}

fn join_proof_input(transcript_hash: &[u8; 32]) -> Vec<u8> {
    Enc::new(JOIN_PROOF_DOMAIN).bytes(transcript_hash).finish()
}

fn join_proof(verifier: &[u8; 32], transcript_hash: &[u8; 32]) -> [u8; 32] {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, verifier);
    let tag = ring::hmac::sign(&key, &join_proof_input(transcript_hash));
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

impl PendingApproval {
    /// Whether `proof` is the join-token proof for THIS session's confirmed transcript
    /// (HMAC-SHA256 under the host's verifier, constant time). The transcript carries the pairing
    /// id, both identity keys and both fresh nonces, so a proof made for another session, or by
    /// a device that does not know the token secret, fails.
    pub fn join_proof_ok(&self, verifier: &[u8; 32], proof: &[u8; 32]) -> bool {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, verifier);
        ring::hmac::verify(&key, &join_proof_input(&self.transcript_hash), proof).is_ok()
    }
}

/// The platform values a joiner may declare on the wire. `ios` was added for the phone;
/// the first three are unchanged, so no existing transcript changes.
pub const PLATFORMS: [&str; 5] = ["macos", "windows", "linux", "ios", "android"];

/// The wire value for a device platform.
fn platform_wire(platform: DevicePlatform) -> Option<&'static str> {
    match platform {
        DevicePlatform::Mac => Some(PLATFORMS[0]),
        DevicePlatform::Windows => Some(PLATFORMS[1]),
        DevicePlatform::Linux => Some(PLATFORMS[2]),
        DevicePlatform::Ios => Some(PLATFORMS[3]),
        DevicePlatform::Android => Some(PLATFORMS[4]),
    }
}

/// The allowlisted static value for a declared platform string that also agrees with the
/// joiner's `DeviceRef`. Anything else is malformed.
fn checked_platform(declared: &str, joiner: &DeviceRef) -> Result<&'static str, LinkError> {
    let wire = PLATFORMS
        .iter()
        .copied()
        .find(|p| ct_eq(p.as_bytes(), declared.as_bytes()))
        .ok_or(LinkError::Malformed("platform"))?;
    if platform_wire(joiner.platform) != Some(wire) {
        return Err(LinkError::Malformed("platform differs from the device"));
    }
    Ok(wire)
}

const FINGERPRINT_DOMAIN: &str = "pulse-link/key-fingerprint/v1";
const MAX_NAME_CHARS: usize = 48;

/// Grouped hex fingerprint of an identity public key (`a1b2 c3d4 ...`, 96 bits) for the
/// owner to compare across two screens.
pub fn key_fingerprint(public_key: &[u8; 32]) -> String {
    let digest = sha256(&Enc::new(FINGERPRINT_DOMAIN).bytes(public_key).finish());
    let full = hex(&digest[..12]);
    full.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The eight digits grouped 4+4 for display (`1234 5678`). `normalize_code` accepts this form
/// (and `1234-5678`), so what is shown can be typed back as shown.
pub fn format_code(code: &str) -> String {
    if code.len() == PAIRING_CODE_DIGITS && code.is_ascii() {
        let (head, tail) = code.split_at(PAIRING_CODE_DIGITS / 2);
        format!("{head} {tail}")
    } else {
        code.to_string()
    }
}

fn display_safe(name: Option<&str>) -> String {
    let cleaned: String = name
        .unwrap_or("")
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "(unnamed device)".to_string()
    } else {
        cleaned
    }
}

/// Everything both sides agree on before the nonces.
struct Parties {
    pairing_id: [u8; 16],
    host: DeviceRef,
    host_key: [u8; 32],
    host_tls_pin: [u8; 32],
    host_link_port: u16,
    joiner: DeviceRef,
    joiner_key: [u8; 32],
    joiner_platform: &'static str,
}

impl Parties {
    fn commitment(&self, joiner_nonce: &[u8; 32]) -> [u8; 32] {
        sha256(
            &Enc::new(COMMIT_DOMAIN)
                .bytes(&self.pairing_id)
                .device(&self.host)
                .bytes(&self.host_key)
                .bytes(&self.host_tls_pin)
                .u64(u64::from(self.host_link_port))
                .device(&self.joiner)
                .bytes(&self.joiner_key)
                .str(self.joiner_platform)
                .bytes(joiner_nonce)
                .finish(),
        )
    }

    fn transcript_hash(&self, host_nonce: &[u8; 32], joiner_nonce: &[u8; 32]) -> [u8; 32] {
        sha256(
            &Enc::new(TRANSCRIPT_DOMAIN)
                .bytes(&self.pairing_id)
                .device(&self.host)
                .bytes(&self.host_key)
                .bytes(&self.host_tls_pin)
                .u64(u64::from(self.host_link_port))
                .device(&self.joiner)
                .bytes(&self.joiner_key)
                .str(self.joiner_platform)
                .bytes(host_nonce)
                .bytes(joiner_nonce)
                .finish(),
        )
    }
}

/// The eight-digit code for a transcript. Taking 64 bits modulo 10^8 leaves a bias far below
/// anything an attacker could use.
fn code_for(transcript_hash: &[u8; 32]) -> String {
    let digest = sha256(&Enc::new(SAS_DOMAIN).bytes(transcript_hash).finish());
    let mut word = [0u8; 8];
    word.copy_from_slice(&digest[..8]);
    let modulus = 10u64.pow(PAIRING_CODE_DIGITS as u32);
    format!(
        "{:0width$}",
        u64::from_be_bytes(word) % modulus,
        width = PAIRING_CODE_DIGITS
    )
}

fn confirm_input(role: &str, transcript_hash: &[u8; 32]) -> Vec<u8> {
    Enc::new(CONFIRM_DOMAIN)
        .str(role)
        .bytes(transcript_hash)
        .finish()
}

fn sign_confirm(
    identity: &DeviceIdentity,
    role: &str,
    pairing_id: &[u8; 16],
    transcript_hash: &[u8; 32],
) -> PairConfirm {
    PairConfirm {
        schema_version: DEVICE_LINK_SCHEMA_VERSION,
        pairing_id: hex(pairing_id),
        device_id: identity.device_id().to_string(),
        signature: identity.sign(&confirm_input(role, transcript_hash)),
    }
}

fn check_confirm(
    msg: &PairConfirm,
    role: &str,
    pairing_id: &[u8; 16],
    signer: &DeviceRef,
    signer_key: &[u8; 32],
    transcript_hash: &[u8; 32],
) -> Result<(), LinkError> {
    check_version(msg.schema_version)?;
    if unhex::<16>(&msg.pairing_id)? != *pairing_id || msg.device_id != signer.device_id {
        return Err(LinkError::Malformed("confirmation names another pairing"));
    }
    let signature = unhex::<64>(&msg.signature)?;
    verify(
        signer_key,
        &confirm_input(role, transcript_hash),
        &signature,
    )
}

/// A pin and a port the Host would actually serve: not all zero, and a real port.
fn check_endpoint(pin: &[u8; 32], port: u16) -> Result<(), LinkError> {
    if pin.iter().all(|b| *b == 0) {
        return Err(LinkError::Malformed("tls pin"));
    }
    if port == 0 || port == u16::MAX {
        // The pairing channel is the link port plus one.
        return Err(LinkError::Malformed("link port"));
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

/// A key and the device reference that claims it. The id must be the one the key derives.
fn checked_peer(device: &DeviceRef, key_hex: &str) -> Result<[u8; 32], LinkError> {
    let key = unhex::<32>(key_hex)?;
    if device.device_id != device_id_for_key(&key) {
        return Err(LinkError::Malformed("device id does not match its key"));
    }
    Ok(key)
}

fn normalize_code(typed: &str) -> Result<String, LinkError> {
    let mut digits = String::with_capacity(PAIRING_CODE_DIGITS);
    for ch in typed.chars() {
        match ch {
            '0'..='9' => digits.push(ch),
            ' ' | '-' => {}
            _ => return Err(LinkError::Malformed("code characters")),
        }
    }
    if digits.len() != PAIRING_CODE_DIGITS {
        return Err(LinkError::Malformed("code length"));
    }
    Ok(digits)
}

enum HostPhase {
    Offered,
    AwaitReveal {
        commitment: [u8; 32],
        host_nonce: [u8; 32],
    },
    AwaitConfirm {
        transcript_hash: [u8; 32],
    },
    /// The joiner's signature verified; waiting for the owner. Holds what `approve` rechecks.
    AwaitApproval {
        pending: Box<PendingApproval>,
    },
    /// Approved and recorded; the enrolment code may now be sent, once.
    Approved,
    Done,
    Failed,
}

/// The device that shows the code. Every method takes the phase out first, so any error
/// (wrong order, expiry, bad message, bad signature) leaves the session void.
pub struct HostPairing {
    pairing_id: [u8; 16],
    host: DeviceRef,
    host_key: [u8; 32],
    host_tls_pin: [u8; 32],
    host_link_port: u16,
    expires_at_ms: u64,
    /// Newest `now_ms` this session has seen; a clock that moves back past the skew is refused.
    seen_ms: u64,
    joiner: Option<(DeviceRef, [u8; 32], &'static str)>,
    joiner_check: Option<super::circle::CheckKey>,
    phase: HostPhase,
}

impl HostPairing {
    /// Open a session. Use [`super::DeviceLink::begin_pairing`], which rate-limits starts.
    pub(crate) fn start(
        identity: &DeviceIdentity,
        host_tls_pin: [u8; 32],
        host_link_port: u16,
        now_ms: u64,
        preset_id: Option<[u8; 16]>,
    ) -> Result<(Self, PairOffer), LinkError> {
        check_endpoint(&host_tls_pin, host_link_port)?;
        let pairing_id = match preset_id {
            Some(id) => id,
            None => random::<16>()?,
        };
        let expires_at_ms = now_ms.saturating_add(PAIRING_TTL_MS);
        let offer = PairOffer {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            pairing_id: hex(&pairing_id),
            host: identity.device().clone(),
            host_key: identity.public_key_hex(),
            host_tls_pin,
            host_link_port,
            expires_at_ms,
        };
        let session = Self {
            pairing_id,
            host: identity.device().clone(),
            host_key: identity.public_key(),
            host_tls_pin,
            host_link_port,
            expires_at_ms,
            seen_ms: now_ms,
            joiner: None,
            joiner_check: None,
            phase: HostPhase::Offered,
        };
        Ok((session, offer))
    }

    /// Step 2 received; answer with the host nonce.
    pub fn accept(&mut self, msg: &PairAccept, now_ms: u64) -> Result<PairHostNonce, LinkError> {
        let phase = std::mem::replace(&mut self.phase, HostPhase::Failed);
        if !matches!(phase, HostPhase::Offered) {
            return Err(LinkError::WrongPhase);
        }
        self.check_live(now_ms)?;
        check_version(msg.schema_version)?;
        if unhex::<16>(&msg.pairing_id)? != self.pairing_id {
            return Err(LinkError::Malformed("another pairing"));
        }
        let joiner_key = checked_peer(&msg.joiner, &msg.joiner_key)?;
        if msg.joiner.device_id == self.host.device_id {
            return Err(LinkError::Malformed("cannot pair a device with itself"));
        }
        let platform = checked_platform(&msg.platform, &msg.joiner)?;
        let commitment = unhex::<32>(&msg.commitment)?;
        if let Some(check) = &msg.check {
            super::circle::check_key_shape(check)?;
        }
        let host_nonce = random::<32>()?;
        self.joiner = Some((msg.joiner.clone(), joiner_key, platform));
        // A claimed key counts only with its binding to this joiner's device id and key (the key's
        // own signature, made when it was enrolled); an unbound or mismatched one is dropped and
        // the joiner is paired without a check key (and so without `admit`).
        self.joiner_check = msg
            .check
            .clone()
            .filter(|c| c.bound_or_log(&msg.joiner.device_id, &msg.joiner_key));
        self.phase = HostPhase::AwaitReveal {
            commitment,
            host_nonce,
        };
        Ok(PairHostNonce {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            pairing_id: hex(&self.pairing_id),
            nonce: hex(&host_nonce),
        })
    }

    /// Step 4 received. Returns the code to SHOW to the user.
    pub fn reveal(&mut self, msg: &PairReveal, now_ms: u64) -> Result<String, LinkError> {
        let phase = std::mem::replace(&mut self.phase, HostPhase::Failed);
        let HostPhase::AwaitReveal {
            commitment,
            host_nonce,
        } = phase
        else {
            return Err(LinkError::WrongPhase);
        };
        self.check_live(now_ms)?;
        check_version(msg.schema_version)?;
        if unhex::<16>(&msg.pairing_id)? != self.pairing_id {
            return Err(LinkError::Malformed("another pairing"));
        }
        let joiner_nonce = unhex::<32>(&msg.nonce)?;
        let parties = self.parties()?;
        if !ct_eq(&parties.commitment(&joiner_nonce), &commitment) {
            return Err(LinkError::CommitmentMismatch);
        }
        let transcript_hash = parties.transcript_hash(&host_nonce, &joiner_nonce);
        self.phase = HostPhase::AwaitConfirm { transcript_hash };
        Ok(code_for(&transcript_hash))
    }

    /// Step 6 received. Verifies the joiner's signature and returns what the owner must be
    /// shown. Records nothing and sends nothing: a device that merely computed the code and
    /// signed is still unapproved. Show [`PendingApproval`] in a trusted host UI, then call
    /// [`Self::approve`] on the owner's yes, or [`Self::reject`] (or let the session expire).
    pub fn confirm(
        &mut self,
        identity: &DeviceIdentity,
        msg: &PairConfirm,
        now_ms: u64,
    ) -> Result<PendingApproval, LinkError> {
        let phase = std::mem::replace(&mut self.phase, HostPhase::Failed);
        let HostPhase::AwaitConfirm { transcript_hash } = phase else {
            return Err(LinkError::WrongPhase);
        };
        self.check_live(now_ms)?;
        if identity.device_id() != self.host.device_id {
            return Err(LinkError::Malformed("not this session's host identity"));
        }
        let (joiner, joiner_key, joiner_platform) =
            self.joiner.clone().ok_or(LinkError::WrongPhase)?;
        check_confirm(
            msg,
            ROLE_JOINER,
            &self.pairing_id,
            &joiner,
            &joiner_key,
            &transcript_hash,
        )?;
        let pending = PendingApproval {
            pairing_id: hex(&self.pairing_id),
            code: code_for(&transcript_hash),
            joiner_name: display_safe(joiner.display_name.as_deref()),
            joiner_device_id: joiner.device_id.clone(),
            joiner_key: hex(&joiner_key),
            joiner_platform,
            joiner_check: self.joiner_check.clone(),
            fingerprint: key_fingerprint(&joiner_key),
            transcript_hash,
        };
        self.phase = HostPhase::AwaitApproval {
            pending: Box::new(pending.clone()),
        };
        Ok(pending)
    }

    /// The owner confirmed that the code, name and fingerprint match the device they hold.
    /// `pending` must be the value this session's [`Self::confirm`] returned (same pairing,
    /// transcript and code). Any mismatch, expiry or wrong order voids the session and
    /// yields no outcome. On success returns the outcome to record and this device's own
    /// confirmation to send back; the caller records the outcome before sending.
    pub fn approve(
        &mut self,
        identity: &DeviceIdentity,
        pending: &PendingApproval,
        now_ms: u64,
    ) -> Result<(PairingOutcome, PairConfirm), LinkError> {
        let phase = std::mem::replace(&mut self.phase, HostPhase::Failed);
        let HostPhase::AwaitApproval { pending: held } = phase else {
            return Err(LinkError::WrongPhase);
        };
        self.check_live(now_ms)?;
        if identity.device_id() != self.host.device_id {
            return Err(LinkError::Malformed("not this session's host identity"));
        }
        if !ct_eq(&held.transcript_hash, &pending.transcript_hash)
            || !ct_eq(held.code.as_bytes(), pending.code.as_bytes())
            || held.pairing_id != pending.pairing_id
            || held.joiner_device_id != pending.joiner_device_id
            || held.joiner_platform != pending.joiner_platform
            || held.joiner_check != pending.joiner_check
        {
            return Err(LinkError::ApprovalMismatch);
        }
        let (joiner, joiner_key, _) = self.joiner.clone().ok_or(LinkError::WrongPhase)?;
        self.phase = HostPhase::Approved;
        let outcome = PairingOutcome {
            pairing_id: hex(&self.pairing_id),
            peer: joiner,
            peer_key: hex(&joiner_key),
            paired_at_ms: now_ms,
            transcript_hash: hex(&held.transcript_hash),
            peer_tls_pin: None,
            peer_link_port: None,
            sealed: (),
        };
        let reply = sign_confirm(identity, ROLE_HOST, &self.pairing_id, &held.transcript_hash);
        Ok((outcome, reply))
    }

    /// Step 8: build the final message that carries the transport enrolment code. Valid once,
    /// only after [`Self::approve`] (the caller issued `enrol_code` after the owner's yes), and
    /// within the session's lifetime. Any other phase or a malformed code voids the session.
    pub fn enrol(&mut self, enrol_code: &str, now_ms: u64) -> Result<PairEnrol, LinkError> {
        let phase = std::mem::replace(&mut self.phase, HostPhase::Failed);
        if !matches!(phase, HostPhase::Approved) {
            return Err(LinkError::WrongPhase);
        }
        self.check_live(now_ms)?;
        check_enrol_code(enrol_code)?;
        self.phase = HostPhase::Done;
        Ok(PairEnrol {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            pairing_id: hex(&self.pairing_id),
            enrol_code: enrol_code.to_string(),
            link_port: self.host_link_port,
            admit: None,
            redemption: None,
        })
    }

    /// The owner said no. Voids the session; nothing was recorded.
    pub fn reject(&mut self) {
        self.phase = HostPhase::Failed;
    }

    fn check_live(&mut self, now_ms: u64) -> Result<(), LinkError> {
        if now_ms.saturating_add(CLOCK_SKEW_MS) < self.seen_ms {
            return Err(LinkError::ClockRolledBack);
        }
        self.seen_ms = self.seen_ms.max(now_ms);
        if now_ms >= self.expires_at_ms {
            Err(LinkError::Expired)
        } else {
            Ok(())
        }
    }

    fn parties(&self) -> Result<Parties, LinkError> {
        let (joiner, joiner_key, joiner_platform) =
            self.joiner.clone().ok_or(LinkError::WrongPhase)?;
        Ok(Parties {
            pairing_id: self.pairing_id,
            host: self.host.clone(),
            host_key: self.host_key,
            host_tls_pin: self.host_tls_pin,
            host_link_port: self.host_link_port,
            joiner,
            joiner_key,
            joiner_platform,
        })
    }
}

enum JoinerPhase {
    AwaitNonce {
        joiner_nonce: [u8; 32],
    },
    AwaitCode {
        transcript_hash: [u8; 32],
    },
    AwaitHostConfirm {
        transcript_hash: [u8; 32],
    },
    /// The Host's confirmation verified and the outcome was handed out; the enrolment code is
    /// the only thing still expected.
    AwaitEnrol,
    Done,
    Failed,
}

/// The device where the user types the code.
pub struct JoinerPairing {
    parties: Parties,
    expires_at_ms: u64,
    /// Newest `now_ms` this session has seen; a clock that moves back past the skew is refused.
    seen_ms: u64,
    phase: JoinerPhase,
}

impl JoinerPairing {
    /// Step 1 received; answer with the commitment. The offer's key is whatever the offer
    /// says: it is authenticated only by the code the user types in step 5.
    ///
    /// `observed_tls_pin` is the SHA-256 SPKI pin of the CA certificate the TLS channel the
    /// offer arrived on actually presented. An offer naming any other pin is refused with
    /// [`LinkError::TlsPinMismatch`]. This does not by itself stop a man in the middle that
    /// terminates TLS with its own certificate and rewrites the offer to match; that case is
    /// caught because the pin is in the transcript, so the typed code then fails.
    pub fn accept_offer(
        identity: &DeviceIdentity,
        offer: &PairOffer,
        observed_tls_pin: &[u8; 32],
        now_ms: u64,
    ) -> Result<(Self, PairAccept), LinkError> {
        check_version(offer.schema_version)?;
        check_endpoint(&offer.host_tls_pin, offer.host_link_port)?;
        if !ct_eq(&offer.host_tls_pin, observed_tls_pin) {
            return Err(LinkError::TlsPinMismatch);
        }
        if now_ms >= offer.expires_at_ms {
            return Err(LinkError::Expired);
        }
        // A far-future expiry would let an old offer live on; the session limit is ours.
        if offer.expires_at_ms > now_ms.saturating_add(PAIRING_TTL_MS) {
            return Err(LinkError::Malformed("offer lifetime"));
        }
        let pairing_id = unhex::<16>(&offer.pairing_id)?;
        let host_key = checked_peer(&offer.host, &offer.host_key)?;
        if offer.host.device_id == identity.device_id() {
            return Err(LinkError::Malformed("cannot pair a device with itself"));
        }
        let joiner_platform = platform_wire(identity.device().platform)
            .ok_or(LinkError::Malformed("platform cannot pair"))?;
        let joiner_nonce = random::<32>()?;
        let parties = Parties {
            pairing_id,
            host: offer.host.clone(),
            host_key,
            host_tls_pin: offer.host_tls_pin,
            host_link_port: offer.host_link_port,
            joiner: identity.device().clone(),
            joiner_key: identity.public_key(),
            joiner_platform,
        };
        let accept = PairAccept {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            pairing_id: offer.pairing_id.clone(),
            joiner: identity.device().clone(),
            joiner_key: identity.public_key_hex(),
            platform: joiner_platform.to_string(),
            commitment: hex(&parties.commitment(&joiner_nonce)),
            // Only the engine process reads the owner-check key file, and only a computer has one.
            check: super::circle::wire_platform_is_computer(joiner_platform)
                .then(rightkit_approvals::approval_sig::engine_owner_check_key)
                .flatten()
                .map(|(device, public_key, binding)| super::circle::CheckKey {
                    device,
                    public_key,
                    // Only a binding that names this device; any other is not sent.
                    binding: binding.filter(|b| {
                        b.device_id == identity.device_id()
                            && b.device_key == identity.public_key_hex()
                    }),
                }),
        };
        let session = Self {
            parties,
            expires_at_ms: offer.expires_at_ms,
            seen_ms: now_ms,
            phase: JoinerPhase::AwaitNonce { joiner_nonce },
        };
        Ok((session, accept))
    }

    /// Step 3 received; reveal the joiner nonce. The session now waits for the typed code.
    pub fn receive_host_nonce(
        &mut self,
        msg: &PairHostNonce,
        now_ms: u64,
    ) -> Result<PairReveal, LinkError> {
        let phase = std::mem::replace(&mut self.phase, JoinerPhase::Failed);
        let JoinerPhase::AwaitNonce { joiner_nonce } = phase else {
            return Err(LinkError::WrongPhase);
        };
        self.check_live(now_ms)?;
        check_version(msg.schema_version)?;
        if unhex::<16>(&msg.pairing_id)? != self.parties.pairing_id {
            return Err(LinkError::Malformed("another pairing"));
        }
        let host_nonce = unhex::<32>(&msg.nonce)?;
        let transcript_hash = self.parties.transcript_hash(&host_nonce, &joiner_nonce);
        self.phase = JoinerPhase::AwaitCode { transcript_hash };
        Ok(PairReveal {
            schema_version: DEVICE_LINK_SCHEMA_VERSION,
            pairing_id: hex(&self.parties.pairing_id),
            nonce: hex(&joiner_nonce),
        })
    }

    /// The user typed the code shown on the host. A wrong code voids the session.
    pub fn enter_code(
        &mut self,
        identity: &DeviceIdentity,
        typed: &str,
        now_ms: u64,
    ) -> Result<PairConfirm, LinkError> {
        let phase = std::mem::replace(&mut self.phase, JoinerPhase::Failed);
        let JoinerPhase::AwaitCode { transcript_hash } = phase else {
            return Err(LinkError::WrongPhase);
        };
        self.check_live(now_ms)?;
        if identity.device_id() != self.parties.joiner.device_id {
            return Err(LinkError::Malformed("not this session's joiner identity"));
        }
        let typed = normalize_code(typed)?;
        if !ct_eq(typed.as_bytes(), code_for(&transcript_hash).as_bytes()) {
            return Err(LinkError::CodeMismatch);
        }
        self.phase = JoinerPhase::AwaitHostConfirm { transcript_hash };
        Ok(sign_confirm(
            identity,
            ROLE_JOINER,
            &self.parties.pairing_id,
            &transcript_hash,
        ))
    }

    /// Join-token variant of [`Self::enter_code`]: nobody types a code. The joiner signs the
    /// transcript as usual and also returns the proof that it knows the token secret (HMAC-SHA256
    /// under [`join_verifier`] of the secret, over the transcript hash). The host owner's check
    /// happened when the token was made; the host approves only on a valid proof.
    pub fn enter_join(
        &mut self,
        identity: &DeviceIdentity,
        secret: &[u8; 32],
        now_ms: u64,
    ) -> Result<(PairConfirm, [u8; 32]), LinkError> {
        let phase = std::mem::replace(&mut self.phase, JoinerPhase::Failed);
        let JoinerPhase::AwaitCode { transcript_hash } = phase else {
            return Err(LinkError::WrongPhase);
        };
        self.check_live(now_ms)?;
        if identity.device_id() != self.parties.joiner.device_id {
            return Err(LinkError::Malformed("not this session's joiner identity"));
        }
        let proof = join_proof(&join_verifier(secret), &transcript_hash);
        self.phase = JoinerPhase::AwaitHostConfirm { transcript_hash };
        Ok((
            sign_confirm(
                identity,
                ROLE_JOINER,
                &self.parties.pairing_id,
                &transcript_hash,
            ),
            proof,
        ))
    }

    /// The host's confirmation arrived. On success the caller records the outcome.
    pub fn finish(&mut self, msg: &PairConfirm, now_ms: u64) -> Result<PairingOutcome, LinkError> {
        let phase = std::mem::replace(&mut self.phase, JoinerPhase::Failed);
        let JoinerPhase::AwaitHostConfirm { transcript_hash } = phase else {
            return Err(LinkError::WrongPhase);
        };
        self.check_live(now_ms)?;
        check_confirm(
            msg,
            ROLE_HOST,
            &self.parties.pairing_id,
            &self.parties.host,
            &self.parties.host_key,
            &transcript_hash,
        )?;
        self.phase = JoinerPhase::AwaitEnrol;
        Ok(PairingOutcome {
            pairing_id: hex(&self.parties.pairing_id),
            peer: self.parties.host.clone(),
            peer_key: hex(&self.parties.host_key),
            paired_at_ms: now_ms,
            transcript_hash: hex(&transcript_hash),
            peer_tls_pin: Some(self.parties.host_tls_pin),
            peer_link_port: Some(self.parties.host_link_port),
            sealed: (),
        })
    }

    /// Step 8 received: the Host's enrolment code. Accepted only after [`Self::finish`] verified
    /// the Host's confirmation, for this pairing, and with the link port the confirmed offer
    /// named. Anything else voids the session and the code is dropped.
    pub fn receive_enrol(&mut self, msg: &PairEnrol, now_ms: u64) -> Result<EnrolGrant, LinkError> {
        let phase = std::mem::replace(&mut self.phase, JoinerPhase::Failed);
        if !matches!(phase, JoinerPhase::AwaitEnrol) {
            return Err(LinkError::WrongPhase);
        }
        self.check_live(now_ms)?;
        check_version(msg.schema_version)?;
        if unhex::<16>(&msg.pairing_id)? != self.parties.pairing_id {
            return Err(LinkError::Malformed("another pairing"));
        }
        if msg.link_port != self.parties.host_link_port {
            return Err(LinkError::Malformed(
                "link port differs from the confirmed offer",
            ));
        }
        check_enrol_code(&msg.enrol_code)?;
        self.phase = JoinerPhase::Done;
        Ok(EnrolGrant {
            pairing_id: msg.pairing_id.clone(),
            enrol_code: msg.enrol_code.clone(),
            link_port: msg.link_port,
            admit: msg.admit.clone(),
            redemption: msg.redemption.clone(),
        })
    }

    fn check_live(&mut self, now_ms: u64) -> Result<(), LinkError> {
        if now_ms.saturating_add(CLOCK_SKEW_MS) < self.seen_ms {
            return Err(LinkError::ClockRolledBack);
        }
        self.seen_ms = self.seen_ms.max(now_ms);
        if now_ms >= self.expires_at_ms {
            Err(LinkError::Expired)
        } else {
            Ok(())
        }
    }
}
