//! Worker side of the RK8 wire: the GUI worker beside its target app.
//!
//! A worker serves one connection at a time. It answers the controller's
//! handshake, executes sealed commands through its [`StepDriver`], and keeps
//! three guards:
//!
//! - A grant fences each step. Commands whose fence is not the current grant
//!   get [`Reply::Fenced`], so a stale controller cannot drive a step. Fences
//!   are a high-water mark that survives reconnects and new sessions, so an
//!   old lease can never be re-granted.
//! - `perform` is deduplicated by idempotency key. A key already performed
//!   returns its recorded outcome and is never run again, even after the
//!   controller reconnects and re-sends the command.
//! - A bundle is kept only after its whole digest verifies. Partial bytes
//!   survive reconnects, so a transfer resumes at the offset the worker holds.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

use crate::driver::{IdempotencyKey, Observation, StepDriver};
use crate::machine::StepRef;
use crate::protocol::{Command, Handshake, Reply, WireObservation};
use crate::transport::{Capability, TransportError};
use crate::wire::{self, Channel, CONTROLLER_TO_WORKER, MAX_CHUNK_BYTES, WORKER_TO_CONTROLLER};

/// Default cap on one bundle's declared size.
pub const DEFAULT_MAX_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;
/// Most bundles a worker will hold partially at once.
pub const MAX_PARTIAL_BUNDLES: usize = 16;
/// Failed enrolment attempts a worker tolerates before it discards the code.
pub const MAX_ENROLMENT_ATTEMPTS: u32 = 3;
/// Shortest enrolment code accepted, in bytes.
const MIN_CODE_BYTES: usize = 32;
const MAX_BUNDLE_ID_BYTES: usize = 128;

/// Verified bundles held by a worker, keyed by bundle id.
pub type BundleStore = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

struct Device {
    worker_id: String,
    key: [u8; 32],
}

struct Partial {
    expected_digest: String,
    declared_bytes: u64,
    data: Vec<u8>,
}

pub struct WorkerEndpoint<D: StepDriver> {
    driver: D,
    label: String,
    capabilities: Vec<Capability>,
    device: Option<Device>,
    /// Enrolment id and code key, held only until enrolment completes.
    pending_enrolment: Option<(String, [u8; 32])>,
    enrolment_attempts: u32,
    session: Option<String>,
    applied: u64,
    /// Highest grant per step. Never reset, so a stale lease stays stale.
    fences: BTreeMap<String, u64>,
    /// Steps cancelled, with the fence the cancel was issued under. A later,
    /// higher grant supersedes the cancel.
    cancelled: BTreeMap<String, u64>,
    /// Outcome per idempotency key. Kept across sessions so a reconnect
    /// cannot repeat a perform.
    performed: BTreeMap<String, Result<(), String>>,
    partials: BTreeMap<String, Partial>,
    verified: BundleStore,
    verified_digests: BTreeMap<String, String>,
    max_bundle_bytes: u64,
}

impl<D: StepDriver> WorkerEndpoint<D> {
    /// A worker that has not enrolled yet. `code` is the operator's copy of
    /// the grant, in the form `enrolment_id:code_hex`.
    pub fn awaiting_enrolment(
        label: &str,
        code: &str,
        driver: D,
        capabilities: Vec<Capability>,
    ) -> Result<Self, TransportError> {
        let (id, code_hex) = code
            .split_once(':')
            .ok_or_else(|| TransportError::Protocol("enrolment code is not id:hex".into()))?;
        let code_bytes = wire::from_hex(code_hex)
            .ok_or_else(|| TransportError::Protocol("bad code hex".into()))?;
        if code_bytes.len() < MIN_CODE_BYTES {
            return Err(TransportError::Protocol(
                "enrolment code is too short".into(),
            ));
        }
        let mut endpoint = Self::base(label, driver, capabilities);
        endpoint.pending_enrolment = Some((id.to_string(), wire::code_key(&code_bytes)));
        Ok(endpoint)
    }

