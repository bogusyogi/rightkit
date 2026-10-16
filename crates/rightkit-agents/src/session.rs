//! Supervised agent sessions: spawn a vendor CLI, stream typed events, answer tool approvals,
//! cancel a turn, and tear down the whole process tree.
//!
//! Host-agnostic and dependency-light: blocking `std` threads and channels only (no async
//! runtime, no HTTP, no SDK, no MCP). The child is spawned through
//! `rightkit_child_process::OwnedCommand`, so it owns its tree: [`Session::shutdown`] and
//! dropping the [`Session`] terminate the CLI and every descendant it started.
//!
//! ```ignore
//! let mut session = Session::spawn(SessionConfig::new(Backend::Claude(ClaudeOptions::default())))?;
//! session.send_user_message("summarise the diff")?;
//! while let Ok(event) = session.events().recv() {
//!     match event {
//!         SessionEvent::Agent(AgentEvent::ApprovalRequested { id, .. }) => {
//!             session.approve(&id, ApprovalDecision::Allow)?;
//!         }
//!         SessionEvent::Closed => break,
//!         _ => {}
//!     }
//! }
//! session.shutdown(None)?;
//! ```
//!
//! Protocol sources (fetched 2026-10-16; wire shapes beyond these pages are marked
//! unverified in `CHANGELOG.md`):
//! - Claude Code stream-json: <https://code.claude.com/docs/en/cli-reference> (`--input-format`,
//!   `--output-format stream-json`, `--include-partial-messages`, `--replay-user-messages`),
//!   <https://code.claude.com/docs/en/agent-sdk/user-input> (allow with `updatedInput`, deny
//!   with `message`), <https://code.claude.com/docs/en/agent-sdk/typescript> (`PermissionResult`,
//!   `control_request` envelope `{type, request_id, request}`). CLI v2.1.x era.
//! - Codex `app-server` JSON-RPC over stdio JSONL, `jsonrpc` header optional:
//!   <https://learn.chatgpt.com/docs/app-server> (decisions `accept`, `acceptForSession`,
//!   `decline`, `cancel`; `turn/interrupt` with `threadId` and `turnId`), pinned to
//!   codex-cli [`PINNED_CODEX_VERSION`](crate::codex::PINNED_CODEX_VERSION).

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rightkit_child_process::{OwnedChild, OwnedCommand, WaitOutcome};
use serde_json::{json, Value};

use crate::claude::{self, ClaudeOptions, StreamParser};
use crate::codex::{self, CodexOptions, Decision, Incoming};
use crate::event::AgentEvent;

/// Default time to wait for the Codex `initialize` and `thread/start` handshake.
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Default grace between the polite stop and the forced tree kill.
pub const DEFAULT_TEARDOWN_GRACE: Duration = Duration::from_secs(3);

/// JSON-RPC "method not found", used to refuse server requests this crate does not implement.
const METHOD_NOT_FOUND: i64 = -32601;

/// Which vendor CLI to drive.
#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    /// `claude -p` with stream-json in and out. Tool approvals are routed to the host.
    Claude(ClaudeOptions),
    /// `codex app-server` over stdio.
    Codex(CodexOptions),
}

/// Replace the program and the whole argv a backend would otherwise use (wrappers, pinned
/// install paths, test doubles). Environment and working directory still come from
/// [`SessionConfig`].
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOverride {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// Everything needed to start a session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub backend: Backend,
    /// Working directory of the CLI. Also sent to Codex as the thread `cwd`.
    pub cwd: Option<PathBuf>,
    /// Environment added on top of the inherited one.
    pub env: Vec<(OsString, OsString)>,
    pub command: Option<CommandOverride>,
    pub startup_timeout: Duration,
    pub teardown_grace: Duration,
}

impl SessionConfig {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend,
            cwd: None,
            env: Vec::new(),
            command: None,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            teardown_grace: DEFAULT_TEARDOWN_GRACE,
        }
    }
}

