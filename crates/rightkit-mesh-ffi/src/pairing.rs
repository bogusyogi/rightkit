//! The Joiner side of the SAS pairing, byte-for-byte with the desktop Host
//! (`core/src/devicelink/pairing.rs` and `wire.rs`, message framing in
//! `core/src/devicelink/transport/pairing_channel.rs`).
//!
//! The domain strings, the field order and the length prefixes below are wire protocol: changing one
//! character makes every typed code fail. Each function names the desktop function it mirrors.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rightkit_crypto::{ct_eq, sha256};
use rightkit_fs::digest::hex;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::oneshot;

// ---- constants (core/src/devicelink/mod.rs and pairing.rs) --------------------------------------

/// `DEVICE_LINK_SCHEMA_VERSION`.
pub const SCHEMA_VERSION: u32 = 2;
/// `DEVICE_REF_SCHEMA_VERSION` (core/src/agent/device.rs).
pub const DEVICE_REF_SCHEMA_VERSION: u32 = 1;
/// `PAIRING_TTL_MS`.
pub const PAIRING_TTL_MS: u64 = 600_000;
/// `CLOCK_SKEW_MS`.
pub const CLOCK_SKEW_MS: u64 = 30_000;
/// `PAIRING_CODE_DIGITS`.
pub const CODE_DIGITS: usize = 8;
/// The host's link port over the mesh (docs/mesh-phone.md: `host_link_port` is 1).
pub const MESH_LINK_PORT: u16 = 1;
/// `RESUME_WINDOW_MS` of pairing_channel.rs: how long after its confirmation the phone may repeat
/// it on a new stream (the host keeps its final messages for the same ten minutes).
pub const RESUME_WINDOW_MS: u64 = 10 * 60 * 1000;
/// `MAX_LINE` of pairing_channel.rs.
const MAX_LINE: u64 = 16 * 1024;

const COMMIT_DOMAIN: &str = "pulse-link/pair-commit/v2";
const TRANSCRIPT_DOMAIN: &str = "pulse-link/pair-transcript/v2";
const SAS_DOMAIN: &str = "pulse-link/pair-code/v1";
const CONFIRM_DOMAIN: &str = "pulse-link/pair-confirm/v1";
const FINGERPRINT_DOMAIN: &str = "pulse-link/key-fingerprint/v1";
const DEVICE_ID_DOMAIN: &[u8] = b"pulse-link/device-id/v1";
const ROLE_HOST: &str = "host";
const ROLE_JOINER: &str = "joiner";
// The join token proof (core/src/devicelink/pairing.rs `join_verifier`, `join_proof`).
const JOIN_VERIFIER_DOMAIN: &str = "pulse-link/join-verifier/v1";
const JOIN_PROOF_DOMAIN: &str = "pulse-link/join-proof/v1";
/// The platform this phone declares (`PLATFORMS` in pairing.rs).
/// The platform this phone library was built for, as the desktop's pairing allowlist names it.
#[cfg(target_os = "android")]
const JOINER_PLATFORM: &str = "android";
#[cfg(not(target_os = "android"))]
const JOINER_PLATFORM: &str = "ios";

// ---- errors -------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PmError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl PmError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn retry(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            retryable: true,
            ..Self::new(code, message)
        }
    }

    fn malformed(what: &str) -> Self {
        Self::new("malformed", what.to_string())
    }
}

impl fmt::Display for PmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

// ---- bytes (core/src/devicelink/wire.rs) --------------------------------------------------------

/// `wire::Enc`: domain-separated, length-prefixed byte builder.
pub struct Enc(Vec<u8>);

impl Enc {
    /// `Enc::new`.
    pub fn new(domain: &str) -> Self {
        Enc(Vec::new()).str(domain)
    }

    /// `Enc::bytes`: u32 big-endian length, then the bytes.
    pub fn bytes(mut self, value: &[u8]) -> Self {
        self.0
            .extend_from_slice(&(value.len() as u32).to_be_bytes());
        self.0.extend_from_slice(value);
        self
    }

    /// `Enc::str`.
    pub fn str(self, value: &str) -> Self {
        self.bytes(value.as_bytes())
    }

    /// `Enc::u64`: eight bytes big-endian.
    pub fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// `Enc::device`: schema version, device id, platform tag, then the display name (1 and the
    /// name, or 0).
    pub fn device(self, device: &DeviceRef) -> Self {
        let this = self
            .u64(u64::from(device.schema_version))
            .str(&device.device_id)
            .str(device.platform.tag());
        match &device.display_name {
            Some(name) => this.u64(1).str(name),
            None => this.u64(0),
        }
    }

    pub fn finish(self) -> Vec<u8> {
        self.0
    }
}

