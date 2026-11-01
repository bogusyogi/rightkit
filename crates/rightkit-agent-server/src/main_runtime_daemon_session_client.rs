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
        let http = rightkit_http::http_client::daemon_client(&base_url)
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
            return Err(format!("read hooks status: HTTP {}", response.status));
        }
        response
            .json::<Value>()
            .map_err(|error| format!("decode hooks status: {error}"))
    }

    fn session_path(session_id: &str, suffix: &str) -> String {
        format!("/sessions/{}{}", url_encode_component(session_id), suffix)
    }

    pub(crate) async fn ensure_session(&self, session_id: &str, body: Value) -> Result<(), String> {
        let response = self
            .send(self.req_json(DaemonVerb::Post, "/sessions", &body))
            .await
            .map_err(|error| format!("create daemon session: {error}"))?;
        if rightkit_http::http_client::is_success(response.status) {
            return Ok(());
        }
        if response.status == 409 {
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
        if !rightkit_http::http_client::is_success(response.status) {
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

    pub(crate) async fn submit_message(
        &self,
        session_id: &str,
        prompt: &str,
        workspace_root: &str,
    ) -> Result<i64, String> {
        self.submit_message_with_attachments(session_id, prompt, workspace_root, None, None)
            .await
    }

    pub(crate) async fn submit_message_with_attachments(
        &self,
        session_id: &str,
        prompt: &str,
        workspace_root: &str,
        attachments: Option<Vec<crate::api::Attachment>>,
        sandbox_grant_prompts: Option<bool>,
    ) -> Result<i64, String> {
        let response = self
            .send(self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/messages"),
                &json!({ "text": prompt, "workspaceRoot": workspace_root, "attachments": attachments, "sandboxGrantPrompts": sandbox_grant_prompts }),
            ))
            .await
            .map_err(|error| format!("submit daemon message: {error}"))?;
        if !rightkit_http::http_client::is_success(response.status) {
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
            if !rightkit_http::http_client::is_success(status) {
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
        let response = self
            .send(self.req(
                DaemonVerb::Get,
                &format!(
                    "{}?after={after}",
                    Self::session_path(session_id, "/events/history")
                ),
            ))
            .await
            .map_err(|error| format!("read daemon event history: {error}"))?;
        if !rightkit_http::http_client::is_success(response.status) {
            return Err(format!(
                "read daemon event history: HTTP {}",
                response.status
            ));
        }
        response
            .json()
            .map_err(|error| format!("decode daemon event history: {error}"))
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if rightkit_http::http_client::is_success(response.status) {
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
        if rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
            return Err(format!("goal {op}: HTTP {}", response.status));
        }
        response
            .json::<Value>()
            .map_err(|error| format!("goal {op}: {error}"))
    }

    pub(crate) async fn resolve_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        approved: bool,
    ) -> Result<(), String> {
        let response = self
            .send(self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/tool-approval"),
                &json!({ "toolCallId": tool_call_id, "approved": approved }),
            ))
            .await
            .map_err(|error| format!("resolve daemon approval: {error}"))?;
        if rightkit_http::http_client::is_success(response.status) {
            Ok(())
        } else {
            Err(format!("resolve daemon approval: HTTP {}", response.status))
        }
    }

    async fn post_empty(&self, session_id: &str, suffix: &str, action: &str) -> Result<(), String> {
        let response = self
            .send(self.req(DaemonVerb::Post, &Self::session_path(session_id, suffix)))
            .await
            .map_err(|error| format!("{action}: {error}"))?;
        if rightkit_http::http_client::is_success(response.status) {
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
        if rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
            "clone" => self.req_json(
                DaemonVerb::Post,
                &Self::session_path(session_id, "/clone"),
                &serde_json::json!({"title": value}),
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
            if !rightkit_http::http_client::is_success(response.status) {
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
            if rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
        if !rightkit_http::http_client::is_success(response.status) {
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
    let permission_mode = if args.safe_mode {
        "ask"
    } else if let Some(mode) = args.permission_mode {
        // An explicit session mode (launch flag, slash command, Shift+Tab)
        // supersedes the legacy auto-approve / bypass booleans.
        mode.wire_name()
    } else if args.bypass_approvals {
        "bypass"
    } else if args.auto_approve {
        "accept-edits"
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

    json!({
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
        "appendSystemPrompt": args.append_system_prompt,
        "bare": args.bare,
        "noMcp": args.no_mcp,
        "addDir": args.add_dir.iter().map(|dir| absolutize_cli_path(dir)).collect::<Vec<_>>(),
        "mcpConfig": args.mcp_config.as_deref().map(absolutize_cli_path),
        "toolset": toolset,
        "safeMode": args.safe_mode,
        "permissionMode": permission_mode,
        "mcpChannel": "cli",
    })
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

fn daemon_event_is_terminal(event: &Value) -> bool {
    matches!(
        daemon_event_kind(event),
        Some(
            "assistantMessageCompleted"
                | "sessionTurnFinished"
                | "streamResyncRequired"
                | "overloaded"
                | "runFailed"
                | "runInterrupted"
                | "budgetExceeded"
        )
    )
}

#[cfg(test)]
mod daemon_session_client_tests {
    use super::run_session_unit_tests::output_mode_args;
    use super::*;

    #[test]
    fn create_body_keeps_client_configuration_at_daemon_boundary() {
        let args = RunArgs {
            context_mode: ContextMode::Membrane,
            prompt: "test".into(),
            model: Some("model-x".into()),
            provider: Some("openrouter".into()),
            effort: None,
            workspace: None,
            print: false,
            output_format: OutputFormat::Text,
            json_event: false,
            auto_approve: true,
            bypass_approvals: false,
            permission_mode: None,
            stream_json: false,
            exit_on_error: false,
            max_turns: None,
            allowed_tools: vec!["read_file".into()],
            disallowed_tools: vec!["shell".into()],
            toolset: Some("dev-safe".into()),
            max_budget_usd: None,
            bare: false,
            safe_mode: false,
            system_prompt: None,
            system_prompt_file: None,
            append_system_prompt: None,
            append_system_prompt_file: None,
            mcp_config: None,
            session_id: None,
            add_dir: vec![],
            input_format: InputFormat::Text,
            image: Vec::new(),
            ephemeral: false,
            output_last_message: None,
            color: crate::cli::CliColor::Auto,
            tools: Vec::new(),
            exclude_tools: Vec::new(),
            no_mcp: false,
            output_schema: None,
        };
        let body = daemon_session_create_body(&args, "s-1", "/work");
        assert_eq!(body["id"], "s-1");
        assert_eq!(body["workspaceRoot"], "/work");
        assert_eq!(body["permissionMode"], "accept-edits");
        assert_eq!(body["allowedTools"], json!(["read_file"]));
        assert_eq!(body["providerId"], "openrouter");
        assert!(body["effort"].is_null());
    }

    #[test]
    fn create_body_carries_run_limits_prompts_and_roots_to_daemon() {
        let mut args = output_mode_args();
        args.max_turns = Some(7);
        args.max_budget_usd = Some(1.5);
        args.system_prompt = Some("replace".into());
        args.append_system_prompt = Some("append".into());
        args.bare = true;
        args.add_dir = vec!["extra".into()];
        args.mcp_config = Some("mcp.json".into());

        let body = daemon_session_create_body(&args, "s-flags", "/work");

        assert_eq!(body["maxIterations"], 7);
        assert_eq!(body["budgetCeilingUsd"], 1.5);
        assert_eq!(body["systemPrompt"], "replace");
        assert_eq!(body["appendSystemPrompt"], "append");
        assert_eq!(body["bare"], true);
        assert!(std::path::Path::new(body["addDir"][0].as_str().unwrap()).is_absolute());
        assert!(std::path::Path::new(body["mcpConfig"].as_str().unwrap()).is_absolute());
    }

    #[test]
    fn create_body_omits_provider_pin_when_flag_absent() {
        let mut args = output_mode_args();
        args.provider = None;
        args.effort = Some("high".into());

        let body = daemon_session_create_body(&args, "s-2", "/work");

        assert!(
            body["providerId"].is_null(),
            "no --provider flag must not pin a provider at the daemon boundary"
        );
        assert_eq!(body["effort"], "high");
        assert_eq!(body["workspaceRoot"], "/work");
    }

    #[test]
    fn explicit_permission_mode_drives_create_body_and_safe_mode_still_wins() {
        use rightkit_agent_policy::PermissionMode;
        let mut args = output_mode_args();
        for (mode, wire) in [
            (PermissionMode::Ask, "ask"),
            (PermissionMode::AcceptEdits, "accept-edits"),
            (PermissionMode::AutoSafe, "auto-safe"),
            (PermissionMode::Bypass, "bypass"),
        ] {
            args.permission_mode = Some(mode);
            let body = daemon_session_create_body(&args, "s-mode", "/work");
            assert_eq!(body["permissionMode"], wire);
            assert_eq!(
                rightkit_agent_policy::parse_permission_mode(body["permissionMode"].as_str()),
                mode
            );
        }
        args.safe_mode = true;
        let body = daemon_session_create_body(&args, "s-mode", "/work");
        assert_eq!(body["permissionMode"], "ask");
    }

    #[test]
    fn safe_mode_create_body_forces_read_only_toolset() {
        let mut args = output_mode_args();
        args.safe_mode = true;
        args.toolset = Some("desktop-full".into());
        args.allowed_tools = vec!["read_file".into(), "shell".into()];

        let body = daemon_session_create_body(&args, "s-safe", "/work");

        assert_eq!(body["toolset"], "read-only");
        assert_eq!(body["safeMode"], true);
        assert_eq!(body["permissionMode"], "ask");
    }

    #[test]
    fn safe_mode_conflict_rejects_unrestricted_session_reuse() {
        let safe_request = json!({"safeMode": true});
        assert!(!session_conflict_preserves_safe_mode(
            &safe_request,
            &json!({"id": "existing", "safeMode": false})
        ));
        assert!(session_conflict_preserves_safe_mode(
            &safe_request,
            &json!({"id": "existing", "safeMode": true})
        ));
        assert!(session_conflict_preserves_safe_mode(
            &json!({"safeMode": false}),
            &json!({"id": "existing", "safeMode": false})
        ));
    }

    #[test]
    fn terminal_detection_accepts_only_turn_closing_events() {
        assert!(daemon_event_is_terminal(
            &json!({ "$kind": "assistantMessageCompleted" })
        ));
        assert!(daemon_event_is_terminal(&json!({ "$kind": "runFailed" })));
        assert!(!daemon_event_is_terminal(
            &json!({ "$kind": "assistantDelta" })
        ));
    }

    // Preserve-list item 3: `streamResyncRequired` must stay in the terminal
    // set — it is what drives the catch-up path in `run_session`, and a
    // silent drop here would make the resync event loop forever instead of
    // triggering history catch-up.
    #[test]
    fn terminal_detection_accepts_stream_resync_required() {
        assert!(daemon_event_is_terminal(
            &json!({ "$kind": "streamResyncRequired" })
        ));
    }

    // Preserve-list item 1: the initial cursor is the submit response's
    // `seq`. Losing this re-replays the user's own message once the stream
    // is opened at 0 instead of at the submitted sequence.
    #[tokio::test]
    async fn submit_message_returns_seq_as_initial_stream_cursor() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/sessions/s-1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"seq": 42}"#)
            .create_async()
            .await;
        let client = DaemonSessionClient::new(server.url(), "tok".into());
        let seq = client
            .submit_message("s-1", "hello", "/work")
            .await
            .expect("submit should succeed");
        mock.assert_async().await;
        assert_eq!(seq, 42, "initial cursor must be the submit response's seq");
    }

    // Resync catch-up pages /events/replay with after=next until complete and
    // returns the events in order.
    #[tokio::test]
    async fn events_after_pages_replay_until_complete() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/sessions/s-1/events/replay")
            .match_query(mockito::Matcher::UrlEncoded("after".into(), "5".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"events":[{"$kind":"a","seq":6},{"$kind":"a","seq":7}],"next":7,"complete":false,"highWaterMark":9}"#,
            )
            .create_async()
            .await;
        let second = server
            .mock("GET", "/sessions/s-1/events/replay")
            .match_query(mockito::Matcher::UrlEncoded("after".into(), "7".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"events":[{"$kind":"a","seq":8},{"$kind":"a","seq":9}],"next":9,"complete":true,"highWaterMark":9}"#,
            )
            .create_async()
            .await;
        let client = DaemonSessionClient::new(server.url(), "tok".into());
        let events = client
            .events_after("s-1", 5)
            .await
            .expect("replay paging should succeed");
        first.assert_async().await;
        second.assert_async().await;
        let seqs: Vec<i64> = events
            .iter()
            .map(|event| event["seq"].as_i64().unwrap())
            .collect();
        assert_eq!(seqs, vec![6, 7, 8, 9]);
    }

    // 4a/4c coverage: the hand-rolled SSE reader must yield each `data:`
    // frame as a parsed event, in order, and report a clean end via
    // `Ok(None)` rather than an error.
    #[tokio::test]
    async fn event_stream_parses_sequential_sse_frames_in_order() {
        let mut server = mockito::Server::new_async().await;
        let body = "data: {\"$kind\":\"assistantDelta\",\"seq\":1}\n\n\
                     data: {\"$kind\":\"sessionTurnFinished\",\"seq\":2}\n\n";
        let mock = server
            .mock("GET", "/sessions/s-1/events")
            .match_query(mockito::Matcher::UrlEncoded("after".into(), "0".into()))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let client = DaemonSessionClient::new(server.url(), "tok".into());
        let mut stream = client
            .open_event_stream("s-1", 0)
            .await
            .expect("stream should open");
        let first = stream
            .next_event()
            .await
            .expect("first event should parse")
            .expect("first event should be present");
        assert_eq!(first["seq"], 1);
        let second = stream
            .next_event()
            .await
            .expect("second event should parse")
            .expect("second event should be present");
        assert_eq!(second["seq"], 2);
        let end = stream.next_event().await.expect("clean end, not an error");
        assert!(
            end.is_none(),
            "stream should end cleanly with no more frames"
        );
        mock.assert_async().await;
    }

    // CR-04: exercise `DaemonEventStream` directly against a hand-fed byte
    // stream so chunk boundaries can be placed exactly where each test
    // needs them — mockito serves a response body as one chunk, which
    // can't reproduce a mid-codepoint or mid-delimiter transport split.
    fn stream_of_chunks(chunks: Vec<&[u8]>) -> DaemonEventStream {
        let mut decoder = rightkit_http::SseDecoder::with_limits(MAX_FRAME_BYTES, MAX_FRAME_BYTES);
        let mut items: Vec<Result<rightkit_http::SseEvent, rightkit_http::HttpError>> = Vec::new();
        for chunk in chunks {
            match decoder.push(chunk) {
                Ok(events) => items.extend(events.into_iter().map(Ok)),
                Err(error) => {
                    items.push(Err(error));
                    break;
                }
            }
        }
        if !matches!(items.last(), Some(Err(_))) {
            match decoder.finish() {
                Ok(Some(event)) => items.push(Ok(event)),
                Ok(None) => {}
                Err(error) => items.push(Err(error)),
            }
        }
        DaemonEventStream {
            body: Box::pin(futures_util::stream::iter(items)),
        }
    }

    // A multibyte UTF-8 code point ('é' = 0xC3 0xA9) split exactly between
    // two chunks must reassemble correctly, not get replaced with U+FFFD by
    // per-chunk lossy decoding.
    #[tokio::test]
    async fn event_stream_reassembles_utf8_codepoint_split_across_chunks() {
        let full = b"data: {\"$kind\":\"assistantDelta\",\"text\":\"caf\xc3\xa9\"}\n\n";
        let split_at = full.iter().position(|&b| b == 0xc3).unwrap() + 1; // cut mid-codepoint
        let mut stream = stream_of_chunks(vec![&full[..split_at], &full[split_at..]]);
        let event = stream
            .next_event()
            .await
            .expect("split codepoint must still decode")
            .expect("event should be present");
        assert_eq!(event["text"], "café");
    }

    // The `\n\n` delimiter itself split across two chunks must still be
    // recognised as a frame boundary once both halves have arrived.
    #[tokio::test]
    async fn event_stream_recognises_delimiter_split_across_chunks() {
        let mut stream = stream_of_chunks(vec![
            b"data: {\"$kind\":\"assistantDelta\",\"seq\":1}\n",
            b"\ndata: {\"$kind\":\"sessionTurnFinished\",\"seq\":2}\n\n",
        ]);
        let first = stream
            .next_event()
            .await
            .expect("first event should parse")
            .expect("first event should be present");
        assert_eq!(first["seq"], 1);
        let second = stream
            .next_event()
            .await
            .expect("second event should parse")
            .expect("second event should be present");
        assert_eq!(second["seq"], 2);
    }

    // CRLF-terminated frames (`\r\n\r\n`) must parse identically to LF, and
    // a per-line trailing `\r` must be stripped before the `data:` prefix
    // check and JSON parse.
    #[tokio::test]
    async fn event_stream_handles_crlf_terminated_frames() {
        let mut stream = stream_of_chunks(vec![
            b"data: {\"$kind\":\"assistantDelta\",\"seq\":1}\r\n\r\n",
        ]);
        let event = stream
            .next_event()
            .await
            .expect("CRLF frame should parse")
            .expect("event should be present");
        assert_eq!(event["seq"], 1);
    }

    // A `:`-prefixed SSE comment line (commonly used for keep-alives) has
    // no `data:` line and must be skipped as wire noise rather than
    // surfaced as an event or an error.
    #[tokio::test]
    async fn event_stream_skips_comment_only_frames() {
        let mut stream = stream_of_chunks(vec![
            b": keep-alive\n\ndata: {\"$kind\":\"assistantDelta\",\"seq\":1}\n\n",
        ]);
        let event = stream
            .next_event()
            .await
            .expect("comment frame must be skipped, not surfaced as an error")
            .expect("the real event after the comment must still arrive");
        assert_eq!(event["seq"], 1);
    }

    // Multiple `data:` lines inside one frame are joined with `\n` before
    // JSON parsing, per the SSE multiline-data convention.
    #[tokio::test]
    async fn event_stream_joins_multiline_data_fields() {
        let mut stream = stream_of_chunks(vec![b"data: {\"a\":\ndata: 1}\n\n"]);
        let event = stream
            .next_event()
            .await
            .expect("joined multiline data must parse as one JSON value")
            .expect("event should be present");
        assert_eq!(event["a"], 1);
    }

    // A stream that ends (clean EOF) right after a complete frame with no
    // trailing blank line must still deliver that frame — not discard it —
    // and only then report `Ok(None)`.
    #[tokio::test]
    async fn event_stream_delivers_trailing_frame_at_eof() {
        let mut stream = stream_of_chunks(vec![b"data: {\"$kind\":\"runFailed\",\"seq\":9}"]);
        let event = stream
            .next_event()
            .await
            .expect("trailing unterminated frame must still parse")
            .expect("event should be present, not dropped at EOF");
        assert_eq!(event["seq"], 9);
        let end = stream
            .next_event()
            .await
            .expect("clean end after EOF frame");
        assert!(end.is_none());
    }

    // An unterminated frame that grows past `MAX_FRAME_BYTES` without a
    // delimiter must fail with a reconnectable `Err`, never buffer without
    // bound.
    #[tokio::test]
    async fn event_stream_rejects_oversized_unterminated_frame() {
        let oversized = vec![b'x'; MAX_FRAME_BYTES + 1];
        let mut stream = stream_of_chunks(vec![b"data: ", &oversized]);
        let result = stream.next_event().await;
        assert!(
            result.is_err(),
            "an unbounded, never-terminated frame must be rejected, not buffered forever"
        );
    }
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

#[cfg(test)]
mod permission_rule_pair_tests {
    use super::*;

    #[test]
    fn pairs_cover_session_user_and_project_rules() {
        let session =
            json!({"rules": [{"pattern": "shell(ls)", "effect": "allow", "source": "session"}]});
        let persistent = json!({
            "user": [{"pattern": "shell(rm *)", "effect": "deny"}],
            "projects": {"D:/repo": [{"pattern": "file_edit(a)", "effect": "deny"}, {"bad": 1}]}
        });
        assert_eq!(
            permission_rule_pairs(&session, &persistent),
            vec![
                ("shell(ls)".to_string(), "allow".to_string()),
                ("shell(rm *)".to_string(), "deny".to_string()),
                ("file_edit(a)".to_string(), "deny".to_string()),
            ]
        );
    }
}