/// What the host reads from [`Session::events`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SessionEvent {
    /// A vendor-neutral event. Approval requests arrive as
    /// [`AgentEvent::ApprovalRequested`]; answer with [`Session::approve`] or [`Session::deny`].
    /// A stdout line that is not JSON arrives as [`AgentEvent::Unknown`] holding a string.
    Agent(AgentEvent),
    /// The session itself failed to do something the host asked (for example a rejected
    /// `turn/start`). The session stays up.
    Error(String),
    /// The CLI closed its stdout. No more events follow; call [`Session::shutdown`] to reap.
    Closed,
}

/// The host's answer to an approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allow,
    /// Codex remembers the approval for the session. Claude has no wire equivalent here and
    /// treats it as [`ApprovalDecision::Allow`].
    AllowForSession,
    Deny {
        /// Shown to the model on Claude. Codex ignores it.
        message: Option<String>,
        /// Also stop the running turn (Claude `interrupt`, Codex `cancel`).
        interrupt: bool,
    },
}

/// Why a session call failed.
#[derive(Debug)]
pub enum SessionError {
    Io(std::io::Error),
    /// Bad configuration, rejected before anything was spawned.
    Config(String),
    /// Spawn succeeded but the CLI did not come up (the tree was torn down).
    Startup(String),
    /// stdin is closed or the session was shut down.
    Closed,
    /// No pending approval has this id (never asked, already answered, or cancelled).
    UnknownApproval(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Io(error) => write!(f, "io: {error}"),
            SessionError::Config(text) => write!(f, "config: {text}"),
            SessionError::Startup(text) => write!(f, "startup: {text}"),
            SessionError::Closed => f.write_str("session input is closed"),
            SessionError::UnknownApproval(id) => write!(f, "no pending approval {id}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<std::io::Error> for SessionError {
    fn from(error: std::io::Error) -> Self {
        SessionError::Io(error)
    }
}

/// How the CLI ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    pub status: ExitStatus,
    /// The tree had to be force-killed after the grace period.
    pub forced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Claude,
    Codex,
}

struct Pending {
    tool: String,
    input: Value,
    /// Claude `request_id` or the Codex JSON-RPC id, exactly as received.
    wire_id: Value,
}

enum Request {
    TurnStart,
    Interrupt,
}

struct Shared {
    kind: Kind,
    stdin: Mutex<Option<std::process::ChildStdin>>,
    pending: Mutex<HashMap<String, Pending>>,
    session_id: Mutex<Option<String>>,
    turn: Mutex<Option<String>>,
    requests: Mutex<HashMap<u64, Request>>,
    next_id: AtomicU64,
    events: Sender<SessionEvent>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn emit(&self, event: SessionEvent) {
        // A dropped receiver must not stop the reader: the child would block on a full pipe.
        let _ = self.events.send(event);
    }

    fn emit_agent(&self, event: AgentEvent) {
        if let AgentEvent::Init { session_id, .. } = &event {
            if !session_id.is_empty() {
                *lock(&self.session_id) = Some(session_id.clone());
            }
        }
        self.emit(SessionEvent::Agent(event));
    }

    fn emit_raw_line(&self, line: &str) {
        self.emit(SessionEvent::Agent(AgentEvent::Unknown {
            raw: Value::String(line.to_string()),
        }));
    }