/// `wire::random`.
pub fn random<const N: usize>() -> Result<[u8; N], PmError> {
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| PmError::new("random", "the system random source failed"))?;
    Ok(out)
}

/// `wire::unhex`: exactly `N` bytes of lower or upper case hex. The decoding is the owner's
/// `rightkit_fs::digest::unhex`; this only fixes the length to `N` bytes.
pub fn unhex<const N: usize>(text: &str) -> Result<[u8; N], PmError> {
    if text.len() != N * 2 {
        return Err(PmError::malformed("hex length"));
    }
    rightkit_fs::digest::unhex(text)
        .ok_or_else(|| PmError::malformed("hex digit"))?
        .try_into()
        .map_err(|_| PmError::malformed("hex length"))
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ---- identity (core/src/devicelink/identity.rs) -------------------------------------------------

/// `DevicePlatform` (core/src/agent/device.rs), serialised in kebab case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    Mac,
    Windows,
    Linux,
    Ios,
}

impl Platform {
    /// `wire::platform_tag`.
    pub fn tag(self) -> &'static str {
        match self {
            Platform::Mac => "mac",
            Platform::Windows => "windows",
            Platform::Linux => "linux",
            Platform::Ios => "ios",
        }
    }
}

/// `DeviceRef` (core/src/agent/device.rs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRef {
    pub schema_version: u32,
    pub device_id: String,
    pub platform: Platform,
    pub display_name: Option<String>,
}

/// `identity::device_id_for_key`: `dev_` plus the first 32 hex characters of SHA-256 over the
/// domain and the public key.
pub fn device_id_for_key(public_key: &[u8]) -> String {
    let mut input = DEVICE_ID_DOMAIN.to_vec();
    input.extend_from_slice(public_key);
    let digest = sha256(&input);
    format!("dev_{}", hex(&digest[..16]))
}

/// `identity::DeviceIdentity`: signing key, public key and the `DeviceRef` it presents.
pub struct Identity {
    key: Ed25519KeyPair,
    public: [u8; 32],
    device: DeviceRef,
}

impl Identity {
    /// `DeviceIdentity::from_seed`.
    pub fn from_seed(seed: &[u8; 32], display_name: Option<String>) -> Result<Self, PmError> {
        let key = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| PmError::new("identity", "identity seed rejected"))?;
        let mut public = [0u8; 32];
        public.copy_from_slice(key.public_key().as_ref());
        let device = DeviceRef {
            schema_version: DEVICE_REF_SCHEMA_VERSION,
            device_id: device_id_for_key(&public),
            platform: Platform::Ios,
            display_name,
        };
        Ok(Self {
            key,
            public,
            device,
        })
    }

    pub fn device(&self) -> &DeviceRef {
        &self.device
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    /// `DeviceIdentity::sign`: the signature as hex.
    pub fn sign(&self, signing_input: &[u8]) -> String {
        hex(self.key.sign(signing_input).as_ref())
    }
}

/// `identity::verify`.
fn verify(public_key: &[u8], signing_input: &[u8], signature: &[u8]) -> Result<(), PmError> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
        .verify(signing_input, signature)
        .map_err(|_| {
            PmError::new(
                "signature_invalid",
                "the host's confirmation did not verify",
            )
        })
}

// ---- transcript math (core/src/devicelink/pairing.rs) -------------------------------------------

/// `Parties`: everything both sides agree on before the nonces.
pub struct Parties {
    pub pairing_id: [u8; 16],
    pub host: DeviceRef,
    pub host_key: [u8; 32],
    pub host_tls_pin: [u8; 32],
    pub host_link_port: u16,
    pub joiner: DeviceRef,
    pub joiner_key: [u8; 32],
    pub joiner_platform: &'static str,
}

