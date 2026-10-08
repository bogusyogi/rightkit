//! Controller side of the RK8 wire.
//!
//! [`ControllerLink`] dials a worker and implements [`WorkerTransport`].
//! [`RemoteDriver`] adapts a link to [`StepDriver`], so `Run::execute` drives a
//! remote worker unchanged. The journal stays on the controller and decides
//! whether a step is performed, observed or resumed.
//!
//! Enrolment and reconnection are both checked. A reconnecting worker must
//! present the session the controller holds for it. Any other claim is refused,
//! so a worker cannot attach to someone else's session.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::driver::{DriverError, IdempotencyKey, Observation, StepDriver, StepSpec};
use crate::machine::StepRef;
use crate::protocol::{Command, Handshake, Reply, WireObservation};
use crate::transport::{
    BundleRef, Capability, EnrolmentRequest, ReconnectState, TransferReport, TransportError,
    WorkerIdentity, WorkerTransport,
};
use crate::wire::{
    self, Channel, CONTROLLER_TO_WORKER, DEFAULT_CHUNK_BYTES, MAX_CHUNK_BYTES, WORKER_TO_CONTROLLER,
};

/// Issued enrolment: the grant id, and the one-time code shown to the operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedEnrolment {
    pub enrolment_id: String,
    /// `enrolment_id:code_hex`. Shown once. The controller keeps only the key.
    pub code: String,
}

/// How long an issued enrolment code stays redeemable.
pub const DEFAULT_ENROLMENT_TTL: Duration = Duration::from_secs(600);

struct Grant {
    label: String,
    code_key: [u8; 32],
    used: bool,
    issued: Instant,
}

#[derive(Clone)]
struct Device {
    key: [u8; 32],
    session: Option<String>,
}

/// Controller-side registry of enrolment grants and device keys.
pub struct Controller {
    grants: BTreeMap<String, Grant>,
    devices: BTreeMap<String, Device>,
    enrolment_ttl: Duration,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            grants: BTreeMap::new(),
            devices: BTreeMap::new(),
            enrolment_ttl: DEFAULT_ENROLMENT_TTL,
        }
    }
}

impl Controller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets how long newly redeemed grants stay valid after issue.
    pub fn with_enrolment_ttl(mut self, ttl: Duration) -> Self {
        self.enrolment_ttl = ttl;
        self
    }

    /// Issues a one-time enrolment grant for a worker labelled `label`.
    pub fn issue_enrolment(&mut self, label: &str) -> Result<IssuedEnrolment, TransportError> {
        let id_bytes = wire::random::<8>()?;
        let code_bytes = wire::random::<32>()?;
        let enrolment_id = format!("e-{}", wire::to_hex(&id_bytes));
        self.grants.insert(
            enrolment_id.clone(),
            Grant {
                label: label.to_string(),
                code_key: wire::code_key(&code_bytes),
                used: false,
                issued: Instant::now(),
            },
        );
        Ok(IssuedEnrolment {
            code: format!("{enrolment_id}:{}", wire::to_hex(&code_bytes)),
            enrolment_id,
        })
    }

    /// Whether a device key exists for `worker_id`.
    pub fn is_enrolled(&self, worker_id: &str) -> bool {
        self.devices.contains_key(worker_id)
    }
}

/// A controller's connection to one worker. `connector` opens a fresh stream
/// each time a session is established.
pub struct ControllerLink<S, C> {
    controller: Controller,
    connector: C,
    identity: Option<WorkerIdentity>,
    channel: Option<Channel<S>>,
    acked: u64,
    chunk_bytes: usize,
    /// A session id offered to a worker but not yet confirmed by a reply. It
    /// is committed to the device record only once the worker answers a
    /// command, so a handshake cut off after `Accept` cannot desynchronise
    /// the two sides.
    provisional: Option<(String, String)>,
}