    fn write_line(&self, line: &str) -> Result<(), SessionError> {
        let mut guard = lock(&self.stdin);
        let stdin = guard.as_mut().ok_or(SessionError::Closed)?;
        stdin.write_all(line.as_bytes())?;
        if !line.ends_with('\n') {
            stdin.write_all(b"\n")?;
        }
        stdin.flush()?;
        Ok(())
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    fn send_request(&self, id: u64, method: &str, params: Value) -> Result<(), SessionError> {
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write_line(&frame.to_string())
    }

    // ---- Claude ----

    fn claude_line(&self, parser: &mut StreamParser, line: &str) {
        match parser.parse_line(line) {
            Ok(events) => {
                for event in events {
                    if let AgentEvent::ApprovalRequested { id, tool, input } = &event {
                        lock(&self.pending).insert(
                            id.clone(),
                            Pending {
                                tool: tool.clone(),
                                input: input.clone(),
                                wire_id: Value::String(id.clone()),
                            },
                        );
                    }
                    self.emit_agent(event);
                }
            }
            Err(_) => self.emit_raw_line(line),
        }
    }

    // ---- Codex ----

    fn codex_line(&self, line: &str) {
        match codex::parse_line(line) {
            Err(_) => self.emit_raw_line(line),
            Ok(Incoming::Response { id, result, error }) => self.codex_response(&id, result, error),
            Ok(Incoming::ServerRequest {
                id,
                method,
                params,
                event,
            }) => match event {
                Some(AgentEvent::ApprovalRequested {
                    id: approval_id,
                    tool,
                    input,
                }) => {
                    lock(&self.pending).insert(
                        approval_id.clone(),
                        Pending {
                            tool: tool.clone(),
                            input: input.clone(),
                            wire_id: id,
                        },
                    );
                    self.emit_agent(AgentEvent::ApprovalRequested {
                        id: approval_id,
                        tool,
                        input,
                    });
                }
                _ => {
                    // Never leave the server waiting on a request nobody answers.
                    let frame = codex::error_response_frame(
                        &id,
                        METHOD_NOT_FOUND,
                        "server request not supported by this host",
                    );
                    let _ = self.write_line(&frame);
                    self.emit_agent(AgentEvent::Unknown {
                        raw: json!({"id": id, "method": method, "params": params}),
                    });
                }
            },
            Ok(Incoming::Notification { method, events }) => {
                match method.as_str() {
                    "turn/started" => {
                        // The notification, not only the `turn/start` response, names the
                        // turn; `cancel` needs it whichever arrives first.
                        let turn = serde_json::from_str::<Value>(line).ok().and_then(|value| {
                            value
                                .pointer("/params/turn/id")
                                .or_else(|| value.pointer("/params/turnId"))
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        });
                        if turn.is_some() {
                            *lock(&self.turn) = turn;
                        }
                    }
                    "turn/completed" => *lock(&self.turn) = None,
                    _ => {}
                }
                for event in events {
                    self.emit_agent(event);
                }
            }
        }
    }

    fn codex_response(&self, id: &Value, result: Option<Value>, error: Option<Value>) {
        let Some(key) = id.as_u64() else { return };
        let Some(request) = lock(&self.requests).remove(&key) else {
            return;
        };
        if let Some(error) = error {
            let what = match request {
                Request::TurnStart => "turn/start",
                Request::Interrupt => "turn/interrupt",
            };
            self.emit(SessionEvent::Error(format!("{what} rejected: {error}")));
            return;
        }
        if let Request::TurnStart = request {
            let result = result.unwrap_or(Value::Null);
            let turn = result
                .pointer("/turn/id")
                .or_else(|| result.get("turnId"))
                .and_then(Value::as_str)
                .map(str::to_string);
            *lock(&self.turn) = turn;
        }
    }
}

/// A running vendor agent session. Methods take `&self` and are safe to call from any
/// thread; only [`Session::shutdown`] and [`Session::exit_status`] need ownership or `&mut`.
pub struct Session {
    child: OwnedChild,
    shared: Arc<Shared>,
    events: Receiver<SessionEvent>,
    teardown_grace: Duration,
}

impl Session {
    /// Spawn the CLI and, for Codex, complete the `initialize` / `thread/start` handshake
    /// (bounded by `startup_timeout`). On any failure the whole tree is already torn down.
    pub fn spawn(config: SessionConfig) -> Result<Session, SessionError> {
        let (kind, program, args, thread_params) = resolve(&config)?;
        let mut command = Command::new(&program);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &config.env {
            command.env(key, value);
        }
        let mut child = OwnedCommand::from_command(command).spawn()?;
        let stdin = child
            .take_stdin()
            .ok_or_else(|| SessionError::Startup("child has no stdin".into()))?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| SessionError::Startup("child has no stdout".into()))?;
        let stderr = child.take_stderr();

