// One daemon-session client shared by terminal surfaces.
//
// It deliberately speaks only existing daemon routes.  CLI/TUI/ACP remain
// clients: session ownership, event durability, approvals, steering, and
// interruption stay in the daemon.

use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use std::pin::Pin;

/// Boxed decoded-event stream backing [`DaemonEventStream`]. In production this is
/// `rightkit_http`'s `SseStream` (WHATWG framing, byte-exact UTF-8, bounded lines and
/// events, `EofPolicy::Deliver` so a final frame the daemon closes right after is not
/// lost); tests feed it from a hand-built decoder.
type DaemonSseStream =
    Pin<Box<dyn Stream<Item = Result<rightkit_http::SseEvent, rightkit_http::HttpError>> + Send>>;

/// CR-04 bound: a single SSE line or event may never grow the client's buffer past
/// this many bytes. Generous enough for a large tool-result payload in one frame, but
/// finite: an unbounded buffer on a malformed or hostile stream is an unbounded-memory
/// bug, not resilience.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Parsed view over the daemon's SSE session stream. Framing is `rightkit_http`'s
/// decoder; this type only turns each event's `data` into JSON.
pub(crate) struct DaemonEventStream {
    body: DaemonSseStream,
}

impl DaemonEventStream {
    /// Returns the next parsed SSE `data:` payload, `Ok(None)` when the stream ends
    /// cleanly (server closed the connection, including a clean EOF after a trailing
    /// unterminated frame, which is delivered first), or `Err` on a transport
    /// failure, malformed non-UTF-8 frame, or an over-cap line or event. Any of those
    /// `Err` cases is reconnectable: the caller (`run_session`) treats a stream-level
    /// `Err` as a transport failure and retries under its existing reconnect budget,
    /// never as a terminal event. A frame with no `data:` line (SSE comments, blank
    /// keep-alives) is never delivered by the decoder, and a `data:` payload that
    /// fails to parse as JSON is wire noise, not a transport error: it is skipped and
    /// the read continues.
    pub(crate) async fn next_event(&mut self) -> Result<Option<Value>, String> {
        loop {
            match self.body.next().await {
                Some(Ok(event)) => {
                    if let Ok(value) = serde_json::from_str::<Value>(&event.data) {
                        return Ok(Some(value));
                    }
                }
                Some(Err(error)) => return Err(format!("read daemon event stream: {error}")),
                None => return Ok(None),
            }
        }
    }
}

pub(crate) struct DaemonSessionClient {
    base_url: String,
    token: String,
    http: rightkit_http::AsyncClient,
}

type DaemonVerb = rightkit_http::Method;

/// JSON body on a request. Serializing a `serde_json::Value` or a derived
/// `Serialize` struct cannot fail; a failure would send the request bodiless and
/// be rejected by the daemon.
fn with_json_body(
    request: rightkit_http::Request,
    body: &impl serde::Serialize,
) -> rightkit_http::Request {
    match serde_json::to_vec(body) {
        Ok(bytes) => request
            .body(bytes)
            .set_header("Content-Type", "application/json"),
        Err(_) => request,
    }
}

