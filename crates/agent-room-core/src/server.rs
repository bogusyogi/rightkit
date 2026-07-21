use crate::auth::{AuthRegistry, JoinRequest, JoinResponse};
use crate::protocol::{Event, Room};
use axum::body::Body;
use axum::extract::{Form, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::path::{Path as FilePath, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

const WATCH_COOKIE: &str = "agent_room_watch";
const CSP: &str = "default-src 'self'; script-src 'nonce-agent-room'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'";
const PAIR_CSP: &str = "default-src 'self'; script-src 'none'; style-src 'self' 'unsafe-inline'; connect-src 'none'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

#[derive(Clone)]
pub struct ServerState {
    room: Arc<RwLock<Room>>,
    participant_tokens: Arc<BTreeMap<String, String>>,
    auth: Arc<RwLock<AuthRegistry>>,
    pairing_code: Arc<str>,
    browser_session: Arc<str>,
    exact_origin: Arc<str>,
    event_tx: broadcast::Sender<Event>,
    run_dir: Option<Arc<PathBuf>>,
}

impl ServerState {
    pub fn new(
        room: Arc<RwLock<Room>>,
        participant_tokens: impl IntoIterator<Item = (String, String)>,
        pairing_code: impl Into<String>,
        exact_origin: impl Into<String>,
    ) -> Self {
        let pairing_code = pairing_code.into();
        let browser_session = format!(
            "{:x}",
            Sha256::digest(format!("agent-room-browser-session\u{1f}{pairing_code}").as_bytes())
        );
        let auth = {
            let room = room.read().expect("room lock poisoned");
            AuthRegistry::new(&room)
        };
        let (event_tx, _) = broadcast::channel(256);
        Self {
            room,
            participant_tokens: Arc::new(participant_tokens.into_iter().collect()),
            auth: Arc::new(RwLock::new(auth)),
            pairing_code: Arc::from(pairing_code),
            browser_session: Arc::from(browser_session),
            exact_origin: Arc::from(exact_origin.into()),
            event_tx,
            run_dir: None,
        }
    }

    pub fn with_run_dir(mut self, run_dir: &FilePath) -> Self {
        self.run_dir = Some(Arc::new(run_dir.to_path_buf()));
        self
    }

    pub fn bootstrap_join(&self, request: JoinRequest) -> Result<JoinResponse, String> {
        let response = {
            let room = self.room.read().map_err(|_| "room lock poisoned")?;
            let mut auth = self.auth.write().map_err(|_| "auth lock poisoned")?;
            auth.join(&room, request)
                .map_err(|error| error.to_string())?
        };
        let should_open_positions = {
            let room = self.room.read().map_err(|_| "room lock poisoned")?;
            let auth = self.auth.read().map_err(|_| "auth lock poisoned")?;
            room.phase == crate::protocol::Phase::Open
                && room
                    .seats
                    .values()
                    .filter(|seat| seat.role != "human")
                    .all(|seat| auth.has_joined(&seat.seat_id))
        };
        if should_open_positions {
            let mut room = self.room.write().map_err(|_| "room lock poisoned")?;
            let human = room
                .seats
                .values()
                .find(|seat| seat.role == "human")
                .map(|seat| seat.seat_id.clone())
                .ok_or("human seat missing")?;
            room.advance(&human, crate::protocol::Phase::Positions)
                .map_err(|error| error.to_string())?;
            self.persist_room(&room)
                .map_err(|error| error.to_string())?;
        }
        Ok(response)
    }

    fn persist_room(&self, room: &Room) -> Result<(), crate::store::StoreError> {
        let Some(run_dir) = &self.run_dir else {
            return Ok(());
        };
        let mut store = crate::store::RoomStore::open(run_dir)?;
        store.save(room)
    }

    fn seat_for_bearer(&self, headers: &HeaderMap) -> Option<String> {
        let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
        let token = value.strip_prefix("Bearer ")?;
        self.participant_tokens.get(token).cloned().or_else(|| {
            self.auth
                .read()
                .ok()?
                .authenticate(token)
                .map(str::to_owned)
        })
    }

    fn browser_authorized(&self, headers: &HeaderMap) -> bool {
        let origin_ok = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .map(|origin| origin == &*self.exact_origin)
            .unwrap_or(true);
        if !origin_ok {
            return false;
        }
        headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(|cookies| {
                cookies.split(';').any(|cookie| {
                    let mut parts = cookie.trim().splitn(2, '=');
                    parts.next() == Some(WATCH_COOKIE)
                        && parts.next() == Some(self.browser_session.as_ref())
                })
            })
            .unwrap_or(false)
    }

    fn browser_mutation_authorized(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|origin| origin == &*self.exact_origin)
            && self.browser_authorized(headers)
    }
}

#[derive(Debug, Serialize)]
struct StatusSnapshot {
    room_id: String,
    seat_id: String,
    mode: String,
    phase: crate::protocol::Phase,
    latest_visible_cursor: u64,
    at_budget: String,
    budget: crate::protocol::SeatBudget,
    usage: crate::protocol::SeatUsage,
    escalation_rate: f64,
    open_finding_ids: Vec<String>,
    motions: Vec<crate::protocol::Motion>,
    events: Vec<Event>,
}

#[derive(Debug, Serialize)]
struct WatchSeat {
    seat_id: String,
    name: String,
    model: String,
    role: String,
    budget: crate::protocol::SeatBudget,
    usage: crate::protocol::SeatUsage,
    activity: SeatActivity,
}

#[derive(Debug, Serialize)]
struct SeatActivity {
    state: String,
    detail: String,
    last_event_seq: Option<u64>,
    ready_event_seq: Option<u64>,
    stdout_log: Option<String>,
    stderr_log: Option<String>,
}

#[derive(Debug, Serialize)]
struct PhaseProgress {
    required: usize,
    responded: usize,
    outstanding_seat_ids: Vec<String>,
    open_findings: usize,
}

#[derive(Debug, Serialize)]
struct RoomHealth {
    status: String,
    stage: Option<String>,
    failures: Vec<WatchFailure>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WatchFailure {
    seat: String,
    code: String,
    detail: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    stdout_log: Option<String>,
    #[serde(default)]
    stderr_log: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SeatSessionsView {
    #[serde(default)]
    status: String,
    #[serde(default)]
    seats: Vec<SeatSessionView>,
}

#[derive(Debug, Deserialize)]
struct SeatSessionView {
    seat_id: String,
    status: String,
    #[serde(default)]
    ready_event_seq: Option<u64>,
    #[serde(default)]
    stdout_log: Option<String>,
    #[serde(default)]
    stderr_log: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LaunchFailureView {
    #[serde(default)]
    stage: Option<String>,
    #[serde(default)]
    seat_failures: Vec<WatchFailure>,
}

#[derive(Debug, Serialize)]
struct WatchSnapshot {
    room_id: String,
    mode: String,
    phase: crate::protocol::Phase,
    sealed: bool,
    seats: Vec<WatchSeat>,
    findings: Vec<crate::protocol::Finding>,
    dispositions: Vec<crate::protocol::Disposition>,
    events: Vec<Event>,
    escalation_rate: f64,
    failed_by_majority_escalation: bool,
    phase_progress: PhaseProgress,
    health: RoomHealth,
}

#[derive(Debug, Deserialize)]
struct StreamQuery {
    #[serde(default)]
    cursor: u64,
    #[serde(default)]
    once: bool,
}

#[derive(Debug, Deserialize)]
struct PairForm {
    pairing_code: String,
}

#[derive(Debug, Deserialize)]
struct SayRequest {
    request_id: String,
    body: String,
    #[serde(default)]
    target_seat_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PostRequest {
    request_id: String,
    kind: crate::protocol::EventKind,
    body: String,
    #[serde(default)]
    reply_to: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RaiseRequest {
    request_id: String,
    claim: String,
    severity: String,
    rationale: String,
    #[serde(default)]
    evidence_refs: Vec<String>,
    #[serde(default)]
    proposed_change: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContestRequest {
    request_id: String,
    finding_id: String,
    rationale: String,
    #[serde(default)]
    evidence_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisposeRequest {
    request_id: String,
    finding_id: String,
    action: crate::protocol::DispositionAction,
    #[serde(default)]
    reason_code: Option<String>,
    receipt_refs: Vec<crate::protocol::ReceiptRef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest {
    request_id: String,
    disposition_id: String,
    choice: crate::protocol::ResolutionChoice,
    reason: String,
    #[serde(default)]
    evidence_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleRequest {
    request_id: String,
    disposition_id: String,
    action: crate::protocol::DispositionAction,
    reason: String,
    receipt_refs: Vec<crate::protocol::ReceiptRef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MotionRequest {
    request_id: String,
    kind: String,
    scope_refs: Vec<crate::protocol::ScopeRef>,
    scope_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NextRequest {
    #[serde(default)]
    cursor: u64,
    #[serde(default = "default_wait_ms")]
    wait_ms: u64,
}

fn default_wait_ms() -> u64 {
    45_000
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VoteRequest {
    request_id: String,
    motion_id: String,
    choice: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdvanceRequest {
    request_id: String,
    target: crate::protocol::Phase,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealRequest {
    request_id: String,
}

pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/pair", get(pair_page).post(pair))
        .route("/join/{capability}", get(join_link))
        .route("/api/status", get(status))
        .route("/api/watch-status", get(watch_status))
        .route("/api/join", post(join))
        .route("/api/stream", get(stream))
        .route("/api/watch-stream", get(watch_stream))
        .route("/api/say", post(say))
        .route("/api/browser-say", post(browser_say))
        .route("/api/tool/{tool}", post(tool))
        .route("/api/admin/advance", post(admin_advance))
        .route("/api/admin/seal", post(admin_seal))
        .with_state(state)
}

async fn admin_seal(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SealRequest>,
) -> impl IntoResponse {
    let Some(seat_id) = state.seat_for_bearer(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut room = state.room.write().expect("room lock poisoned");
    if room
        .seats
        .get(&seat_id)
        .is_none_or(|seat| seat.role != "human")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    match room.seal_idempotent(&seat_id, &request.request_id) {
        Ok(receipt) => {
            if let Err(error) = state.persist_room(&room) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error":error.code,"detail":error.detail})),
                )
                    .into_response();
            }
            Json(receipt).into_response()
        }
        Err(error) => protocol_error(error),
    }
}

async fn admin_advance(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<AdvanceRequest>,
) -> impl IntoResponse {
    let Some(seat_id) = state.seat_for_bearer(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut room = state.room.write().expect("room lock poisoned");
    if room
        .seats
        .get(&seat_id)
        .is_none_or(|seat| seat.role != "human")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    match room.advance_idempotent(&seat_id, &request.request_id, request.target) {
        Ok(event) => {
            if let Err(error) = state.persist_room(&room) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error":error.code,"detail":error.detail})),
                )
                    .into_response();
            }
            let _ = state.event_tx.send(event.clone());
            Json(event).into_response()
        }
        Err(error) => protocol_error(error),
    }
}

fn private_json<T: for<'de> Deserialize<'de>>(state: &ServerState, name: &str) -> Option<T> {
    let run_dir = state.run_dir.as_deref()?;
    let bytes = std::fs::read(run_dir.join(name)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn log_name(value: Option<&str>) -> Option<String> {
    value
        .and_then(|path| FilePath::new(path).file_name())
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

fn phase_progress(room: &Room) -> PhaseProgress {
    let required_ids = room
        .seats
        .values()
        .filter(|seat| seat.role != "human")
        .map(|seat| seat.seat_id.clone())
        .collect::<Vec<_>>();
    let responded_ids = room
        .events
        .iter()
        .filter(|event| event.phase == room.phase)
        .filter(|event| required_ids.contains(&event.seat_id))
        .filter(|event| event.kind != crate::protocol::EventKind::System)
        .filter(|event| !event.body.starts_with("agent-room-ready:"))
        .map(|event| event.seat_id.clone())
        .collect::<BTreeSet<_>>();
    let outstanding_seat_ids = if room.sealed {
        Vec::new()
    } else {
        required_ids
            .iter()
            .filter(|seat_id| !responded_ids.contains(*seat_id))
            .cloned()
            .collect()
    };
    PhaseProgress {
        required: required_ids.len(),
        responded: responded_ids.len(),
        outstanding_seat_ids,
        open_findings: room.open_finding_ids().len(),
    }
}

fn room_health(state: &ServerState, room: &Room, sessions: &SeatSessionsView) -> RoomHealth {
    if let Some(mut failure) = private_json::<LaunchFailureView>(state, "launch.failure.json") {
        for item in &mut failure.seat_failures {
            item.action = match item.code.as_str() {
                "authentication_failed" => "Sign in to the seat CLI, then relaunch the Council.",
                "permission_denied" | "interactive_permission_required" => {
                    "Enable unattended MCP/tool permission for this seat, then relaunch the Council."
                }
                "readiness_timeout" | "seat_exited_before_ready" => {
                    "Open the preserved seat logs, fix the startup failure, then relaunch the Council."
                }
                _ => "Open the preserved seat logs and relaunch the Council.",
            }
            .into();
            item.stdout_log = log_name(item.stdout_log.as_deref());
            item.stderr_log = log_name(item.stderr_log.as_deref());
        }
        return RoomHealth {
            status: "fallback_required".into(),
            stage: failure.stage,
            failures: failure.seat_failures,
        };
    }
    RoomHealth {
        status: if sessions.status == "aborted" {
            "fallback_required"
        } else if room.sealed {
            "sealed"
        } else {
            "active"
        }
        .into(),
        stage: None,
        failures: Vec::new(),
    }
}

fn seat_activity(
    room: &Room,
    seat: &crate::protocol::SeatConfig,
    usage: &crate::protocol::SeatUsage,
    joined: bool,
    session: Option<&SeatSessionView>,
) -> SeatActivity {
    let last_event_seq = room
        .events
        .iter()
        .rev()
        .find(|event| event.seat_id == seat.seat_id)
        .map(|event| event.seq);
    let pending_attention = room.events.iter().rev().find(|event| {
        event.target_seat_id.as_deref() == Some(seat.seat_id.as_str())
            && last_event_seq.is_none_or(|seq| event.seq > seq)
    });
    let responded_this_phase = room.events.iter().any(|event| {
        event.phase == room.phase
            && event.seat_id == seat.seat_id
            && event.kind != crate::protocol::EventKind::System
            && !event.body.starts_with("agent-room-ready:")
    });
    let (state, detail) = if seat.role == "human" {
        ("observer", "Watching and able to interject".into())
    } else if session.is_some_and(|record| record.status == "failed") {
        ("failed", "Seat process failed".into())
    } else if room.sealed {
        ("complete", "Room sealed".into())
    } else if usage.abstained {
        ("at_budget", "Seat abstained at budget".into())
    } else if let Some(event) = pending_attention {
        ("attention", format!("Addressed in message #{}", event.seq))
    } else if responded_this_phase {
        ("responded", format!("Posted in {:?}", room.phase))
    } else if usage.wakes > 0 {
        ("reading", format!("Reading {:?} context", room.phase))
    } else if joined || session.is_some_and(|record| record.status == "ready") {
        ("ready", format!("Ready for {:?}", room.phase))
    } else {
        ("starting", "Waiting to join".into())
    };
    SeatActivity {
        state: state.into(),
        detail,
        last_event_seq,
        ready_event_seq: session.and_then(|record| record.ready_event_seq),
        stdout_log: log_name(session.and_then(|record| record.stdout_log.as_deref())),
        stderr_log: log_name(session.and_then(|record| record.stderr_log.as_deref())),
    }
}

async fn watch_status(State(state): State<ServerState>, headers: HeaderMap) -> impl IntoResponse {
    if !state.browser_authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let room = state.room.read().expect("room lock poisoned");
    let sessions =
        private_json::<SeatSessionsView>(&state, "seat-sessions.json").unwrap_or_default();
    let joined = state.auth.read().expect("auth lock poisoned");
    let seats = room
        .seats
        .values()
        .map(|seat| {
            let usage = room
                .seat_usage
                .get(&seat.seat_id)
                .cloned()
                .unwrap_or_default();
            let session = sessions
                .seats
                .iter()
                .find(|record| record.seat_id == seat.seat_id);
            WatchSeat {
                seat_id: seat.seat_id.clone(),
                name: seat.name.clone(),
                model: seat.model.clone(),
                role: seat.role.clone(),
                budget: seat.budget.clone(),
                activity: seat_activity(
                    &room,
                    seat,
                    &usage,
                    joined.has_joined(&seat.seat_id),
                    session,
                ),
                usage,
            }
        })
        .collect();
    let phase_progress = phase_progress(&room);
    let health = room_health(&state, &room, &sessions);
    Json(WatchSnapshot {
        room_id: room.room_id.clone(),
        mode: room.mode.clone(),
        phase: room.phase,
        sealed: room.sealed,
        seats,
        findings: room.findings.values().cloned().collect(),
        dispositions: room.dispositions.values().cloned().collect(),
        events: room.events.clone(),
        escalation_rate: room.escalation_rate(),
        failed_by_majority_escalation: room.failed_by_majority_escalation(),
        phase_progress,
        health,
    })
    .into_response()
}

pub async fn serve(listener: tokio::net::TcpListener, state: ServerState) -> std::io::Result<()> {
    axum::serve(listener, router(state)).await
}

async fn tool(
    State(state): State<ServerState>,
    Path(tool): Path<String>,
    headers: HeaderMap,
    Json(value): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(seat_id) = state.seat_for_bearer(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if tool == "tasks" {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":"mode_unshipped","detail":"worksplit"})),
        )
            .into_response();
    }
    if tool == "next" {
        return next_tool(state, seat_id, value).await;
    }
    let mut room = state.room.write().expect("room lock poisoned");
    let prior_event_count = room.events.len();
    let result = match tool.as_str() {
        "post" => parse_request::<PostRequest>(value).and_then(|request| {
            if !matches!(
                request.kind,
                crate::protocol::EventKind::Position
                    | crate::protocol::EventKind::Question
                    | crate::protocol::EventKind::Evidence
                    | crate::protocol::EventKind::Info
            ) {
                return Err(crate::protocol::ProtocolError::external(
                    "kind_forbidden",
                    "",
                ));
            }
            room.post(
                &seat_id,
                &request.request_id,
                request.kind,
                &request.body,
                request.reply_to,
            )
            .and_then(to_json)
        }),
        "raise" => parse_request::<RaiseRequest>(value).and_then(|request| {
            room.raise(
                &seat_id,
                &request.request_id,
                crate::protocol::FindingInput {
                    claim: request.claim,
                    severity: request.severity,
                    rationale: request.rationale,
                    evidence_refs: request.evidence_refs,
                    proposed_change: request.proposed_change,
                    confidence: request.confidence,
                },
            )
            .and_then(to_json)
        }),
        "contest" => parse_request::<ContestRequest>(value).and_then(|request| {
            room.contest(
                &seat_id,
                &request.request_id,
                &request.finding_id,
                &request.rationale,
                request.evidence_refs,
            )
            .and_then(to_json)
        }),
        "dispose" => parse_request::<DisposeRequest>(value).and_then(|request| {
            room.dispose(
                &seat_id,
                &request.request_id,
                &request.finding_id,
                request.action,
                request.reason_code,
                request.receipt_refs,
            )
            .and_then(to_json)
        }),
        "resolve" => parse_request::<ResolveRequest>(value).and_then(|request| {
            room.resolve(
                &seat_id,
                &request.request_id,
                &request.disposition_id,
                request.choice,
                &request.reason,
                request.evidence_refs,
            )
            .and_then(to_json)
        }),
        "rule" => parse_request::<RuleRequest>(value).and_then(|request| {
            room.rule(
                &seat_id,
                &request.request_id,
                &request.disposition_id,
                request.action,
                &request.reason,
                request.receipt_refs,
            )
            .and_then(to_json)
        }),
        "motion" => parse_request::<MotionRequest>(value).and_then(|request| {
            if request.kind != "review_scope" {
                return Err(crate::protocol::ProtocolError::external(
                    "unknown_motion_kind",
                    request.kind,
                ));
            }
            room.create_review_scope(
                &seat_id,
                &request.request_id,
                request.scope_refs,
                &request.scope_digest,
            )
            .and_then(to_json)
        }),
        "next" => parse_request::<NextRequest>(value).and_then(|request| {
            if !(30_000..=60_000).contains(&request.wait_ms) {
                return Err(crate::protocol::ProtocolError::external(
                    "wait_ms_invalid",
                    request.wait_ms.to_string(),
                ));
            }
            room.next(&seat_id, request.cursor).and_then(to_json)
        }),
        "vote" => parse_request::<VoteRequest>(value).and_then(|request| {
            room.vote(
                &seat_id,
                &request.request_id,
                &request.motion_id,
                &request.choice,
                &request.reason,
            )
            .and_then(to_json)
        }),
        _ => Err(crate::protocol::ProtocolError::external(
            "unknown_tool",
            tool.clone(),
        )),
    };
    match result {
        Ok(value) => {
            if tool == "post" && room.phase == crate::protocol::Phase::Positions {
                let all_positions =
                    room.seats
                        .values()
                        .filter(|seat| seat.role != "human")
                        .all(|seat| {
                            room.events.iter().any(|event| {
                                event.kind == crate::protocol::EventKind::Position
                                    && event.seat_id == seat.seat_id
                            })
                        });
                if all_positions {
                    if let Some(human) = room
                        .seats
                        .values()
                        .find(|seat| seat.role == "human")
                        .map(|seat| seat.seat_id.clone())
                    {
                        if let Err(error) = room.advance(&human, crate::protocol::Phase::PeerDebate)
                        {
                            return protocol_error(error);
                        }
                    }
                }
            }
            if let Err(error) = state.persist_room(&room) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error":error.code,"detail":error.detail})),
                )
                    .into_response();
            }
            for event in room.events.iter().skip(prior_event_count) {
                let _ = state.event_tx.send(event.clone());
            }
            Json(value).into_response()
        }
        Err(error) => protocol_error(error),
    }
}

async fn next_tool(
    state: ServerState,
    seat_id: String,
    value: serde_json::Value,
) -> Response<Body> {
    let request = match parse_request::<NextRequest>(value) {
        Ok(request) => request,
        Err(error) => return protocol_error(error),
    };
    if !(30_000..=60_000).contains(&request.wait_ms) {
        return protocol_error(crate::protocol::ProtocolError::external(
            "wait_ms_invalid",
            request.wait_ms.to_string(),
        ));
    }
    let mut receiver = state.event_tx.subscribe();
    {
        let mut room = state.room.write().expect("room lock poisoned");
        match room.next(&seat_id, request.cursor) {
            Ok(events) => {
                if let Err(error) = state.persist_room(&room) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"error":error.code,"detail":error.detail})),
                    )
                        .into_response();
                }
                if !events.is_empty() {
                    return Json(events).into_response();
                }
            }
            Err(error) => return protocol_error(error),
        }
    }
    let wait = async {
        loop {
            match receiver.recv().await {
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    let mut room = state.room.write().expect("room lock poisoned");
                    match room.deliver_after_wait(&seat_id, request.cursor) {
                        Ok(events) if !events.is_empty() => {
                            let _ = state.persist_room(&room);
                            return Json(events).into_response();
                        }
                        Ok(_) => continue,
                        Err(error) => return protocol_error(error),
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Json(Vec::<Event>::new()).into_response()
                }
            }
        }
    };
    match tokio::time::timeout(std::time::Duration::from_millis(request.wait_ms), wait).await {
        Ok(response) => response,
        Err(_) => Json(Vec::<Event>::new()).into_response(),
    }
}

fn parse_request<T: for<'de> Deserialize<'de>>(
    value: serde_json::Value,
) -> Result<T, crate::protocol::ProtocolError> {
    serde_json::from_value(value).map_err(|error| {
        crate::protocol::ProtocolError::external("invalid_request", error.to_string())
    })
}

fn to_json<T: Serialize>(value: T) -> Result<serde_json::Value, crate::protocol::ProtocolError> {
    serde_json::to_value(value).map_err(|error| {
        crate::protocol::ProtocolError::external("serialization_failed", error.to_string())
    })
}

fn protocol_error(error: crate::protocol::ProtocolError) -> Response<Body> {
    let status = match error.code {
        "request_id_conflict" | "mode_unshipped" => StatusCode::CONFLICT,
        "unknown_seat" | "role_forbidden" => StatusCode::FORBIDDEN,
        "unknown_finding" | "unknown_disposition" | "unknown_motion" => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_REQUEST,
    };
    (
        status,
        Json(serde_json::json!({"error":error.code,"detail":error.detail})),
    )
        .into_response()
}

async fn join(
    State(state): State<ServerState>,
    Json(request): Json<JoinRequest>,
) -> impl IntoResponse {
    let joined = {
        let room = state.room.read().expect("room lock poisoned");
        let mut auth = state.auth.write().expect("auth lock poisoned");
        auth.join(&room, request)
    };
    match joined {
        Ok(response) => {
            let should_open_positions = {
                let room = state.room.read().expect("room lock poisoned");
                let auth = state.auth.read().expect("auth lock poisoned");
                room.phase == crate::protocol::Phase::Open
                    && room
                        .seats
                        .values()
                        .filter(|seat| seat.role != "human")
                        .all(|seat| auth.has_joined(&seat.seat_id))
            };
            if should_open_positions {
                let mut room = state.room.write().expect("room lock poisoned");
                if let Some(human) = room
                    .seats
                    .values()
                    .find(|seat| seat.role == "human")
                    .map(|seat| seat.seat_id.clone())
                {
                    if let Err(error) = room.advance(&human, crate::protocol::Phase::Positions) {
                        return protocol_error(error);
                    }
                    if let Err(error) = state.persist_room(&room) {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error":error.code,"detail":error.detail})),
                        )
                            .into_response();
                    }
                }
            }
            Json(response).into_response()
        }
        Err(error) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":error.code,"detail":error.detail})),
        )
            .into_response(),
    }
}

async fn say(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SayRequest>,
) -> impl IntoResponse {
    let Some(seat_id) = state.seat_for_bearer(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    post_human_message(&state, &seat_id, request)
}

async fn browser_say(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SayRequest>,
) -> impl IntoResponse {
    if !state.browser_mutation_authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let seat_id = {
        let room = state.room.read().expect("room lock poisoned");
        room.seats
            .values()
            .find(|seat| seat.role == "human")
            .map(|seat| seat.seat_id.clone())
    };
    let Some(seat_id) = seat_id else {
        return StatusCode::FORBIDDEN.into_response();
    };
    post_human_message(&state, &seat_id, request)
}

fn post_human_message(state: &ServerState, seat_id: &str, request: SayRequest) -> Response<Body> {
    let mut room = state.room.write().expect("room lock poisoned");
    let is_human = room
        .seats
        .get(seat_id)
        .is_some_and(|seat| seat.role == "human");
    if !is_human {
        return StatusCode::FORBIDDEN.into_response();
    }
    let prior_event_count = room.events.len();
    match room.post_targeted(
        seat_id,
        &request.request_id,
        crate::protocol::EventKind::Info,
        &request.body,
        None,
        request.target_seat_id.as_deref(),
    ) {
        Ok(event) => {
            if let Err(error) = state.persist_room(&room) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error":error.code,"detail":error.detail})),
                )
                    .into_response();
            }
            if room.events.len() > prior_event_count {
                let _ = state.event_tx.send(event.clone());
            }
            Json(event).into_response()
        }
        Err(error) if error.code == "request_id_conflict" => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.code,"detail":error.detail})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":error.code,"detail":error.detail})),
        )
            .into_response(),
    }
}

async fn pair_page() -> impl IntoResponse {
    let html = r#"<!doctype html><html><head><meta charset="utf-8"><title>Pair Council Room</title></head>
<body><main><h1>Pair Council Room</h1><form method="post" action="/pair">
<label>Pairing code <input name="pairing_code" type="password" autocomplete="one-time-code" required></label>
<button type="submit">Open room</button></form></main></body></html>"#;
    let mut response = Html(html).into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PAIR_CSP),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn pair(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Form(form): Form<PairForm>,
) -> impl IntoResponse {
    let exact_origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == &*state.exact_origin);
    let supplied = Sha256::digest(form.pairing_code.as_bytes());
    let expected = Sha256::digest(state.pairing_code.as_bytes());
    if !exact_origin || supplied != expected {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    browser_session_response(&state)
}

async fn join_link(
    State(state): State<ServerState>,
    Path(capability): Path<String>,
) -> impl IntoResponse {
    let supplied = Sha256::digest(capability.as_bytes());
    let expected = Sha256::digest(state.pairing_code.as_bytes());
    if supplied != expected {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    browser_session_response(&state)
}

fn browser_session_response(state: &ServerState) -> Response<Body> {
    let cookie = format!(
        "{WATCH_COOKIE}={}; HttpOnly; SameSite=Strict; Path=/",
        state.browser_session
    );
    let mut response = StatusCode::SEE_OTHER.into_response();
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/"));
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("hex session cookie is a valid header"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn status(State(state): State<ServerState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(seat_id) = state.seat_for_bearer(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let room = state.room.read().expect("room lock poisoned");
    let Ok(events) = room.visible_events(&seat_id, 0) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let latest_visible_cursor = events.last().map(|event| event.seq).unwrap_or(0);
    let seat = room.seats.get(&seat_id).expect("authenticated seat exists");
    let usage = room
        .seat_usage
        .get(&seat_id)
        .expect("authenticated seat usage exists");
    Json(StatusSnapshot {
        room_id: room.room_id.clone(),
        seat_id,
        mode: room.mode.clone(),
        phase: room.phase,
        latest_visible_cursor,
        at_budget: room.at_budget.clone(),
        budget: seat.budget.clone(),
        usage: usage.clone(),
        escalation_rate: room.escalation_rate(),
        open_finding_ids: room.open_finding_ids(),
        motions: room.motions.values().cloned().collect(),
        events,
    })
    .into_response()
}

async fn dashboard(State(state): State<ServerState>, headers: HeaderMap) -> impl IntoResponse {
    if !state.browser_authorized(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let html = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Council Room</title>
<style>
:root{color-scheme:dark;--bg:#0b0b0a;--sidebar:#111110;--panel:#181817;--panel-2:#242423;--raised:#2e2e2c;--line:#2f2f2d;--line-2:#444440;--text:#f2f1ed;--text-2:#d7d5cf;--text-3:#9a978f;--accent:#e8e6e1;--success:#5bbe86;--warning:#f59e0b;--danger:#fa423e;--focus:#9a978f;--ease:cubic-bezier(.23,1,.32,1);font-family:"General Sans",Inter,ui-sans-serif,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}
*{box-sizing:border-box}html,body{min-height:100%;max-width:100%;margin:0;overflow-x:hidden;background:var(--bg);color:var(--text)}body{font-size:14px;line-height:1.5}button,textarea{font:inherit}button{color:inherit}.skip-link{position:fixed;left:12px;top:8px;z-index:20;transform:translateY(-150%);background:var(--accent);color:#16130f;padding:8px 12px;border-radius:7px}.skip-link:focus{transform:translateY(0)}
.app-shell{width:100%;max-width:100vw;min-height:100vh;display:grid;grid-template-rows:auto auto 1fr;background:radial-gradient(circle at 48% -20%,#252522 0,transparent 34%),var(--bg)}
.topbar{min-height:68px;display:grid;grid-template-columns:minmax(240px,1fr) auto minmax(240px,1fr);align-items:center;gap:24px;padding:12px 20px;border-bottom:1px solid var(--line);background:rgba(11,11,10,.94)}
.brand{display:flex;align-items:center;gap:11px;min-width:0}.brand-mark{display:grid;place-items:center;width:32px;height:32px;border:1px solid var(--line-2);border-radius:9px;background:var(--panel-2);font:700 13px ui-monospace,Consolas,monospace;letter-spacing:-.08em}.brand-copy{min-width:0}.eyebrow{color:var(--text-3);font:600 12px ui-monospace,Consolas,monospace;letter-spacing:.12em;text-transform:uppercase}.brand h1{margin:1px 0 0;font-size:17px;line-height:1.25}.room-heading{text-align:center;min-width:0}.room-heading strong{display:block;max-width:520px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:15px}.room-heading span{color:var(--text-3);font:12px ui-monospace,Consolas,monospace}.connection{justify-self:end;display:flex;align-items:center;gap:10px}.connection-badge,.local-badge,.phase-badge{display:inline-flex;align-items:center;gap:7px;min-height:28px;padding:3px 9px;border:1px solid var(--line);border-radius:999px;background:var(--panel);color:var(--text-2);font-size:12px}.status-dot{width:7px;height:7px;border-radius:50%;background:var(--warning);box-shadow:0 0 0 3px rgba(245,158,11,.1)}.connection-badge[data-state="live"] .status-dot{background:var(--success);box-shadow:0 0 0 3px rgba(91,190,134,.1)}.connection-badge[data-state="error"] .status-dot{background:var(--danger);box-shadow:0 0 0 3px rgba(250,66,62,.1)}
.phase-nav{width:100%;max-width:100vw;padding:0 20px;border-bottom:1px solid var(--line);background:var(--sidebar);overflow-x:auto}.phase-list{display:flex;justify-content:center;width:max-content;min-width:100%;margin:0;padding:0;list-style:none}.phase-step{position:relative;display:flex;align-items:center;gap:7px;min-height:42px;padding:0 14px;color:var(--text-3);font:600 10px ui-monospace,Consolas,monospace;letter-spacing:.04em;text-transform:uppercase}.phase-step::after{content:"";position:absolute;right:-2px;width:4px;height:4px;border-radius:50%;background:var(--line-2)}.phase-step:last-child::after{display:none}.phase-step[data-state="done"]{color:var(--text-2)}.phase-step[data-state="current"]{color:var(--text)}.phase-step[data-state="current"]::before{content:"";position:absolute;left:14px;right:14px;bottom:-1px;height:2px;background:var(--accent)}
.workspace{width:100%;max-width:100vw;min-height:0;display:grid;grid-template-columns:minmax(210px,250px) minmax(420px,1fr) minmax(240px,300px);gap:1px;background:var(--line)}.rail,.discussion{min-width:0;background:var(--bg)}.rail{padding:18px;background:var(--sidebar)}.rail-left{border-right:1px solid var(--line)}.rail-right{border-left:1px solid var(--line)}.section-head{display:flex;align-items:center;justify-content:space-between;gap:10px;margin-bottom:12px}.section-head h2{margin:0;font-size:11px;letter-spacing:.08em;text-transform:uppercase}.count{min-width:24px;padding:1px 7px;border:1px solid var(--line);border-radius:999px;color:var(--text-3);font:10px ui-monospace,Consolas,monospace;text-align:center}
.participant-list{display:grid;gap:8px}.participant{padding:11px;border:1px solid var(--line);border-radius:9px;background:var(--panel)}.participant-top{display:grid;grid-template-columns:30px 1fr auto;align-items:center;gap:9px}.avatar{display:grid;place-items:center;width:30px;height:30px;border:1px solid var(--line-2);border-radius:8px;background:var(--panel-2);color:var(--text-2);font:700 10px ui-monospace,Consolas,monospace}.participant-name{min-width:0}.participant-name strong,.participant-name span{display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.participant-name strong{font-size:12px}.participant-name span{color:var(--text-3);font-size:10px}.seat-state{display:flex;align-items:center;gap:5px;color:var(--text-3);font-size:10px}.seat-state::before{content:"";width:6px;height:6px;border-radius:50%;background:var(--line-2)}.seat-state[data-active="true"]{color:var(--success)}.seat-state[data-active="true"]::before{background:var(--success)}.usage{display:grid;gap:5px;margin-top:10px}.usage-row{display:flex;justify-content:space-between;color:var(--text-3);font:9px ui-monospace,Consolas,monospace}.usage-track{height:3px;overflow:hidden;border-radius:3px;background:var(--raised)}.usage-fill{display:block;height:100%;background:var(--text-3);transform-origin:left}
.discussion{display:grid;grid-template-rows:auto auto auto minmax(260px,1fr) auto;min-height:calc(100vh - 111px)}.discussion-header{display:flex;align-items:center;justify-content:space-between;gap:18px;padding:18px 22px 14px;border-bottom:1px solid var(--line)}.discussion-header h2{margin:0;font-size:17px}.discussion-header p{margin:2px 0 0;color:var(--text-3);font-size:11px}.phase-badge{border-radius:7px;font-family:ui-monospace,Consolas,monospace}.view-tabs{display:flex;gap:18px;padding:0 22px;border-bottom:1px solid var(--line);background:var(--sidebar)}.view-tab{position:relative;min-height:40px;border:0;background:none;padding:0;color:var(--text-3);font-weight:650;cursor:pointer}.view-tab[aria-selected="true"]{color:var(--text)}.view-tab[aria-selected="true"]::after{content:"";position:absolute;left:0;right:0;bottom:-1px;height:2px;background:var(--accent)}.connection-banner{display:none;align-items:center;justify-content:space-between;gap:12px;padding:9px 22px;border-bottom:1px solid rgba(250,66,62,.3);background:rgba(250,66,62,.08);color:#ffaaa7;font-size:11px}.connection-banner[data-visible="true"]{display:flex}.text-button{border:0;background:none;padding:0;color:inherit;text-decoration:underline;text-underline-offset:3px;cursor:pointer}
.timeline{min-height:0;overflow-y:auto;padding:20px 22px 28px;scrollbar-color:var(--line-2) transparent}.timeline[hidden]{display:none}.empty-state{display:grid;place-items:center;min-height:280px;text-align:center}.empty-state div{max-width:340px}.empty-icon{display:grid;place-items:center;width:48px;height:48px;margin:0 auto 14px;border:1px solid var(--line);border-radius:14px;background:var(--panel);color:var(--text-3);font:700 16px ui-monospace,Consolas,monospace}.empty-state h3{margin:0;font-size:14px}.empty-state p{margin:6px 0 0;color:var(--text-3);font-size:12px}.event{display:grid;grid-template-columns:34px minmax(0,1fr);gap:11px;max-width:780px;margin:0 auto 14px;animation:event-in .18s var(--ease) both}.event-avatar{display:grid;place-items:center;width:32px;height:32px;border:1px solid var(--line);border-radius:9px;background:var(--panel);color:var(--text-2);font:700 10px ui-monospace,Consolas,monospace}.event-card{min-width:0;padding:11px 13px;border:1px solid var(--line);border-radius:4px 10px 10px;background:var(--panel)}.event[data-human="true"] .event-card{border-color:var(--line-2);background:var(--panel-2)}.event-meta{display:flex;align-items:center;flex-wrap:wrap;gap:7px;margin-bottom:5px}.event-meta strong{font-size:11px}.kind,.target-badge{padding:1px 5px;border-radius:4px;background:var(--raised);color:var(--text-3);font:9px ui-monospace,Consolas,monospace;text-transform:uppercase}.target-badge{color:var(--warning)}.event-seq{margin-left:auto;color:var(--text-3);font:9px ui-monospace,Consolas,monospace}.reply{margin:0 0 7px;padding-left:8px;border-left:2px solid var(--line-2);color:var(--text-3);font-size:10px}.event-body{margin:0;color:var(--text-2);font-size:12px;line-height:1.65;overflow-wrap:anywhere;white-space:pre-wrap}.phase-event{display:flex;align-items:center;gap:10px;max-width:780px;margin:18px auto;color:var(--text-3);font:9px ui-monospace,Consolas,monospace;text-transform:uppercase;letter-spacing:.08em}.phase-event::before,.phase-event::after{content:"";height:1px;flex:1;background:var(--line)}.activity-list,.artifact-list{display:grid;gap:8px;max-width:780px;margin:0 auto}.activity-row,.artifact-card{display:grid;grid-template-columns:auto minmax(0,1fr) auto;gap:10px;align-items:start;padding:10px 12px;border:1px solid var(--line);border-radius:8px;background:var(--panel)}.activity-row p,.artifact-card p{margin:2px 0 0;color:var(--text-3);font-size:11px;overflow-wrap:anywhere}.activity-seq{color:var(--text-3);font:10px ui-monospace,Consolas,monospace}.artifact-card{display:block;width:100%;color:inherit;text-align:left}.artifact-card[role="button"]{cursor:pointer}.artifact-card[role="button"]:hover{border-color:var(--line-2)}.artifact-meta{display:flex;justify-content:space-between;gap:12px;color:var(--text-3);font:10px ui-monospace,Consolas,monospace}.artifact-title{margin-top:5px;font-weight:650}.participant-detail{margin-top:8px;color:var(--text-3);font-size:10px}.participant-logs{margin-top:4px;color:var(--danger);font:9px ui-monospace,Consolas,monospace;overflow-wrap:anywhere}.health-alert{display:none;margin-bottom:14px;padding:10px;border:1px solid rgba(250,66,62,.35);border-radius:8px;background:rgba(250,66,62,.08);color:#ffaaa7;font-size:10px}.health-alert[data-visible="true"]{display:block}.health-alert strong{display:block;margin-bottom:4px}.health-failure{margin-top:7px;padding-top:7px;border-top:1px solid rgba(250,66,62,.2)}
.composer{padding:14px 22px 18px;border-top:1px solid var(--line);background:var(--sidebar)}.composer-box{max-width:780px;margin:0 auto;border:1px solid var(--line-2);border-radius:10px;background:var(--panel);transition:border-color .15s var(--ease),box-shadow .15s var(--ease)}.composer-box:focus-within{border-color:var(--focus);box-shadow:0 0 0 3px rgba(154,151,143,.12)}.composer label{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)}.composer textarea{display:block;width:100%;min-height:72px;max-height:180px;resize:vertical;border:0;outline:0;padding:12px 13px;background:transparent;color:var(--text);line-height:1.5}.composer textarea::placeholder{color:#77746e}.composer-actions{display:flex;align-items:center;gap:10px;padding:8px;border-top:1px solid var(--line)}.target-select{max-width:160px;min-height:30px;border:1px solid var(--line);border-radius:7px;padding:4px 8px;background:var(--panel-2);color:var(--text-2);font-size:10px}.composer-hint{color:var(--text-3);font:10px ui-monospace,Consolas,monospace}.send-status{margin-left:auto;color:var(--text-3);font-size:10px}.send-button{min-height:30px;border:0;border-radius:7px;padding:5px 12px;background:var(--accent);color:#16130f;font-weight:700;font-size:11px;cursor:pointer;transition:opacity .15s var(--ease),transform 80ms var(--ease)}.send-button:hover{opacity:.9}.send-button:active{transform:translateY(1px)}.send-button:disabled{cursor:not-allowed;opacity:.35}
.state-grid{display:grid;gap:1px;margin-bottom:18px;border:1px solid var(--line);border-radius:9px;overflow:hidden;background:var(--line)}.state-row{display:flex;justify-content:space-between;gap:14px;padding:9px 10px;background:var(--panel);font-size:10px}.state-row dt{color:var(--text-3)}.state-row dd{margin:0;color:var(--text-2);font:10px ui-monospace,Consolas,monospace;text-align:right}.finding-list{display:grid;gap:8px}.finding{padding:10px;border:1px solid var(--line);border-radius:8px;background:var(--panel)}.finding-top{display:flex;justify-content:space-between;gap:8px;margin-bottom:5px}.finding-status{color:var(--warning);font:9px ui-monospace,Consolas,monospace;text-transform:uppercase}.finding[data-terminal="true"] .finding-status{color:var(--success)}.finding p{margin:0;color:var(--text-2);font-size:11px;line-height:1.5;overflow-wrap:anywhere}.findings-empty{padding:16px 12px;border:1px dashed var(--line);border-radius:8px;color:var(--text-3);font-size:11px;text-align:center}.local-note{margin-top:18px;padding-top:14px;border-top:1px solid var(--line);color:var(--text-3);font-size:10px}.local-note strong{display:block;margin-bottom:2px;color:var(--text-2);font-size:10px}
@keyframes event-in{from{opacity:0;transform:translateY(6px)}}
button:focus-visible,textarea:focus-visible,select:focus-visible,[role="button"]:focus-visible{outline:2px solid var(--focus);outline-offset:2px}
@media(max-width:980px){.topbar{grid-template-columns:1fr auto}.room-heading{display:none}.workspace{grid-template-columns:210px minmax(0,1fr)}.rail-right{display:none}}
@media(max-width:680px){.topbar{grid-template-columns:minmax(0,1fr) auto;min-height:58px;padding:10px 12px}.local-badge{display:none}.phase-nav{padding:0}.workspace{display:block}.rail-left{border:0;border-bottom:1px solid var(--line);padding:12px}.participant-list{display:flex;max-width:100%;overflow-x:auto}.participant{min-width:190px}.discussion{width:100%;min-height:calc(100vh - 100px);overflow:hidden}.discussion-header,.timeline,.composer{padding-left:14px;padding-right:14px}.composer-box{width:100%}.event{grid-template-columns:28px minmax(0,1fr)}.event-avatar{width:28px;height:28px}.phase-step{padding:0 10px}.phase-step[data-state="current"]::before{left:10px;right:10px}}
.phase-step,.section-head h2,.count,.avatar,.event-avatar,.participant-name span,.seat-state,.usage-row,.discussion-header p,.connection-banner,.text-button,.event-meta strong,.kind,.event-seq,.reply,.composer-hint,.send-status,.send-button,.state-row,.state-row dd,.finding-status,.finding p,.findings-empty,.local-note,.local-note strong{font-size:12px}.participant-name strong,.empty-state h3{font-size:14px}.event-body,.empty-state p{font-size:13px}.discussion-header h2{font-size:19px}
@media(prefers-reduced-motion:reduce){*,*::before,*::after{scroll-behavior:auto!important;animation-duration:.01ms!important;animation-iteration-count:1!important;transition-duration:.01ms!important}.event{transform:none}}
</style></head>
<body><a class="skip-link" href="#discussion">Skip to discussion</a>
<div class="app-shell" id="app" aria-busy="true">
  <header class="topbar">
    <div class="brand"><div class="brand-mark" aria-hidden="true">CR</div><div class="brand-copy"><div class="eyebrow">Local review</div><h1>Council Room</h1></div></div>
    <div class="room-heading"><strong id="room-name">Loading room…</strong><span id="room-mode">Connecting to local service</span></div>
    <div class="connection"><span class="local-badge">Local only</span><span class="connection-badge" id="connection" data-state="connecting"><span class="status-dot" aria-hidden="true"></span><span id="connection-label">Connecting</span></span></div>
  </header>
  <nav class="phase-nav" aria-label="Council phases"><ol class="phase-list" id="phase-list"></ol></nav>
  <main class="workspace" id="discussion">
    <aside class="rail rail-left" aria-labelledby="participants-title"><div class="section-head"><h2 id="participants-title">Participants</h2><span class="count" id="seat-count">0</span></div><div class="participant-list" id="seats"></div></aside>
    <section class="discussion" aria-labelledby="discussion-title">
      <header class="discussion-header"><div><h2 id="discussion-title">Discussion</h2><p id="discussion-summary">Waiting for the room</p></div><span class="phase-badge" id="phase-badge">Connecting</span></header>
      <div class="view-tabs" role="tablist" aria-label="Room views"><button class="view-tab" type="button" role="tab" aria-selected="true" aria-controls="events" data-view="discussion">Discussion</button><button class="view-tab" type="button" role="tab" aria-selected="false" aria-controls="activity" data-view="activity">Activity</button><button class="view-tab" type="button" role="tab" aria-selected="false" aria-controls="artifacts" data-view="artifacts">Artifacts</button></div>
      <div class="connection-banner" id="connection-banner" role="alert"><span id="connection-message">Room service disconnected. Existing messages remain available; launcher diagnostics and fallback state are preserved.</span><button class="text-button" id="retry" type="button">Retry</button></div>
      <div class="timeline" id="events" role="tabpanel" aria-live="polite" aria-relevant="additions"><div class="empty-state"><div><div class="empty-icon" aria-hidden="true">•••</div><h3>No messages yet</h3><p>Agents' positions and replies will appear here as the review progresses.</p></div></div></div>
      <div class="timeline" id="activity" role="tabpanel" aria-live="polite" hidden></div>
      <div class="timeline" id="artifacts" role="tabpanel" aria-live="polite" hidden></div>
      <div class="composer"><div class="composer-box"><label for="say">Interject in Council Room</label><textarea id="say" rows="3" placeholder="Ask a question, challenge a claim, or add context…"></textarea><div class="composer-actions"><label for="say-target">Address</label><select class="target-select" id="say-target" aria-label="Address intervention"><option value="">Everyone</option></select><span class="composer-hint">Ctrl + Enter to send</span><span class="send-status" id="say-status" role="status"></span><button class="send-button" id="send" type="button">Interject</button></div></div></div>
    </section>
    <aside class="rail rail-right" aria-labelledby="state-title"><div class="section-head"><h2 id="state-title">Room state</h2></div><div class="health-alert" id="room-health" role="alert"></div><dl class="state-grid" id="room-state"></dl><div class="section-head"><h2>Findings</h2><span class="count" id="finding-count">0</span></div><div class="finding-list" id="findings"></div><div class="local-note"><strong>Private by design</strong>Room traffic and transcripts stay on this machine.</div></aside>
  </main>
</div>
<script nonce="agent-room">
const app=document.querySelector('#app'),seats=document.querySelector('#seats'),seatCount=document.querySelector('#seat-count'),out=document.querySelector('#events'),activityOut=document.querySelector('#activity'),artifactOut=document.querySelector('#artifacts'),viewTabs=[...document.querySelectorAll('.view-tab')],phaseList=document.querySelector('#phase-list'),phaseBadge=document.querySelector('#phase-badge'),roomName=document.querySelector('#room-name'),roomMode=document.querySelector('#room-mode'),summary=document.querySelector('#discussion-summary'),connection=document.querySelector('#connection'),connectionLabel=document.querySelector('#connection-label'),banner=document.querySelector('#connection-banner'),retry=document.querySelector('#retry'),roomState=document.querySelector('#room-state'),roomHealth=document.querySelector('#room-health'),findings=document.querySelector('#findings'),findingCount=document.querySelector('#finding-count'),say=document.querySelector('#say'),sayTarget=document.querySelector('#say-target'),send=document.querySelector('#send'),sayStatus=document.querySelector('#say-status');
const phases=['Open','Positions','PeerDebate','Dispositions','AuthorReview','Escalation','Voting','Verdict'];let snapshot=null,source=null,cursor=0,roomSealed=false,currentView='discussion';
function node(tag,className,text){const item=document.createElement(tag);if(className)item.className=className;if(text!==undefined)item.textContent=String(text);return item}
function initials(value){return String(value||'?').split(/\s+/).slice(0,2).map(part=>part[0]||'').join('').toUpperCase()||'?'}
function number(value){return new Intl.NumberFormat('en',{notation:value>999?'compact':'standard',maximumFractionDigits:1}).format(value||0)}
function label(value){return String(value||'').replace(/([a-z])([A-Z])/g,'$1 $2').replace(/_/g,' ')}
function setConnection(state){connection.dataset.state=state;connectionLabel.textContent=state==='live'?'Live':state==='error'?'Disconnected':'Connecting';banner.dataset.visible=state==='error'?'true':'false'}
function setView(view){currentView=view;for(const tab of viewTabs)tab.setAttribute('aria-selected',String(tab.dataset.view===view));out.hidden=view!=='discussion';activityOut.hidden=view!=='activity';artifactOut.hidden=view!=='artifacts'}
function renderPhases(current){phaseList.replaceChildren();const currentIndex=Math.max(0,phases.indexOf(current));for(const [index,value] of phases.entries()){const item=node('li','phase-step',label(value));item.dataset.state=index<currentIndex?'done':index===currentIndex?'current':'upcoming';if(index===currentIndex)item.setAttribute('aria-current','step');phaseList.append(item)}}
function seatFor(id){return snapshot?.seats.find(seat=>seat.seat_id===id)}
function renderSeats(){seats.replaceChildren();seatCount.textContent=String(snapshot.seats.length);for(const seat of snapshot.seats){const card=node('article','participant');card.dataset.state=seat.activity.state;const top=node('div','participant-top');top.append(node('div','avatar',initials(seat.name)));const name=node('div','participant-name');name.append(node('strong','',seat.name),node('span','',`${seat.model} · ${seat.role}`));const state=node('span','seat-state',label(seat.activity.state));state.dataset.active=String(['ready','reading','responded','attention'].includes(seat.activity.state));top.append(name,state);const detail=node('div','participant-detail',seat.activity.detail);const usage=node('div','usage');const row=node('div','usage-row');row.append(node('span','',`${seat.usage.sent_messages} messages`),node('span','',`${number(seat.usage.context_bytes)} context`));const track=node('div','usage-track');const fill=node('span','usage-fill');fill.style.width=`${Math.min(100,(seat.usage.sent_messages/Math.max(1,seat.budget.max_messages))*100)}%`;track.append(fill);usage.append(row,track);card.append(top,detail);if(seat.activity.state==='failed'){const logs=[seat.activity.stdout_log,seat.activity.stderr_log].filter(Boolean);if(logs.length)card.append(node('div','participant-logs',`Logs: ${logs.join(' · ')}`))}card.append(usage);seats.append(card)}}
function renderPhaseProgress(){const progress=snapshot.phase_progress;const names=progress.outstanding_seat_ids.map(id=>seatFor(id)?.name||id);return [['Seat activity',`${progress.responded}/${progress.required}`],['Outstanding',names.length?names.join(', '):'None'],['Open findings',progress.open_findings]]}
function renderRoomState(){roomState.replaceChildren();const rows=[['Phase',label(snapshot.phase)],['Mode',label(snapshot.mode)],...renderPhaseProgress(),['Escalation',`${Math.round(snapshot.escalation_rate*100)}%`],['Room',label(snapshot.health.status)]];for(const [term,value] of rows){const row=node('div','state-row');row.append(node('dt','',term),node('dd','',value));roomState.append(row)}}
function renderHealth(){roomHealth.replaceChildren();const failed=snapshot.health.status==='fallback_required';roomHealth.dataset.visible=String(failed);if(!failed)return;roomHealth.append(node('strong','','Room aborted · fallback review required'),node('span','',snapshot.health.stage?`Stage: ${label(snapshot.health.stage)}`:'Diagnostic state preserved'));for(const failure of snapshot.health.failures){const item=node('div','health-failure');item.append(node('strong','',`${failure.seat}: ${label(failure.code)}`),node('div','',failure.detail),node('div','participant-detail',failure.action));const logs=[failure.stdout_log,failure.stderr_log].filter(Boolean);if(logs.length)item.append(node('div','participant-logs',`Logs: ${logs.join(' · ')}`));roomHealth.append(item)}}
function renderFindings(){findings.replaceChildren();findingCount.textContent=String(snapshot.findings.length);if(!snapshot.findings.length){findings.append(node('div','findings-empty','No findings raised yet.'));return}for(const item of snapshot.findings){const card=node('article','finding');const terminal=['Folded','Refuted'].includes(item.status);card.dataset.terminal=String(terminal);const top=node('div','finding-top');top.append(node('span','finding-status',label(item.status)),node('span','event-seq',item.severity));card.append(top,node('p','',item.claim));findings.append(card)}}
function renderEmpty(){const empty=node('div','empty-state'),wrap=node('div'),icon=node('div','empty-icon','•••');icon.setAttribute('aria-hidden','true');wrap.append(icon,node('h3','','No messages yet'),node('p','',"Agents' positions and replies will appear here as the review progresses."));empty.append(wrap);out.replaceChildren(empty)}
function renderEvent(event,animate){if(event.kind==='system'&&String(event.body).startsWith('phase:')){const divider=node('div','phase-event',`Entered ${label(String(event.body).slice(6))}`);divider.dataset.seq=event.seq;out.append(divider);return}const seat=seatFor(event.seat_id);const article=node('article','event');article.dataset.seq=event.seq;article.dataset.human=String(seat?.role==='human');if(!animate)article.style.animation='none';article.append(node('div','event-avatar',initials(seat?.name||event.seat_id)));const card=node('div','event-card'),meta=node('div','event-meta');meta.append(node('strong','',seat?.name||event.seat_id),node('span','kind',label(event.kind)));if(event.target_seat_id){const target=seatFor(event.target_seat_id);meta.append(node('span','target-badge',`To ${target?.name||event.target_seat_id}`))}meta.append(node('span','event-seq',`#${event.seq}`));card.append(meta);if(event.reply_to){const prior=snapshot.events.find(item=>item.seq===event.reply_to),priorSeat=prior?seatFor(prior.seat_id):null;card.append(node('p','reply',`Replying to ${priorSeat?.name||`message #${event.reply_to}`}`))}card.append(node('p','event-body',event.body));article.append(card);out.append(article)}
function renderTimeline(){out.replaceChildren();if(!snapshot.events.length){renderEmpty();return}for(const event of snapshot.events)renderEvent(event,false);out.scrollTop=out.scrollHeight}
function renderActivity(){activityOut.replaceChildren();const list=node('div','activity-list');for(const event of snapshot.events){const seat=seatFor(event.seat_id),row=node('article','activity-row'),copy=node('div');copy.append(node('strong','',`${seat?.name||event.seat_id} · ${label(event.kind)}`),node('p','',event.kind==='system'?label(String(event.body).replace(/^phase:/,'')):String(event.body).slice(0,180)));row.append(node('span','activity-seq',`#${event.seq}`),copy,node('span','activity-seq',label(event.phase)));list.append(row)}if(!snapshot.events.length)list.append(node('div','findings-empty','No activity yet.'));activityOut.append(list)}
function renderArtifacts(){artifactOut.replaceChildren();const list=node('div','artifact-list');for(const finding of snapshot.findings){const card=node('article','artifact-card');card.append(node('div','artifact-meta',`${finding.finding_id} · ${label(finding.status)}`),node('div','artifact-title',finding.claim));list.append(card)}for(const disposition of snapshot.dispositions){for(const receipt of disposition.receipt_refs){const card=node('div','artifact-card');card.tabIndex=0;card.setAttribute('role','button');card.setAttribute('aria-label',`Copy receipt locator ${receipt.locator}`);card.dataset.locator=receipt.locator;card.append(node('div','artifact-meta',`${label(receipt.kind)} · ${label(disposition.status)}`),node('div','artifact-title',receipt.locator),node('p','',`Finding ${disposition.finding_id}`));list.append(card)}}if(!list.children.length)list.append(node('div','findings-empty','Findings, dispositions, receipts, and verdict artifacts will appear here.'));artifactOut.append(list)}
function renderTargets(){const selected=sayTarget.value;sayTarget.replaceChildren(new Option('Everyone',''));for(const seat of snapshot.seats.filter(item=>item.role!=='human'))sayTarget.append(new Option(seat.name,seat.seat_id));if([...sayTarget.options].some(option=>option.value===selected))sayTarget.value=selected}
function renderSnapshot(value){snapshot=value;roomSealed=value.sealed;roomName.textContent=value.room_id;roomMode.textContent=`${label(value.mode)} council · local session`;phaseBadge.textContent=label(value.phase);summary.textContent=value.sealed?`${value.events.length} events · review complete`:value.events.length?`${value.events.length} events · live review`:'Waiting for first positions';renderPhases(value.phase);renderSeats();renderRoomState();renderHealth();renderFindings();renderTimeline();renderActivity();renderArtifacts();renderTargets();setView(currentView);say.disabled=value.sealed;sayTarget.disabled=value.sealed;send.disabled=value.sealed;if(value.sealed)sayStatus.textContent='Review complete · room sealed';cursor=value.events.at(-1)?.seq||0;app.setAttribute('aria-busy','false')}
async function fetchSnapshot(){const response=await fetch('/api/watch-status');if(!response.ok)throw new Error(`Room status ${response.status}`);return response.json()}
function connectStream(){if(source)source.close();source=new EventSource(`/api/stream?cursor=${cursor}`);source.onopen=()=>setConnection('live');source.onmessage=message=>{const event=JSON.parse(message.data);if(snapshot.events.some(item=>item.seq===event.seq))return;cursor=Math.max(cursor,event.seq);fetchSnapshot().then(renderSnapshot).catch(()=>setConnection('error'))};source.onerror=()=>setConnection('error')}
async function load(){setConnection('connecting');app.setAttribute('aria-busy','true');try{renderSnapshot(await fetchSnapshot());connectStream()}catch(error){setConnection('error');summary.textContent='Could not load this room';sayStatus.textContent=error.message;app.setAttribute('aria-busy','false')}}
retry.addEventListener('click',load);
async function interject(){const body=say.value.trim(),target_seat_id=sayTarget.value||null;if(!body||roomSealed)return;send.disabled=true;sayStatus.textContent='Sending…';try{const response=await fetch('/api/browser-say',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:`browser-${Date.now()}-${Math.random().toString(16).slice(2)}`,body,target_seat_id})});if(!response.ok)throw new Error(`Send failed (${response.status})`);say.value='';const target=target_seat_id?seatFor(target_seat_id)?.name||target_seat_id:'every seat';sayStatus.textContent=`Sent to ${target}`}catch(error){sayStatus.textContent=error.message}finally{send.disabled=roomSealed}}
async function copyArtifact(card){const locator=card.dataset.locator;if(!locator)return;try{await navigator.clipboard.writeText(locator);sayStatus.textContent=`Copied ${locator}`}catch{sayStatus.textContent='Could not copy receipt locator'}}
for(const tab of viewTabs)tab.addEventListener('click',()=>setView(tab.dataset.view));artifactOut.addEventListener('click',event=>{const card=event.target.closest('[data-locator]');if(card)copyArtifact(card)});artifactOut.addEventListener('keydown',event=>{if(event.key==='Enter'||event.key===' '){const card=event.target.closest('[data-locator]');if(card){event.preventDefault();copyArtifact(card)}}});send.addEventListener('click',interject);say.addEventListener('keydown',event=>{if(event.key==='Enter'&&event.ctrlKey){event.preventDefault();interject()}});load();
</script></body></html>"##;
    let mut response = Html(html).into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn stream(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Response<Body> {
    if !state.browser_authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    event_stream_response(state, query)
}

async fn watch_stream(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Response<Body> {
    let Some(seat_id) = state.seat_for_bearer(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let is_human = state
        .room
        .read()
        .expect("room lock poisoned")
        .seats
        .get(&seat_id)
        .is_some_and(|seat| seat.role == "human");
    if !is_human {
        return StatusCode::FORBIDDEN.into_response();
    }
    event_stream_response(state, query)
}

fn event_stream_response(state: ServerState, query: StreamQuery) -> Response<Body> {
    let receiver = state.event_tx.subscribe();
    let replay_events = {
        let room = state.room.read().expect("room lock poisoned");
        room.events
            .iter()
            .filter(|event| event.seq > query.cursor)
            .cloned()
            .collect::<Vec<_>>()
    };
    let replay_cursor = replay_events
        .last()
        .map(|event| event.seq)
        .unwrap_or(query.cursor);
    let replay = tokio_stream::iter(replay_events.into_iter().map(sse_event));
    let live = BroadcastStream::new(receiver)
        .filter_map(move |event| event.ok().filter(|event| event.seq > replay_cursor))
        .map(sse_event);
    let stream: Pin<Box<dyn Stream<Item = Result<SseEvent, Infallible>> + Send>> = if query.once {
        Box::pin(replay)
    } else {
        Box::pin(replay.chain(live))
    };
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn sse_event(event: Event) -> Result<SseEvent, Infallible> {
    Ok(SseEvent::default()
        .json_data(event)
        .expect("Event is serializable"))
}