impl Parties {
    /// `Parties::commitment`.
    pub fn commitment(&self, joiner_nonce: &[u8; 32]) -> [u8; 32] {
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

    /// `Parties::transcript_hash`.
    pub fn transcript_hash(&self, host_nonce: &[u8; 32], joiner_nonce: &[u8; 32]) -> [u8; 32] {
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

/// `code_for`: the eight-digit code, 64 bits of SHA-256 modulo 10^8.
pub fn code_for(transcript_hash: &[u8; 32]) -> String {
    let digest = sha256(&Enc::new(SAS_DOMAIN).bytes(transcript_hash).finish());
    let mut word = [0u8; 8];
    word.copy_from_slice(&digest[..8]);
    let modulus = 10u64.pow(CODE_DIGITS as u32);
    format!(
        "{:0width$}",
        u64::from_be_bytes(word) % modulus,
        width = CODE_DIGITS
    )
}

/// `confirm_input`.
pub fn confirm_input(role: &str, transcript_hash: &[u8; 32]) -> Vec<u8> {
    Enc::new(CONFIRM_DOMAIN)
        .str(role)
        .bytes(transcript_hash)
        .finish()
}

/// `key_fingerprint`: grouped hex of the first 96 bits (the owner compares it across two screens).
pub fn key_fingerprint(public_key: &[u8; 32]) -> String {
    let digest = sha256(&Enc::new(FINGERPRINT_DOMAIN).bytes(public_key).finish());
    let full = hex(&digest[..12]);
    full.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `normalize_code`: digits, with spaces and dashes ignored; exactly eight digits.
pub fn normalize_code(typed: &str) -> Result<String, PmError> {
    let mut digits = String::with_capacity(CODE_DIGITS);
    for ch in typed.chars() {
        match ch {
            '0'..='9' => digits.push(ch),
            ' ' | '-' => {}
            _ => return Err(PmError::malformed("code characters")),
        }
    }
    if digits.len() != CODE_DIGITS {
        return Err(PmError::malformed("code length"));
    }
    Ok(digits)
}

/// `check_endpoint`: a pin and a port the Host would actually serve.
fn check_endpoint(pin: &[u8; 32], port: u16) -> Result<(), PmError> {
    if pin.iter().all(|b| *b == 0) {
        return Err(PmError::malformed("tls pin"));
    }
    if port == 0 || port == u16::MAX {
        return Err(PmError::malformed("link port"));
    }
    Ok(())
}

/// `check_version`.
fn check_version(version: u32) -> Result<(), PmError> {
    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(PmError::new(
            "schema_version",
            format!("unsupported pairing version {version}"),
        ))
    }
}

/// `checked_peer`: the id must be the one the key derives.
fn checked_peer(device: &DeviceRef, key_hex: &str) -> Result<[u8; 32], PmError> {
    let key = unhex::<32>(key_hex)?;
    if device.device_id != device_id_for_key(&key) {
        return Err(PmError::malformed("device id does not match its key"));
    }
    Ok(key)
}

/// `check_enrol_code`: printable ASCII, no whitespace, sane length.
fn check_enrol_code(code: &str) -> Result<(), PmError> {
    if code.is_empty() || code.len() > 128 || !code.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(PmError::malformed("enrolment code"));
    }
    Ok(())
}

/// `check_confirm`: the Host's signed confirmation over the transcript hash.
fn check_confirm(
    msg: &PairConfirm,
    role: &str,
    pairing_id: &[u8; 16],
    signer: &DeviceRef,
    signer_key: &[u8; 32],
    transcript_hash: &[u8; 32],
) -> Result<(), PmError> {
    check_version(msg.schema_version)?;
    if unhex::<16>(&msg.pairing_id)? != *pairing_id || msg.device_id != signer.device_id {
        return Err(PmError::malformed("confirmation names another pairing"));
    }
    let signature = unhex::<64>(&msg.signature)?;
    verify(
        signer_key,
        &confirm_input(role, transcript_hash),
        &signature,
    )
}

// ---- messages (pairing.rs structs; framing: pairing_channel.rs `Frame`) -------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairOffer {
    pub schema_version: u32,
    pub pairing_id: String,
    pub host: DeviceRef,
    pub host_key: String,
    /// Hex; over the mesh it holds the host's endpoint id (docs/mesh-phone.md).
    pub host_tls_pin: String,
    pub host_link_port: u16,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairAccept {
    pub schema_version: u32,
    pub pairing_id: String,
    pub joiner: DeviceRef,
    pub joiner_key: String,
    pub platform: String,
    pub commitment: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairHostNonce {
    pub schema_version: u32,
    pub pairing_id: String,
    pub nonce: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairReveal {
    pub schema_version: u32,
    pub pairing_id: String,
    pub nonce: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairConfirm {
    pub schema_version: u32,
    pub pairing_id: String,
    pub device_id: String,
    pub signature: String,
}

/// The Host's enrolment message. Over the mesh it carries a placeholder code; the phone only
/// validates it (`receive_enrol`) and drops it.
#[derive(Clone, Serialize, Deserialize)]
pub struct PairEnrol {
    pub schema_version: u32,
    pub pairing_id: String,
    pub enrol_code: String,
    pub link_port: u16,
    /// The endorsement batch the host owner's owner-check signature made when it admitted this
    /// phone (`circle::EndorsementBatch`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admit: Option<crate::circle::EndorsementBatch>,
    /// A join token or QR: `admit` is then the batch whose grant the owner check signed when the
    /// token was made, and this is the host's device-key redemption of it by this phone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redemption: Option<crate::circle::Redemption>,
}

/// The joiner's confirmation plus the proof that it knows the QR's single-use secret.
#[derive(Serialize, Deserialize)]
struct JoinProofMsg {
    confirm: PairConfirm,
    proof: String,
}

/// `join_verifier(secret)` then `HMAC-SHA256(verifier, Enc(JOIN_PROOF_DOMAIN).bytes(transcript))`,
/// byte for byte what the computer checks.
fn join_proof(secret: &[u8; 32], transcript_hash: &[u8; 32]) -> [u8; 32] {
    let verifier = sha256(&Enc::new(JOIN_VERIFIER_DOMAIN).bytes(secret).finish());
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &verifier);
    let tag = ring::hmac::sign(
        &key,
        &Enc::new(JOIN_PROOF_DOMAIN).bytes(transcript_hash).finish(),
    );
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "msg", rename_all = "kebab-case")]
enum Frame {
    Offer(PairOffer),
    Accept(PairAccept),
    HostNonce(PairHostNonce),
    Reveal(PairReveal),
    JoinerConfirm(PairConfirm),
    JoinerJoinConfirm(JoinProofMsg),
    HostConfirm(PairConfirm),
    Enrol(PairEnrol),
}

// ---- the session ---------------------------------------------------------------------------------

/// What the phone shows before asking for the code (`JoinPrompt` in pairing_channel.rs).
#[derive(Clone, Debug)]
pub struct Prompt {
    pub pairing_id: String,
    pub host: DeviceRef,
    pub host_fingerprint: String,
    pub expires_at_ms: u64,
}

/// `JoinerPairing::check_live`: a clock that moves back past the skew is refused; so is expiry.
struct Live {
    seen_ms: u64,
    expires_at_ms: u64,
}

impl Live {
    fn check(&mut self) -> Result<(), PmError> {
        let now = now_ms();
        if now.saturating_add(CLOCK_SKEW_MS) < self.seen_ms {
            return Err(PmError::new(
                "clock_rolled_back",
                "the clock moved backwards",
            ));
        }
        self.seen_ms = self.seen_ms.max(now);
        if now >= self.expires_at_ms {
            Err(PmError::new("expired", "the pairing expired"))
        } else {
            Ok(())
        }
    }

    fn remaining(&self) -> Duration {
        Duration::from_millis(self.expires_at_ms.saturating_sub(now_ms()).max(1))
    }
}

pub(crate) async fn within<T>(
    wait: Duration,
    work: impl std::future::Future<Output = Result<T, PmError>>,
) -> Result<T, PmError> {
    match tokio::time::timeout(wait, work).await {
        Ok(result) => result,
        Err(_) => Err(PmError::retry("timeout", "timed out")),
    }
}

/// `display_safe`: a peer-chosen label with control and bidirectional-override characters removed
/// and the length capped. Attacker-controlled: identification comes from the id and fingerprint.
pub fn display_safe(name: Option<&str>) -> String {
    let cleaned: String = name
        .unwrap_or("")
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .take(48)
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "(unnamed device)".to_string()
    } else {
        cleaned
    }
}

/// `Wire::write`: one JSON object per line.
async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> Result<(), PmError> {
    let mut line = serde_json::to_vec(frame).map_err(|e| PmError::new("io", e.to_string()))?;
    line.push(b'\n');
    w.write_all(&line)
        .await
        .map_err(|e| PmError::retry("io", e.to_string()))?;
    w.flush()
        .await
        .map_err(|e| PmError::retry("io", e.to_string()))
}

/// `Wire::read`: one line of at most `MAX_LINE`.
async fn read_frame<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Frame, PmError> {
    let mut line = Vec::new();
    let read = AsyncBufReadExt::read_until(
        &mut AsyncReadExt::take(&mut *r, MAX_LINE + 1),
        b'\n',
        &mut line,
    )
    .await
    .map_err(|e| PmError::retry("io", e.to_string()))?;
    if read == 0 {
        return Err(PmError::new(
            "closed",
            "the computer closed the pairing (declined, expired or no pairing is open)",
        ));
    }
    if line.last() != Some(&b'\n') {
        return Err(PmError::malformed("line too long or cut short"));
    }
    serde_json::from_slice(&line).map_err(|_| PmError::malformed("malformed message"))
}

/// A pairing whose confirmation was sent and whose answer has not arrived: everything needed to
/// repeat the same confirmation on a new stream and verify the host's final messages. Holds no
/// key material beyond the public signature already sent (the identity seed stays in the keychain
/// path of the app); kept in memory and in the state directory, dropped on any end.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InFlight {
    pub pairing_id: String,
    pub host: DeviceRef,
    pub host_key: String,
    pub transcript_hash: String,
    pub confirm: PairConfirm,
    pub link_port: u16,
    pub relay: Option<String>,
    /// The repeat is refused by the host after this moment.
    pub resume_until_ms: u64,
    /// SHA-256 of the QR's token secret (hex), when the QR carried one: what the host's token grant
    /// must name. The secret itself is never kept.
    #[serde(default)]
    pub secret_hash: Option<String>,
}

/// `check_version`, `link_port` and the placeholder code of the host's enrolment message.
///
/// Returns the host's owner-check key when its enrolment carries the owner's signature that
/// admitted THIS phone in THIS pairing (`circle::host_admit_check`); the phone pins it.
#[allow(clippy::too_many_arguments)]
fn check_enrol(
    enrol: &PairEnrol,
    pairing_id: &[u8; 16],
    link_port: u16,
    me: &Identity,
    transcript_hash: &[u8; 32],
    host: &DeviceRef,
    host_key: &[u8; 32],
    secret_hash: Option<&str>,
) -> Result<Option<crate::circle::CheckKey>, PmError> {
    check_version(enrol.schema_version)?;
    if unhex::<16>(&enrol.pairing_id)? != *pairing_id {
        return Err(PmError::malformed("another pairing"));
    }
    if enrol.link_port != link_port {
        return Err(PmError::malformed(
            "link port differs from the confirmed offer",
        ));
    }
    check_enrol_code(&enrol.enrol_code)?;
    // A token or QR: the host's owner signed a grant, and its device key redeemed it for this phone
    // (`circle::host_grant_check`). Only a QR secret this phone holds can match the grant.
    if let (Some(batch), Some(redemption), Some(secret_hash)) =
        (&enrol.admit, &enrol.redemption, secret_hash)
    {
        return Ok(crate::circle::host_grant_check(
            batch,
            redemption,
            &me.device().device_id,
            &hex(&me.public_key()),
            &host.device_id,
            &hex(host_key),
            &hex(pairing_id),
            secret_hash,
            now_ms(),
        ));
    }
    Ok(enrol.admit.as_ref().and_then(|batch| {
        crate::circle::host_admit_check(
            batch,
            &me.device().device_id,
            &hex(&me.public_key()),
            &host.device_id,
            &hex(host_key),
            &hex(transcript_hash),
            now_ms(),
        )
    }))
}

/// Repeat the confirmation of `state` on a new stream the mesh dialled to the same endpoint
/// (`dialled`): read the host's offer (it must be this pairing, this key), send the SAME
/// confirmation instead of an accept, then read the host's confirmation and enrolment again.
/// A host that no longer holds the pairing closes the stream: `closed`, which is final. The offer's
/// own expiry is not checked (the host's cache window governs); `state.resume_until_ms` bounds us.
pub async fn resume<R, W>(
    r: &mut R,
    w: &mut W,
    dialled: [u8; 32],
    state: &InFlight,
    me: &Identity,
) -> Result<Joined, PmError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let remaining = || {
        let left = state.resume_until_ms.saturating_sub(now_ms());
        if left == 0 {
            Err(PmError::new(
                "expired",
                "the pairing could not be resumed in time",
            ))
        } else {
            Ok(Duration::from_millis(left))
        }
    };
    let pairing_id = unhex::<16>(&state.pairing_id)?;
    let host_key = unhex::<32>(&state.host_key)?;
    let transcript_hash = unhex::<32>(&state.transcript_hash)?;
    if !ct_eq(&host_key, &dialled) {
        return Err(PmError::malformed("resume dialled another key"));
    }
    let Frame::Offer(offer) =
        within(Duration::from_secs(15).min(remaining()?), read_frame(r)).await?
    else {
        return Err(PmError::malformed("expected offer"));
    };
    check_version(offer.schema_version)?;
    if !ct_eq(&unhex::<32>(&offer.host_key)?, &host_key)
        || !ct_eq(&unhex::<32>(&offer.host_tls_pin)?, &host_key)
        || !ct_eq(&unhex::<16>(&offer.pairing_id)?, &pairing_id)
        || offer.host_link_port != state.link_port
    {
        return Err(PmError::malformed(
            "the offer is not the pairing being resumed",
        ));
    }
    within(
        remaining()?,
        write_frame(w, &Frame::JoinerConfirm(state.confirm.clone())),
    )
    .await?;
    let admit_check = receive_final(
        r,
        remaining()?,
        &pairing_id,
        &state.host,
        &host_key,
        &transcript_hash,
        state.link_port,
        me,
        state.secret_hash.as_deref(),
    )
    .await?;
    Ok((state.host.clone(), host_key, admit_check))
}

