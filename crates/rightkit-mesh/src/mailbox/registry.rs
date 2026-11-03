//! Customer-grade relay admission (#952): a registry of devices a relay may admit, with
//! registration by proof of work, a pairing attestation from an existing member of the circle,
//! eviction that takes effect without a relay restart, and quotas per circle and per network
//! prefix.
//!
//! The relay (iroh-relay) asks "may this endpoint id connect?" once per connection, so every rule
//! here is enforced at that moment: [`AdmissionRegistry::admits`] answers from memory, and an
//! eviction is persisted and effective for the next connection attempt. A connection that is
//! already up is not cut (iroh-relay 1.4.1 has no per-connection revocation hook and cannot
//! meter bytes per endpoint); a revocation that must cut a live device also restarts the relay.
//!
//! Rules of [`AdmissionRegistry::register`]:
//! * the endpoint id is 64 hex (an Ed25519 public key) and not evicted;
//! * a circle that already has members admits a new device only with an attestation: an Ed25519
//!   signature, made by a current member with the key its endpoint id names, over
//!   [`attestation_input`] (this is the pairing proof, the member vouching for the new device);
//! * a new circle needs only the proof of work ([`pow_ok`], `Limits::pow_bits` leading zero bits
//!   of SHA-256 over the domain, endpoint id, circle id and the nonce) and counts against the
//!   per-prefix quota of the caller's /24 (IPv4) or /48 (IPv6);
//! * a circle holds at most `Limits::max_devices_per_circle` devices, the registry at most
//!   `max_circles` circles and `max_devices` devices.
//!
//! State is one JSON file, replaced atomically after every change. Events are counts only; no
//! endpoint id is ever logged.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use ring::digest::{SHA256, digest};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

use super::admission::is_endpoint_id;
use rightkit_fs::atomic_write_private;

const POW_DOMAIN: &[u8] = b"rightkit-relay-register-v1\0";
const ATTEST_DOMAIN: &[u8] = b"rightkit-relay-attest-v1\0";
const STATE_VERSION: u32 = 1;
/// Longest circle id.
pub const MAX_CIRCLE_ID: usize = 64;

/// Quotas and the proof-of-work difficulty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Leading zero bits the registration hash must have (2^bits hashes on average).
    pub pow_bits: u32,
    /// Devices one circle may hold.
    pub max_devices_per_circle: usize,
    /// Devices registered through unattested (new-circle) registrations from one network prefix.
    pub max_devices_per_prefix: usize,
    /// Circles the registry holds.
    pub max_circles: usize,
    /// Devices the registry holds.
    pub max_devices: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            pow_bits: 20,
            max_devices_per_circle: 16,
            max_devices_per_prefix: 8,
            max_circles: 4096,
            max_devices: 32768,
        }
    }
}

/// One registration request.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistrationRequest {
    /// The device's endpoint id, 64 hex.
    pub endpoint_id: String,
    /// The circle to join or create: 1 to 64 ASCII letters, digits, `-` or `_`.
    pub circle_id: String,
    /// The proof-of-work nonce (required unless an attestation is given).
    #[serde(default)]
    pub pow_nonce: u64,
    /// The endpoint id of the circle member vouching for this device.
    #[serde(default)]
    pub attested_by: Option<String>,
    /// Hex of the member's Ed25519 signature over [`attestation_input`].
    #[serde(default)]
    pub attestation: Option<String>,
}

/// Why a registration was refused. [`RegisterError::code`] is the stable wire word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    BadEndpointId,
    BadCircleId,
    /// The endpoint id was evicted and has not been readmitted.
    Evicted,
    /// The endpoint id is registered in a different circle.
    InOtherCircle,
    PowTooWeak,
    /// The circle has members and the request carries no attestation.
    AttestationRequired,
    /// The attestation is malformed, from a non-member, or does not verify.
    AttestationInvalid,
    CircleFull,
    PrefixFull,
    CapacityFull,
    /// The change could not be written; nothing was changed.
    Persist(String),
}

impl RegisterError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadEndpointId => "bad_endpoint_id",
            Self::BadCircleId => "bad_circle_id",
            Self::Evicted => "evicted",
            Self::InOtherCircle => "in_other_circle",
            Self::PowTooWeak => "pow_too_weak",
            Self::AttestationRequired => "attestation_required",
            Self::AttestationInvalid => "attestation_invalid",
            Self::CircleFull => "circle_full",
            Self::PrefixFull => "prefix_full",
            Self::CapacityFull => "capacity_full",
            Self::Persist(_) => "persist_failed",
        }
    }
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Persist(message) => write!(f, "could not save the registry: {message}"),
            other => f.write_str(other.code()),
        }
    }
}