        let (events_tx, events_rx) = channel();
        let shared = Arc::new(Shared {
            kind,
            stdin: Mutex::new(Some(stdin)),
            pending: Mutex::new(HashMap::new()),
            session_id: Mutex::new(None),
            turn: Mutex::new(None),
            requests: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            events: events_tx,
        });

        if let Some(stderr) = stderr {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("rightkit-agents-stderr".into())
                .spawn(move || {
                    for chunk in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
                        let line = String::from_utf8_lossy(&chunk).trim_end().to_string();
                        if !line.is_empty() {
                            shared.emit(SessionEvent::Agent(AgentEvent::Stderr { line }));
                        }
                    }
                })?;
        }

        match kind {
            Kind::Claude => {
                let reader_shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("rightkit-agents-claude".into())
                    .spawn(move || {
                        let mut parser = StreamParser::new();
                        for chunk in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
                            let text = String::from_utf8_lossy(&chunk);
                            let line = text.trim();
                            if !line.is_empty() {
                                reader_shared.claude_line(&mut parser, line);
                            }
                        }
                        reader_shared.emit(SessionEvent::Closed);
                    })?;
            }
            Kind::Codex => {
                let (ready_tx, ready_rx) = channel::<Result<(), String>>();
                let reader_shared = Arc::clone(&shared);
                let resume = matches!(
                    &config.backend,
                    Backend::Codex(options) if options.resume_thread_id.is_some()
                );
                std::thread::Builder::new()
                    .name("rightkit-agents-codex".into())
                    .spawn(move || {
                        codex_reader(reader_shared, stdout, thread_params, resume, ready_tx)
                    })?;
                match ready_rx.recv_timeout(config.startup_timeout) {
                    Ok(Ok(())) => {}
                    Ok(Err(reason)) => return Err(SessionError::Startup(reason)),
                    Err(_) => {
                        return Err(SessionError::Startup(format!(
                            "no app-server handshake within {:?}",
                            config.startup_timeout
                        )))
                    }
                }
            }
        }