/// A finished pairing: the host, its key and its owner-check key when its owner's signature
/// admitted this phone (`circle::host_admit_check`).
pub type Joined = (DeviceRef, [u8; 32], Option<crate::circle::CheckKey>);

/// `finish` and `receive_enrol`: the host's signed confirmation, then its enrolment message.
async fn receive_final<R: AsyncBufRead + Unpin>(
    r: &mut R,
    wait: Duration,
    pairing_id: &[u8; 16],
    host: &DeviceRef,
    host_key: &[u8; 32],
    transcript_hash: &[u8; 32],
    link_port: u16,
    me: &Identity,
    secret_hash: Option<&str>,
) -> Result<Option<crate::circle::CheckKey>, PmError> {
    let Frame::HostConfirm(host_confirm) = within(wait, read_frame(r)).await? else {
        return Err(PmError::malformed("expected host confirm"));
    };
    check_confirm(
        &host_confirm,
        ROLE_HOST,
        pairing_id,
        host,
        host_key,
        transcript_hash,
    )?;
    let Frame::Enrol(enrol) = within(wait, read_frame(r)).await? else {
        return Err(PmError::malformed("expected enrolment"));
    };
    check_enrol(
        &enrol,
        pairing_id,
        link_port,
        me,
        transcript_hash,
        host,
        host_key,
        secret_hash,
    )
}