    fn base(label: &str, driver: D, capabilities: Vec<Capability>) -> Self {
        Self {
            driver,
            label: label.to_string(),
            capabilities,
            device: None,
            pending_enrolment: None,
            enrolment_attempts: 0,
            session: None,
            applied: 0,
            fences: BTreeMap::new(),
            cancelled: BTreeMap::new(),
            performed: BTreeMap::new(),
            partials: BTreeMap::new(),
            verified: Arc::new(Mutex::new(BTreeMap::new())),
            verified_digests: BTreeMap::new(),
            max_bundle_bytes: DEFAULT_MAX_BUNDLE_BYTES,
        }
    }

    /// Caps the declared size of any one bundle.
    pub fn with_max_bundle_bytes(mut self, max: u64) -> Self {
        self.max_bundle_bytes = max;
        self
    }

    /// The enrolled worker id, once enrolment has completed.
    pub fn worker_id(&self) -> Option<&str> {
        self.device.as_ref().map(|d| d.worker_id.as_str())
    }

    /// Shared view of verified bundles, keyed by bundle id.
    pub fn bundles(&self) -> BundleStore {
        Arc::clone(&self.verified)
    }

    /// Serves one connection until the controller closes it. Returns `Ok` on a
    /// clean close, including one the controller makes mid-session.
    pub fn serve_connection<S: Read + Write>(
        &mut self,
        mut stream: S,
    ) -> Result<(), TransportError> {
        let first: Handshake = wire::read_json(&mut stream)?;
        match first {
            Handshake::EnrolChallenge {
                enrolment_id,
                challenge,
            } => self.serve_enrolment(&mut stream, &enrolment_id, &challenge),
            Handshake::Challenge { controller_nonce } => {
                let mut channel = self.handshake(stream, &controller_nonce)?;
                self.serve_commands(&mut channel)
            }
            other => Err(TransportError::Protocol(format!(
                "unexpected first message {other:?}"
            ))),
        }
    }

    fn serve_enrolment<S: Read + Write>(
        &mut self,
        stream: &mut S,
        enrolment_id: &str,
        challenge: &str,
    ) -> Result<(), TransportError> {
        let (pending_id, code_key) = self
            .pending_enrolment
            .clone()
            .ok_or_else(|| TransportError::Auth("worker has no enrolment code".into()))?;
        if !wire::ct_eq(pending_id.as_bytes(), enrolment_id.as_bytes()) {
            return Err(TransportError::Auth("enrolment id does not match".into()));
        }
        // Every attempt against the live code counts. After a few failures the
        // code is discarded and the operator must issue a new one.
        self.enrolment_attempts += 1;
        let result = self.run_enrolment(stream, enrolment_id, challenge, &code_key);
        if result.is_err() && self.enrolment_attempts >= MAX_ENROLMENT_ATTEMPTS {
            self.pending_enrolment = None;
        }
        result
    }

    fn run_enrolment<S: Read + Write>(
        &mut self,
        stream: &mut S,
        enrolment_id: &str,
        challenge: &str,
        code_key: &[u8; 32],
    ) -> Result<(), TransportError> {
        let proof = wire::enrol_proof(code_key, enrolment_id, &self.label, challenge);
        wire::write_json(
            stream,
            &Handshake::EnrolProof {
                worker_label: self.label.clone(),
                proof: wire::to_hex(&proof),
            },
        )?;
        match wire::read_json::<_, Handshake>(stream)? {
            Handshake::EnrolGrant {
                worker_id,
                wrapped,
                mac,
            } => {
                let expected =
                    wire::grant_mac(code_key, enrolment_id, challenge, &worker_id, &wrapped);
                let presented = wire::from_hex(&mac).unwrap_or_default();
                if !wire::ct_eq(&expected, &presented) {
                    return Err(TransportError::Auth("grant MAC failed".into()));
                }
                let wrapped_key = wire::from_hex_32(&wrapped)?;
                let key = wire::xor32(
                    &wrapped_key,
                    &wire::wrap_pad(code_key, enrolment_id, challenge),
                );
                self.device = Some(Device { worker_id, key });
                self.pending_enrolment = None;
                Ok(())
            }
            Handshake::Reject { reason } => Err(TransportError::Auth(reason)),
            other => Err(TransportError::Protocol(format!(
                "expected enrolment grant, got {other:?}"
            ))),
        }
    }