impl std::error::Error for RegisterError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    circle: String,
    /// Network prefix of an unattested registration; counts against the per-prefix quota.
    #[serde(default)]
    prefix: Option<String>,
    at_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    version: u32,
    devices: HashMap<String, Entry>,
    evicted: HashSet<String>,
}

/// The registry. One per relay admission service.
pub struct AdmissionRegistry {
    path: Option<PathBuf>,
    limits: Limits,
    state: Mutex<State>,
}

/// The bytes a circle member signs to vouch for `endpoint_id` joining `circle_id`.
pub fn attestation_input(circle_id: &str, endpoint_id: &str) -> Vec<u8> {
    let mut input = ATTEST_DOMAIN.to_vec();
    input.extend_from_slice(circle_id.as_bytes());
    input.push(0);
    input.extend_from_slice(endpoint_id.to_ascii_lowercase().as_bytes());
    input
}

/// Whether `nonce` is a valid proof of work for this registration at `bits` leading zero bits.
pub fn pow_ok(endpoint_id: &str, circle_id: &str, nonce: u64, bits: u32) -> bool {
    let mut input = POW_DOMAIN.to_vec();
    input.extend_from_slice(endpoint_id.to_ascii_lowercase().as_bytes());
    input.push(0);
    input.extend_from_slice(circle_id.as_bytes());
    input.push(0);
    input.extend_from_slice(&nonce.to_be_bytes());
    leading_zero_bits(digest(&SHA256, &input).as_ref()) >= bits
}

fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in bytes {
        if *byte == 0 {
            bits += 8;
        } else {
            bits += byte.leading_zeros();
            break;
        }
    }
    bits
}

/// The quota prefix of a client address: the /24 of an IPv4 address, the /48 of an IPv6 one.
pub fn network_prefix(address: &std::net::IpAddr) -> String {
    match address {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return network_prefix(&std::net::IpAddr::V4(v4));
            }
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}::/48", s[0], s[1], s[2])
        }
    }
}

fn valid_circle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_CIRCLE_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

impl AdmissionRegistry {
    /// An in-memory registry that persists nothing.
    pub fn new(limits: Limits) -> Self {
        Self {
            path: None,
            limits,
            state: Mutex::new(State {
                version: STATE_VERSION,
                ..State::default()
            }),
        }
    }

