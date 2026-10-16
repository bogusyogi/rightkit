//! Capture ownership (protocol minor 4): who may hold the microphone, who preempts whom, and
//! how a stuck owner is fenced. The rules live here once so every speech host and every
//! client reads the same ones.
//!
//! One capture mode is active at a time. Priority: HeardRight dictation, then a Pulse
//! conversation, then batch. A meeting is never silently preempted: dictation takes it only
//! with an explicit confirmation, and nothing else takes it.
//!
//! [`CaptureArbiter`] is the reference state machine for a host. It owns no I/O and reads no
//! clock: the caller passes `now_ms`, so tests and hosts agree on every transition.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How often an owner should send a heartbeat.
pub const CAPTURE_HEARTBEAT_MS: u64 = 2_000;
/// An owner silent for this long is fenced.
pub const CAPTURE_LEASE_MS: u64 = 6_000;
/// A preemptor's claim on a capture that is being released lapses after this long.
pub const CAPTURE_RESERVATION_MS: u64 = 6_000;

/// What a capture is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureRole {
    /// HeardRight dictation: the highest priority.
    HeardRightDictation,
    /// A Pulse conversation.
    PulseConversation,
    /// A meeting recording. Never silently preempted.
    Meeting,
    /// Batch capture: the lowest priority.
    Batch,
}

impl CaptureRole {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HeardRightDictation => "heard_right_dictation",
            Self::PulseConversation => "pulse_conversation",
            Self::Meeting => "meeting",
            Self::Batch => "batch",
        }
    }
    pub fn from_wire(s: &str) -> Option<Self> {
        [
            Self::HeardRightDictation,
            Self::PulseConversation,
            Self::Meeting,
            Self::Batch,
        ]
        .into_iter()
        .find(|r| r.as_str() == s)
    }
    /// Priority rank, lower wins. A meeting has none: it is outside the ordering.
    fn rank(self) -> Option<u8> {
        match self {
            Self::HeardRightDictation => Some(0),
            Self::PulseConversation => Some(1),
            Self::Batch => Some(2),
            Self::Meeting => None,
        }
    }
    /// Whether `self` has strictly higher priority than `other`. A meeting outranks nothing
    /// and is outranked by nothing here; [`decide`] handles it separately.
    pub fn outranks(self, other: Self) -> bool {
        matches!((self.rank(), other.rank()), (Some(a), Some(b)) if a < b)
    }
}

/// One capture owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureOwner {
    pub role: CaptureRole,
    /// Stable id of the owning session; the key for heartbeat, release and cancel.
    pub owner_id: String,
    pub pid: u32,
}

/// The rule for a request against the current capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureDecision {
    /// Nothing is capturing.
    Free,
    /// The requester outranks the owner: the owner must release at once.
    Preempt,
    /// The owner is a meeting and the requester is dictation without a confirmation.
    ConfirmRequired,
    /// The owner keeps the capture.
    Busy,
}

/// Decide `requested` against the active role. `confirmed` is the user's explicit yes to
/// ending a meeting for dictation; it changes nothing else.
pub fn decide(
    active: Option<CaptureRole>,
    requested: CaptureRole,
    confirmed: bool,
) -> CaptureDecision {
    let Some(active) = active else {
        return CaptureDecision::Free;
    };
    if active == CaptureRole::Meeting {
        return match (requested, confirmed) {
            (CaptureRole::HeardRightDictation, true) => CaptureDecision::Preempt,
            (CaptureRole::HeardRightDictation, false) => CaptureDecision::ConfirmRequired,
            _ => CaptureDecision::Busy,
        };
    }
    if requested.outranks(active) {
        CaptureDecision::Preempt
    } else {
        CaptureDecision::Busy
    }
}

/// A capture request on the wire (`capture_control` frames).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureCommand {
    /// Ask for the capture. `confirm_preempt` carries the user's confirmation to end a meeting.
    Acquire {
        owner: CaptureOwner,
        confirm_preempt: bool,
    },
    /// The owner is alive. Send every [`CAPTURE_HEARTBEAT_MS`].
    Heartbeat { owner_id: String },
    /// The owner has stopped capturing.
    Release { owner_id: String },
    /// Stop: close the capture and discard pending uploads. The sender does not wait for a reply.
    Cancel { owner_id: String },
}