    fn handshake<S: Read + Write>(
        &mut self,
        mut stream: S,
        controller_nonce: &str,
    ) -> Result<Channel<S>, TransportError> {
        let (worker_id, key) = {
            let device = self
                .device
                .as_ref()
                .ok_or_else(|| TransportError::Auth("worker is not enrolled".into()))?;
            (device.worker_id.clone(), device.key)
        };
        let worker_nonce = wire::to_hex(&wire::random::<16>()?);
        let claim = self.session.clone();
        let mac = wire::identify_mac(
            &key,
            &worker_id,
            claim.as_deref(),
            self.applied,
            &worker_nonce,
            controller_nonce,
        );
        wire::write_json(
            &mut stream,
            &Handshake::Identify {
                worker_id: worker_id.clone(),
                session_claim: claim.clone(),
                applied: self.applied,
                worker_nonce: worker_nonce.clone(),
                mac: wire::to_hex(&mac),
            },
        )?;
        let (session_id, accept_mac) = match wire::read_json::<_, Handshake>(&mut stream)? {
            Handshake::Accept { session_id, mac } => (session_id, mac),
            Handshake::Reject { reason } => return Err(TransportError::Auth(reason)),
            other => {
                return Err(TransportError::Protocol(format!(
                    "expected accept, got {other:?}"
                )))
            }
        };
        // The controller must prove it holds the device key, not just echo a session.
        let expected = wire::accept_mac(
            &key,
            &session_id,
            &worker_id,
            &worker_nonce,
            controller_nonce,
        );
        let presented = wire::from_hex(&accept_mac).unwrap_or_default();
        if !wire::ct_eq(&expected, &presented) {
            return Err(TransportError::Auth(
                "controller failed to prove the device key".into(),
            ));
        }
        if claim.as_deref() != Some(session_id.as_str()) {
            // A new session restarts the applied count. Fences, cancellations,
            // performed outcomes and bundles belong to the device and survive.
            self.applied = 0;
            self.session = Some(session_id.clone());
        }
        let session_key = wire::session_key(
            &key,
            controller_nonce,
            &worker_nonce,
            &worker_id,
            &session_id,
        );
        Ok(Channel::new(
            stream,
            session_key,
            WORKER_TO_CONTROLLER,
            CONTROLLER_TO_WORKER,
        ))
    }

    fn serve_commands<S: Read + Write>(
        &mut self,
        channel: &mut Channel<S>,
    ) -> Result<(), TransportError> {
        loop {
            let command: Command = match channel.recv() {
                Ok(c) => c,
                Err(TransportError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Ok(())
                }
                Err(TransportError::Io(e))
                    if e.kind() == io::ErrorKind::ConnectionAborted
                        || e.kind() == io::ErrorKind::ConnectionReset =>
                {
                    return Ok(())
                }
                Err(e) => return Err(e),
            };
            let reply = self.apply(command);
            self.applied += 1;
            channel.send(&reply)?;
        }
    }

    fn check_fence(&self, step_key: &str, fence: u64) -> Result<(), Reply> {
        let current = self.fences.get(step_key).copied().unwrap_or(0);
        if current == 0 || fence != current {
            return Err(Reply::Fenced { current });
        }
        Ok(())
    }