/// The Joiner's message flow: `join_session` in pairing_channel.rs with `JoinerPairing`
/// (`accept_offer`, `receive_host_nonce`, `enter_code`, `finish`, `receive_enrol`) inlined in
/// order. `dialled` is the endpoint the mesh authenticated; the offer must name it as host key and
/// as pin. `expected_pairing` is the id from the scanned URI. A prompt is sent once the host nonce
/// is revealed; the typed code arrives on `code_rx`.
pub async fn join<R, W>(
    r: &mut R,
    w: &mut W,
    identity: &Identity,
    dialled: [u8; 32],
    expected_pairing: [u8; 16],
    prompt_tx: &mut Option<oneshot::Sender<Result<Prompt, PmError>>>,
    code_rx: oneshot::Receiver<String>,
    secret: Option<[u8; 32]>,
    relay: Option<String>,
    keep_inflight: &mut (dyn FnMut(InFlight) + Send),
) -> Result<Joined, PmError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Step 1, `join_session` + `accept_offer`.
    let Frame::Offer(offer) = within(Duration::from_secs(15), read_frame(r)).await? else {
        return Err(PmError::malformed("expected offer"));
    };
    let host_key = unhex::<32>(&offer.host_key)?;
    if !ct_eq(&host_key, &dialled) {
        return Err(PmError::malformed(
            "the offer names another key than the endpoint dialled",
        ));
    }
    check_version(offer.schema_version)?;
    let pin = unhex::<32>(&offer.host_tls_pin)?;
    check_endpoint(&pin, offer.host_link_port)?;
    // `TlsPinMismatch`: the pin must be the dialled endpoint id.
    if !ct_eq(&pin, &dialled) {
        return Err(PmError::new(
            "pin_mismatch",
            "the offer's pin is not the dialled key",
        ));
    }
    if offer.host_link_port != MESH_LINK_PORT {
        return Err(PmError::malformed("link port"));
    }
    let now = now_ms();
    if now >= offer.expires_at_ms {
        return Err(PmError::new("expired", "the pairing expired"));
    }
    if offer.expires_at_ms > now.saturating_add(PAIRING_TTL_MS) {
        return Err(PmError::malformed("offer lifetime"));
    }
    let pairing_id = unhex::<16>(&offer.pairing_id)?;
    if !ct_eq(&pairing_id, &expected_pairing) {
        return Err(PmError::malformed(
            "the offer is for another pairing than the QR",
        ));
    }
    let checked_host = checked_peer(&offer.host, &offer.host_key)?;
    if offer.host.device_id == identity.device().device_id {
        return Err(PmError::malformed("cannot pair a device with itself"));
    }
    let joiner_nonce = random::<32>()?;
    let parties = Parties {
        pairing_id,
        host: offer.host.clone(),
        host_key: checked_host,
        host_tls_pin: pin,
        host_link_port: offer.host_link_port,
        joiner: identity.device().clone(),
        joiner_key: identity.public_key(),
        joiner_platform: JOINER_PLATFORM,
    };
    let mut live = Live {
        seen_ms: now,
        expires_at_ms: offer.expires_at_ms,
    };
    let accept = PairAccept {
        schema_version: SCHEMA_VERSION,
        pairing_id: offer.pairing_id.clone(),
        joiner: identity.device().clone(),
        joiner_key: hex(&identity.public_key()),
        platform: JOINER_PLATFORM.to_string(),
        commitment: hex(&parties.commitment(&joiner_nonce)),
    };
    within(live.remaining(), write_frame(w, &Frame::Accept(accept))).await?;

    // Step 3, `receive_host_nonce`.
    live.check()?;
    let Frame::HostNonce(nonce) = within(live.remaining(), read_frame(r)).await? else {
        return Err(PmError::malformed("expected host nonce"));
    };
    live.check()?;
    check_version(nonce.schema_version)?;
    if unhex::<16>(&nonce.pairing_id)? != pairing_id {
        return Err(PmError::malformed("another pairing"));
    }
    let host_nonce = unhex::<32>(&nonce.nonce)?;
    let transcript_hash = parties.transcript_hash(&host_nonce, &joiner_nonce);
    let reveal = PairReveal {
        schema_version: SCHEMA_VERSION,
        pairing_id: hex(&pairing_id),
        nonce: hex(&joiner_nonce),
    };
    within(live.remaining(), write_frame(w, &Frame::Reveal(reveal))).await?;

    // Step 5: show the host, wait for the typed code.
    if let Some(tx) = prompt_tx.take() {
        let _ = tx.send(Ok(Prompt {
            pairing_id: offer.pairing_id.clone(),
            host: offer.host.clone(),
            host_fingerprint: key_fingerprint(&host_key),
            expires_at_ms: offer.expires_at_ms,
        }));
    }
    // A QR with a single-use secret needs no typed code: the proof below is the phone's whole
    // proof. The receiver stays alive so the app's one `pair_code` call still reaches its wait.
    let _code_rx_kept;
    if secret.is_some() {
        _code_rx_kept = code_rx;
    } else {
        let typed = match tokio::time::timeout(live.remaining(), code_rx).await {
            Ok(Ok(code)) => code,
            _ => return Err(PmError::new("no_code", "the code was not entered")),
        };

        // `enter_code`: a wrong code voids the session.
        live.check()?;
        let typed = normalize_code(&typed)?;
        if !ct_eq(typed.as_bytes(), code_for(&transcript_hash).as_bytes()) {
            return Err(PmError::new("code_mismatch", "that code does not match"));
        }
    }
    live.check()?;
    // `sign_confirm` (role joiner).
    let confirm = PairConfirm {
        schema_version: SCHEMA_VERSION,
        pairing_id: hex(&pairing_id),
        device_id: identity.device().device_id.clone(),
        signature: identity.sign(&confirm_input(ROLE_JOINER, &transcript_hash)),
    };
    // From here the host may approve and record us before the answer reaches us: keep what a
    // repeat of this same confirmation needs, BEFORE it is sent.
    keep_inflight(InFlight {
        pairing_id: hex(&pairing_id),
        host: offer.host.clone(),
        host_key: hex(&host_key),
        transcript_hash: hex(&transcript_hash),
        confirm: confirm.clone(),
        link_port: offer.host_link_port,
        relay,
        resume_until_ms: now_ms().saturating_add(RESUME_WINDOW_MS),
        secret_hash: secret.map(|s| hex(&sha256(&s))),
    });
    let confirm_frame = match secret {
        Some(secret) => Frame::JoinerJoinConfirm(JoinProofMsg {
            confirm,
            proof: hex(&join_proof(&secret, &transcript_hash)),
        }),
        None => Frame::JoinerConfirm(confirm),
    };
    within(live.remaining(), write_frame(w, &confirm_frame)).await?;

    // The owner approves in their own time, within the session. `finish`, `receive_enrol`.
    let admit_check = receive_final(
        r,
        live.remaining(),
        &pairing_id,
        &offer.host,
        &host_key,
        &transcript_hash,
        offer.host_link_port,
        identity,
        secret.map(|s| hex(&sha256(&s))).as_deref(),
    )
    .await?;
    live.check()?;
    Ok((offer.host, host_key, admit_check))
}