        Ok(Session {
            child,
            shared,
            events: events_rx,
            teardown_grace: config.teardown_grace,
        })
    }

    /// The typed event stream. Single consumer.
    pub fn events(&self) -> &Receiver<SessionEvent> {
        &self.events
    }

    /// Claude `session_id` from `system/init`, or the Codex thread id. `None` until known.
    pub fn session_id(&self) -> Option<String> {
        lock(&self.shared.session_id).clone()
    }

    /// OS pid of the CLI process (the tree root).
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Ids of approvals the host has not answered yet.
    pub fn pending_approvals(&self) -> Vec<String> {
        lock(&self.shared.pending).keys().cloned().collect()
    }

    /// Send one user turn. Claude queues it on stdin; Codex starts a turn with `turn/start`.
    pub fn send_user_message(&self, text: &str) -> Result<(), SessionError> {
        match self.shared.kind {
            Kind::Claude => self.shared.write_line(&claude::user_turn_frame(text)),
            Kind::Codex => {
                let thread = self.session_id().ok_or(SessionError::Closed)?;
                let id = self.shared.next_request_id();
                lock(&self.shared.requests).insert(id, Request::TurnStart);
                self.shared
                    .send_request(
                        id,
                        "turn/start",
                        json!({"threadId": thread, "input": [{"type": "text", "text": text}]}),
                    )
                    .inspect_err(|_| {
                        lock(&self.shared.requests).remove(&id);
                    })
            }
        }
    }

    /// Approve a pending request (see [`SessionEvent::Agent`]).
    pub fn approve(&self, id: &str, decision: ApprovalDecision) -> Result<(), SessionError> {
        let pending = lock(&self.shared.pending)
            .remove(id)
            .ok_or_else(|| SessionError::UnknownApproval(id.to_string()))?;
        self.answer(id, &pending, &decision)
    }

    /// Deny a pending request. Shorthand for [`ApprovalDecision::Deny`].
    pub fn deny(
        &self,
        id: &str,
        message: Option<&str>,
        interrupt: bool,
    ) -> Result<(), SessionError> {
        self.approve(
            id,
            ApprovalDecision::Deny {
                message: message.map(str::to_string),
                interrupt,
            },
        )
    }

    fn answer(
        &self,
        id: &str,
        pending: &Pending,
        decision: &ApprovalDecision,
    ) -> Result<(), SessionError> {
        let _ = &pending.tool;
        let result = match self.shared.kind {
            Kind::Claude => {
                let request_id = pending.wire_id.as_str().unwrap_or(id);
                let frame = match decision {
                    ApprovalDecision::Allow | ApprovalDecision::AllowForSession => {
                        claude::permission_allow_frame(request_id, &pending.input)
                    }
                    ApprovalDecision::Deny { message, interrupt } => claude::permission_deny_frame(
                        request_id,
                        message.as_deref().unwrap_or("Denied by the host"),
                        *interrupt,
                    ),
                };
                self.shared.write_line(&frame).map(|()| {
                    // Claude sends no resolution notice, so the session reports it.
                    self.shared
                        .emit_agent(AgentEvent::ApprovalResolved { id: id.to_string() });
                })
            }
            Kind::Codex => {
                let wire = match decision {
                    ApprovalDecision::Allow => Decision::Accept,
                    ApprovalDecision::AllowForSession => Decision::AcceptForSession,
                    ApprovalDecision::Deny {
                        interrupt: true, ..
                    } => Decision::Cancel,
                    ApprovalDecision::Deny { .. } => Decision::Decline,
                };
                // Codex announces resolution itself (`serverRequest/resolved`).
                self.shared
                    .write_line(&codex::approval_response_frame(&pending.wire_id, wire))
            }
        };
        if result.is_err() {
            // The answer never left: keep the request answerable.
            lock(&self.shared.pending).insert(
                id.to_string(),
                Pending {
                    tool: pending.tool.clone(),
                    input: pending.input.clone(),
                    wire_id: pending.wire_id.clone(),
                },
            );
        }
        result
    }

    /// Interrupt the running turn; the session stays up for the next message. Pending
    /// approvals are released first (Codex is told `cancel`; Claude's interrupt clears its
    /// own). Returns whether an interrupt was sent: Codex has nothing to interrupt before
    /// its `turn/start` response arrived or after `turn/completed`.
    pub fn cancel(&self) -> Result<bool, SessionError> {
        let released: Vec<(String, Pending)> = lock(&self.shared.pending).drain().collect();
        // Read the turn before answering anything: the answers can make the server finish
        // the turn, and the reader would clear it under us.
        let thread = self.session_id();
        let turn = lock(&self.shared.turn).clone();
        match self.shared.kind {
            Kind::Claude => {
                let id = format!("rk-{}", self.shared.next_request_id());
                self.shared.write_line(&claude::interrupt_frame(&id))?;
                // Claude sends no resolution notice, so the session reports the release.
                for (approval, _) in released {
                    self.shared
                        .emit_agent(AgentEvent::ApprovalResolved { id: approval });
                }
                Ok(true)
            }
            Kind::Codex => {
                for (_, pending) in &released {
                    let frame =
                        codex::approval_response_frame(&pending.wire_id, Decision::Cancel);
                    self.shared.write_line(&frame)?;
                }
                let (Some(thread), Some(turn)) = (thread, turn) else {
                    return Ok(false);
                };
                let id = self.shared.next_request_id();
                lock(&self.shared.requests).insert(id, Request::Interrupt);
                self.shared.send_request(
                    id,
                    "turn/interrupt",
                    json!({"threadId": thread, "turnId": turn}),
                )?;
                Ok(true)
            }
        }
    }

    /// Exit status if the CLI already exited, without waiting.
    pub fn exit_status(&mut self) -> Result<Option<ExitStatus>, SessionError> {
        Ok(self.child.try_wait()?)
    }

    /// Close stdin, then stop the whole process tree: polite stop, `grace` (default
    /// `teardown_grace`), forced kill, reap. Descendants the CLI started do not survive.
    pub fn shutdown(mut self, grace: Option<Duration>) -> Result<ExitInfo, SessionError> {
        drop(lock(&self.shared.stdin).take());
        let grace = grace.unwrap_or(self.teardown_grace);
        Ok(match self.child.wait_or_kill(grace)? {
            WaitOutcome::Exited(status) => ExitInfo {
                status,
                forced: false,
            },
            WaitOutcome::Terminated(status) => ExitInfo {
                status,
                forced: true,
            },
        })
    }
}