    fn apply(&mut self, command: Command) -> Reply {
        match command {
            Command::Grant { step, take, fence } => {
                let key = StepRef { step, take }.to_string();
                let current = self.fences.get(&key).copied().unwrap_or(0);
                if fence == 0 || fence < current {
                    return Reply::Fenced { current };
                }
                self.fences.insert(key, fence);
                Reply::Granted
            }
            Command::Discover => Reply::Capabilities {
                items: self.capabilities.clone(),
            },
            Command::Perform {
                key,
                step,
                take,
                fence,
                spec,
            } => {
                let step_key = StepRef { step, take }.to_string();
                if let Err(reply) = self.check_fence(&step_key, fence) {
                    return reply;
                }
                if let Some(outcome) = self.performed.get(&key) {
                    return match outcome {
                        Ok(()) => Reply::Performed,
                        Err(reason) => Reply::PerformErrored {
                            reason: reason.clone(),
                        },
                    };
                }
                if self.cancelled.get(&step_key) == Some(&fence) {
                    return Reply::PerformRefused {
                        reason: "step cancelled before perform".into(),
                    };
                }
                let idem = IdempotencyKey::from_wire(key.clone());
                let outcome = self.driver.perform(&spec, &idem).map_err(|e| e.0);
                self.performed.insert(key, outcome.clone());
                match outcome {
                    Ok(()) => Reply::Performed,
                    Err(reason) => Reply::PerformErrored { reason },
                }
            }
            Command::Observe {
                key,
                step,
                take,
                fence,
                spec,
            } => {
                let step_key = StepRef { step, take }.to_string();
                if let Err(reply) = self.check_fence(&step_key, fence) {
                    return reply;
                }
                let idem = IdempotencyKey::from_wire(key);
                let outcome = match self.driver.observe(&spec, &idem) {
                    Ok(Observation::Satisfied(value)) => WireObservation::Satisfied { value },
                    Ok(Observation::Pending) => WireObservation::Pending,
                    Ok(Observation::Failed(reason)) => WireObservation::Failed { reason },
                    Err(e) => WireObservation::Error { reason: e.0 },
                };
                Reply::Observed { outcome }
            }
            Command::Cancel {
                key: _,
                step,
                take,
                fence,
            } => {
                let step_key = StepRef { step, take }.to_string();
                if let Err(reply) = self.check_fence(&step_key, fence) {
                    return reply;
                }
                self.cancelled.insert(step_key, fence);
                Reply::Cancelled
            }
            Command::BundleBegin {
                bundle_id,
                blake3,
                bytes,
            } => self.bundle_begin(bundle_id, blake3, bytes),
            Command::BundleChunk {
                bundle_id,
                blake3,
                offset,
                data,
            } => self.bundle_chunk(&bundle_id, &blake3, offset, &data),
            Command::BundleFinish { bundle_id, blake3 } => self.bundle_finish(&bundle_id, &blake3),
        }
    }

    fn bundle_begin(&mut self, bundle_id: String, digest: String, bytes: u64) -> Reply {
        if !valid_bundle_id(&bundle_id) {
            return Reply::BundleRejected {
                reason: "bundle id must be 1-128 of [A-Za-z0-9._-]".into(),
            };
        }
        if !valid_digest(&digest) {
            return Reply::BundleRejected {
                reason: "bundle digest must be 64 lowercase hex digits".into(),
            };
        }
        if bytes > self.max_bundle_bytes {
            return Reply::BundleRejected {
                reason: format!("bundle exceeds the {} byte limit", self.max_bundle_bytes),
            };
        }
        if let Some(held) = self.verified_digests.get(&bundle_id) {
            if *held != digest {
                return Reply::BundleRejected {
                    reason: "a different bundle already uses this id".into(),
                };
            }
            return Reply::BundleStatus { received: bytes };
        }
        if let Some(existing) = self.partials.get(&bundle_id) {
            if existing.expected_digest != digest || existing.declared_bytes != bytes {
                return Reply::BundleRejected {
                    reason: "a different bundle already uses this id".into(),
                };
            }
            return Reply::BundleStatus {
                received: existing.data.len() as u64,
            };
        }
        if self.partials.len() >= MAX_PARTIAL_BUNDLES {
            return Reply::BundleRejected {
                reason: "too many partial transfers are open".into(),
            };
        }
        self.partials.insert(
            bundle_id,
            Partial {
                expected_digest: digest,
                declared_bytes: bytes,
                data: Vec::new(),
            },
        );
        Reply::BundleStatus { received: 0 }
    }