impl<S, C> ControllerLink<S, C>
where
    S: Read + Write,
    C: FnMut() -> io::Result<S>,
{
    pub fn new(controller: Controller, connector: C) -> Self {
        Self {
            controller,
            connector,
            identity: None,
            channel: None,
            acked: 0,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            provisional: None,
        }
    }

    pub fn with_chunk_bytes(mut self, chunk_bytes: usize) -> Self {
        self.chunk_bytes = chunk_bytes.clamp(1, MAX_CHUNK_BYTES);
        self
    }

    pub fn controller(&mut self) -> &mut Controller {
        &mut self.controller
    }

    /// Commands the controller has seen acknowledged in this session.
    pub fn acked_seq(&self) -> u64 {
        self.acked
    }

    pub fn is_connected(&self) -> bool {
        self.channel.is_some()
    }

    /// Grants `fence` for `step` on the worker. A lower fence than the current
    /// grant is refused.
    pub fn grant(&mut self, step: &StepRef, fence: u64) -> Result<(), TransportError> {
        self.ensure_connected()?;
        let command = Command::Grant {
            step: step.step.clone(),
            take: step.take,
            fence,
        };
        match self.call(command)? {
            Reply::Granted => Ok(()),
            other => Err(refusal(other)),
        }
    }

    fn check_worker(&self, worker: &WorkerIdentity) -> Result<(), TransportError> {
        match &self.identity {
            Some(id) if id == worker => Ok(()),
            Some(id) => Err(TransportError::WrongWorker {
                expected: id.worker_id.clone(),
                found: worker.worker_id.clone(),
            }),
            None => Err(TransportError::NotConnected),
        }
    }

    fn ensure_connected(&mut self) -> Result<(), TransportError> {
        if self.channel.is_none() {
            self.establish()?;
        }
        Ok(())
    }

    /// Dials the worker and runs the session handshake. Returns the number of
    /// commands the worker has applied in the session.
    fn establish(&mut self) -> Result<u64, TransportError> {
        let identity = self.identity.clone().ok_or(TransportError::NotConnected)?;
        let (key, expected_session) = {
            let device = self
                .controller
                .devices
                .get(&identity.worker_id)
                .ok_or_else(|| TransportError::Auth("unknown worker".into()))?;
            (device.key, device.session.clone())
        };
        let mut stream = (self.connector)()?;
        let controller_nonce = wire::to_hex(&wire::random::<16>()?);
        wire::write_json(
            &mut stream,
            &Handshake::Challenge {
                controller_nonce: controller_nonce.clone(),
            },
        )?;
        let (worker_id, claim, applied, worker_nonce, mac) =
            match wire::read_json::<_, Handshake>(&mut stream)? {
                Handshake::Identify {
                    worker_id,
                    session_claim,
                    applied,
                    worker_nonce,
                    mac,
                } => (worker_id, session_claim, applied, worker_nonce, mac),
                Handshake::Reject { reason } => return Err(TransportError::Auth(reason)),
                other => {
                    return Err(TransportError::Protocol(format!(
                        "expected identify, got {other:?}"
                    )))
                }
            };
        if worker_id != identity.worker_id {
            return Err(TransportError::WrongWorker {
                expected: identity.worker_id,
                found: worker_id,
            });
        }
        let expected = wire::identify_mac(
            &key,
            &worker_id,
            claim.as_deref(),
            applied,
            &worker_nonce,
            &controller_nonce,
        );
        let presented = wire::from_hex(&mac).unwrap_or_default();
        if !wire::ct_eq(&expected, &presented) {
            return Err(TransportError::Auth(
                "worker failed to prove the device key".into(),
            ));
        }

        // No held session means none was ever confirmed, so the worker's claim
        // (if any) is a leftover from a handshake that never completed. Start
        // fresh. A held session must be claimed back exactly.
        let resumed = matches!((&expected_session, &claim), (Some(held), Some(c)) if held == c);
        let session_id = match (&expected_session, &claim) {
            (None, _) => wire::to_hex(&wire::random::<16>()?),
            (Some(held), Some(claimed)) if held == claimed => held.clone(),
            _ => {
                let reason = "session mismatch".to_string();
                wire::write_json(
                    &mut stream,
                    &Handshake::Reject {
                        reason: reason.clone(),
                    },
                )?;
                return Err(TransportError::Auth(format!(
                    "{reason}: worker presented a session this controller does not hold for it"
                )));
            }
        };
        let accept = wire::accept_mac(
            &key,
            &session_id,
            &worker_id,
            &worker_nonce,
            &controller_nonce,
        );
        wire::write_json(
            &mut stream,
            &Handshake::Accept {
                session_id: session_id.clone(),
                mac: wire::to_hex(&accept),
            },
        )?;
        let session_key = wire::session_key(
            &key,
            &controller_nonce,
            &worker_nonce,
            &worker_id,
            &session_id,
        );
        self.channel = Some(Channel::new(
            stream,
            session_key,
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        ));
        if resumed {
            self.provisional = None;
        } else {
            self.acked = 0;
            self.provisional = Some((identity.worker_id.clone(), session_id));
        }
        Ok(if resumed { applied } else { 0 })
    }

    /// Sends one command and reads its reply. Any failure drops the connection,
    /// so the next call needs a reconnect.
    fn call(&mut self, command: Command) -> Result<Reply, TransportError> {
        let channel = self.channel.as_mut().ok_or(TransportError::NotConnected)?;
        let result = channel
            .send(&command)
            .and_then(|()| channel.recv::<Reply>());
        match result {
            Ok(reply) => {
                self.acked += 1;
                if let Some((worker_id, session)) = self.provisional.take() {
                    if let Some(device) = self.controller.devices.get_mut(&worker_id) {
                        device.session = Some(session);
                    }
                }
                Ok(reply)
            }
            Err(e) => {
                self.channel = None;
                Err(e)
            }
        }
    }
}