type Resolved = (Kind, PathBuf, Vec<OsString>, Value);

fn resolve(config: &SessionConfig) -> Result<Resolved, SessionError> {
    let cwd = config.cwd.as_ref().map(|path| path.to_string_lossy().into_owned());
    match &config.backend {
        Backend::Claude(options) => {
            let mut options = options.clone();
            options.host_approvals = true;
            let args = claude::build_args(&options).map_err(SessionError::Config)?;
            Ok(pick(config, Kind::Claude, "claude", args, Value::Null))
        }
        Backend::Codex(options) => {
            let params = codex::thread_params(options, cwd.as_deref());
            Ok(pick(config, Kind::Codex, "codex", codex::build_args(), params))
        }
    }
}

fn pick(
    config: &SessionConfig,
    kind: Kind,
    default_program: &str,
    default_args: Vec<String>,
    params: Value,
) -> Resolved {
    match &config.command {
        Some(over) => (kind, over.program.clone(), over.args.clone(), params),
        None => (
            kind,
            PathBuf::from(default_program),
            default_args.into_iter().map(OsString::from).collect(),
            params,
        ),
    }
}

fn codex_reader(
    shared: Arc<Shared>,
    stdout: std::process::ChildStdout,
    thread_params: Value,
    resume: bool,
    ready: Sender<Result<(), String>>,
) {
    let mut lines = BufReader::new(stdout).split(b'\n').map_while(Result::ok);
    let mut next_line = move || -> Option<String> {
        loop {
            let chunk = lines.next()?;
            let text = String::from_utf8_lossy(&chunk).trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    };

    // Handshake. Anything that is not the awaited response is replayed afterwards.
    let mut backlog: Vec<String> = Vec::new();
    let mut handshake = |method: &str, params: Value, backlog: &mut Vec<String>| -> Result<Value, String> {
        let id = shared.next_request_id();
        shared
            .send_request(id, method, params)
            .map_err(|error| format!("{method}: {error}"))?;
        loop {
            let Some(line) = next_line() else {
                return Err(format!("app-server closed stdout during {method}"));
            };
            match codex::parse_line(&line) {
                Ok(Incoming::Response {
                    id: response_id,
                    result,
                    error,
                }) if response_id == json!(id) => {
                    return match error {
                        Some(error) => Err(format!("{method} failed: {error}")),
                        None => Ok(result.unwrap_or(Value::Null)),
                    };
                }
                _ => backlog.push(line),
            }
        }
    };

    let started = (|| {
        handshake(
            "initialize",
            json!({"clientInfo": {"name": "rightkit-agents", "version": env!("CARGO_PKG_VERSION")}}),
            &mut backlog,
        )?;
        shared
            .write_line(&json!({"jsonrpc": "2.0", "method": "initialized"}).to_string())
            .map_err(|error| format!("initialized: {error}"))?;
        let method = if resume { "thread/resume" } else { "thread/start" };
        let result = handshake(method, thread_params, &mut backlog)?;
        result
            .pointer("/thread/id")
            .or_else(|| result.get("threadId"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("{method} response carried no thread id"))
    })();

    match started {
        Ok(thread) => {
            *lock(&shared.session_id) = Some(thread);
            for line in backlog {
                shared.codex_line(&line);
            }
            let _ = ready.send(Ok(()));
        }
        Err(reason) => {
            let _ = ready.send(Err(reason));
            shared.emit(SessionEvent::Closed);
            return;
        }
    }

    while let Some(line) = next_line() {
        shared.codex_line(&line);
    }
    shared.emit(SessionEvent::Closed);
}