// ---- the scanned URI (docs/mesh-phone.md, "Pairing" step 2) -------------------------------------

/// A parsed pairing URI: `<scheme>://pair?v=1&e=<endpoint 64 hex>&p=<pairing id 32 hex>&x=<expiry
/// ms>&r=<relay, percent-encoded, optional>&n=<computer name, optional>`. Public data only.
#[derive(Clone, Debug)]
pub struct PairUri {
    pub endpoint: [u8; 32],
    pub pairing_id: [u8; 16],
    pub expires_at_ms: u64,
    pub relay: Option<String>,
    pub name: Option<String>,
    /// `a=`: the computer's direct addresses on its network (comma-separated ip:port).
    pub addrs: Vec<std::net::SocketAddr>,
    /// `s=`: the single-use join secret (32 bytes as 64 hex). With it the phone proves itself and
    /// types no code.
    pub secret: Option<[u8; 32]>,
}

fn percent_decode(text: &str) -> Result<String, PmError> {
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'%' => {
                let pair = text
                    .get(i + 1..i + 3)
                    .ok_or_else(|| PmError::malformed("percent escape"))?;
                out.push(unhex::<1>(pair)?[0]);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| PmError::malformed("utf-8"))
}

/// Longest relay URL the desktop publishes or a join token carries (`MAX_RELAY_CHARS` in
/// join_token.rs, `relay_setting` in engine/src/devicelink_host.rs).
const MAX_RELAY_CHARS: usize = 256;
/// Longest scanned pairing text the app accepts (`PairingURI.check` in MeshLink.swift).
const MAX_PAIR_URI_BYTES: usize = 2048;