    fn bundle_chunk(
        &mut self,
        bundle_id: &str,
        digest: &str,
        offset: u64,
        data_hex: &str,
    ) -> Reply {
        let Some(partial) = self.partials.get_mut(bundle_id) else {
            return Reply::Error {
                reason: "no transfer begun for this bundle".into(),
            };
        };
        if partial.expected_digest != digest {
            return Reply::BundleRejected {
                reason: "chunk digest does not match the bundle in progress".into(),
            };
        }
        let held = partial.data.len() as u64;
        if offset != held {
            // The controller resends from the offset reported here.
            return Reply::BundleStatus { received: held };
        }
        if data_hex.len() > MAX_CHUNK_BYTES * 2 {
            return Reply::BundleRejected {
                reason: "bundle chunk exceeds the chunk limit".into(),
            };
        }
        let Some(chunk) = wire::from_hex(data_hex) else {
            return Reply::Error {
                reason: "bundle chunk is not hex".into(),
            };
        };
        if held.saturating_add(chunk.len() as u64) > partial.declared_bytes {
            return Reply::BundleRejected {
                reason: "chunk runs past the declared length".into(),
            };
        }
        partial.data.extend_from_slice(&chunk);
        Reply::BundleStatus {
            received: partial.data.len() as u64,
        }
    }

    fn bundle_finish(&mut self, bundle_id: &str, digest: &str) -> Reply {
        let Some(partial) = self.partials.get(bundle_id) else {
            // A retry after a lost reply: the bundle is already kept.
            if self.verified_digests.get(bundle_id).map(String::as_str) == Some(digest) {
                return Reply::BundleVerified;
            }
            return Reply::Error {
                reason: "no transfer begun for this bundle".into(),
            };
        };
        if partial.expected_digest != digest {
            return Reply::BundleRejected {
                reason: "finish digest does not match the bundle in progress".into(),
            };
        }
        if (partial.data.len() as u64) < partial.declared_bytes {
            return Reply::BundleStatus {
                received: partial.data.len() as u64,
            };
        }
        let Some(partial) = self.partials.remove(bundle_id) else {
            return Reply::Error {
                reason: "transfer vanished".into(),
            };
        };
        let actual = blake3::hash(&partial.data).to_hex().to_string();
        if !wire::ct_eq(actual.as_bytes(), partial.expected_digest.as_bytes()) {
            return Reply::BundleRejected {
                reason: "digest mismatch; the transfer is discarded".into(),
            };
        }
        self.verified_digests
            .insert(bundle_id.to_string(), partial.expected_digest);
        self.verified
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(bundle_id.to_string(), partial.data);
        Reply::BundleVerified
    }
}