pub(crate) const KIND_ACQUIRE: &str = "capture_acquire";
pub(crate) const KIND_HEARTBEAT: &str = "capture_heartbeat";
pub(crate) const KIND_RELEASE: &str = "capture_release";
pub(crate) const KIND_CANCEL: &str = "capture_cancel";
pub(crate) const KIND_STATE: &str = "capture_state";

impl CaptureCommand {
    pub(crate) fn to_payload(&self) -> Value {
        match self {
            Self::Acquire {
                owner,
                confirm_preempt,
            } => json!({
                "kind": KIND_ACQUIRE,
                "role": owner.role.as_str(),
                "owner_id": owner.owner_id,
                "pid": owner.pid,
                "confirm_preempt": confirm_preempt,
            }),
            Self::Heartbeat { owner_id } => json!({"kind": KIND_HEARTBEAT, "owner_id": owner_id}),
            Self::Release { owner_id } => json!({"kind": KIND_RELEASE, "owner_id": owner_id}),
            Self::Cancel { owner_id } => json!({"kind": KIND_CANCEL, "owner_id": owner_id}),
        }
    }

    pub(crate) fn from_payload(kind: &str, payload: &Value) -> Option<Self> {
        let owner_id = payload
            .get("owner_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())?
            .to_string();
        match kind {
            KIND_ACQUIRE => Some(Self::Acquire {
                owner: CaptureOwner {
                    role: CaptureRole::from_wire(payload.get("role")?.as_str()?)?,
                    owner_id,
                    pid: u32::try_from(payload.get("pid")?.as_u64()?).ok()?,
                },
                confirm_preempt: payload
                    .get("confirm_preempt")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }),
            KIND_HEARTBEAT => Some(Self::Heartbeat { owner_id }),
            KIND_RELEASE => Some(Self::Release { owner_id }),
            KIND_CANCEL => Some(Self::Cancel { owner_id }),
            _ => None,
        }
    }
}

/// The host's answer to a capture command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStatus {
    /// The requester holds the capture.
    Granted,
    /// The owner (`owner`) is releasing for a higher-priority requester; retry after it releases.
    Preempting,
    /// The owner is a meeting; send the acquire again with `confirm_preempt` after asking the user.
    ConfirmRequired,
    /// `owner` keeps the capture.
    Busy,
    /// `owner` missed its heartbeats and is fenced. The capture stays with it until release is proven.
    Fenced,
    /// Heartbeat accepted.
    Ok,
    /// Heartbeat refused: this owner is being preempted. Release at once and close pending uploads.
    Preempted,
    /// Released. `owner` is the requester that now holds the capture, if any.
    Released,
    /// The sender is not the owner. Nothing changed.
    NotOwner,
}

impl CaptureStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Preempting => "preempting",
            Self::ConfirmRequired => "confirm_required",
            Self::Busy => "busy",
            Self::Fenced => "fenced",
            Self::Ok => "ok",
            Self::Preempted => "preempted",
            Self::Released => "released",
            Self::NotOwner => "not_owner",
        }
    }
    pub fn from_wire(s: &str) -> Option<Self> {
        [
            Self::Granted,
            Self::Preempting,
            Self::ConfirmRequired,
            Self::Busy,
            Self::Fenced,
            Self::Ok,
            Self::Preempted,
            Self::Released,
            Self::NotOwner,
        ]
        .into_iter()
        .find(|s2| s2.as_str() == s)
    }
}

/// A `capture_state` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureReply {
    pub status: CaptureStatus,
    /// The other party the status is about, when there is one (see [`CaptureStatus`]).
    pub owner: Option<CaptureOwner>,
}