/// Checks the QR's relay before anything dials it (docs/contracts/device-link.md L24, review R22):
/// `https` only, at most 256 characters, a DNS host name (letters, digits and hyphens in dotted
/// labels, so no IP literal, no `localhost`, no single-label name) with an optional port, no
/// user info, query, fragment or backslash, and no path beyond `/`. This is the desktop's own
/// `https` and length bound plus the host shape; it returns the URL unchanged.
///
/// It cannot show that the URL is the one the computer advertises (`deviceLinkRelayUrl`): the
/// phone has no authenticated copy of that value before the dial (docs/device-link.md,
/// "Phone QR").
pub fn validate_relay(url: &str) -> Result<String, PmError> {
    let bad = |why: &str| {
        PmError::new(
            "invalid_relay",
            format!("the pairing code names an unusable relay ({why})"),
        )
    };
    if url.is_empty() || url.len() > MAX_RELAY_CHARS {
        return Err(bad("length"));
    }
    if !url.is_ascii()
        || url
            .bytes()
            .any(|b| b.is_ascii_control() || b == b' ' || b == b'\\')
    {
        return Err(bad("characters"));
    }
    let rest = match url.get(..8) {
        Some(p) if p.eq_ignore_ascii_case("https://") => &url[8..],
        _ => return Err(bad("not https")),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if !(path.is_empty() || path == "/") {
        return Err(bad("path"));
    }
    if authority
        .bytes()
        .any(|b| matches!(b, b'@' | b'?' | b'#' | b'[' | b']' | b'%'))
    {
        return Err(bad("authority"));
    }
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if let Some(port) = port {
        let digits_only =
            !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit());
        if !digits_only || port.parse::<u16>().ok().filter(|p| *p != 0).is_none() {
            return Err(bad("port"));
        }
    }
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    // A numeric last label would make an IPv4 address (or a shorthand for one).
    let last_numeric = labels
        .last()
        .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()));
    if host.len() > 253 || labels.len() < 2 || !labels.iter().all(|l| label_ok(*l)) || last_numeric
    {
        return Err(bad("host"));
    }
    Ok(url.to_string())
}

