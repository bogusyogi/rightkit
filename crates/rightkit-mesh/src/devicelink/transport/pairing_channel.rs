//! The pairing channel: the pairing messages in order, carried over one stream the transport
//! provides (the mesh's iroh stream, `engine/src/devicelink_host.rs`):
//!
//! offer, accept, host nonce, reveal, joiner confirm, (host owner's approval), host confirm,
//! enrolment.
//!
//! One JSON object per line. The Host's session lives for `PAIRING_TTL_MS` and serves one
//! attempt at a time on streams its transport accepts ([`StreamHost`]); the Joiner dials the
//! Host's endpoint id and runs [`join_over`]. The offer's `host_tls_pin` field holds the Host's
//! endpoint id (its identity key), which the transport authenticated in its handshake; the
//! Joiner refuses an offer naming another key. The typed code then confirms that key, because it
//! is part of the transcript. There is no TCP, TLS or LAN listener in this module.
//!
//! The protocol state machines live in `pairing`; this module only moves their messages and
//! enforces time. Every state-machine error voids the session, as there.

use std::io::{self, BufReader, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::super::LinkError;
use super::super::identity::DeviceIdentity;
use super::super::join_token::JoinToken;
use super::super::pairing::{
    EnrolGrant, HostPairing, JoinerPairing, PairAccept, PairConfirm, PairEnrol, PairHostNonce,
    PairOffer, PairReveal, PairingOutcome, PendingApproval, key_fingerprint,
};
use super::super::wire::unhex;
use rightkit_crypto::ct_eq;
use rightkit_device_link_types::DeviceRef;
use rightkit_fs::digest::hex;

/// Longest accepted line. The largest message is a few hundred bytes.
const MAX_LINE: u64 = 16 * 1024;
/// A new connection must deliver its first message within this time, so a connection that only
/// holds the port cannot stall the session for long.
const FIRST_MESSAGE_WAIT: Duration = Duration::from_secs(5);
const DECISION_POLL: Duration = Duration::from_millis(250);
/// How long after the owner's approval a Joiner that lost the stream may repeat its confirmation
/// and be served the same final messages again (10 minutes). Memory only; never persisted.
pub const RESUME_WINDOW_MS: u64 = 10 * 60 * 1000;
/// Repeated confirmations served per approval; the next one drops the cache.
const MAX_RESUMES: u8 = 5;
/// A join-token session has nobody to wait for: the joiner sends its confirmation and proof
/// right after the reveal, so a connection that sits silent past this ends the session.
const JOIN_CONFIRM_WAIT_MS: u64 = 20_000;

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("pairing channel I/O: {0}")]
    Io(String),
    #[error("the peer sent a message the pairing protocol does not allow here: {0}")]
    Protocol(&'static str),
    #[error("timed out")]
    Timeout,
    #[error("the pairing session expired")]
    Expired,
    #[error("the host owner declined, or the channel was closed")]
    Declined,
    #[error("the code was not entered")]
    NoCode,
    #[error("{0}")]
    Hook(String),
}

impl From<io::Error> for ChannelError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Self::Timeout,
            _ => Self::Io(error.to_string()),
        }
    }
}