impl CaptureReply {
    pub(crate) fn to_payload(&self) -> Value {
        json!({"kind": KIND_STATE, "status": self.status.as_str(), "owner": self.owner})
    }
    pub(crate) fn from_payload(payload: &Value) -> Option<Self> {
        (payload.get("kind").and_then(Value::as_str) == Some(KIND_STATE)).then_some(())?;
        Some(Self {
            status: CaptureStatus::from_wire(payload.get("status")?.as_str()?)?,
            owner: payload
                .get("owner")
                .filter(|o| !o.is_null())
                .and_then(|o| serde_json::from_value(o.clone()).ok()),
        })
    }
}

impl From<&AcquireOutcome> for CaptureReply {
    fn from(outcome: &AcquireOutcome) -> Self {
        let (status, owner) = match outcome {
            AcquireOutcome::Granted => (CaptureStatus::Granted, None),
            AcquireOutcome::Preempting { victim } => {
                (CaptureStatus::Preempting, Some(victim.clone()))
            }
            AcquireOutcome::ConfirmRequired { owner } => {
                (CaptureStatus::ConfirmRequired, Some(owner.clone()))
            }
            AcquireOutcome::Busy { owner } => (CaptureStatus::Busy, Some(owner.clone())),
            AcquireOutcome::Fenced { owner } => (CaptureStatus::Fenced, Some(owner.clone())),
        };
        Self { status, owner }
    }
}

impl From<&HeartbeatOutcome> for CaptureReply {
    fn from(outcome: &HeartbeatOutcome) -> Self {
        let status = match outcome {
            HeartbeatOutcome::Ok => CaptureStatus::Ok,
            HeartbeatOutcome::Preempted => CaptureStatus::Preempted,
            HeartbeatOutcome::Fenced => CaptureStatus::Fenced,
            HeartbeatOutcome::NotOwner => CaptureStatus::NotOwner,
        };
        Self {
            status,
            owner: None,
        }
    }
}

impl From<&ReleaseOutcome> for CaptureReply {
    fn from(outcome: &ReleaseOutcome) -> Self {
        match outcome {
            ReleaseOutcome::Released { handoff_to, .. } => Self {
                status: CaptureStatus::Released,
                owner: handoff_to.clone(),
            },
            ReleaseOutcome::StillFenced { owner } => Self {
                status: CaptureStatus::Fenced,
                owner: Some(owner.clone()),
            },
            ReleaseOutcome::NotOwner => Self {
                status: CaptureStatus::NotOwner,
                owner: None,
            },
        }
    }
}

/// Where the current owner stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePhase {
    /// Capturing and heartbeating.
    Active,
    /// A higher-priority requester is waiting; the owner has been told to release at once.
    Preempting,
    /// The owner missed its heartbeats. Nobody else gets the capture until release is proven.
    Fenced,
}

/// The result of [`CaptureArbiter::acquire`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    /// The requester holds the capture (also for an owner asking again).
    Granted,
    /// `victim` must release; the requester is next and gets the capture the moment it does.
    Preempting { victim: CaptureOwner },
    /// `owner` is a meeting. Ask the user, then acquire again with the confirmation.
    ConfirmRequired { owner: CaptureOwner },
    /// `owner` keeps the capture.
    Busy { owner: CaptureOwner },
    /// `owner` is fenced and the capture is not free until its release is proven.
    Fenced { owner: CaptureOwner },
}

/// The result of [`CaptureArbiter::heartbeat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    Ok,
    /// The owner is being preempted: release at once and close pending uploads.
    Preempted,
    /// The lease lapsed. A late heartbeat does not revive the owner.
    Fenced,
    NotOwner,
}

/// The result of [`CaptureArbiter::release`], [`CaptureArbiter::cancel`] and [`CaptureArbiter::prove_release`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// The capture is free, or went at once to the waiting requester `handoff_to`.
    /// `discarded_pending` is true when pending uploads are to be closed without waiting.
    Released {
        handoff_to: Option<CaptureOwner>,
        discarded_pending: bool,
    },
    /// The owner is fenced and its own word is not proof. The host must prove release
    /// (the process is gone or the device is closed) and call [`CaptureArbiter::prove_release`].
    StillFenced {
        owner: CaptureOwner,
    },
    NotOwner,
}