pub fn parse_pair_uri(uri: &str) -> Result<PairUri, PmError> {
    let invalid = || PmError::new("invalid_uri", "that is not a pairing code");
    if uri.len() > MAX_PAIR_URI_BYTES {
        return Err(invalid());
    }
    let (scheme, rest) = uri.trim().split_once("://").ok_or_else(invalid)?;
    if scheme.is_empty() {
        return Err(invalid());
    }
    let query = rest.strip_prefix("pair?").ok_or_else(invalid)?;
    let (mut v, mut e, mut p, mut x, mut r, mut n) = (None, None, None, None, None, None);
    let mut a = None;
    let mut sec = None;
    for field in query.split('&') {
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        // A repeated field could make two readers disagree about which value counts.
        let slot = match key {
            "v" => &mut v,
            "e" => &mut e,
            "p" => &mut p,
            "x" => &mut x,
            "r" => &mut r,
            "n" => &mut n,
            "a" => &mut a,
            "s" => &mut sec,
            _ => continue,
        };
        if slot.replace(value).is_some() {
            return Err(invalid());
        }
    }
    if v != Some("1") {
        return Err(PmError::new(
            "invalid_uri",
            "unsupported pairing code version",
        ));
    }
    let endpoint = unhex::<32>(e.ok_or_else(invalid)?).map_err(|_| invalid())?;
    let pairing_id = unhex::<16>(p.ok_or_else(invalid)?).map_err(|_| invalid())?;
    let expires_at_ms = x
        .ok_or_else(invalid)?
        .parse::<u64>()
        .map_err(|_| invalid())?;
    let relay = match r {
        Some(text) if !text.is_empty() => Some(validate_relay(&percent_decode(text)?)?),
        _ => None,
    };
    let name = match n {
        Some(text) if !text.is_empty() => Some(percent_decode(text)?),
        _ => None,
    };
    // Unparseable entries are skipped: addresses only help the dial, they are never trusted.
    let addrs = match a {
        Some(text) if !text.is_empty() => percent_decode(text)?
            .split(',')
            .filter_map(|item| item.trim().parse().ok())
            .take(8)
            .collect(),
        _ => Vec::new(),
    };
    Ok(PairUri {
        endpoint,
        pairing_id,
        expires_at_ms,
        relay,
        name,
        addrs,
        secret: match sec {
            Some(text) if !text.is_empty() => Some(unhex::<32>(text).map_err(|_| invalid())?),
            _ => None,
        },
    })
}