    /// Opens the registry stored at `path`, creating an empty one when the file is missing. An
    /// unreadable or malformed file is an error (fail closed): the service must not start with
    /// an empty list in place of the real one.
    pub fn open(path: &Path, limits: Limits) -> Result<Self, String> {
        let state = match std::fs::read(path) {
            Ok(bytes) => {
                let state: State = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                if state.version != STATE_VERSION {
                    return Err(format!(
                        "{}: unsupported version {}",
                        path.display(),
                        state.version
                    ));
                }
                state
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State {
                version: STATE_VERSION,
                ..State::default()
            },
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Self {
            path: Some(path.to_path_buf()),
            limits,
            state: Mutex::new(state),
        })
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Whether the relay may admit `endpoint_id` now: registered and not evicted.
    pub fn admits(&self, endpoint_id: &str) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let id = endpoint_id.to_ascii_lowercase();
        state.devices.contains_key(&id) && !state.evicted.contains(&id)
    }

    /// Registers a device. `prefix` is the caller's [`network_prefix`]; `now_ms` is the clock.
    /// Registering an id already in the same circle succeeds without changing anything.
    pub fn register(
        &self,
        request: &RegistrationRequest,
        prefix: &str,
        now_ms: u64,
    ) -> Result<(), RegisterError> {
        if !is_endpoint_id(&request.endpoint_id) {
            return Err(RegisterError::BadEndpointId);
        }
        if !valid_circle_id(&request.circle_id) {
            return Err(RegisterError::BadCircleId);
        }
        let id = request.endpoint_id.to_ascii_lowercase();
        let circle = request.circle_id.as_str();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);

        if state.evicted.contains(&id) {
            return Err(RegisterError::Evicted);
        }
        if let Some(existing) = state.devices.get(&id) {
            return if existing.circle == circle {
                Ok(())
            } else {
                Err(RegisterError::InOtherCircle)
            };
        }

        let members: Vec<&String> = state
            .devices
            .iter()
            .filter(|(_, entry)| entry.circle == circle)
            .map(|(member, _)| member)
            .collect();
        let new_circle = members.is_empty();

        let counted_prefix = if new_circle {
            if !pow_ok(&id, circle, request.pow_nonce, self.limits.pow_bits) {
                return Err(RegisterError::PowTooWeak);
            }
            let circles: HashSet<&str> = state
                .devices
                .values()
                .map(|entry| entry.circle.as_str())
                .collect();
            if circles.len() >= self.limits.max_circles {
                return Err(RegisterError::CapacityFull);
            }
            let from_prefix = state
                .devices
                .values()
                .filter(|entry| entry.prefix.as_deref() == Some(prefix))
                .count();
            if from_prefix >= self.limits.max_devices_per_prefix {
                return Err(RegisterError::PrefixFull);
            }
            Some(prefix.to_string())
        } else {
            let (Some(by), Some(signature)) = (&request.attested_by, &request.attestation) else {
                return Err(RegisterError::AttestationRequired);
            };
            let by = by.to_ascii_lowercase();
            if !members.iter().any(|member| **member == by) || state.evicted.contains(&by) {
                return Err(RegisterError::AttestationInvalid);
            }
            let (Some(key), Some(signature)) = (unhex(&by), unhex(signature)) else {
                return Err(RegisterError::AttestationInvalid);
            };
            UnparsedPublicKey::new(&ED25519, &key)
                .verify(&attestation_input(circle, &id), &signature)
                .map_err(|_| RegisterError::AttestationInvalid)?;
            if members.len() >= self.limits.max_devices_per_circle {
                return Err(RegisterError::CircleFull);
            }
            None
        };
        if state.devices.len() >= self.limits.max_devices {
            return Err(RegisterError::CapacityFull);
        }

        state.devices.insert(
            id.clone(),
            Entry {
                circle: circle.to_string(),
                prefix: counted_prefix,
                at_ms: now_ms,
            },
        );
        if let Err(message) = self.persist(&state) {
            state.devices.remove(&id);
            return Err(RegisterError::Persist(message));
        }
        super::server::event("registry.register", state.devices.len() as u64);
        Ok(())
    }

    /// Evicts one device: it is removed and may not register again until [`readmit`](Self::readmit).
    /// Takes effect for the next relay connection with no relay restart. Returns whether the id
    /// was registered or newly tombstoned.
    pub fn evict(&self, endpoint_id: &str) -> Result<bool, String> {
        let id = endpoint_id.to_ascii_lowercase();
        if !is_endpoint_id(&id) {
            return Err("not a 64-hex endpoint id".to_string());
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let removed = state.devices.remove(&id);
        let tombstoned = state.evicted.insert(id.clone());
        if let Err(message) = self.persist(&state) {
            if let Some(entry) = removed {
                state.devices.insert(id.clone(), entry);
            }
            if tombstoned {
                state.evicted.remove(&id);
            }
            return Err(message);
        }
        super::server::event("registry.evict", 1);
        Ok(true)
    }

    /// Evicts every device of a circle. Returns how many were registered.
    pub fn evict_circle(&self, circle_id: &str) -> Result<usize, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let before = (state.devices.clone(), state.evicted.clone());
        let ids: Vec<String> = state
            .devices
            .iter()
            .filter(|(_, entry)| entry.circle == circle_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            state.devices.remove(id);
            state.evicted.insert(id.clone());
        }
        if let Err(message) = self.persist(&state) {
            state.devices = before.0;
            state.evicted = before.1;
            return Err(message);
        }
        super::server::event("registry.evict", ids.len() as u64);
        Ok(ids.len())
    }

    /// Lifts an eviction so the device may register again.
    pub fn readmit(&self, endpoint_id: &str) -> Result<bool, String> {
        let id = endpoint_id.to_ascii_lowercase();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.evicted.remove(&id) {
            return Ok(false);
        }
        if let Err(message) = self.persist(&state) {
            state.evicted.insert(id);
            return Err(message);
        }
        Ok(true)
    }

    /// Number of devices the registry admits.
    pub fn device_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .devices
            .len()
    }

    fn persist(&self, state: &State) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
        atomic_write_private(path, &bytes).map_err(|e| e.to_string())
    }
}