fn valid_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_BUNDLE_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{DriverError, StepSpec};
    use serde_json::json;

    struct Null;
    impl StepDriver for Null {
        fn perform(&mut self, _: &StepSpec, _: &IdempotencyKey) -> Result<(), DriverError> {
            Ok(())
        }
        fn observe(
            &mut self,
            _: &StepSpec,
            _: &IdempotencyKey,
        ) -> Result<Observation, DriverError> {
            Ok(Observation::Pending)
        }
    }

    fn endpoint() -> WorkerEndpoint<Null> {
        let code = format!("e-1:{}", "ab".repeat(32));
        WorkerEndpoint::awaiting_enrolment("w", &code, Null, Vec::new()).expect("endpoint")
    }

    fn spec() -> StepSpec {
        StepSpec {
            step_id: "s".into(),
            label: "s".into(),
            action: json!({}),
            expect: json!({}),
        }
    }

    fn perform(key: &str, fence: u64) -> Command {
        Command::Perform {
            key: key.into(),
            step: "s".into(),
            take: 0,
            fence,
            spec: spec(),
        }
    }

    fn grant(fence: u64) -> Command {
        Command::Grant {
            step: "s".into(),
            take: 0,
            fence,
        }
    }

    #[test]
    fn fence_zero_and_ungranted_fences_never_act() {
        let mut w = endpoint();
        assert_eq!(w.apply(grant(0)), Reply::Fenced { current: 0 });
        assert_eq!(w.apply(perform("k", 0)), Reply::Fenced { current: 0 });
        assert_eq!(w.apply(perform("k", 7)), Reply::Fenced { current: 0 });
        assert_eq!(w.apply(grant(3)), Reply::Granted);
        assert_eq!(w.apply(perform("k", 4)), Reply::Fenced { current: 3 });
        assert_eq!(w.apply(perform("k", 3)), Reply::Performed);
    }

    #[test]
    fn a_cancel_binds_to_its_fence_and_a_newer_grant_supersedes_it() {
        let mut w = endpoint();
        w.apply(grant(1));
        assert_eq!(
            w.apply(Command::Cancel {
                key: "k1".into(),
                step: "s".into(),
                take: 0,
                fence: 1
            }),
            Reply::Cancelled
        );
        assert!(matches!(
            w.apply(perform("k1", 1)),
            Reply::PerformRefused { .. }
        ));
        // A new lease at a higher fence is not blocked by the old cancel.
        w.apply(grant(2));
        assert_eq!(w.apply(perform("k2", 2)), Reply::Performed);
        // And the old lease stays fenced out.
        assert_eq!(w.apply(grant(1)), Reply::Fenced { current: 2 });
    }

    #[test]
    fn a_new_session_keeps_the_fence_high_water_mark() {
        let mut w = endpoint();
        w.apply(grant(5));
        // A new session resets the applied count only.
        w.applied = 0;
        w.session = Some("new".into());
        assert_eq!(w.apply(grant(4)), Reply::Fenced { current: 5 });
    }

    #[test]
    fn chunks_for_another_digest_cannot_splice_into_a_partial() {
        let mut w = endpoint();
        let a = blake3::hash(b"aaaa").to_hex().to_string();
        let b = blake3::hash(b"bbbb").to_hex().to_string();
        assert_eq!(
            w.bundle_begin("x".into(), a.clone(), 4),
            Reply::BundleStatus { received: 0 }
        );
        assert_eq!(
            w.bundle_chunk("x", &a, 0, &wire::to_hex(b"aa")),
            Reply::BundleStatus { received: 2 }
        );
        // A chunk claiming a different bundle's digest is refused outright.
        assert!(matches!(
            w.bundle_chunk("x", &b, 2, &wire::to_hex(b"bb")),
            Reply::BundleRejected { .. }
        ));
        assert!(matches!(
            w.bundle_finish("x", &b),
            Reply::BundleRejected { .. }
        ));
        assert_eq!(
            w.bundle_chunk("x", &a, 2, &wire::to_hex(b"aa")),
            Reply::BundleStatus { received: 4 }
        );
        assert_eq!(w.bundle_finish("x", &a), Reply::BundleVerified);
    }

    #[test]
    fn oversized_chunks_and_too_many_partials_are_refused() {
        let mut w = endpoint();
        let d = blake3::hash(b"x").to_hex().to_string();
        let big = u64::try_from(MAX_CHUNK_BYTES).expect("fits") * 4;
        w.bundle_begin("big".into(), d.clone(), big);
        let huge = "00".repeat(MAX_CHUNK_BYTES + 1);
        assert!(matches!(
            w.bundle_chunk("big", &d, 0, &huge),
            Reply::BundleRejected { .. }
        ));
        for i in 0..MAX_PARTIAL_BUNDLES {
            let _ = w.bundle_begin(format!("p{i}"), d.clone(), 1);
        }
        assert!(matches!(
            w.bundle_begin("one-too-many".into(), d, 1),
            Reply::BundleRejected { .. }
        ));
    }
}