#[derive(Debug, Clone)]
struct Held {
    owner: CaptureOwner,
    phase: CapturePhase,
    /// Last heartbeat; for a preempted owner, the time it was told to release.
    since_ms: u64,
}

#[derive(Debug, Clone)]
struct Reservation {
    owner: CaptureOwner,
    at_ms: u64,
}

/// Single-owner capture arbitration. Not thread-safe by itself: a host keeps it behind its own lock.
#[derive(Debug, Clone, Default)]
pub struct CaptureArbiter {
    held: Option<Held>,
    reserved: Option<Reservation>,
}

impl CaptureArbiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current owner, in any phase.
    pub fn current(&self) -> Option<&CaptureOwner> {
        self.held.as_ref().map(|h| &h.owner)
    }

    /// The current owner's phase. Call [`tick`](Self::tick) first for an up-to-date answer.
    pub fn phase(&self) -> Option<CapturePhase> {
        self.held.as_ref().map(|h| h.phase)
    }

    /// Fence an owner whose lease lapsed and drop a stale reservation. Every other method
    /// calls this first, so a host only needs it to answer a status query between commands.
    pub fn tick(&mut self, now_ms: u64) {
        if let Some(h) = self.held.as_mut() {
            if h.phase != CapturePhase::Fenced
                && now_ms.saturating_sub(h.since_ms) >= CAPTURE_LEASE_MS
            {
                h.phase = CapturePhase::Fenced;
            }
        }
        if self
            .reserved
            .as_ref()
            .is_some_and(|r| now_ms.saturating_sub(r.at_ms) >= CAPTURE_RESERVATION_MS)
        {
            self.reserved = None;
        }
    }

    /// Ask for the capture for `who`. `confirmed` is the user's confirmation to end a meeting for dictation.
    pub fn acquire(&mut self, who: CaptureOwner, confirmed: bool, now_ms: u64) -> AcquireOutcome {
        self.tick(now_ms);
        let Some(held) = self.held.clone() else {
            if let Some(r) = &self.reserved {
                if r.owner.owner_id != who.owner_id && !who.role.outranks(r.owner.role) {
                    return AcquireOutcome::Busy {
                        owner: r.owner.clone(),
                    };
                }
            }
            self.reserved = None;
            self.held = Some(Held {
                owner: who,
                phase: CapturePhase::Active,
                since_ms: now_ms,
            });
            return AcquireOutcome::Granted;
        };
        if held.owner.owner_id == who.owner_id {
            return match held.phase {
                CapturePhase::Active => {
                    if let Some(h) = self.held.as_mut() {
                        h.since_ms = now_ms;
                    }
                    AcquireOutcome::Granted
                }
                CapturePhase::Preempting => AcquireOutcome::Preempting { victim: held.owner },
                CapturePhase::Fenced => AcquireOutcome::Fenced { owner: held.owner },
            };
        }
        match held.phase {
            CapturePhase::Fenced => AcquireOutcome::Fenced { owner: held.owner },
            CapturePhase::Preempting => {
                let waiting = self.reserved.as_ref().map(|r| r.owner.clone());
                match waiting {
                    Some(w) if w.owner_id == who.owner_id => {
                        AcquireOutcome::Preempting { victim: held.owner }
                    }
                    Some(w) if who.role.outranks(w.role) => {
                        self.reserved = Some(Reservation {
                            owner: who,
                            at_ms: now_ms,
                        });
                        AcquireOutcome::Preempting { victim: held.owner }
                    }
                    _ => AcquireOutcome::Busy { owner: held.owner },
                }
            }
            CapturePhase::Active => match decide(Some(held.owner.role), who.role, confirmed) {
                CaptureDecision::Preempt => {
                    self.held = Some(Held {
                        owner: held.owner.clone(),
                        phase: CapturePhase::Preempting,
                        since_ms: now_ms,
                    });
                    self.reserved = Some(Reservation {
                        owner: who,
                        at_ms: now_ms,
                    });
                    AcquireOutcome::Preempting { victim: held.owner }
                }
                CaptureDecision::ConfirmRequired => {
                    AcquireOutcome::ConfirmRequired { owner: held.owner }
                }
                CaptureDecision::Busy | CaptureDecision::Free => {
                    AcquireOutcome::Busy { owner: held.owner }
                }
            },
        }
    }

    /// The owner is alive. A preempted owner learns it here and must release at once.
    pub fn heartbeat(&mut self, owner_id: &str, now_ms: u64) -> HeartbeatOutcome {
        self.tick(now_ms);
        match self.held.as_mut() {
            Some(h) if h.owner.owner_id == owner_id => match h.phase {
                CapturePhase::Active => {
                    h.since_ms = now_ms;
                    HeartbeatOutcome::Ok
                }
                CapturePhase::Preempting => HeartbeatOutcome::Preempted,
                CapturePhase::Fenced => HeartbeatOutcome::Fenced,
            },
            _ => HeartbeatOutcome::NotOwner,
        }
    }

    /// The owner says it stopped capturing. Proof enough for an active or preempted owner;
    /// not for a fenced one (see [`ReleaseOutcome::StillFenced`]).
    pub fn release(&mut self, owner_id: &str, now_ms: u64) -> ReleaseOutcome {
        self.finish(owner_id, now_ms, false)
    }

    /// Stop: like [`release`](Self::release), and the host closes pending uploads without waiting.
    pub fn cancel(&mut self, owner_id: &str, now_ms: u64) -> ReleaseOutcome {
        self.finish(owner_id, now_ms, true)
    }

    fn finish(&mut self, owner_id: &str, now_ms: u64, discard: bool) -> ReleaseOutcome {
        self.tick(now_ms);
        let Some(held) = self.held.as_ref() else {
            return ReleaseOutcome::NotOwner;
        };
        if held.owner.owner_id != owner_id {
            return ReleaseOutcome::NotOwner;
        }
        if held.phase == CapturePhase::Fenced {
            return ReleaseOutcome::StillFenced {
                owner: held.owner.clone(),
            };
        }
        self.clear(now_ms, discard)
    }

    /// The host has proven the owner no longer captures (its process is gone, or the device is
    /// closed). The only way out of the fenced phase. Pending uploads are discarded.
    pub fn prove_release(&mut self, owner_id: &str, now_ms: u64) -> ReleaseOutcome {
        self.tick(now_ms);
        let ours = self
            .held
            .as_ref()
            .is_some_and(|h| h.owner.owner_id == owner_id);
        if ours {
            self.clear(now_ms, true)
        } else {
            ReleaseOutcome::NotOwner
        }
    }

    fn clear(&mut self, now_ms: u64, discarded_pending: bool) -> ReleaseOutcome {
        self.held = None;
        let handoff_to = self.reserved.take().map(|r| r.owner);
        if let Some(next) = &handoff_to {
            self.held = Some(Held {
                owner: next.clone(),
                phase: CapturePhase::Active,
                since_ms: now_ms,
            });
        }
        ReleaseOutcome::Released {
            handoff_to,
            discarded_pending,
        }
    }

    /// Apply a wire command and return the reply. `now_ms` is the host's clock.
    pub fn apply(&mut self, command: &CaptureCommand, now_ms: u64) -> CaptureReply {
        match command {
            CaptureCommand::Acquire {
                owner,
                confirm_preempt,
            } => CaptureReply::from(&self.acquire(owner.clone(), *confirm_preempt, now_ms)),
            CaptureCommand::Heartbeat { owner_id } => {
                CaptureReply::from(&self.heartbeat(owner_id, now_ms))
            }
            CaptureCommand::Release { owner_id } => {
                CaptureReply::from(&self.release(owner_id, now_ms))
            }
            CaptureCommand::Cancel { owner_id } => {
                CaptureReply::from(&self.cancel(owner_id, now_ms))
            }
        }
    }
}