impl DaemonSessionClient {
    pub(crate) fn new(base_url: String, token: String) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let http = rightkit_agent_daemon::http_client::daemon_client(&base_url)
            .expect("rightkit-http client builds with the platform verifier");
        Self {
            base_url,
            token,
            http,
        }
    }

    /// Daemon request for `path` (relative to the base URL), bearer token attached.
    fn req(&self, method: DaemonVerb, path: &str) -> rightkit_http::Request {
        rightkit_http::Request::new(method, format!("{}{}", self.base_url, path))
            .bearer(&self.token)
    }

    /// [`Self::req`] with a JSON body.
    fn req_json(
        &self,
        method: DaemonVerb,
        path: &str,
        body: &impl serde::Serialize,
    ) -> rightkit_http::Request {
        with_json_body(self.req(method, path), body)
    }

    async fn send(
        &self,
        request: rightkit_http::Request,
    ) -> Result<rightkit_http::Response, rightkit_http::HttpError> {
        self.http.send_any(&request).await
    }

    pub(crate) async fn context_degraded_reason(&self) -> Result<Option<String>, String> {
        let response = self
            .send(self.req(DaemonVerb::Get, "/runtime/context/status"))
            .await
            .map_err(|error| format!("read daemon context status: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!(
                "read daemon context status: HTTP {}",
                response.status
            ));
        }
        let body = response
            .json::<Value>()
            .map_err(|error| format!("decode daemon context status: {error}"))?;
        Ok(body
            .get("degradedReason")
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    pub(crate) async fn hooks_status(&self) -> Result<Value, String> {
        let response = self
            .send(self.req(DaemonVerb::Get, "/hooks/status"))
            .await
            .map_err(|error| format!("read hooks status: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("read hooks status: HTTP {}", response.status));
        }
        response
            .json::<Value>()
            .map_err(|error| format!("decode hooks status: {error}"))
    }

    /// `GET /sessions/{id}/rewind/points`: prior user prompts, oldest first.
    pub(crate) async fn rewind_points(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::api_sessions::rewind::RewindPoint>, String> {
        let response = self
            .send(self.req(
                DaemonVerb::Get,
                &Self::session_path(session_id, "/rewind/points"),
            ))
            .await
            .map_err(|error| format!("read rewind points: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("read rewind points: HTTP {}", response.status));
        }
        let body: Value = response
            .json()
            .map_err(|error| format!("decode rewind points: {error}"))?;
        parse_rewind_points(&body)
    }

    /// `POST /sessions/{id}/rewind` to `point`.
    pub(crate) async fn rewind_to(
        &self,
        session_id: &str,
        point: &crate::api_sessions::rewind::RewindPoint,
        mode: crate::api_sessions::rewind::RewindMode,
    ) -> Result<crate::api_sessions::rewind::RewindReport, String> {
        let mode = match mode {
            crate::api_sessions::rewind::RewindMode::Conversation => "conversation",
            crate::api_sessions::rewind::RewindMode::Files => "files",
            crate::api_sessions::rewind::RewindMode::Both => "both",
        };
        let body = json!({
            "point": {
                "text": point.text,
                "nthFromEnd": point.nth_from_end,
                "createdAt": point.created_at,
            },
            "mode": mode,
        });
        let response = self
            .send(self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/rewind"),
                &body,
            ))
            .await
            .map_err(|error| format!("rewind session: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("rewind session: HTTP {}", response.status));
        }
        let body: Value = response
            .json()
            .map_err(|error| format!("decode rewind reply: {error}"))?;
        Ok(parse_rewind_report(&body))
    }

    /// `POST /sessions/{id}/redo`. A refusal (409) is its `message`.
    pub(crate) async fn redo(
        &self,
        session_id: &str,
    ) -> Result<crate::api_sessions::rewind::RewindReport, String> {
        let response = self
            .send(self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/redo"),
                &json!({}),
            ))
            .await
            .map_err(|error| format!("redo session: {error}"))?;
        let status = response.status;
        let body: Value = response.json().unwrap_or(Value::Null);
        if rightkit_agent_daemon::http_client::is_success(status) {
            return Ok(parse_rewind_report(&body));
        }
        Err(parse_redo_refusal(status, &body))
    }

    fn session_path(session_id: &str, suffix: &str) -> String {
        format!("/sessions/{}{}", url_encode_component(session_id), suffix)
    }

    pub(crate) async fn ensure_session(&self, session_id: &str, body: Value) -> Result<(), String> {
        let response = self
            .send(self.req_json(DaemonVerb::Post, "/sessions", &body))
            .await
            .map_err(|error| format!("create daemon session: {error}"))?;
        let grant_asked = body
            .get("trustWorkspace")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if rightkit_agent_daemon::http_client::is_success(response.status) {
            // Issue 494 item 4: the daemon says when it did not apply the grant.
            let created: Value = response.json().unwrap_or(Value::Null);
            if let Some(reason) = created.get("trustWorkspaceDropped").and_then(Value::as_str) {
                eprintln!("warning: --trust-workspace: {reason}");
            }
            return Ok(());
        }
        if response.status == 409 {
            // Issue 494 item 5: the grant applies only when a session is created.
            if grant_asked {
                eprintln!(
                    "warning: --trust-workspace: session {session_id} already exists, so it is resumed as it was created; the trust grant applies only when a session is created"
                );
            }
            if !body
                .get("safeMode")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(());
            }
            let existing = self.session_snapshot(session_id).await?.ok_or_else(|| {
                format!("conflicting daemon session {session_id} was not visible")
            })?;
            if session_conflict_preserves_safe_mode(&body, &existing) {
                return Ok(());
            }
            return Err(format!(
                "safe mode cannot resume unrestricted daemon session {session_id}"
            ));
        }
        Err(format!(
            "create daemon session {session_id}: HTTP {}",
            response.status
        ))
    }

    async fn session_snapshot(&self, session_id: &str) -> Result<Option<Value>, String> {
        let response = self
            .send(self.req(DaemonVerb::Get, "/sessions?includeSubagents=true"))
            .await
            .map_err(|error| format!("inspect conflicting daemon session: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!(
                "inspect conflicting daemon session: HTTP {}",
                response.status
            ));
        }
        let sessions = response
            .json::<Vec<Value>>()
            .map_err(|error| format!("decode daemon session list: {error}"))?;
        Ok(sessions
            .into_iter()
            .find(|session| session.get("id").and_then(Value::as_str) == Some(session_id)))
    }

    /// Submit with an idempotency nonce: the same nonce replays the first
    /// admission and never starts a second turn.
    pub(crate) async fn submit_message_with_nonce(
        &self,
        session_id: &str,
        prompt: &str,
        workspace_root: &str,
        attachments: Option<Vec<crate::api::Attachment>>,
        sandbox_grant_prompts: Option<bool>,
        nonce: Option<&str>,
    ) -> Result<i64, String> {
        let response = self
            .send(self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/messages"),
                &json!({ "text": prompt, "workspaceRoot": workspace_root, "attachments": attachments, "sandboxGrantPrompts": sandbox_grant_prompts, "nonce": nonce }),
            ))
            .await
            .map_err(|error| format!("submit daemon message: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            if response.status == 413 {
                return Err("submit daemon message: the message is larger than the daemon accepts (HTTP 413, about 2 MB); \
                            send less text, or point to a file in the workspace and ask the agent to read it"
                    .to_string());
            }
            return Err(format!("submit daemon message: HTTP {}", response.status));
        }
        let response: Value = response
            .json()
            .map_err(|error| format!("decode daemon message response: {error}"))?;
        Ok(response.get("seq").and_then(Value::as_i64).unwrap_or(0))
    }

    /// Catch-up after `streamResyncRequired`: page `/events/replay` with
    /// `after=next` until `complete`, returning every event in order. Falls
    /// back to the unpaged `/events/history` when the daemon predates replay.
    pub(crate) async fn events_after(
        &self,
        session_id: &str,
        after: i64,
    ) -> Result<Vec<Value>, String> {
        let mut cursor = after;
        let mut out: Vec<Value> = Vec::new();
        loop {
            let response = self
                .send(self.req(
                    DaemonVerb::Get,
                    &format!(
                        "{}?after={cursor}",
                        Self::session_path(session_id, "/events/replay")
                    ),
                ))
                .await
                .map_err(|error| format!("read daemon event replay: {error}"))?;
            let status = response.status;
            if !rightkit_agent_daemon::http_client::is_success(status) {
                if out.is_empty() && (status == 404 || status == 405) {
                    return self.events_history_after(session_id, after).await;
                }
                return Err(format!("read daemon event replay: HTTP {status}"));
            }
            let page: Value = response
                .json()
                .map_err(|error| format!("decode daemon event replay: {error}"))?;
            if let Some(events) = page.get("events").and_then(Value::as_array) {
                out.extend(events.iter().cloned());
            }
            let complete = page
                .get("complete")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if complete {
                return Ok(out);
            }
            let next = page.get("next").and_then(Value::as_i64).unwrap_or(cursor);
            if next <= cursor {
                return Err(format!(
                    "daemon event replay made no progress at after={cursor}"
                ));
            }
            cursor = next;
        }
    }

    async fn events_history_after(
        &self,
        session_id: &str,
        after: i64,
    ) -> Result<Vec<Value>, String> {
        // `/events/history` answers at most one page (1000 events by default), so a long tail is read
        // page by page from the last seq returned.
        const HISTORY_PAGE: usize = 1000;
        let mut cursor = after;
        let mut out: Vec<Value> = Vec::new();
        loop {
            let response = self
                .send(self.req(
                    DaemonVerb::Get,
                    &format!(
                        "{}?after={cursor}",
                        Self::session_path(session_id, "/events/history")
                    ),
                ))
                .await
                .map_err(|error| format!("read daemon event history: {error}"))?;
            if !rightkit_agent_daemon::http_client::is_success(response.status) {
                return Err(format!(
                    "read daemon event history: HTTP {}",
                    response.status
                ));
            }
            let page: Vec<Value> = response
                .json()
                .map_err(|error| format!("decode daemon event history: {error}"))?;
            let next = page
                .last()
                .and_then(|event| event.get("seq"))
                .and_then(Value::as_i64)
                .unwrap_or(cursor);
            let full_page = page.len() >= HISTORY_PAGE;
            out.extend(page);
            if !full_page || next <= cursor {
                return Ok(out);
            }
            cursor = next;
        }
    }

    /// Open the live SSE stream from `after`. Reuses the client's shared
    /// `http` — no separate client is constructed for streaming.
    pub(crate) async fn open_event_stream(
        &self,
        session_id: &str,
        after: i64,
    ) -> Result<DaemonEventStream, String> {
        let response = self
            .http
            .stream_any(
                &self
                    .req(
                        DaemonVerb::Get,
                        &format!(
                            "{}?after={after}",
                            Self::session_path(session_id, "/events")
                        ),
                    )
                    .header("Accept", "text/event-stream"),
            )
            .await
            .map_err(|error| format!("open daemon event stream: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!(
                "open daemon event stream: HTTP {}",
                response.status
            ));
        }
        let events = response
            .sse_with_limits(MAX_FRAME_BYTES, MAX_FRAME_BYTES)
            .eof_policy(rightkit_http::EofPolicy::Deliver);
        Ok(DaemonEventStream {
            body: Box::pin(events),
        })
    }

    /// Backwards event history page: `GET /sessions/{id}/events/history`
    /// with `before`/`limit`. Used by surfaces that page older durable
    /// events above the retained SSE window.
    pub(crate) async fn events_before(
        &self,
        session_id: &str,
        before: i64,
        limit: Option<usize>,
    ) -> Result<Vec<Value>, String> {
        let mut path = format!(
            "{}?before={before}",
            Self::session_path(session_id, "/events/history")
        );
        if let Some(limit) = limit {
            path.push_str(&format!("&limit={limit}"));
        }
        let response = self
            .send(self.req(DaemonVerb::Get, &path))
            .await
            .map_err(|error| format!("read daemon event history: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!(
                "read daemon event history: HTTP {}",
                response.status
            ));
        }
        response
            .json()
            .map_err(|error| format!("decode daemon event history: {error}"))
    }

    /// Re-route a session's model/provider/effort through the daemon's
    /// authoritative PATCH. The body is forwarded verbatim (`model`,
    /// `providerId`, `effort`); daemon-side validation decides whether the
    /// switch is admissible (400 unknown provider/effort, 409 busy or
    /// qualified-route session).
    pub(crate) async fn patch_route(&self, session_id: &str, route: &Value) -> Result<(), String> {
        let response = self
            .send(self.req_json(
                DaemonVerb::Patch,
                &Self::session_path(session_id, ""),
                route,
            ))
            .await
            .map_err(|error| format!("patch daemon session route: {error}"))?;
        if rightkit_agent_daemon::http_client::is_success(response.status) {
            Ok(())
        } else {
            let status = response.status;
            let detail = response.text();
            Err(format!(
                "patch daemon session route: HTTP {status} {detail}"
            ))
        }
    }

    pub(crate) async fn interrupt(&self, session_id: &str) -> Result<(), String> {
        self.post_empty(session_id, "/interrupt", "interrupt daemon session")
            .await
    }

    pub(crate) async fn turn_is_active(&self, session_id: &str) -> Result<bool, String> {
        let snapshot = self
            .session_snapshot(session_id)
            .await?
            .ok_or_else(|| format!("session {session_id} disappeared before turn settlement"))?;
        snapshot
            .get("turnActive")
            .and_then(Value::as_bool)
            .ok_or_else(|| "ServiceHost does not expose authoritative turn activity".to_string())
    }

    pub(crate) async fn steer(&self, session_id: &str, text: &str) -> Result<(), String> {
        let response = self
            .send(self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/steer"),
                &json!({ "text": text }),
            ))
            .await
            .map_err(|error| format!("steer daemon session: {error}"))?;
        if rightkit_agent_daemon::http_client::is_success(response.status) {
            Ok(())
        } else {
            Err(format!("steer daemon session: HTTP {}", response.status))
        }
    }

    /// Goal control over the daemon routes (`/goal`, `/goal/pause|resume|edit`).
    /// `op` is one of `get|set|edit|pause|resume|clear`; the JSON body is the
    /// route's own request (`{"text":..}` for set/edit).
    pub(crate) async fn goal_op(
        &self,
        session_id: &str,
        op: &str,
        body: &Value,
    ) -> Result<Value, String> {
        let (suffix, request) = match op {
            "get" => (
                "/goal",
                self.req(DaemonVerb::Get, &Self::session_path(session_id, "/goal")),
            ),
            "clear" => (
                "/goal",
                self.req(DaemonVerb::Delete, &Self::session_path(session_id, "/goal")),
            ),
            "set" => (
                "/goal",
                self.req_json(
                    DaemonVerb::Post,
                    &Self::session_path(session_id, "/goal"),
                    body,
                ),
            ),
            "edit" => (
                "/goal/edit",
                self.req_json(
                    DaemonVerb::Post,
                    &Self::session_path(session_id, "/goal/edit"),
                    body,
                ),
            ),
            "pause" => (
                "/goal/pause",
                self.req(
                    DaemonVerb::Post,
                    &Self::session_path(session_id, "/goal/pause"),
                ),
            ),
            "resume" => (
                "/goal/resume",
                self.req(
                    DaemonVerb::Post,
                    &Self::session_path(session_id, "/goal/resume"),
                ),
            ),
            other => return Err(format!("unknown goal operation `{other}`")),
        };
        let response = self
            .send(request)
            .await
            .map_err(|error| format!("goal {op} ({suffix}): {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("goal {op}: HTTP {}", response.status));
        }
        response
            .json::<Value>()
            .map_err(|error| format!("goal {op}: {error}"))
    }

    async fn post_empty(&self, session_id: &str, suffix: &str, action: &str) -> Result<(), String> {
        let response = self
            .send(self.req(DaemonVerb::Post, &Self::session_path(session_id, suffix)))
            .await
            .map_err(|error| format!("{action}: {error}"))?;
        if rightkit_agent_daemon::http_client::is_success(response.status) {
            Ok(())
        } else {
            Err(format!("{action}: HTTP {}", response.status))
        }
    }

    pub(crate) async fn respond_interaction(
        &self,
        session_id: &str,
        suffix: &str,
        body: &Value,
    ) -> Result<(), String> {
        let response = self
            .send(self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, suffix),
                body,
            ))
            .await
            .map_err(|error| format!("respond to daemon interaction: {error}"))?;
        if rightkit_agent_daemon::http_client::is_success(response.status) {
            Ok(())
        } else {
            Err(format!(
                "respond to daemon interaction: HTTP {}",
                response.status
            ))
        }
    }

    /// Fetch the daemon's session list (`GET /sessions`). Each row carries
    /// `id`, `title`, `model`, `status`, `costUsd`, and `parentSessionId`
    /// (non-null for sub-agents). This is the feed the TUI's `/sessions`
    /// picker projects; the daemon is the owner of what "a session" means.
    pub(crate) async fn list_sessions(&self) -> Result<Vec<Value>, String> {
        let response = self
            .send(self.req(DaemonVerb::Get, "/sessions"))
            .await
            .map_err(|error| format!("list daemon sessions: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("list daemon sessions: HTTP {}", response.status));
        }
        response
            .json::<Vec<Value>>()
            .map_err(|error| format!("decode daemon session list: {error}"))
    }

    /// Session maintenance through existing daemon lifecycle/export routes.
    pub(crate) async fn session_action(
        &self,
        action: &str,
        session_id: &str,
        value: Option<&str>,
    ) -> Result<Value, String> {
        let request = match action {
            "rename" => self.req_json(
                DaemonVerb::Patch,
                &Self::session_path(session_id, ""),
                &serde_json::json!({"title": value.unwrap_or_default()}),
            ),
            "archive" => self.req_json(
                DaemonVerb::Patch,
                &Self::session_path(session_id, ""),
                &serde_json::json!({"status": "archived"}),
            ),
            "delete" => self.req(DaemonVerb::Delete, &Self::session_path(session_id, "")),
            "fork" => self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/fork"),
                &serde_json::json!({"title": value}),
            ),
            // Fork into another folder: `value` is the folder the fork works in.
            "fork_into" => self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/fork"),
                &serde_json::json!({"workspaceRoot": value}),
            ),
            // Turn-scoped fork: `value` is N, branch before the Nth-last user turn.
            "fork_back" => self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/fork"),
                &serde_json::json!({
                    "turnsBack": value.and_then(|n| n.trim().parse::<usize>().ok())
                }),
            ),
            "export" => self.req(
                DaemonVerb::Get,
                &format!(
                    "{}?format={}",
                    Self::session_path(session_id, "/export"),
                    rightkit_http::percent_encode(value.unwrap_or("md"))
                ),
            ),
            _ => return Err(format!("unsupported session action `{action}`")),
        };
        let response = self
            .send(request)
            .await
            .map_err(|error| format!("{action} daemon session: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("{action} daemon session: HTTP {}", response.status));
        }
        response
            .json::<Value>()
            .map_err(|error| format!("decode {action} response: {error}"))
    }

    pub(crate) async fn adapt_insights(&self, body: &Value) -> Result<Value, String> {
        let response = self
            .send(self.req_json(DaemonVerb::Post, "/runtime/context/adapt/insights", body))
            .await
            .map_err(|error| format!("request Adapt insights: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("Adapt insights: HTTP {}", response.status));
        }
        response
            .json::<Value>()
            .map_err(|error| format!("decode Adapt insights: {error}"))
    }

    pub(crate) async fn worktree_action(
        &self,
        action: &str,
        body: &Value,
    ) -> Result<Value, String> {
        let request = match action {
            "list" => self.req(
                DaemonVerb::Get,
                &format!(
                    "/delivery/worktrees?workspaceRoot={}",
                    rightkit_http::percent_encode(
                        body.get("workspaceRoot")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    )
                ),
            ),
            "add" => self.req_json(DaemonVerb::Post, "/delivery/worktree", body),
            "remove" => self.req_json(DaemonVerb::Delete, "/delivery/worktree", body),
            _ => return Err(format!("unsupported worktree action `{action}`")),
        };
        let response = self
            .send(request)
            .await
            .map_err(|error| format!("{action} worktree: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("{action} worktree: HTTP {}", response.status));
        }
        response
            .json::<Value>()
            .map_err(|error| format!("decode worktree response: {error}"))
    }

    /// Session rules plus every persistent rule, as `(pattern, effect)`.
    pub(crate) async fn permission_rules(
        &self,
        session_id: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let get = |path: String| async move {
            let response = self
                .send(self.req(DaemonVerb::Get, &path))
                .await
                .map_err(|error| format!("read permission rules: {error}"))?;
            if !rightkit_agent_daemon::http_client::is_success(response.status) {
                return Err(format!("read permission rules: HTTP {}", response.status));
            }
            response
                .json::<Value>()
                .map_err(|error| format!("decode permission rules: {error}"))
        };
        let session = get(Self::session_path(session_id, "/permission-rules")).await?;
        let persistent = get("/permission-rules?scope=all".to_string()).await?;
        Ok(permission_rule_pairs(&session, &persistent))
    }

    /// Install an allow rule on the live session, then durably for the project.
    pub(crate) async fn install_permission_rule(
        &self,
        session_id: &str,
        pattern: &str,
        workspace_root: &str,
    ) -> Result<(), String> {
        let post = |path: String, body: Value| async move {
            let response = self
                .send(self.req_json(DaemonVerb::Post, &path, &body))
                .await
                .map_err(|error| format!("install permission rule: {error}"))?;
            if rightkit_agent_daemon::http_client::is_success(response.status) {
                Ok(())
            } else {
                Err(format!("install permission rule: HTTP {}", response.status))
            }
        };
        post(
            Self::session_path(session_id, "/permission-rules"),
            json!({"pattern": pattern, "effect": "allow"}),
        )
        .await?;
        post(
            "/permission-rules".to_string(),
            json!({"scope": "project", "workspaceRoot": workspace_root, "pattern": pattern, "effect": "allow"}),
        ).await
    }

    pub(crate) async fn resolve_credential_request(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<(), String> {
        self.respond_interaction(session_id, "/credential-request", body)
            .await
    }

    pub(crate) async fn resolve_mcp_elicitation(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<(), String> {
        self.respond_interaction(session_id, "/resolve-elicitation", body)
            .await
    }

    /// Fetch the agent roster for a session (`GET /sessions/{id}/agents`).
    /// Returns the flat `AgentRosterRow` list the daemon tracks for that
    /// session: each row names `subSessionId`, `role`, `status`, `model`,
    /// `elapsedMs`, `tokensIn/Out`, `cacheRead`, `toolUses`, `lastTools`.
    /// The TUI builds its agent tree by joining these to the session rows.
    pub(crate) async fn get_session_agents(&self, session_id: &str) -> Result<Vec<Value>, String> {
        let response = self
            .send(self.req(DaemonVerb::Get, &Self::session_path(session_id, "/agents")))
            .await
            .map_err(|error| format!("get daemon session agents: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!(
                "get daemon session agents: HTTP {}",
                response.status
            ));
        }
        response
            .json::<Vec<Value>>()
            .map_err(|error| format!("decode daemon session agents: {error}"))
    }

    pub(crate) async fn get_session_tasks(&self, session_id: &str) -> Result<Vec<Value>, String> {
        let response = self
            .send(self.req(DaemonVerb::Get, &Self::session_path(session_id, "/tasks")))
            .await
            .map_err(|error| format!("get daemon session tasks: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!(
                "get daemon session tasks: HTTP {}",
                response.status
            ));
        }
        let body = response
            .json::<Value>()
            .map_err(|error| format!("decode daemon session tasks: {error}"))?;
        Ok(body
            .get("tasks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) async fn list_execution_processes(&self) -> Result<Vec<Value>, String> {
        let response = self
            .send(self.req(DaemonVerb::Get, "/execution/processes"))
            .await
            .map_err(|error| format!("list daemon processes: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("list daemon processes: HTTP {}", response.status));
        }
        let body = response
            .json::<Value>()
            .map_err(|error| format!("decode daemon processes: {error}"))?;
        Ok(body
            .get("processes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) async fn execution_process_logs(&self, id: &str) -> Result<String, String> {
        let response = self
            .send(self.req(
                DaemonVerb::Get,
                &format!("/execution/processes/{}/logs", url_encode_component(id)),
            ))
            .await
            .map_err(|error| format!("read daemon process output: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!(
                "read daemon process output: HTTP {}",
                response.status
            ));
        }
        let body = response
            .json::<Value>()
            .map_err(|error| format!("decode daemon process output: {error}"))?;
        Ok(body
            .pointer("/logs/output")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// A070: `POST /execution/processes/{id}/attach` -> `(epoch, next input sequence)`.
    pub(crate) async fn attach_execution_process(&self, id: &str) -> Result<(u64, u64), String> {
        let response = self
            .send(self.req(
                DaemonVerb::Post,
                &format!("/execution/processes/{}/attach", url_encode_component(id)),
            ))
            .await
            .map_err(|error| format!("attach daemon process: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("attach daemon process: HTTP {}", response.status));
        }
        let body = response
            .json::<Value>()
            .map_err(|error| format!("decode daemon attach: {error}"))?;
        let epoch = body
            .get("leaseEpoch")
            .and_then(Value::as_u64)
            .ok_or("daemon attach omitted leaseEpoch")?;
        let next = body
            .get("nextInputSequence")
            .and_then(Value::as_u64)
            .ok_or("daemon attach omitted nextInputSequence")?;
        Ok((epoch, next))
    }

    pub(crate) async fn write_execution_process_stdin(
        &self,
        id: &str,
        input: &str,
        epoch: u64,
        sequence: u64,
    ) -> Result<(), String> {
        let response = self.send(self.req_json(
            DaemonVerb::Post,
            &format!("/execution/processes/{}/stdin", url_encode_component(id)),
            &serde_json::json!({ "input": input, "leaseEpoch": epoch, "inputSequence": sequence }),
        ))
            .await.map_err(|error| format!("write daemon process input: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!(
                "write daemon process input: HTTP {}",
                response.status
            ));
        }
        Ok(())
    }

    pub(crate) async fn resize_execution_process(
        &self,
        id: &str,
        rows: u16,
        cols: u16,
    ) -> Result<(), String> {
        let response = self
            .send(self.req_json(
                DaemonVerb::Post,
                &format!("/execution/processes/{}/resize", url_encode_component(id)),
                &serde_json::json!({ "rows": rows, "cols": cols }),
            ))
            .await
            .map_err(|error| format!("resize daemon process: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("resize daemon process: HTTP {}", response.status));
        }
        Ok(())
    }

    pub(crate) async fn detach_execution_process(&self, id: &str) -> Result<(), String> {
        let response = self
            .send(self.req(
                DaemonVerb::Post,
                &format!("/execution/processes/{}/detach", url_encode_component(id)),
            ))
            .await
            .map_err(|error| format!("detach daemon process: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("detach daemon process: HTTP {}", response.status));
        }
        Ok(())
    }

    pub(crate) async fn stop_execution_process(&self, id: &str) -> Result<(), String> {
        let response = self
            .send(self.req(
                DaemonVerb::Post,
                &format!("/execution/processes/{}/kill", url_encode_component(id)),
            ))
            .await
            .map_err(|error| format!("stop daemon process: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("stop daemon process: HTTP {}", response.status));
        }
        Ok(())
    }

    pub(crate) async fn cancel_session_task(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(), String> {
        let path = format!("/tasks/{}/cancel", url_encode_component(task_id));
        let response = self
            .send(self.req(DaemonVerb::Post, &Self::session_path(session_id, &path)))
            .await
            .map_err(|error| format!("cancel daemon task: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("cancel daemon task: HTTP {}", response.status));
        }
        Ok(())
    }

    /// Execute a slash command through the daemon's own surface
    /// (`POST /commands/execute`). The daemon resolves names and aliases
    /// through the shared `COMMAND_REGISTRY` (P1-1), so the TUI host can run
    /// every registered command without re-implementing executors per surface.
    /// Returns the daemon's `CommandResult` JSON (`success`, `output`, `action`).
    pub(crate) async fn execute_slash(
        &self,
        input: &str,
        active_session_id: Option<&str>,
    ) -> Result<Value, String> {
        let response = self
            .send(self.req_json(
                DaemonVerb::Post,
                "/commands/execute",
                &json!({
                    "input": input,
                    "sessionId": active_session_id,
                }),
            ))
            .await
            .map_err(|error| format!("execute daemon slash command: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!(
                "execute daemon slash command: HTTP {}",
                response.status
            ));
        }
        response
            .json()
            .map_err(|error| format!("decode daemon slash result: {error}"))
    }
}

fn session_conflict_preserves_safe_mode(requested: &Value, existing: &Value) -> bool {
    !requested
        .get("safeMode")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || existing
            .get("safeMode")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// The daemon resolves paths against its own cwd, so CLI-relative paths cross
/// the boundary absolute.
fn absolutize_cli_path(path: &str) -> String {
    std::path::absolute(path)
        .map(|absolute| absolute.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn daemon_session_create_body(args: &RunArgs, session_id: &str, workspace_root: &str) -> Value {
    // A headless run tells the session nobody is attached (after the user's own appended text).
    let append_system_prompt = if args.unattended {
        Some(match args.append_system_prompt.as_deref() {
            Some(user) => format!("{user}\n\n{}", crate::main_runtime::UNATTENDED_RUN_NOTE),
            None => crate::main_runtime::UNATTENDED_RUN_NOTE.to_string(),
        })
    } else {
        args.append_system_prompt.clone()
    };
    let permission_mode = if args.safe_mode {
        "ask"
    } else if let Some(mode) = args.permission_mode {
        // An explicit session mode (`--permission-mode`, launch flag, slash
        // command, Shift+Tab); `--yes` never selects one.
        mode.wire_name()
    } else {
        "ask"
    };
    let toolset = if args.safe_mode {
        Some("read-only")
    } else {
        args.toolset.as_deref()
    };
    // Explicit route identity crosses the daemon boundary verbatim: absent
    // means "daemon resolves" (configured default/provider routes win), so a
    // bare run must serialize `providerId`/`effort` as null rather than
    // inventing a provider.
    let provider_id = args
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let effort = args
        .effort
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut disallowed_tools = args.disallowed_tools.clone();
    if args.no_mcp && !disallowed_tools.iter().any(|tool| tool == "mcp_call") {
        disallowed_tools.push("mcp_call".to_string());
    }

    let mut body = json!({
        "id": session_id,
        "title": "CLI session",
        "workspaceRoot": workspace_root,
        "model": args.model,
        "providerId": provider_id,
        "effort": effort,
        "allowedTools": args.allowed_tools,
        "disallowedTools": disallowed_tools,
        "maxIterations": args.max_turns,
        "budgetCeilingUsd": args.max_budget_usd,
        "systemPrompt": args.system_prompt,
        "appendSystemPrompt": append_system_prompt,
        "outputSchema": output_schema_value(args.output_schema.as_deref()),
        "bare": args.bare,
        "noMcp": args.no_mcp,
        "addDir": args.add_dir.iter().map(|dir| absolutize_cli_path(dir)).collect::<Vec<_>>(),
        "mcpConfig": args.mcp_config.as_deref().map(absolutize_cli_path),
        "toolset": toolset,
        "ownToolset": args.own_toolset,
        "safeMode": args.safe_mode,
        "permissionMode": permission_mode,
        "mcpChannel": "cli",
    });
    // Per-session grant for this session's workspace only, sent solely when the
    // user passed `--trust-workspace`. The daemon must honour it at create only
    // and never persist it (issue 445).
    if args.trust_workspace {
        body["trustWorkspace"] = Value::Bool(true);
    }
    body
}

fn daemon_event_seq(event: &Value) -> i64 {
    event.get("seq").and_then(Value::as_i64).unwrap_or(0)
}

/// Event kind at the raw `Value` level — the discriminant both terminal
/// detection and stream-resync recognition read. Kept off `ProtocolEvent`
/// deliberately (preserve-list item 4): `daemon_event_is_terminal` and
/// `daemon_event_seq` operate on the raw payload so a per-event parse
/// failure (item 2) never blocks terminal/cursor tracking.
fn daemon_event_kind(event: &Value) -> Option<&str> {
    event
        .get("$kind")
        .and_then(Value::as_str)
        .or_else(|| event.get("type").and_then(Value::as_str))
}

// A turn ends on `sessionTurnFinished` (issue 450), never on a completion.
fn daemon_event_is_terminal(event: &Value) -> bool {
    matches!(
        daemon_event_kind(event),
        Some(
            "sessionTurnFinished"
                | "streamResyncRequired"
                | "overloaded"
                | "runFailed"
                | "runInterrupted"
                | "budgetExceeded"
        )
    )
}

/// Flatten the session-rule and persistent-rule listings into
/// `(pattern, effect)` pairs. Rows that lack either field are skipped.
pub(crate) fn permission_rule_pairs(session: &Value, persistent: &Value) -> Vec<(String, String)> {
    let row = |row: &Value| {
        Some((
            row.get("pattern")?.as_str()?.to_string(),
            row.get("effect")?.as_str()?.to_string(),
        ))
    };
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(rows) = session.get("rules").and_then(Value::as_array) {
        out.extend(rows.iter().filter_map(row));
    }
    if let Some(rows) = persistent.get("user").and_then(Value::as_array) {
        out.extend(rows.iter().filter_map(row));
    }
    if let Some(projects) = persistent.get("projects").and_then(Value::as_object) {
        for rows in projects.values().filter_map(Value::as_array) {
            out.extend(rows.iter().filter_map(row));
        }
    }
    out
}

/// The rewind points of a `GET .../rewind/points` body.
pub(crate) fn parse_rewind_points(
    body: &Value,
) -> Result<Vec<crate::api_sessions::rewind::RewindPoint>, String> {
    let rows = body
        .as_array()
        .ok_or_else(|| "decode rewind points: not a list".to_string())?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            Some(crate::api_sessions::rewind::RewindPoint {
                text: row.get("text")?.as_str()?.to_string(),
                nth_from_end: usize::try_from(row.get("nthFromEnd")?.as_u64()?).ok()?,
                created_at: row
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}

/// The report of a rewind or redo reply.
pub(crate) fn parse_rewind_report(body: &Value) -> crate::api_sessions::rewind::RewindReport {
    crate::api_sessions::rewind::RewindReport {
        text: body
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        conversation_rewound: body
            .get("conversationRewound")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        files_restored: body
            .get("filesRestored")
            .and_then(Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| path.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        file_conflicts: body
            .get("fileConflicts")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some((
                            row.get("path")?.as_str()?.to_string(),
                            row.get("reason")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        background_shells_running: body
            .get("backgroundShellsRunning")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some((
                            row.get("shellId")?.as_str()?.to_string(),
                            row.get("command")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// What to show for a refused redo: the 409 body's `message`.
pub(crate) fn parse_redo_refusal(status: u16, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("redo session: HTTP {status}"))
}