/// The joiner's confirmation and, in a join-token session, its proof that it knows the token
/// secret (`pairing::join_verifier`, HMAC over the confirmed transcript), hex.
#[derive(Serialize, Deserialize)]
struct JoinProofMsg {
    confirm: PairConfirm,
    proof: String,
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
    /// Boxed: the enrolment is several times the size of the other frames (clippy large_enum_variant).
    Enrol(Box<PairEnrol>),
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn remaining(until_ms: u64) -> Result<Duration, ChannelError> {
    let now = now_ms();
    if now >= until_ms {
        return Err(ChannelError::Expired);
    }
    Ok(Duration::from_millis(until_ms - now))
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// The side of a pairing stream the channel cannot do through `Read` and `Write`: bounding each
/// wait, and ending the sending direction. The TCP channel uses its socket; the mesh channel
/// (`engine/src/devicelink_host.rs`, over an iroh stream) supplies its own.
pub trait PairControl: Send {
    /// Bound the next reads and writes to `wait` (a wait that runs out is `ErrorKind::TimedOut`).
    fn set_timeout(&self, wait: Duration) -> io::Result<()>;
    /// Finish the sending direction after the last message.
    fn shutdown_write(&self);
}

/// A line-framed stream. `control` bounds the waits and ends the connection.
struct Wire {
    reader: BufReader<Box<dyn Stream>>,
    control: Box<dyn PairControl>,
}

impl Wire {
    fn new(stream: Box<dyn Stream>, control: Box<dyn PairControl>) -> Self {
        Self {
            reader: BufReader::new(stream),
            control,
        }
    }

    fn set_timeout(&self, wait: Duration) -> Result<(), ChannelError> {
        let wait = wait.max(Duration::from_millis(1));
        self.control.set_timeout(wait)?;
        Ok(())
    }

    fn set_deadline(&self, until_ms: u64) -> Result<(), ChannelError> {
        self.set_timeout(remaining(until_ms)?)
    }

    fn write(&mut self, frame: &Frame) -> Result<(), ChannelError> {
        // One compact line within the reader's own limit, written and flushed by the owner.
        rightkit_sidecar::write_json_line_flush(self.reader.get_mut(), frame, MAX_LINE as usize)
            .map_err(|e| ChannelError::Io(e.to_string()))
    }

    /// Write bytes the caller already framed (whole lines), then flush.
    fn write_raw(&mut self, lines: &[u8]) -> Result<(), ChannelError> {
        let out = self.reader.get_mut();
        out.write_all(lines)
            .and_then(|()| out.flush())
            .map_err(|e| ChannelError::Io(e.to_string()))
    }

    fn read(&mut self) -> Result<Frame, ChannelError> {
        let mut line = Vec::new();
        let limit = MAX_LINE as usize;
        match rightkit_sidecar::read_json_line_with(
            &mut self.reader,
            &mut line,
            limit,
            rightkit_sidecar::ReadLimits {
                max_discard: limit,
                deadline: None,
            },
        ) {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => Err(ChannelError::Io("the peer closed the channel".to_string())),
            Err(rightkit_sidecar::FrameError::TooLarge { .. }) => {
                Err(ChannelError::Protocol("line too long or cut short"))
            }
            Err(rightkit_sidecar::FrameError::Io(e)) => Err(e.into()),
            Err(_) => Err(ChannelError::Protocol("malformed message")),
        }
    }

    fn finish(self) {
        self.control.shutdown_write();
    }
}

// ---- Host --------------------------------------------------------------------------------------

/// Where the Host reports to its engine. Each method runs on the channel's thread; keep them
/// short. The code and the enrolment code are secrets: never log them.
pub trait HostHooks: Send + 'static {
    /// The code to SHOW the owner so they can type it on the Joiner. Called once, after the
    /// Joiner's reveal verified.
    fn code_ready(&mut self, pairing_id: &str, code: &str);
    /// The Joiner's confirmation verified; the owner must now approve or reject
    /// ([`StreamHost::decide`]).
    fn pending(&mut self, pending: &PendingApproval);
    /// The owner approved. Record `outcome` (`DeviceLink::complete_pairing`), then return the
    /// enrolment text the Joiner receives (the mesh has nothing to enrol and returns a placeholder). An error ends the session without
    /// sending anything further.
    ///
    /// `peer` is the key the transport authenticated for the stream THIS session ran on.
    fn approved(&mut self, outcome: &PairingOutcome, peer: &[u8; 32]) -> Result<String, String>;
    /// After `approved`: the endorsement batch the owner-check signature made when it admitted the
    /// Joiner, if the owner approved with one (not a PIN). It travels in the final message so the
    /// Joiner can verify it and trust the key that signed it.
    fn admit_proof(&mut self) -> Option<super::super::circle::EndorsementBatch> {
        None
    }
    /// After `approved`, for a join token or QR: the Host's device-key redemption of the token
    /// grant that `admit_proof` carries. It travels in the same final message.
    fn admit_redemption(&mut self) -> Option<super::super::circle::Redemption> {
        None
    }
    /// The session ended: `Ok` after the enrolment message was sent.
    fn finished(&mut self, result: &Result<(), ChannelError>);
    /// A Joiner repeated its confirmation after the approval. Return true only while the record
    /// this session wrote is still active, for this pairing id and this transport-authenticated
    /// key. The default refuses.
    fn resume_allowed(&mut self, _pairing_id: &str, _peer: &[u8; 32]) -> bool {
        false
    }
}

/// The exact final lines of an approved pairing, kept in memory for [`RESUME_WINDOW_MS`] so a
/// Joiner that lost the stream can be served them again. Holds the enrolment text (a bearer
/// secret on a LAN): never logged, never persisted, never `Debug`.
struct DeliveryCache {
    joiner_confirm: PairConfirm,
    lines: Vec<u8>,
}

struct ResumeState {
    pairing_id: String,
    /// The key the transport authenticated on the original stream.
    peer: [u8; 32],
    cache: DeliveryCache,
    until_ms: u64,
    attempts: u8,
    hooks: Box<dyn HostHooks>,
    /// `hooks.finished` already ran (the first delivery went out).
    ended: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

struct HostRun {
    identity: Arc<DeviceIdentity>,
    session: HostPairing,
    pairing_id: String,
    expires_at_ms: u64,
    hooks: Box<dyn HostHooks>,
    /// A join-token session: the verifier of the token's secret and whether the proof is
    /// required. The owner's check made the token, so a valid proof approves at once; no code is
    /// shown or typed. When not required (the phone QR), a joiner without a proof takes the code
    /// path and the owner approves its card as before; a WRONG proof always ends the session.
    join: Option<([u8; 32], bool)>,
    decisions: mpsc::Receiver<Decision>,
    stop: Arc<AtomicBool>,
    /// The key the transport authenticated for the stream being served (set by `serve`).
    peer: [u8; 32],
    /// Set once the owner approved and the record exists, BEFORE the final lines are written.
    cache: Option<DeliveryCache>,
}

/// What a connection opens with: a new attempt (accept) or a repeated confirmation.
enum First {
    Accept(Box<PairAccept>),
    Resume(PairConfirm),
}

/// The Host's first exchange on a stream: send the offer, read the accept (or a repeated
/// joiner confirmation, which is the same frame as in the first run).
fn exchange_offer(wire: &mut Wire, offer: &PairOffer) -> Result<First, ChannelError> {
    wire.write(&Frame::Offer(offer.clone()))?;
    match wire.read()? {
        Frame::Accept(accept) => Ok(First::Accept(Box::new(accept))),
        Frame::JoinerConfirm(confirm) => Ok(First::Resume(confirm)),
        Frame::JoinerJoinConfirm(message) => Ok(First::Resume(message.confirm)),
        _ => Err(ChannelError::Protocol("expected accept")),
    }
}

fn frame_line(frame: &Frame) -> Result<Vec<u8>, ChannelError> {
    let mut line = serde_json::to_vec(frame).map_err(|e| ChannelError::Io(e.to_string()))?;
    line.push(b'\n');
    Ok(line)
}

fn host_session(
    mut wire: Wire,
    accept: &PairAccept,
    run: &mut HostRun,
) -> Result<(), ChannelError> {
    let expires = run.expires_at_ms;
    let nonce = run.session.accept(accept, now_ms())?;
    wire.set_deadline(expires)?;
    wire.write(&Frame::HostNonce(nonce))?;

    wire.set_deadline(expires)?;
    let Frame::Reveal(reveal) = wire.read()? else {
        return Err(ChannelError::Protocol("expected reveal"));
    };
    let code = run.session.reveal(&reveal, now_ms())?;
    let strict = matches!(run.join, Some((_, true)));
    if !strict {
        run.hooks.code_ready(&run.pairing_id, &code);
    }
    drop(code);

    // The owner types the code on the Joiner, so this wait is bounded by the session only. A
    // join-token joiner sends its confirmation at once, so that wait is short.
    let confirm_by = if strict {
        expires.min(now_ms().saturating_add(JOIN_CONFIRM_WAIT_MS))
    } else {
        expires
    };
    wire.set_deadline(confirm_by)?;
    let (confirm, proof) = match wire.read()? {
        Frame::JoinerConfirm(confirm) => (confirm, None),
        Frame::JoinerJoinConfirm(message) => (message.confirm, Some(message.proof)),
        _ => return Err(ChannelError::Protocol("expected confirm")),
    };
    let pending = run.session.confirm(&run.identity, &confirm, now_ms())?;

    let decision = match (proof, run.join) {
        (Some(proof), Some((verifier, _))) => {
            // Any failure here voids the session, so a wrong proof burns the token.
            let Some(proof) = unhex::<32>(&proof).ok() else {
                run.session.reject();
                return Err(ChannelError::Protocol("join proof malformed"));
            };
            if !pending.join_proof_ok(&verifier, &proof) {
                run.session.reject();
                return Err(ChannelError::Protocol("join proof does not match"));
            }
            Decision::Approve
        }
        (Some(_), None) => {
            return Err(ChannelError::Protocol("this is not a join-token session"));
        }
        (None, Some((_, true))) => {
            run.session.reject();
            return Err(ChannelError::Protocol("join proof required"));
        }
        (None, _) => {
            run.hooks.pending(&pending);
            await_decision(run)?
        }
    };

    match decision {
        Decision::Reject => {
            run.session.reject();
            Err(ChannelError::Declined)
        }
        Decision::Approve => {
            let (outcome, host_confirm) = run.session.approve(&run.identity, &pending, now_ms())?;
            let enrol_code = run
                .hooks
                .approved(&outcome, &run.peer)
                .map_err(ChannelError::Hook)?;
            let mut enrol = run.session.enrol(&enrol_code, now_ms())?;
            enrol.admit = run.hooks.admit_proof();
            enrol.redemption = run.hooks.admit_redemption();
            // The record exists. Keep the exact final lines before sending them, so a stream
            // that dies mid-delivery can be served the same bytes again.
            let mut lines = frame_line(&Frame::HostConfirm(host_confirm))?;
            lines.extend(frame_line(&Frame::Enrol(Box::new(enrol)))?);
            run.cache = Some(DeliveryCache {
                joiner_confirm: confirm,
                lines: lines.clone(),
            });
            wire.set_deadline(expires)?;
            wire.write_raw(&lines)?;
            wire.finish();
            Ok(())
        }
    }
}

fn await_decision(run: &HostRun) -> Result<Decision, ChannelError> {
    loop {
        if run.stop.load(Ordering::SeqCst) {
            return Err(ChannelError::Declined);
        }
        let wait = remaining(run.expires_at_ms)?.min(DECISION_POLL);
        match run.decisions.recv_timeout(wait) {
            Ok(decision) => return Ok(decision),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(ChannelError::Declined),
        }
    }
}

// ---- Joiner ------------------------------------------------------------------------------------

/// What the Joiner shows the user before asking for the code.
pub struct JoinPrompt<'a> {
    pub host: &'a DeviceRef,
    /// `key_fingerprint` of the Host's identity key as the offer named it; the code check is
    /// what authenticates it.
    pub host_fingerprint: String,
    pub pairing_id: &'a str,
    pub expires_at_ms: u64,
}

/// What a pairing yields: the outcome and the enrolment grant (a placeholder; the transport is
/// authenticated by the Host's endpoint id, which is its identity key).
pub struct MeshJoinResult {
    pub outcome: PairingOutcome,
    pub grant: EnrolGrant,
}

/// The Joiner side over an already dialled stream (the mesh: an iroh stream to the endpoint id
/// `host_key`, which iroh authenticated in the handshake). The offer must name that key, and the
/// offer's pin must equal it (the mesh Host puts its endpoint id where the LAN Host puts its
/// certificate pin), so the typed code confirms the key exactly as it confirms the pin on the LAN.
pub fn join_over(
    stream: impl Read + Write + Send + 'static,
    control: impl PairControl + 'static,
    host_key: [u8; 32],
    identity: &DeviceIdentity,
    ask_code: impl FnOnce(&JoinPrompt<'_>) -> Option<String>,
) -> Result<MeshJoinResult, ChannelError> {
    let mut wire = Wire::new(Box::new(stream), Box::new(control));
    wire.set_timeout(FIRST_MESSAGE_WAIT)?;
    let (outcome, grant) = join_session(&mut wire, identity, host_key, ask_code)?;
    wire.finish();
    Ok(MeshJoinResult { outcome, grant })
}

/// The Joiner's message flow. `dialled_key` is the endpoint the transport authenticated; the
/// offer must name it as its host key and as its pin.
fn join_session(
    wire: &mut Wire,
    identity: &DeviceIdentity,
    dialled_key: [u8; 32],
    ask_code: impl FnOnce(&JoinPrompt<'_>) -> Option<String>,
) -> Result<(PairingOutcome, EnrolGrant), ChannelError> {
    wire.set_timeout(FIRST_MESSAGE_WAIT)?;
    let Frame::Offer(offer) = wire.read()? else {
        return Err(ChannelError::Protocol("expected offer"));
    };
    let host_key = unhex::<32>(&offer.host_key)?;
    if !ct_eq(&host_key, &dialled_key) {
        return Err(ChannelError::Protocol(
            "the offer names another key than the endpoint dialled",
        ));
    }
    // `accept_offer` also requires the offer's pin to equal the dialled key.
    let (mut session, accept) =
        JoinerPairing::accept_offer(identity, &offer, &dialled_key, now_ms())?;
    let expires = offer.expires_at_ms;

    wire.set_deadline(expires)?;
    wire.write(&Frame::Accept(accept))?;
    wire.set_deadline(expires)?;
    let Frame::HostNonce(nonce) = wire.read()? else {
        return Err(ChannelError::Protocol("expected host nonce"));
    };
    let reveal = session.receive_host_nonce(&nonce, now_ms())?;
    wire.set_deadline(expires)?;
    wire.write(&Frame::Reveal(reveal))?;

    let typed = ask_code(&JoinPrompt {
        host: &offer.host,
        host_fingerprint: key_fingerprint(&host_key),
        pairing_id: &offer.pairing_id,
        expires_at_ms: expires,
    })
    .ok_or(ChannelError::NoCode)?;
    let confirm = session.enter_code(identity, &typed, now_ms())?;
    wire.set_deadline(expires)?;
    wire.write(&Frame::JoinerConfirm(confirm))?;

    // The Host owner approves in their own time, within the session.
    wire.set_deadline(expires)?;
    let Frame::HostConfirm(host_confirm) = wire.read()? else {
        return Err(ChannelError::Protocol("expected host confirm"));
    };
    let outcome = session.finish(&host_confirm, now_ms())?;
    wire.set_deadline(expires)?;
    let Frame::Enrol(enrol) = wire.read()? else {
        return Err(ChannelError::Protocol("expected enrolment"));
    };
    let grant = session.receive_enrol(&enrol, now_ms())?;
    Ok((outcome, grant))
}

/// The Joiner side of a join token over an already dialled stream (the mesh: an iroh stream to
/// the endpoint id the token names). Nothing is typed: the joiner proves it knows the token
/// secret inside the pairing transcript. The offer must name the token's endpoint and pairing id.
pub fn join_over_token(
    stream: impl Read + Write + Send + 'static,
    control: impl PairControl + 'static,
    identity: &DeviceIdentity,
    token: &JoinToken,
) -> Result<MeshJoinResult, ChannelError> {
    let mut wire = Wire::new(Box::new(stream), Box::new(control));
    wire.set_timeout(FIRST_MESSAGE_WAIT)?;
    let (outcome, grant) = join_token_session(&mut wire, identity, token)?;
    wire.finish();
    Ok(MeshJoinResult { outcome, grant })
}

fn join_token_session(
    wire: &mut Wire,
    identity: &DeviceIdentity,
    token: &JoinToken,
) -> Result<(PairingOutcome, EnrolGrant), ChannelError> {
    wire.set_timeout(FIRST_MESSAGE_WAIT)?;
    let Frame::Offer(offer) = wire.read()? else {
        return Err(ChannelError::Protocol("expected offer"));
    };
    let host_key = unhex::<32>(&offer.host_key)?;
    if !ct_eq(&host_key, &token.endpoint) {
        return Err(ChannelError::Protocol(
            "the offer names another key than the join token",
        ));
    }
    if unhex::<16>(&offer.pairing_id)? != token.pairing_id {
        return Err(ChannelError::Protocol(
            "the offer is for another pairing than the join token",
        ));
    }
    // `accept_offer` also requires the offer's pin to equal the endpoint the token names.
    let (mut session, accept) =
        JoinerPairing::accept_offer(identity, &offer, &token.endpoint, now_ms())?;
    let expires = offer.expires_at_ms;

    wire.set_deadline(expires)?;
    wire.write(&Frame::Accept(accept))?;
    wire.set_deadline(expires)?;
    let Frame::HostNonce(nonce) = wire.read()? else {
        return Err(ChannelError::Protocol("expected host nonce"));
    };
    let reveal = session.receive_host_nonce(&nonce, now_ms())?;
    wire.set_deadline(expires)?;
    wire.write(&Frame::Reveal(reveal))?;

    let (confirm, proof) = session.enter_join(identity, &token.secret, now_ms())?;
    wire.set_deadline(expires)?;
    wire.write(&Frame::JoinerJoinConfirm(JoinProofMsg {
        confirm,
        proof: hex(&proof),
    }))?;

    wire.set_deadline(expires)?;
    let Frame::HostConfirm(host_confirm) = wire.read()? else {
        return Err(ChannelError::Protocol("expected host confirm"));
    };
    let outcome = session.finish(&host_confirm, now_ms())?;
    wire.set_deadline(expires)?;
    let Frame::Enrol(enrol) = wire.read()? else {
        return Err(ChannelError::Protocol("expected enrolment"));
    };
    let grant = session.receive_enrol(&enrol, now_ms())?;
    Ok((outcome, grant))
}

// ---- Host over a stream the caller accepted (the mesh) -----------------------------------------

/// A pairing session the Host serves over streams its transport accepts, one connection at a
/// time (the mesh hands each accepted iroh stream to [`StreamHost::serve`]). Same state machine,
/// hooks, expiry and single attempt as the TCP channel; no listener is opened here.
pub struct StreamHost {
    decisions: mpsc::Sender<Decision>,
    stop: Arc<AtomicBool>,
    offer: PairOffer,
    /// The host's own deadline: the offer's, or a lower one for a request-bound join link.
    expires_at_ms: u64,
    run: Arc<std::sync::Mutex<Option<HostRun>>>,
    resume: Arc<std::sync::Mutex<Option<ResumeState>>>,
}

fn lock_run(run: &std::sync::Mutex<Option<HostRun>>) -> std::sync::MutexGuard<'_, Option<HostRun>> {
    run.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open a stream-served session. `offer` comes from `DeviceLink::begin_pairing`; its pin is the
/// Host's endpoint id (its identity key). A watcher thread ends the session through
/// `hooks.finished` if it expires or is closed before any peer used it.
pub fn open_stream_host(
    identity: Arc<DeviceIdentity>,
    session: HostPairing,
    offer: PairOffer,
    hooks: Box<dyn HostHooks>,
) -> Result<Arc<StreamHost>, ChannelError> {
    open_host(identity, session, offer, hooks, None, None)
}

/// Open a join-token session: like [`open_stream_host`], but the one connection it serves must
/// carry the proof of the token secret (`verifier` is `pairing::join_verifier` of it), and a
/// valid proof approves without a code or a second owner step. Any failure ends the session, so
/// the token is burned by its first wrong attempt. With `required` the hooks' `code_ready` and
/// `pending` are never called; without it (a phone QR) a joiner that sends no proof takes the
/// code path.
pub fn open_stream_host_join(
    identity: Arc<DeviceIdentity>,
    session: HostPairing,
    offer: PairOffer,
    hooks: Box<dyn HostHooks>,
    verifier: [u8; 32],
    required: bool,
    deadline_ms: Option<u64>,
) -> Result<Arc<StreamHost>, ChannelError> {
    open_host(
        identity,
        session,
        offer,
        hooks,
        Some((verifier, required)),
        deadline_ms,
    )
}

fn open_host(
    identity: Arc<DeviceIdentity>,
    session: HostPairing,
    offer: PairOffer,
    hooks: Box<dyn HostHooks>,
    join: Option<([u8; 32], bool)>,
    deadline_ms: Option<u64>,
) -> Result<Arc<StreamHost>, ChannelError> {
    // A request-bound join link stops being accepted at the request's deadline (never later than
    // the offer's): the offer the joiner sees is unchanged, the host's own deadline is lower.
    let expires_at_ms = deadline_ms.map_or(offer.expires_at_ms, |d| d.min(offer.expires_at_ms));
    let (decide_tx, decide_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let run = Arc::new(std::sync::Mutex::new(Some(HostRun {
        identity,
        session,
        pairing_id: offer.pairing_id.clone(),
        expires_at_ms,
        hooks,
        join,
        decisions: decide_rx,
        stop: Arc::clone(&stop),
        peer: [0u8; 32],
        cache: None,
    })));
    let host = Arc::new(StreamHost {
        decisions: decide_tx,
        stop,
        offer,
        expires_at_ms,
        run,
        resume: Arc::new(std::sync::Mutex::new(None)),
    });
    let watcher = Arc::clone(&host);
    thread::Builder::new()
        .name("rightkit-pairing-watch".to_string())
        .spawn(move || {
            loop {
                thread::sleep(DECISION_POLL);
                if lock_run(&watcher.run).is_none() {
                    return;
                }
                let ended = if watcher.stop.load(Ordering::SeqCst) {
                    Some(ChannelError::Declined)
                } else if remaining(watcher.expires_at_ms).is_err() {
                    Some(ChannelError::Expired)
                } else {
                    None
                };
                if let Some(error) = ended {
                    let taken = lock_run(&watcher.run).take();
                    if let Some(mut run) = taken {
                        run.hooks.finished(&Err(error));
                    }
                    return;
                }
            }
        })?;
    Ok(host)
}

impl StreamHost {
    pub fn decide(&self, decision: Decision) -> bool {
        self.decisions.send(decision).is_ok()
    }

    pub fn pairing_id(&self) -> &str {
        &self.offer.pairing_id
    }

    pub fn close(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Run the session on `stream`, blocking until it ends. A connection that fails before its
    /// first valid message leaves the session untouched for the next. The transport must have
    /// authenticated the peer's key; the pairing itself authenticates the person (the typed code
    /// and the owner's approval), as on the LAN.
    pub fn serve(
        &self,
        peer: [u8; 32],
        stream: impl Read + Write + Send + 'static,
        control: impl PairControl + 'static,
    ) {
        // A finished session still serves a Joiner that repeats its confirmation (the window is
        // checked in `serve_resume`); a fresh attempt needs a live, open session.
        let resumable = lock_resume(&self.resume).is_some();
        if !resumable
            && (self.stop.load(Ordering::SeqCst) || remaining(self.expires_at_ms).is_err())
        {
            return;
        }
        let mut wire = Wire::new(Box::new(stream), Box::new(control));
        if wire.set_timeout(FIRST_MESSAGE_WAIT).is_err() {
            return;
        }
        let accept = match exchange_offer(&mut wire, &self.offer) {
            Ok(First::Accept(accept)) => *accept,
            Ok(First::Resume(confirm)) => {
                self.serve_resume(peer, wire, &confirm);
                return;
            }
            Err(_) => return,
        };
        let Some(mut run) = lock_run(&self.run).take() else {
            return;
        };
        run.peer = peer;
        let result = host_session(wire, &accept, &mut run);
        let Some(cache) = run.cache.take() else {
            run.hooks.finished(&result);
            return;
        };
        // The owner approved and the record exists. A failed delivery is not final yet: the
        // Joiner may come back within the window; only the window's end revokes it.
        let delivered = result.is_ok();
        let pairing_id = run.pairing_id.clone();
        let until_ms = now_ms().saturating_add(RESUME_WINDOW_MS);
        let mut hooks = run.hooks;
        if delivered {
            hooks.finished(&result);
        }
        *lock_resume(&self.resume) = Some(ResumeState {
            pairing_id: pairing_id.clone(),
            peer,
            cache,
            until_ms,
            attempts: 0,
            hooks,
            ended: delivered,
        });
        let state = Arc::clone(&self.resume);
        let _ = thread::Builder::new()
            .name("rightkit-pairing-resume".to_string())
            .spawn(move || {
                thread::sleep(Duration::from_millis(RESUME_WINDOW_MS));
                let mut guard = lock_resume(&state);
                if guard.as_ref().is_some_and(|s| s.pairing_id == pairing_id)
                    && let Some(mut dropped) = guard.take()
                    && !dropped.ended
                {
                    dropped.hooks.finished(&Err(ChannelError::Expired));
                }
            });
    }

    /// A Joiner repeated its confirmation on a new stream. Served only for the same pairing id,
    /// the same Joiner key (the transport's and the signed confirmation's) and the same
    /// confirmation, within the window, while the record is still active. Anything else ends the
    /// stream; only an expired window, too many repeats or a revoked record drop the cache. The stream is the only error channel: the Joiner
    /// treats a close here as final.
    fn serve_resume(&self, peer: [u8; 32], mut wire: Wire, confirm: &PairConfirm) {
        let lines = {
            let mut guard = lock_resume(&self.resume);
            let Some(state) = guard.as_mut() else {
                return;
            };
            let same = ct_eq(&state.peer, &peer)
                && ct_eq(state.pairing_id.as_bytes(), confirm.pairing_id.as_bytes())
                && ct_eq(
                    state.cache.joiner_confirm.pairing_id.as_bytes(),
                    confirm.pairing_id.as_bytes(),
                )
                && ct_eq(
                    state.cache.joiner_confirm.device_id.as_bytes(),
                    confirm.device_id.as_bytes(),
                )
                && ct_eq(
                    state.cache.joiner_confirm.signature.as_bytes(),
                    confirm.signature.as_bytes(),
                )
                && state.cache.joiner_confirm.schema_version == confirm.schema_version;
            // A stranger's or a different key's request changes nothing: it cannot burn the
            // cache or end the pending delivery. Only a matching request counts as an attempt.
            if !same {
                return;
            }
            state.attempts = state.attempts.saturating_add(1);
            let live = now_ms() < state.until_ms
                && state.attempts <= MAX_RESUMES
                && !self.stop.load(Ordering::SeqCst);
            if !(live && state.hooks.resume_allowed(&state.pairing_id, &peer)) {
                if let Some(mut dropped) = guard.take()
                    && !dropped.ended
                {
                    dropped.hooks.finished(&Err(ChannelError::Expired));
                }
                return;
            }
            state.cache.lines.clone()
        };
        let sent = wire
            .set_timeout(FIRST_MESSAGE_WAIT)
            .and_then(|()| wire.write_raw(&lines))
            .is_ok();
        if sent {
            if let Some(state) = lock_resume(&self.resume).as_mut()
                && !state.ended
            {
                state.ended = true;
                state.hooks.finished(&Ok(()));
            }
            wire.finish();
        }
    }
}

fn lock_resume(
    state: &std::sync::Mutex<Option<ResumeState>>,
) -> std::sync::MutexGuard<'_, Option<ResumeState>> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