fn refusal(reply: Reply) -> TransportError {
    match reply {
        Reply::Fenced { current } => TransportError::Fenced { current },
        Reply::Error { reason } | Reply::PerformRefused { reason } => {
            TransportError::Refused(reason)
        }
        Reply::BundleRejected { reason } => TransportError::Digest(reason),
        other => TransportError::Protocol(format!("unexpected reply {other:?}")),
    }
}

impl<S, C> WorkerTransport for ControllerLink<S, C>
where
    S: Read + Write,
    C: FnMut() -> io::Result<S>,
{
    type Error = TransportError;

    fn enrol(&mut self, request: &EnrolmentRequest) -> Result<WorkerIdentity, TransportError> {
        let (code_key, label) = {
            let grant = self
                .controller
                .grants
                .get(&request.enrolment_id)
                .ok_or_else(|| TransportError::Auth("unknown enrolment grant".into()))?;
            if grant.used {
                return Err(TransportError::Auth("enrolment grant already used".into()));
            }
            if grant.issued.elapsed() > self.controller.enrolment_ttl {
                return Err(TransportError::Auth("enrolment grant expired".into()));
            }
            (grant.code_key, grant.label.clone())
        };
        if label != request.worker_label {
            return Err(TransportError::Auth(
                "worker label does not match the grant".into(),
            ));
        }
        let mut stream = (self.connector)()?;
        let challenge = wire::to_hex(&wire::random::<16>()?);
        wire::write_json(
            &mut stream,
            &Handshake::EnrolChallenge {
                enrolment_id: request.enrolment_id.clone(),
                challenge: challenge.clone(),
            },
        )?;
        let proof = match wire::read_json::<_, Handshake>(&mut stream)? {
            Handshake::EnrolProof {
                worker_label,
                proof,
            } => (worker_label, proof),
            other => {
                return Err(TransportError::Protocol(format!(
                    "expected enrolment proof, got {other:?}"
                )))
            }
        };
        // A one-time grant is burned on any proof attempt, right or wrong.
        if let Some(grant) = self.controller.grants.get_mut(&request.enrolment_id) {
            grant.used = true;
        }
        let expected = wire::enrol_proof(&code_key, &request.enrolment_id, &label, &challenge);
        let presented = wire::from_hex(&proof.1).unwrap_or_default();
        if proof.0 != label || !wire::ct_eq(&expected, &presented) {
            wire::write_json(
                &mut stream,
                &Handshake::Reject {
                    reason: "enrolment code rejected".into(),
                },
            )?;
            return Err(TransportError::Auth("enrolment code rejected".into()));
        }

        let worker_id = format!("w-{}", wire::to_hex(&wire::random::<8>()?));
        let key = wire::random::<32>()?;
        let wrapped = wire::to_hex(&wire::xor32(
            &key,
            &wire::wrap_pad(&code_key, &request.enrolment_id, &challenge),
        ));
        let mac = wire::grant_mac(
            &code_key,
            &request.enrolment_id,
            &challenge,
            &worker_id,
            &wrapped,
        );
        wire::write_json(
            &mut stream,
            &Handshake::EnrolGrant {
                worker_id: worker_id.clone(),
                wrapped,
                mac: wire::to_hex(&mac),
            },
        )?;
        self.controller
            .devices
            .insert(worker_id.clone(), Device { key, session: None });
        let identity = WorkerIdentity {
            worker_id,
            key_fingerprint: blake3::hash(&key).to_hex()[..16].to_string(),
        };
        self.identity = Some(identity.clone());
        Ok(identity)
    }

    fn discover(&mut self, worker: &WorkerIdentity) -> Result<Vec<Capability>, TransportError> {
        self.check_worker(worker)?;
        self.ensure_connected()?;
        match self.call(Command::Discover)? {
            Reply::Capabilities { items } => Ok(items),
            other => Err(refusal(other)),
        }
    }

    fn transfer_bundle(
        &mut self,
        worker: &WorkerIdentity,
        bundle: &BundleRef,
        data: &[u8],
    ) -> Result<TransferReport, TransportError> {
        self.check_worker(worker)?;
        if data.len() as u64 != bundle.bytes {
            return Err(TransportError::Digest(
                "length differs from the declared size".into(),
            ));
        }
        if blake3::hash(data).to_hex().as_str() != bundle.blake3 {
            return Err(TransportError::Digest(
                "digest differs from the declared value".into(),
            ));
        }
        self.ensure_connected()?;
        let total = data.len() as u64;
        let begin = Command::BundleBegin {
            bundle_id: bundle.bundle_id.clone(),
            blake3: bundle.blake3.clone(),
            bytes: total,
        };
        let resumed_from = match self.call(begin)? {
            Reply::BundleStatus { received } => received,
            other => return Err(refusal(other)),
        };
        let mut offset = resumed_from;
        let mut stalls = 0u32;
        while offset < total {
            let end = offset.saturating_add(self.chunk_bytes as u64).min(total);
            let chunk = Command::BundleChunk {
                bundle_id: bundle.bundle_id.clone(),
                blake3: bundle.blake3.clone(),
                offset,
                data: wire::to_hex(&data[offset as usize..end as usize]),
            };
            let received = match self.call(chunk)? {
                Reply::BundleStatus { received } => received,
                other => return Err(refusal(other)),
            };
            if received > offset {
                stalls = 0;
            } else {
                stalls += 1;
                if stalls > 3 {
                    return Err(TransportError::Protocol(
                        "bundle transfer is not advancing".into(),
                    ));
                }
            }
            offset = received;
        }
        match self.call(Command::BundleFinish {
            bundle_id: bundle.bundle_id.clone(),
            blake3: bundle.blake3.clone(),
        })? {
            Reply::BundleVerified => Ok(TransferReport {
                resumed_from,
                bytes: total,
            }),
            other => Err(refusal(other)),
        }
    }

    fn reconnect(
        &mut self,
        worker: &WorkerIdentity,
        state: ReconnectState,
    ) -> Result<ReconnectState, TransportError> {
        self.check_worker(worker)?;
        self.channel = None;
        let applied = self.establish()?;
        if applied < state.last_acked_seq {
            return Err(TransportError::Protocol(format!(
                "worker applied {applied} commands but {} were acknowledged",
                state.last_acked_seq
            )));
        }
        Ok(ReconnectState {
            last_acked_seq: applied,
        })
    }
}

