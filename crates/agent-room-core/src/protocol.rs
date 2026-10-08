use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Open,
    Positions,
    PeerDebate,
    Dispositions,
    AuthorReview,
    Escalation,
    Voting,
    Verdict,
    AbortedFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Position,
    Question,
    Evidence,
    Info,
    Finding,
    Disposition,
    Resolution,
    Ruling,
    Motion,
    Vote,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatBudget {
    pub max_messages: u64,
    pub max_send_bytes: u64,
    pub max_wakes: u64,
    pub max_context_bytes: u64,
}

impl Default for SeatBudget {
    fn default() -> Self {
        Self {
            max_messages: 12,
            max_send_bytes: 65_536,
            max_wakes: 12,
            max_context_bytes: 262_144,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SeatUsage {
    pub sent_messages: u64,
    pub sent_bytes: u64,
    pub wakes: u64,
    pub context_bytes: u64,
    pub abstained: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatConfig {
    pub seat_id: String,
    pub name: String,
    pub model: String,
    pub role: String,
    #[serde(default)]
    pub budget: SeatBudget,
}

impl SeatConfig {
    pub fn new(seat_id: &str, name: &str, model: &str, role: &str) -> Self {
        Self {
            seat_id: seat_id.into(),
            name: name.into(),
            model: model.into(),
            role: role.into(),
            budget: SeatBudget::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomConfig {
    pub room_id: String,
    pub mode: String,
    pub at_budget: String,
    pub workspace_root: PathBuf,
    pub seats: Vec<SeatConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub room_id: String,
    pub seat_id: String,
    pub phase: Phase,
    pub request_id: String,
    pub kind: EventKind,
    pub body: String,
    pub reply_to: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_seat_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    pub code: &'static str,
    pub detail: String,
}

impl ProtocolError {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    /// HTTP-facing errors only; the server is the sole caller.
    #[cfg(feature = "http")]
    pub(crate) fn external(code: &'static str, detail: impl Into<String>) -> Self {
        Self::new(code, detail)
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingInput {
    pub claim: String,
    pub severity: String,
    pub rationale: String,
    pub evidence_refs: Vec<String>,
    pub proposed_change: Option<String>,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingStatus {
    Open,
    ExitProposed,
    Recontested,
    Folded,
    Refuted,
}

impl FindingStatus {
    fn terminal(self) -> bool {
        matches!(self, Self::Folded | Self::Refuted)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub finding_id: String,
    pub author_seat: String,
    pub claim: String,
    pub severity: String,
    pub rationale: String,
    pub evidence_refs: Vec<String>,
    pub proposed_change: Option<String>,
    pub confidence: Option<f64>,
    pub status: FindingStatus,
    pub disposition_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispositionAction {
    Folded,
    Refuted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DispositionStatus {
    #[default]
    Proposed,
    Accepted,
    Recontested,
    Ruled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionChoice {
    Accept,
    Recontest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeoutOutcome {
    Rewake,
    Escalated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    FileLine,
    Artifact,
    Measurement,
    Web,
    ReviewScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptRef {
    pub kind: ReceiptKind,
    pub locator: String,
    pub line: Option<u64>,
    pub digest: Option<String>,
}

impl ReceiptRef {
    pub fn file_line(locator: &str, line: u64) -> Self {
        Self {
            kind: ReceiptKind::FileLine,
            locator: locator.into(),
            line: Some(line),
            digest: None,
        }
    }

    pub fn artifact(locator: &str, digest: &str) -> Self {
        Self {
            kind: ReceiptKind::Artifact,
            locator: locator.into(),
            line: None,
            digest: Some(digest.into()),
        }
    }

    pub fn review_scope(motion_id: &str, digest: &str) -> Self {
        Self {
            kind: ReceiptKind::ReviewScope,
            locator: motion_id.into(),
            line: None,
            digest: Some(digest.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disposition {
    pub disposition_id: String,
    pub finding_id: String,
    pub implementer_seat: String,
    pub action: DispositionAction,
    pub reason_code: Option<String>,
    pub receipt_refs: Vec<ReceiptRef>,
    #[serde(default)]
    pub status: DispositionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contest {
    pub contest_id: String,
    pub finding_id: String,
    pub author_seat: String,
    pub rationale: String,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeRef {
    pub locator: String,
    pub digest: String,
}

impl ScopeRef {
    pub fn pinned(locator: &str, digest: &str) -> Self {
        Self {
            locator: locator.into(),
            digest: digest.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Motion {
    pub motion_id: String,
    pub kind: String,
    pub text: String,
    pub scope_refs: Vec<ScopeRef>,
    pub scope_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealReceipt {
    pub room_id: String,
    pub event_count: usize,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RequestRecord {
    canonical: String,
    result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub room_id: String,
    pub mode: String,
    pub at_budget: String,
    pub workspace_root: PathBuf,
    pub phase: Phase,
    pub seats: BTreeMap<String, SeatConfig>,
    #[serde(default)]
    pub seat_usage: BTreeMap<String, SeatUsage>,
    pub events: Vec<Event>,
    pub findings: BTreeMap<String, Finding>,
    pub contests: BTreeMap<String, Contest>,
    pub dispositions: BTreeMap<String, Disposition>,
    pub motions: BTreeMap<String, Motion>,
    #[serde(default)]
    pub response_rewakes: BTreeMap<String, u8>,
    #[serde(default)]
    pub escalated_dispositions: BTreeMap<String, bool>,
    requests: BTreeMap<String, RequestRecord>,
    pub sealed: bool,
}

impl Room {
    pub fn new(config: RoomConfig) -> Result<Self, ProtocolError> {
        if config.room_id.trim().is_empty()
            || !config
                .room_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err(ProtocolError::new("invalid_room_id", config.room_id));
        }
        if matches!(config.mode.as_str(), "worksplit" | "freeform") {
            return Err(ProtocolError::new("mode_unshipped", config.mode));
        }
        if !matches!(config.mode.as_str(), "review" | "debate") {
            return Err(ProtocolError::new("invalid_mode", config.mode));
        }
        if !matches!(
            config.at_budget.as_str(),
            "abstain" | "summarize-and-continue"
        ) {
            return Err(ProtocolError::new("invalid_at_budget", config.at_budget));
        }
        let workspace_root = config
            .workspace_root
            .canonicalize()
            .map_err(|error| ProtocolError::new("workspace_missing", error.to_string()))?;
        let seats = config
            .seats
            .into_iter()
            .map(|seat| (seat.seat_id.clone(), seat))
            .collect::<BTreeMap<_, _>>();
        if seats.is_empty() {
            return Err(ProtocolError::new("seats_required", "room has no seats"));
        }
        let seat_usage = seats
            .keys()
            .map(|seat_id| (seat_id.clone(), SeatUsage::default()))
            .collect();
        Ok(Self {
            room_id: config.room_id,
            mode: config.mode,
            at_budget: config.at_budget,
            workspace_root,
            phase: Phase::Open,
            seats,
            seat_usage,
            events: Vec::new(),
            findings: BTreeMap::new(),
            contests: BTreeMap::new(),
            dispositions: BTreeMap::new(),
            motions: BTreeMap::new(),
            response_rewakes: BTreeMap::new(),
            escalated_dispositions: BTreeMap::new(),
            requests: BTreeMap::new(),
            sealed: false,
        })
    }

    fn stable_id(&self, prefix: &str, parts: &[&str]) -> String {
        let mut digest = Sha256::new();
        digest.update(self.room_id.as_bytes());
        for part in parts {
            digest.update([0x1f]);
            digest.update(part.as_bytes());
        }
        format!("{prefix}_{:x}", digest.finalize())[..prefix.len() + 1 + 20].to_string()
    }

    fn require_seat(&self, seat_id: &str) -> Result<&SeatConfig, ProtocolError> {
        self.seats
            .get(seat_id)
            .ok_or_else(|| ProtocolError::new("unknown_seat", seat_id))
    }

    fn require_role(&self, seat_id: &str, roles: &[&str]) -> Result<(), ProtocolError> {
        let seat = self.require_seat(seat_id)?;
        if roles.contains(&seat.role.as_str()) {
            Ok(())
        } else {
            Err(ProtocolError::new(
                "role_forbidden",
                format!("{seat_id}:{}", seat.role),
            ))
        }
    }

    fn apply_idempotent<T, F>(
        &mut self,
        actor: &str,
        request_id: &str,
        operation: &str,
        payload: Value,
        apply: F,
    ) -> Result<T, ProtocolError>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce(&mut Self) -> Result<T, ProtocolError>,
    {
        self.require_seat(actor)?;
        if request_id.trim().is_empty() {
            return Err(ProtocolError::new("request_id_required", ""));
        }
        let canonical = serde_json::to_string(&json!({"operation": operation, "payload": payload}))
            .map_err(|error| ProtocolError::new("serialization_failed", error.to_string()))?;
        let key = format!("{actor}\u{1f}{request_id}");
        if let Some(prior) = self.requests.get(&key) {
            if prior.canonical != canonical {
                return Err(ProtocolError::new("request_id_conflict", request_id));
            }
            return serde_json::from_value(prior.result.clone())
                .map_err(|error| ProtocolError::new("replay_failed", error.to_string()));
        }
        if self.sealed {
            return Err(ProtocolError::new("room_sealed", self.room_id.clone()));
        }
        let result = apply(self)?;
        let result_value = serde_json::to_value(&result)
            .map_err(|error| ProtocolError::new("serialization_failed", error.to_string()))?;
        self.requests.insert(
            key,
            RequestRecord {
                canonical,
                result: result_value,
            },
        );
        Ok(result)
    }

    fn append_event(
        &mut self,
        actor: &str,
        request_id: &str,
        kind: EventKind,
        body: String,
        reply_to: Option<u64>,
    ) -> Event {
        self.append_event_targeted(actor, request_id, kind, body, reply_to, None)
    }

    fn append_event_targeted(
        &mut self,
        actor: &str,
        request_id: &str,
        kind: EventKind,
        body: String,
        reply_to: Option<u64>,
        target_seat_id: Option<String>,
    ) -> Event {
        let event = Event {
            seq: self.events.len() as u64 + 1,
            room_id: self.room_id.clone(),
            seat_id: actor.into(),
            phase: self.phase,
            request_id: request_id.into(),
            kind,
            body,
            reply_to,
            target_seat_id,
        };
        self.events.push(event.clone());
        event
    }

    fn consume_send(&mut self, actor: &str, bytes: u64) -> Result<(), ProtocolError> {
        let budget = self.require_seat(actor)?.budget.clone();
        let usage = self
            .seat_usage
            .get_mut(actor)
            .expect("usage exists for every seat");
        if usage.sent_messages >= budget.max_messages {
            return Err(ProtocolError::new("message_budget_exhausted", actor));
        }
        if usage.sent_bytes.saturating_add(bytes) > budget.max_send_bytes {
            return Err(ProtocolError::new("byte_budget_exhausted", actor));
        }
        usage.sent_messages += 1;
        usage.sent_bytes += bytes;
        Ok(())
    }

    pub fn advance(&mut self, actor: &str, target: Phase) -> Result<(), ProtocolError> {
        self.require_role(actor, &["human", "moderator"])?;
        let allowed = matches!(
            (self.phase, target),
            (Phase::Open, Phase::Positions)
                | (Phase::Positions, Phase::PeerDebate)
                | (Phase::PeerDebate, Phase::Dispositions)
                | (Phase::Dispositions, Phase::AuthorReview)
                | (Phase::AuthorReview, Phase::Escalation)
                | (Phase::AuthorReview, Phase::Voting)
                | (Phase::Escalation, Phase::Voting)
                | (Phase::Voting, Phase::Verdict)
        );
        if !allowed {
            return Err(ProtocolError::new(
                "invalid_transition",
                format!("{:?}->{target:?}", self.phase),
            ));
        }
        if self.phase == Phase::Dispositions && target == Phase::AuthorReview {
            let open = self
                .findings
                .values()
                .filter(|finding| finding.status == FindingStatus::Open)
                .map(|finding| finding.finding_id.clone())
                .collect::<Vec<_>>();
            if !open.is_empty() {
                return Err(ProtocolError::new("open_findings", open.join(",")));
            }
        }
        if matches!(
            (self.phase, target),
            (Phase::AuthorReview, Phase::Voting) | (Phase::Escalation, Phase::Voting)
        ) {
            let open = self.open_finding_ids();
            if !open.is_empty() {
                return Err(ProtocolError::new("open_findings", open.join(",")));
            }
        }
        if self.phase == Phase::AuthorReview
            && target == Phase::Escalation
            && !self
                .findings
                .values()
                .any(|finding| finding.status == FindingStatus::Recontested)
        {
            return Err(ProtocolError::new("no_escalations", ""));
        }
        self.phase = target;
        Ok(())
    }

    pub fn advance_idempotent(
        &mut self,
        actor: &str,
        request_id: &str,
        target: Phase,
    ) -> Result<Event, ProtocolError> {
        self.require_role(actor, &["human", "moderator"])?;
        self.apply_idempotent(
            actor,
            request_id,
            "advance",
            json!({"target":target}),
            move |room| {
                room.advance(actor, target)?;
                Ok(room.append_event(
                    actor,
                    request_id,
                    EventKind::System,
                    format!("phase:{target:?}"),
                    None,
                ))
            },
        )
    }

    pub fn post(
        &mut self,
        actor: &str,
        request_id: &str,
        kind: EventKind,
        body: &str,
        reply_to: Option<u64>,
    ) -> Result<Event, ProtocolError> {
        self.post_targeted(actor, request_id, kind, body, reply_to, None)
    }

    pub fn post_targeted(
        &mut self,
        actor: &str,
        request_id: &str,
        kind: EventKind,
        body: &str,
        reply_to: Option<u64>,
        target_seat_id: Option<&str>,
    ) -> Result<Event, ProtocolError> {
        let body = body.trim().to_string();
        if body.is_empty() {
            return Err(ProtocolError::new("body_required", ""));
        }
        if kind == EventKind::Position && self.phase != Phase::Positions {
            return Err(ProtocolError::new("phase_forbidden", "position"));
        }
        let target_seat_id = target_seat_id
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .map(str::to_owned);
        if let Some(target) = target_seat_id.as_deref() {
            if !self.seats.contains_key(target) {
                return Err(ProtocolError::new("unknown_target_seat", target));
            }
        }
        self.apply_idempotent(
            actor,
            request_id,
            "post",
            json!({"kind":kind,"body":body,"reply_to":reply_to,"target_seat_id":target_seat_id}),
            move |room| {
                room.consume_send(actor, body.len() as u64)?;
                Ok(room.append_event_targeted(
                    actor,
                    request_id,
                    kind,
                    body,
                    reply_to,
                    target_seat_id,
                ))
            },
        )
    }

    pub fn visible_events(&self, actor: &str, cursor: u64) -> Result<Vec<Event>, ProtocolError> {
        self.require_seat(actor)?;
        Ok(self
            .events
            .iter()
            .filter(|event| event.seq > cursor)
            .filter(|event| {
                !(self.phase == Phase::Positions
                    && event.kind == EventKind::Position
                    && event.seat_id != actor)
            })
            .cloned()
            .collect())
    }

    pub fn next(&mut self, actor: &str, cursor: u64) -> Result<Vec<Event>, ProtocolError> {
        let budget = self.require_seat(actor)?.budget.clone();
        let events = self.visible_events(actor, cursor)?;
        let usage = self
            .seat_usage
            .get_mut(actor)
            .expect("usage exists for every seat");
        if usage.wakes >= budget.max_wakes {
            return Err(ProtocolError::new("wake_budget_exhausted", actor));
        }
        usage.wakes += 1;
        self.record_context(actor, &events)?;
        Ok(events)
    }

    pub fn deliver_after_wait(
        &mut self,
        actor: &str,
        cursor: u64,
    ) -> Result<Vec<Event>, ProtocolError> {
        let events = self.visible_events(actor, cursor)?;
        self.record_context(actor, &events)?;
        Ok(events)
    }

    fn record_context(&mut self, actor: &str, events: &[Event]) -> Result<(), ProtocolError> {
        let budget = self.require_seat(actor)?.budget.clone();
        let delivered_bytes = events
            .iter()
            .map(|event| event.body.len() as u64)
            .sum::<u64>();
        let usage = self
            .seat_usage
            .get_mut(actor)
            .expect("usage exists for every seat");
        usage.context_bytes = usage.context_bytes.saturating_add(delivered_bytes);
        if usage.context_bytes > budget.max_context_bytes {
            return match self.at_budget.as_str() {
                "abstain" => {
                    usage.abstained = true;
                    Err(ProtocolError::new("seat_abstained_at_budget", actor))
                }
                "summarize-and-continue" => {
                    Err(ProtocolError::new("summary_required_at_budget", actor))
                }
                _ => unreachable!("Room::new validates at_budget"),
            };
        }
        Ok(())
    }

    pub fn seat_usage(&self, actor: &str) -> Result<&SeatUsage, ProtocolError> {
        self.require_seat(actor)?;
        Ok(self
            .seat_usage
            .get(actor)
            .expect("usage exists for every seat"))
    }

    pub(crate) fn request_rows(&self) -> Result<Vec<(String, String)>, ProtocolError> {
        self.requests
            .iter()
            .map(|(key, record)| {
                serde_json::to_string(record)
                    .map(|json| (key.clone(), json))
                    .map_err(|error| ProtocolError::new("serialization_failed", error.to_string()))
            })
            .collect()
    }

    pub fn raise(
        &mut self,
        actor: &str,
        request_id: &str,
        input: FindingInput,
    ) -> Result<Finding, ProtocolError> {
        if !matches!(self.phase, Phase::PeerDebate | Phase::Dispositions) {
            return Err(ProtocolError::new("phase_forbidden", "finding"));
        }
        if input.claim.trim().is_empty() || input.rationale.trim().is_empty() {
            return Err(ProtocolError::new(
                "finding_invalid",
                "claim and rationale are required",
            ));
        }
        if !matches!(input.severity.as_str(), "P0" | "P1" | "P2") {
            return Err(ProtocolError::new("finding_invalid", "severity"));
        }
        self.apply_idempotent(actor, request_id, "raise", json!(&input), move |room| {
            let finding_id = room.stable_id("finding", &[actor, request_id, &input.claim]);
            let finding = Finding {
                finding_id: finding_id.clone(),
                author_seat: actor.into(),
                claim: input.claim,
                severity: input.severity,
                rationale: input.rationale,
                evidence_refs: input.evidence_refs,
                proposed_change: input.proposed_change,
                confidence: input.confidence,
                status: FindingStatus::Open,
                disposition_id: None,
            };
            room.findings.insert(finding_id, finding.clone());
            room.append_event(
                actor,
                request_id,
                EventKind::Finding,
                finding.claim.clone(),
                None,
            );
            Ok(finding)
        })
    }

    pub fn contest(
        &mut self,
        actor: &str,
        request_id: &str,
        finding_id: &str,
        rationale: &str,
        evidence_refs: Vec<String>,
    ) -> Result<Contest, ProtocolError> {
        if !matches!(self.phase, Phase::PeerDebate | Phase::Dispositions) {
            return Err(ProtocolError::new("phase_forbidden", "contest"));
        }
        let finding = self
            .findings
            .get(finding_id)
            .ok_or_else(|| ProtocolError::new("unknown_finding", finding_id))?;
        if finding.author_seat == actor {
            return Err(ProtocolError::new("self_contest_forbidden", actor));
        }
        if rationale.trim().is_empty() {
            return Err(ProtocolError::new("rationale_required", ""));
        }
        let finding_id = finding_id.to_string();
        let rationale = rationale.trim().to_string();
        self.apply_idempotent(
            actor,
            request_id,
            "contest",
            json!({"finding_id":finding_id,"rationale":rationale,"evidence_refs":evidence_refs}),
            move |room| {
                let contest_id = room.stable_id("contest", &[actor, request_id, &finding_id]);
                let contest = Contest {
                    contest_id: contest_id.clone(),
                    finding_id,
                    author_seat: actor.into(),
                    rationale: rationale.clone(),
                    evidence_refs,
                };
                room.contests.insert(contest_id, contest.clone());
                room.append_event(actor, request_id, EventKind::Info, rationale, None);
                Ok(contest)
            },
        )
    }

    pub fn calculate_scope_digest(&self, refs: &[ScopeRef]) -> Result<String, ProtocolError> {
        if refs.is_empty() {
            return Err(ProtocolError::new("scope_refs_required", ""));
        }
        let mut verified = Vec::with_capacity(refs.len());
        for scope_ref in refs {
            let path = self.resolve_workspace_path(&scope_ref.locator)?;
            let actual =
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(path).map_err(|error| ProtocolError::new(
                        "scope_unreadable",
                        error.to_string()
                    ))?)
                );
            if actual != scope_ref.digest {
                return Err(ProtocolError::new(
                    "scope_artifact_digest_mismatch",
                    scope_ref.locator.clone(),
                ));
            }
            verified.push(scope_ref.clone());
        }
        verified.sort_by(|left, right| left.locator.cmp(&right.locator));
        let canonical = serde_json::to_vec(&verified)
            .map_err(|error| ProtocolError::new("serialization_failed", error.to_string()))?;
        Ok(format!("{:x}", Sha256::digest(canonical)))
    }

    pub fn create_review_scope(
        &mut self,
        actor: &str,
        request_id: &str,
        scope_refs: Vec<ScopeRef>,
        claimed_digest: &str,
    ) -> Result<Motion, ProtocolError> {
        self.require_role(actor, &["human", "moderator"])?;
        if self.phase != Phase::Open {
            return Err(ProtocolError::new("phase_forbidden", "review_scope"));
        }
        if self
            .motions
            .values()
            .any(|motion| motion.kind == "review_scope")
        {
            return Err(ProtocolError::new("review_scope_exists", ""));
        }
        let digest = self.calculate_scope_digest(&scope_refs)?;
        if digest != claimed_digest {
            return Err(ProtocolError::new("scope_digest_mismatch", claimed_digest));
        }
        self.apply_idempotent(
            actor,
            request_id,
            "create_review_scope",
            json!({"scope_refs":scope_refs,"scope_digest":digest}),
            move |room| {
                let motion_id = room.stable_id("motion", &["review_scope", &digest]);
                let text = format!(
                    "Did the built phase prove the {} pinned Loop-1 obligations?",
                    scope_refs.len()
                );
                let motion = Motion {
                    motion_id: motion_id.clone(),
                    kind: "review_scope".into(),
                    text: text.clone(),
                    scope_refs,
                    scope_digest: Some(digest),
                };
                room.motions.insert(motion_id, motion.clone());
                room.append_event(actor, request_id, EventKind::Motion, text, None);
                Ok(motion)
            },
        )
    }

    fn resolve_workspace_path(&self, locator: &str) -> Result<PathBuf, ProtocolError> {
        let candidate = self.workspace_root.join(locator);
        let resolved = candidate
            .canonicalize()
            .map_err(|_| ProtocolError::new("receipt_missing", locator))?;
        if !resolved.starts_with(&self.workspace_root) || !resolved.is_file() {
            return Err(ProtocolError::new("receipt_outside_scope", locator));
        }
        Ok(resolved)
    }

    fn validate_receipts(
        &self,
        action: DispositionAction,
        receipts: &[ReceiptRef],
    ) -> Result<(), ProtocolError> {
        if receipts.is_empty() {
            return Err(ProtocolError::new("receipt_required", ""));
        }
        let required = match action {
            DispositionAction::Folded => ReceiptKind::Artifact,
            DispositionAction::Refuted => ReceiptKind::FileLine,
        };
        if action == DispositionAction::Folded
            && !receipts.iter().any(|receipt| receipt.kind == required)
        {
            return Err(ProtocolError::new("fold_artifact_required", ""));
        }
        for receipt in receipts {
            match receipt.kind {
                ReceiptKind::FileLine => {
                    let path = self.resolve_workspace_path(&receipt.locator)?;
                    let lines = std::fs::read_to_string(&path)
                        .map_err(|error| {
                            ProtocolError::new("receipt_unreadable", error.to_string())
                        })?
                        .lines()
                        .count() as u64;
                    let line = receipt
                        .line
                        .ok_or_else(|| ProtocolError::new("receipt_line_required", ""))?;
                    if line == 0 || line > lines {
                        return Err(ProtocolError::new("receipt_line_invalid", line.to_string()));
                    }
                }
                ReceiptKind::Artifact | ReceiptKind::Measurement => {
                    let path = self.resolve_workspace_path(&receipt.locator)?;
                    let digest = receipt
                        .digest
                        .as_deref()
                        .ok_or_else(|| ProtocolError::new("receipt_digest_required", ""))?;
                    let actual = format!(
                        "{:x}",
                        Sha256::digest(std::fs::read(path).map_err(|error| ProtocolError::new(
                            "receipt_unreadable",
                            error.to_string()
                        ))?)
                    );
                    if actual != digest {
                        return Err(ProtocolError::new(
                            "receipt_digest_mismatch",
                            receipt.locator.clone(),
                        ));
                    }
                }
                ReceiptKind::Web => {
                    if !(receipt.locator.starts_with("https://")
                        || receipt.locator.starts_with("http://"))
                    {
                        return Err(ProtocolError::new(
                            "receipt_url_invalid",
                            receipt.locator.clone(),
                        ));
                    }
                }
                ReceiptKind::ReviewScope => {}
            }
        }
        Ok(())
    }

    pub fn dispose(
        &mut self,
        actor: &str,
        request_id: &str,
        finding_id: &str,
        action: DispositionAction,
        reason_code: Option<String>,
        receipts: Vec<ReceiptRef>,
    ) -> Result<Disposition, ProtocolError> {
        self.require_role(actor, &["implementer"])?;
        if self.phase != Phase::Dispositions {
            return Err(ProtocolError::new("phase_forbidden", "dispose"));
        }
        let finding = self
            .findings
            .get(finding_id)
            .ok_or_else(|| ProtocolError::new("unknown_finding", finding_id))?;
        if finding.status != FindingStatus::Open {
            return Err(ProtocolError::new("finding_not_open", finding_id));
        }
        if reason_code.as_deref() == Some("out_of_scope") {
            let valid_scope = receipts.iter().any(|receipt| {
                if receipt.kind != ReceiptKind::ReviewScope {
                    return false;
                }
                self.motions.get(&receipt.locator).is_some_and(|motion| {
                    motion.kind == "review_scope"
                        && motion.scope_digest.as_deref() == receipt.digest.as_deref()
                })
            });
            if !valid_scope {
                return Err(ProtocolError::new("scope_receipt_invalid", finding_id));
            }
        }
        self.validate_receipts(action, &receipts)?;
        let finding_id_owned = finding_id.to_string();
        self.apply_idempotent(
            actor,
            request_id,
            "dispose",
            json!({"finding_id":finding_id,"action":action,"reason_code":reason_code,"receipts":receipts}),
            move |room| {
                let disposition_id = room.stable_id("disposition", &[actor, request_id, &finding_id_owned]);
                let disposition = Disposition {
                    disposition_id: disposition_id.clone(),
                    finding_id: finding_id_owned.clone(),
                    implementer_seat: actor.into(),
                    action,
                    reason_code,
                    receipt_refs: receipts,
                    status: DispositionStatus::Proposed,
                };
                let finding = room.findings.get_mut(&finding_id_owned).unwrap();
                finding.status = FindingStatus::ExitProposed;
                finding.disposition_id = Some(disposition_id.clone());
                room.dispositions.insert(disposition_id, disposition.clone());
                room.append_event(actor, request_id, EventKind::Disposition, finding_id_owned, None);
                Ok(disposition)
            },
        )
    }

    pub fn resolve(
        &mut self,
        actor: &str,
        request_id: &str,
        disposition_id: &str,
        choice: ResolutionChoice,
        reason: &str,
        evidence_refs: Vec<String>,
    ) -> Result<Finding, ProtocolError> {
        if self.phase != Phase::AuthorReview {
            return Err(ProtocolError::new("phase_forbidden", "resolve"));
        }
        let disposition = self
            .dispositions
            .get(disposition_id)
            .ok_or_else(|| ProtocolError::new("unknown_disposition", disposition_id))?;
        let finding = self.findings.get(&disposition.finding_id).unwrap();
        if finding.author_seat != actor {
            return Err(ProtocolError::new("author_required", actor));
        }
        if reason.trim().is_empty() {
            return Err(ProtocolError::new("reason_required", ""));
        }
        let disposition_id_owned = disposition_id.to_string();
        let finding_id = disposition.finding_id.clone();
        self.apply_idempotent(
            actor,
            request_id,
            "resolve",
            json!({"disposition_id":disposition_id,"choice":choice,"reason":reason,"evidence_refs":evidence_refs}),
            move |room| {
                let disposition = room.dispositions.get_mut(&disposition_id_owned).unwrap();
                let action = disposition.action;
                disposition.status = match choice {
                    ResolutionChoice::Accept => DispositionStatus::Accepted,
                    ResolutionChoice::Recontest => DispositionStatus::Recontested,
                };
                let finding = room.findings.get_mut(&finding_id).unwrap();
                finding.status = match choice {
                    ResolutionChoice::Accept => match action {
                        DispositionAction::Folded => FindingStatus::Folded,
                        DispositionAction::Refuted => FindingStatus::Refuted,
                    },
                    ResolutionChoice::Recontest => FindingStatus::Recontested,
                };
                let result = finding.clone();
                room.append_event(actor, request_id, EventKind::Resolution, reason.into(), None);
                Ok(result)
            },
        )
    }

    pub fn rule(
        &mut self,
        actor: &str,
        request_id: &str,
        disposition_id: &str,
        action: DispositionAction,
        reason: &str,
        receipts: Vec<ReceiptRef>,
    ) -> Result<Finding, ProtocolError> {
        self.require_role(actor, &["human"])?;
        if self.phase != Phase::Escalation {
            return Err(ProtocolError::new("phase_forbidden", "rule"));
        }
        self.validate_receipts(action, &receipts)?;
        let disposition = self
            .dispositions
            .get(disposition_id)
            .ok_or_else(|| ProtocolError::new("unknown_disposition", disposition_id))?;
        let finding_id = disposition.finding_id.clone();
        if self.findings.get(&finding_id).unwrap().status != FindingStatus::Recontested {
            return Err(ProtocolError::new("finding_not_recontested", finding_id));
        }
        self.apply_idempotent(
            actor,
            request_id,
            "rule",
            json!({"disposition_id":disposition_id,"action":action,"reason":reason,"receipts":receipts}),
            move |room| {
                let disposition = room.dispositions.get_mut(disposition_id).unwrap();
                disposition.action = action;
                disposition.receipt_refs = receipts;
                disposition.status = DispositionStatus::Ruled;
                let finding = room.findings.get_mut(&finding_id).unwrap();
                finding.status = match action {
                    DispositionAction::Folded => FindingStatus::Folded,
                    DispositionAction::Refuted => FindingStatus::Refuted,
                };
                let result = finding.clone();
                room.append_event(actor, request_id, EventKind::Ruling, reason.into(), None);
                Ok(result)
            },
        )
    }

    pub fn author_timeout(
        &mut self,
        actor: &str,
        request_id: &str,
        disposition_id: &str,
    ) -> Result<TimeoutOutcome, ProtocolError> {
        self.require_role(actor, &["human", "moderator"])?;
        if self.phase != Phase::AuthorReview {
            return Err(ProtocolError::new("phase_forbidden", "author_timeout"));
        }
        let disposition = self
            .dispositions
            .get(disposition_id)
            .ok_or_else(|| ProtocolError::new("unknown_disposition", disposition_id))?;
        let finding = self
            .findings
            .get(&disposition.finding_id)
            .expect("disposition finding exists");
        if finding.status != FindingStatus::ExitProposed {
            return Err(ProtocolError::new(
                "author_review_not_pending",
                disposition_id,
            ));
        }
        let disposition_id = disposition_id.to_string();
        self.apply_idempotent(
            actor,
            request_id,
            "author_timeout",
            json!({"disposition_id":disposition_id}),
            move |room| {
                let rewakes = room
                    .response_rewakes
                    .entry(disposition_id.clone())
                    .or_default();
                if *rewakes == 0 {
                    *rewakes = 1;
                    room.append_event(
                        actor,
                        request_id,
                        EventKind::System,
                        format!("response_rewake_required:{disposition_id}"),
                        None,
                    );
                    return Ok(TimeoutOutcome::Rewake);
                }
                let finding_id = room
                    .dispositions
                    .get(&disposition_id)
                    .expect("validated disposition")
                    .finding_id
                    .clone();
                room.findings
                    .get_mut(&finding_id)
                    .expect("disposition finding exists")
                    .status = FindingStatus::Recontested;
                room.escalated_dispositions
                    .insert(disposition_id.clone(), true);
                room.append_event(
                    actor,
                    request_id,
                    EventKind::System,
                    format!("author_timeout_escalated:{disposition_id}"),
                    None,
                );
                Ok(TimeoutOutcome::Escalated)
            },
        )
    }

    pub fn escalation_rate(&self) -> f64 {
        if self.dispositions.is_empty() {
            0.0
        } else {
            self.escalated_dispositions.len() as f64 / self.dispositions.len() as f64
        }
    }

    pub fn failed_by_majority_escalation(&self) -> bool {
        self.escalation_rate() > 0.5
    }

    pub fn vote(
        &mut self,
        actor: &str,
        request_id: &str,
        motion_id: &str,
        choice: &str,
        reason: &str,
    ) -> Result<Event, ProtocolError> {
        if !self.motions.contains_key(motion_id) {
            return Err(ProtocolError::new("unknown_motion", motion_id));
        }
        if self.phase != Phase::Voting {
            return Err(ProtocolError::new("phase_forbidden", "vote"));
        }
        self.post(
            actor,
            request_id,
            EventKind::Vote,
            &format!("{motion_id}:{choice}:{reason}"),
            None,
        )
    }

    pub fn implementer_timeout(&mut self, actor: &str) -> Result<Event, ProtocolError> {
        self.require_role(actor, &["human", "moderator"])?;
        if self.phase != Phase::Dispositions {
            return Err(ProtocolError::new("phase_forbidden", "implementer_timeout"));
        }
        self.phase = Phase::AbortedFallback;
        Ok(self.append_event(
            actor,
            "timeout",
            EventKind::System,
            "fallback_required".into(),
            None,
        ))
    }

    pub fn finding(&self, finding_id: &str) -> Option<&Finding> {
        self.findings.get(finding_id)
    }

    pub fn open_finding_ids(&self) -> Vec<String> {
        self.findings
            .values()
            .filter(|finding| !finding.status.terminal())
            .map(|finding| finding.finding_id.clone())
            .collect()
    }

    pub fn seal(&mut self, actor: &str) -> Result<SealReceipt, ProtocolError> {
        self.require_role(actor, &["human", "moderator"])?;
        if self.phase != Phase::Verdict {
            return Err(ProtocolError::new("phase_forbidden", "seal"));
        }
        let open = self.open_finding_ids();
        if !open.is_empty() {
            return Err(ProtocolError::new("open_findings", open.join(",")));
        }
        let bytes = events_jsonl_bytes(&self.events)?;
        let receipt = SealReceipt {
            room_id: self.room_id.clone(),
            event_count: self.events.len(),
            digest: format!("{:x}", Sha256::digest(bytes)),
        };
        self.sealed = true;
        Ok(receipt)
    }

    pub fn seal_idempotent(
        &mut self,
        actor: &str,
        request_id: &str,
    ) -> Result<SealReceipt, ProtocolError> {
        self.require_role(actor, &["human", "moderator"])?;
        self.apply_idempotent(actor, request_id, "seal", json!({}), move |room| {
            room.seal(actor)
        })
    }
}

pub fn events_jsonl_bytes(events: &[Event]) -> Result<Vec<u8>, ProtocolError> {
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, event)
            .map_err(|error| ProtocolError::new("serialization_failed", error.to_string()))?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}