/// Adapts a link and one step's lease to [`StepDriver`]. `perform` is sent at
/// most once by the machine. An `observe` after a dropped connection reconnects
/// and asks again. The worker's dedup makes that safe.
pub struct RemoteDriver<'a, S, C> {
    link: &'a mut ControllerLink<S, C>,
    step: StepRef,
    fence: u64,
}

impl<'a, S, C> RemoteDriver<'a, S, C>
where
    S: Read + Write,
    C: FnMut() -> io::Result<S>,
{
    pub fn new(link: &'a mut ControllerLink<S, C>, step: StepRef, fence: u64) -> Self {
        Self { link, step, fence }
    }
}

impl<S, C> StepDriver for RemoteDriver<'_, S, C>
where
    S: Read + Write,
    C: FnMut() -> io::Result<S>,
{
    fn perform(&mut self, step: &StepSpec, key: &IdempotencyKey) -> Result<(), DriverError> {
        let command = Command::Perform {
            key: key.as_str().to_string(),
            step: self.step.step.clone(),
            take: self.step.take,
            fence: self.fence,
            spec: step.clone(),
        };
        match self.link.call(command) {
            Ok(Reply::Performed) => Ok(()),
            Ok(Reply::PerformErrored { reason }) | Ok(Reply::PerformRefused { reason }) => {
                Err(DriverError(reason))
            }
            Ok(other) => Err(DriverError(refusal(other).to_string())),
            Err(e) => Err(DriverError(e.to_string())),
        }
    }

    fn observe(
        &mut self,
        step: &StepSpec,
        key: &IdempotencyKey,
    ) -> Result<Observation, DriverError> {
        let command = Command::Observe {
            key: key.as_str().to_string(),
            step: self.step.step.clone(),
            take: self.step.take,
            fence: self.fence,
            spec: step.clone(),
        };
        let reply = match self.link.call(command.clone()) {
            Err(TransportError::NotConnected) | Err(TransportError::Io(_)) => {
                self.link
                    .establish()
                    .map_err(|e| DriverError(format!("reconnect failed: {e}")))?;
                self.link.call(command)
            }
            other => other,
        };
        match reply {
            Ok(Reply::Observed { outcome }) => match outcome {
                WireObservation::Satisfied { value } => Ok(Observation::Satisfied(value)),
                WireObservation::Pending => Ok(Observation::Pending),
                WireObservation::Failed { reason } => Ok(Observation::Failed(reason)),
                WireObservation::Error { reason } => Err(DriverError(reason)),
            },
            Ok(other) => Err(DriverError(refusal(other).to_string())),
            Err(e) => Err(DriverError(e.to_string())),
        }
    }
}
