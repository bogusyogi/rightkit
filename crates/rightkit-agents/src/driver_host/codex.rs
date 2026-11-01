// RIGHTKIT-SERIOUS: rightkit-agents 0.2.0 `codex::parse_line` cannot replace `normalize_notification`: it treats `mcpServer/startupStatus/updated` as bookkeeping, so the the host app MCP gateway readiness that releases a Codex run (`GatewayReady`) and its startup-failure refusal are not derivable; it also drops turn failure text, tool-result content and the `account/rateLimits/updated` windows (report T-5). Usage-limit detection below is RightKit's.
use crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS;
use crate::driver_host::{
    event_key, invalid, stable_event_key, validate_model_id, DriverHostError, DriverKind,
    LaunchSpec, NormalizedEvent, NormalizedEventKind,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexLaunchOptions {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub strict_config: bool,
    /// Optional caller-selected model id, e.g. `"o3"`. Emitted as `-c model="<id>"` *before* the
    /// `app-server` subcommand per `codex app-server --help`. Validated against
    /// `validate_model_id` before it is interpolated into the argv.
    pub model: Option<String>,
}

impl CodexLaunchOptions {
    pub fn launch_spec(&self) -> Result<LaunchSpec, DriverHostError> {
        let mut args = Vec::new();
        if let Some(model) = self.model.as_deref() {
            let model =
                validate_model_id(model).map_err(|reason| invalid(DriverKind::Codex, reason))?;
            args.push("-c".into());
            args.push(format!("model=\"{model}\""));
        }
        args.push("app-server".into());
        args.push("--stdio".into());
        if self.strict_config {
            args.push("--strict-config".into());
        }
        Ok(LaunchSpec {
            program: self.executable.clone(),
            args,
            env: BTreeMap::new(),
            env_remove: FORBIDDEN_BYOK_ENV_VARS
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            cwd: self.cwd.clone(),
        })
    }
}

/// Credential variables a selected Codex account must not inherit from the daemon environment.
const CODEX_ACCOUNT_STRIPPED_ENV: [&str; 2] = ["OPENAI_API_KEY", "CODEX_API_KEY"];

/// Apply the selected account to a Codex launch spec; the Codex twin of
/// `apply_claude_account_env` and it reuses the same `ClaudeAccountLaunch` selection type.
/// `None` leaves default behaviour untouched.
///
/// - subscription: ambient API-key variables are stripped so the profile's `codex login` auth
///   (reached through the run-scoped `CODEX_HOME`, which the caller seeds from the profile's
///   managed home) is the only credential in play.
/// - api key: the key is set on this spec only.
pub fn apply_codex_account_env(
    spec: &mut LaunchSpec,
    account: Option<&crate::driver_host::ClaudeAccountLaunch>,
) {
    let Some(account) = account else { return };
    match account {
        crate::driver_host::ClaudeAccountLaunch::Subscription { .. } => {
            for name in CODEX_ACCOUNT_STRIPPED_ENV {
                spec.env.remove(name);
                spec.env_remove.insert(name.to_string());
            }
        }
        crate::driver_host::ClaudeAccountLaunch::ApiKey { key } => {
            for name in CODEX_ACCOUNT_STRIPPED_ENV {
                spec.env_remove.remove(name);
            }
            spec.env.insert("OPENAI_API_KEY".into(), key.clone());
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CodexMessage {
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<Value>,
    },
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
    Notification(NormalizedEvent),
}

pub fn parse_codex_jsonrpc_line(
    line: &str,
    source_offset: u64,
) -> Result<Option<CodexMessage>, DriverHostError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| invalid(DriverKind::Codex, error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid(DriverKind::Codex, "root must be an object"))?;
    let id = object.get("id").cloned();
    let method = object.get("method").and_then(Value::as_str);

    match (id, method) {
        (Some(id), None) => Ok(Some(CodexMessage::Response {
            id,
            result: object.get("result").cloned(),
            error: object.get("error").cloned(),
        })),
        (Some(id), Some(method)) => {
            if !is_known_server_request(method) {
                return Err(DriverHostError::UnknownRequiredEvent {
                    driver: DriverKind::Codex,
                    method: method.into(),
                });
            }
            Ok(Some(CodexMessage::ServerRequest {
                id,
                method: method.into(),
                params: object.get("params").cloned().unwrap_or(Value::Null),
            }))
        }
        (None, Some(method)) => normalize_notification(method, object.get("params"), source_offset)
            .map(|event| {
                event.map(|mut event| {
                    event.event_key =
                        stable_event_key(DriverKind::Codex, method, &value, source_offset);
                    CodexMessage::Notification(event)
                })
            }),
        (None, None) => Err(invalid(DriverKind::Codex, "missing id and method")),
    }
}

fn is_known_server_request(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "mcpServer/elicitation/request"
    )
}

fn normalize_notification(
    method: &str,
    params: Option<&Value>,
    source_offset: u64,
) -> Result<Option<NormalizedEvent>, DriverHostError> {
    let params = params.cloned().unwrap_or(Value::Null);
    let kind = match method {
        "thread/started" => NormalizedEventKind::SessionStarted {
            vendor_session_id: required_string(&params, &["thread", "id"], "thread.id")?,
            init: None,
        },
        "turn/started" => NormalizedEventKind::TurnStarted {
            vendor_turn_id: required_string(&params, &["turn", "id"], "turn.id")?,
        },
        "item/agentMessage/delta" => NormalizedEventKind::AssistantDelta {
            text: required_string(&params, &["delta"], "delta")?,
        },
        "item/started" => return normalize_started_item(params, source_offset),
        "item/completed" => return normalize_completed_item(params, source_offset),
        "turn/completed" => match nested(&params, &["turn", "status"])
            .and_then(Value::as_str)
            .unwrap_or("completed")
        {
            "completed" => NormalizedEventKind::TurnCompleted,
            "interrupted" => NormalizedEventKind::TurnInterrupted,
            "failed" => NormalizedEventKind::TurnFailed {
                message: nested(&params, &["turn", "error", "message"])
                    .and_then(Value::as_str)
                    .unwrap_or("Codex turn failed")
                    .to_string(),
            },
            unknown => {
                return Err(DriverHostError::UnknownRequiredEvent {
                    driver: DriverKind::Codex,
                    method: format!("turn/completed:{unknown}"),
                })
            }
        },
        "turn/interrupted" => NormalizedEventKind::TurnInterrupted,
        "turn/failed" => NormalizedEventKind::TurnFailed {
            message: nested(&params, &["error", "message"])
                .and_then(Value::as_str)
                .unwrap_or("Codex turn failed")
                .to_string(),
        },
        // Account rate-limit windows are surfaced as the same `RateLimit` event Claude emits.
        // A payload without a usable window carries nothing to report and is dropped.
        "account/rateLimits/updated" => match codex_rate_limit_kind(&params) {
            Some(kind) => kind,
            None => return Ok(None),
        },
        // Explicitly optional presentation/account notifications do not affect execution truth.
        "account/updated"
        | "remoteControl/status/changed"
        | "configWarning"
        | "warning"
        | "thread/tokenUsage/updated"
        | "thread/name/updated"
        | "thread/goal/cleared"
        | "serverRequest/resolved"
        | "model/rerouted"
        | "thread/status/changed"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/textDelta"
        | "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta"
        | "item/plan/delta"
        | "turn/plan/updated" => return Ok(None),
        "mcpServer/startupStatus/updated" => {
            // App-server 0.145 emits the server identity directly in `server`,
            // while earlier schema snapshots nested it in a server object.
            let server = params.get("server").or_else(|| params.get("mcpServer"));
            let name = server
                .and_then(|value| {
                    value
                        .as_str()
                        .or_else(|| value.get("name").and_then(Value::as_str))
                        .or_else(|| value.get("serverName").and_then(Value::as_str))
                        .or_else(|| value.get("id").and_then(Value::as_str))
                })
                .or_else(|| params.get("serverName").and_then(Value::as_str))
                .or_else(|| params.get("name").and_then(Value::as_str))
                .or_else(|| params.get("serverId").and_then(Value::as_str))
                .ok_or_else(|| {
                    let keys = params
                        .as_object()
                        .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
                        .unwrap_or_default();
                    invalid(
                        DriverKind::Codex,
                        format!("startup status missing server name; fields={keys}"),
                    )
                })?;
            if name != "rightkit-agent" {
                return Err(invalid(DriverKind::Codex, "unexpected MCP server"));
            }
            let status = server
                .and_then(|value| value.get("status").and_then(Value::as_str))
                .or_else(|| params.get("status").and_then(Value::as_str))
                .unwrap_or_default();
            if matches!(status, "starting" | "initializing") {
                return Ok(None);
            }
            if !matches!(status, "connected" | "ready" | "running") {
                return Err(invalid(
                    DriverKind::Codex,
                    format!("Agent MCP startup failed: {status}"),
                ));
            }
            return Ok(Some(NormalizedEvent {
                event_key: event_key(DriverKind::Codex, method, source_offset),
                driver: DriverKind::Codex,
                source_offset,
                vendor_method: method.into(),
                kind: NormalizedEventKind::GatewayReady,
            }));
        }
        unknown => {
            return Err(DriverHostError::UnknownRequiredEvent {
                driver: DriverKind::Codex,
                method: unknown.into(),
            })
        }
    };
    Ok(Some(NormalizedEvent {
        event_key: event_key(DriverKind::Codex, method, source_offset),
        driver: DriverKind::Codex,
        source_offset,
        vendor_method: method.into(),
        kind,
    }))
}

/// Utilization (percent) at which a Codex window is reported as `allowed_warning`.
const CODEX_WARNING_PERCENT: f64 = 80.0;

fn codex_window_label(minutes: Option<u64>, fallback: &str) -> String {
    match minutes {
        Some(300) => "five_hour".to_string(),
        Some(10080) => "seven_day".to_string(),
        Some(minutes) => format!("{minutes}_minute"),
        None => fallback.to_string(),
    }
}

/// Map an `account/rateLimits/updated` payload (`rateLimits.primary` / `rateLimits.secondary`
/// windows with `usedPercent`, `windowDurationMins`, `resetsAt`) onto the shared `RateLimit`
/// event, reporting the most-used window. `None` when no window carries a usage figure.
fn codex_rate_limit_kind(params: &Value) -> Option<NormalizedEventKind> {
    let snapshot = params
        .get("rateLimits")
        .or_else(|| params.get("rate_limits"))
        .unwrap_or(params);
    let mut best: Option<(f64, Option<u64>, String)> = None;
    for key in ["primary", "secondary"] {
        let Some(window) = snapshot.get(key).filter(|window| window.is_object()) else {
            continue;
        };
        let Some(used) = window
            .get("usedPercent")
            .or_else(|| window.get("used_percent"))
            .and_then(Value::as_f64)
        else {
            continue;
        };
        let number = |names: [&str; 2]| {
            names.iter().find_map(|name| {
                window.get(*name).and_then(|field| {
                    field
                        .as_u64()
                        .or_else(|| field.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
                })
            })
        };
        let minutes = number(["windowDurationMins", "window_minutes"]);
        let resets_at = number(["resetsAt", "resets_at"]);
        if best
            .as_ref()
            .map_or(true, |(current, _, _)| used > *current)
        {
            best = Some((used, resets_at, codex_window_label(minutes, key)));
        }
    }
    let (used, resets_at, label) = best?;
    let status = if used >= 100.0 {
        "rejected"
    } else if used >= CODEX_WARNING_PERCENT {
        "allowed_warning"
    } else {
        "allowed"
    };
    Some(NormalizedEventKind::RateLimit {
        account: None,
        status: status.to_string(),
        rate_limit_type: Some(label),
        resets_at,
        utilization: Some(used / 100.0),
        overage_status: None,
        error_code: None,
    })
}

/// A failed Codex turn that hit the account usage limit, as a `RateLimit` event with status
/// `rejected`. Emitted in addition to the ordinary `TurnFailed` that [`parse_codex_jsonrpc_line`]
/// yields for the same line; `Ok(None)` for every other line. The detection (`codexErrorInfo`
/// naming `usageLimitExceeded`, or a "usage limit" message, on `turn/completed`, `turn/failed` or
/// `error`) is `crate::codex::parse_line`'s.
pub fn parse_codex_usage_limit_event(
    line: &str,
    source_offset: u64,
) -> Result<Option<NormalizedEvent>, DriverHostError> {
    if !line.to_ascii_lowercase().contains("usage") {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(line)
        .map_err(|error| invalid(DriverKind::Codex, error.to_string()))?;
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !matches!(method, "turn/completed" | "turn/failed" | "error") {
        return Ok(None);
    }
    let incoming = crate::codex::parse_line(line)
        .map_err(|error| invalid(DriverKind::Codex, error.to_string()))?;
    let crate::codex::Incoming::Notification { events, .. } = incoming else {
        return Ok(None);
    };
    let Some(limit) = events.iter().find_map(|event| match event {
        crate::event::AgentEvent::RateLimit(limit) => Some(limit),
        _ => None,
    }) else {
        return Ok(None);
    };
    Ok(Some(NormalizedEvent {
        event_key: stable_event_key(DriverKind::Codex, "usage_limit", &value, source_offset),
        driver: DriverKind::Codex,
        source_offset,
        vendor_method: method.into(),
        kind: NormalizedEventKind::RateLimit {
            account: None,
            status: limit.status.clone(),
            rate_limit_type: None,
            resets_at: limit.resets_at,
            utilization: Some(1.0),
            overage_status: None,
            error_code: Some("usage_limit_exceeded".into()),
        },
    }))
}

fn normalize_started_item(
    params: Value,
    source_offset: u64,
) -> Result<Option<NormalizedEvent>, DriverHostError> {
    let item = nested(&params, &["item"])
        .ok_or_else(|| invalid(DriverKind::Codex, "item/started missing item"))?;
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    let kind = match item_type {
        "mcpToolCall" | "functionCall" | "commandExecution" | "fileChange" => {
            NormalizedEventKind::ToolCallStarted {
                call_id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid(DriverKind::Codex, "tool item missing id"))?
                    .to_string(),
                tool_name: item
                    .get("name")
                    .or_else(|| item.get("type"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid(DriverKind::Codex, "tool item missing name"))?
                    .to_string(),
                arguments: item
                    .get("arguments")
                    .or_else(|| item.get("command"))
                    .cloned()
                    .unwrap_or(Value::Null),
            }
        }
        // Message/reasoning/plan starts carry no durable outcome by themselves.
        "userMessage" | "agentMessage" | "reasoning" | "plan" | "webSearch" | "imageView"
        | "enteredReviewMode" | "exitedReviewMode" | "contextCompaction" => return Ok(None),
        unknown => {
            return Err(DriverHostError::UnknownRequiredEvent {
                driver: DriverKind::Codex,
                method: format!("item/started:{unknown}"),
            })
        }
    };
    Ok(Some(NormalizedEvent {
        event_key: event_key(DriverKind::Codex, "item/started", source_offset),
        driver: DriverKind::Codex,
        source_offset,
        vendor_method: "item/started".into(),
        kind,
    }))
}

fn normalize_completed_item(
    params: Value,
    source_offset: u64,
) -> Result<Option<NormalizedEvent>, DriverHostError> {
    let item = nested(&params, &["item"])
        .ok_or_else(|| invalid(DriverKind::Codex, "item/completed missing item"))?;
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    let kind = match item_type {
        "agentMessage" => NormalizedEventKind::AssistantCompleted {
            text: item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        "mcpToolCall" | "functionCall" | "commandExecution" | "fileChange" => {
            NormalizedEventKind::ToolResult {
                call_id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid(DriverKind::Codex, "completed tool item missing id"))?
                    .to_string(),
                is_error: item
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| matches!(status, "failed" | "declined")),
                content: item
                    .get("result")
                    .or_else(|| item.get("aggregatedOutput"))
                    .cloned()
                    .unwrap_or(Value::Null),
            }
        }
        "userMessage" | "reasoning" | "plan" | "webSearch" | "imageView" | "enteredReviewMode"
        | "exitedReviewMode" | "contextCompaction" => return Ok(None),
        unknown => {
            return Err(DriverHostError::UnknownRequiredEvent {
                driver: DriverKind::Codex,
                method: format!("item/completed:{unknown}"),
            })
        }
    };
    Ok(Some(NormalizedEvent {
        event_key: event_key(DriverKind::Codex, "item/completed", source_offset),
        driver: DriverKind::Codex,
        source_offset,
        vendor_method: "item/completed".into(),
        kind,
    }))
}

fn nested<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
}

fn required_string(value: &Value, path: &[&str], label: &str) -> Result<String, DriverHostError> {
    nested(value, path)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid(DriverKind::Codex, format!("missing {label}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_codex_launch_uses_stable_stdio_and_strict_config_flags() {
        let spec = CodexLaunchOptions {
            executable: "/usr/local/bin/codex".into(),
            cwd: "/work".into(),
            strict_config: true,
            model: None,
        }
        .launch_spec()
        .unwrap();
        assert_eq!(spec.args, ["app-server", "--stdio", "--strict-config"]);
    }

    #[test]
    fn selected_model_is_emitted_as_config_override_before_the_app_server_subcommand() {
        let spec = CodexLaunchOptions {
            executable: "/usr/local/bin/codex".into(),
            cwd: "/work".into(),
            strict_config: true,
            model: Some("o3".into()),
        }
        .launch_spec()
        .unwrap();
        assert_eq!(
            spec.args,
            [
                "-c",
                "model=\"o3\"",
                "app-server",
                "--stdio",
                "--strict-config"
            ]
        );
    }

    #[test]
    fn no_model_selected_omits_the_config_override_entirely() {
        let spec = CodexLaunchOptions {
            executable: "/usr/local/bin/codex".into(),
            cwd: "/work".into(),
            strict_config: false,
            model: None,
        }
        .launch_spec()
        .unwrap();
        assert!(!spec.args.iter().any(|arg| arg == "-c"));
    }

    #[test]
    fn hostile_model_string_is_rejected_before_reaching_argv() {
        let error = CodexLaunchOptions {
            executable: "/usr/local/bin/codex".into(),
            cwd: "/work".into(),
            strict_config: false,
            model: Some("o3\"; rm -rf / #".into()),
        }
        .launch_spec()
        .unwrap_err();
        assert_eq!(
            error,
            DriverHostError::InvalidMessage {
                driver: DriverKind::Codex,
                message: "model id contains disallowed characters: o3\"; rm -rf / #".into(),
            }
        );
    }

    #[test]
    fn launch_spec_denies_provider_keys_and_the_daemon_auth_token() {
        let spec = CodexLaunchOptions {
            executable: "/usr/local/bin/codex".into(),
            cwd: "/work".into(),
            strict_config: false,
            model: None,
        }
        .launch_spec()
        .unwrap();
        let expected: std::collections::BTreeSet<String> =
            crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS
                .iter()
                .map(|name| (*name).to_string())
                .collect();
        assert_eq!(spec.env_remove, expected);
        assert!(spec.env_remove.contains("OPENROUTER_API_KEY"));
        assert!(spec.env_remove.contains("RIGHTKIT_DAEMON_TOKEN"));
    }

    #[test]
    fn parses_stable_agent_delta_notification() {
        let line = r#"{"method":"item/agentMessage/delta","params":{"delta":"hello"}}"#;
        let Some(CodexMessage::Notification(event)) = parse_codex_jsonrpc_line(line, 9).unwrap()
        else {
            panic!("expected notification");
        };
        assert!(event.event_key.starts_with("codex:sha256:"));
        assert_eq!(
            event.kind,
            NormalizedEventKind::AssistantDelta {
                text: "hello".into()
            }
        );
    }

    #[test]
    fn replayed_delta_keeps_identity_when_the_reader_cursor_changes() {
        let line = r#"{"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":"hello"}}"#;
        let event_key = |offset| {
            let Some(CodexMessage::Notification(event)) =
                parse_codex_jsonrpc_line(line, offset).unwrap()
            else {
                panic!("expected notification");
            };
            event.event_key
        };
        assert_eq!(event_key(1), event_key(101));
    }

    #[test]
    fn identical_deltas_from_distinct_codex_items_remain_distinct() {
        let event_key = |item_id: &str, turn_id: &str| {
            let line = format!(
                r#"{{"method":"item/agentMessage/delta","params":{{"threadId":"thread-1","turnId":"{turn_id}","itemId":"{item_id}","delta":"same text"}}}}"#
            );
            let Some(CodexMessage::Notification(event)) =
                parse_codex_jsonrpc_line(&line, 1).unwrap()
            else {
                panic!("expected notification");
            };
            event.event_key
        };
        assert_ne!(event_key("item-1", "turn-1"), event_key("item-2", "turn-2"));
    }

    #[test]
    fn unknown_notification_fails_closed() {
        let error = parse_codex_jsonrpc_line(r#"{"method":"item/newMutationKind","params":{}}"#, 1)
            .unwrap_err();
        assert_eq!(
            error,
            DriverHostError::UnknownRequiredEvent {
                driver: DriverKind::Codex,
                method: "item/newMutationKind".into()
            }
        );
    }

    #[test]
    fn current_user_message_items_are_known_presentation_events() {
        for method in ["item/started", "item/completed"] {
            let line = format!(
                r#"{{"method":"{method}","params":{{"item":{{"type":"userMessage","id":"user-1","content":[]}}}}}}"#
            );
            assert!(parse_codex_jsonrpc_line(&line, 1).unwrap().is_none());
        }
    }

    #[test]
    fn resolved_server_requests_are_known_lifecycle_events() {
        let line = r#"{"method":"serverRequest/resolved","params":{"requestId":"7"}}"#;
        assert_eq!(parse_codex_jsonrpc_line(line, 1).unwrap(), None);
    }

    #[test]
    fn direct_codex_startup_status_marks_gateway_ready() {
        let line = r#"{"method":"mcpServer/startupStatus/updated","params":{"server":"host","status":"connected"}}"#;
        let Some(CodexMessage::Notification(event)) = parse_codex_jsonrpc_line(line, 1).unwrap()
        else {
            panic!("expected gateway readiness");
        };
        assert_eq!(event.kind, NormalizedEventKind::GatewayReady);
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;
    use crate::driver_host::ClaudeAccountLaunch;

    fn bare_spec() -> LaunchSpec {
        CodexLaunchOptions {
            executable: PathBuf::from("codex"),
            cwd: PathBuf::from("."),
            strict_config: true,
            model: None,
        }
        .launch_spec()
        .unwrap()
    }

    #[test]
    fn codex_account_env_none_keeps_default_launch() {
        let mut spec = bare_spec();
        let before = spec.clone();
        apply_codex_account_env(&mut spec, None);
        assert_eq!(spec, before);
        assert!(spec.env_remove.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn codex_account_env_subscription_strips_ambient_keys() {
        let mut spec = bare_spec();
        spec.env.insert("OPENAI_API_KEY".into(), "ambient".into());
        let who = ClaudeAccountLaunch::Subscription {
            managed_home: PathBuf::from("/profiles/codex/personal"),
        };
        apply_codex_account_env(&mut spec, Some(&who));
        assert!(!spec.env.contains_key("OPENAI_API_KEY"));
        assert!(spec.env_remove.contains("OPENAI_API_KEY"));
        assert!(spec.env_remove.contains("CODEX_API_KEY"));
    }

    #[test]
    fn codex_account_env_api_key_sets_key_on_this_spec_only() {
        let mut spec = bare_spec();
        let who = ClaudeAccountLaunch::ApiKey {
            key: concat!("sk", "-secret-value").into(),
        };
        apply_codex_account_env(&mut spec, Some(&who));
        assert_eq!(
            spec.env.get("OPENAI_API_KEY").map(String::as_str),
            Some(concat!("sk", "-secret-value"))
        );
        assert!(!spec.env_remove.contains("OPENAI_API_KEY"));
        assert!(!spec.args.iter().any(|arg| arg.contains("sk-secret")));
    }
}

#[cfg(test)]
mod rate_limit_tests {
    use super::*;

    fn notification(line: &str) -> NormalizedEvent {
        match parse_codex_jsonrpc_line(line, 1).unwrap() {
            Some(CodexMessage::Notification(event)) => event,
            other => panic!("expected notification, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_update_becomes_the_shared_rate_limit_event() {
        let line = r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":42,"windowDurationMins":300,"resetsAt":1790000000},"secondary":{"usedPercent":10,"windowDurationMins":10080,"resetsAt":1790500000}}}}"#;
        let event = notification(line);
        assert_eq!(event.driver, DriverKind::Codex);
        assert_eq!(event.vendor_method, "account/rateLimits/updated");
        match event.kind {
            NormalizedEventKind::RateLimit {
                account,
                status,
                rate_limit_type,
                resets_at,
                utilization,
                ..
            } => {
                assert_eq!(account, None);
                assert_eq!(status, "allowed");
                assert_eq!(rate_limit_type.as_deref(), Some("five_hour"));
                assert_eq!(resets_at, Some(1790000000));
                assert_eq!(utilization, Some(0.42));
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn most_used_window_wins_and_thresholds_set_the_status() {
        let warn = r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":20,"windowDurationMins":300},"secondary":{"usedPercent":85,"windowDurationMins":10080,"resetsAt":1790500000}}}}"#;
        match notification(warn).kind {
            NormalizedEventKind::RateLimit {
                status,
                rate_limit_type,
                resets_at,
                ..
            } => {
                assert_eq!(status, "allowed_warning");
                assert_eq!(rate_limit_type.as_deref(), Some("seven_day"));
                assert_eq!(resets_at, Some(1790500000));
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
        let full = r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":100,"windowDurationMins":45}}}}"#;
        match notification(full).kind {
            NormalizedEventKind::RateLimit {
                status,
                rate_limit_type,
                utilization,
                ..
            } => {
                assert_eq!(status, "rejected");
                assert_eq!(rate_limit_type.as_deref(), Some("45_minute"));
                assert_eq!(utilization, Some(1.0));
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_update_without_a_usable_window_is_dropped() {
        for line in [
            r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":null,"secondary":null}}}"#,
            r#"{"method":"account/rateLimits/updated","params":{}}"#,
        ] {
            assert_eq!(parse_codex_jsonrpc_line(line, 1).unwrap(), None);
        }
    }

    #[test]
    fn repeated_updates_keep_distinct_event_keys() {
        let a = r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":10}}}}"#;
        let b = r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":11}}}}"#;
        assert_ne!(notification(a).event_key, notification(b).event_key);
    }

    #[test]
    fn usage_limit_failure_yields_a_rejected_rate_limit_event() {
        let completed = r#"{"method":"turn/completed","params":{"turn":{"id":"t","status":"failed","error":{"message":"You've hit your usage limit","codexErrorInfo":"usageLimitExceeded"}}}}"#;
        let event = parse_codex_usage_limit_event(completed, 3)
            .unwrap()
            .unwrap();
        assert!(matches!(
            event.kind,
            NormalizedEventKind::RateLimit { ref status, utilization: Some(u), ref error_code, .. }
                if status == "rejected" && u == 1.0 && error_code.as_deref() == Some("usage_limit_exceeded")
        ));
        // The ordinary failure still parses as TurnFailed alongside it.
        assert!(matches!(
            notification(completed).kind,
            NormalizedEventKind::TurnFailed { .. }
        ));
        let failed = r#"{"method":"turn/failed","params":{"error":{"message":"usage limit reached","resetsAt":1790000000}}}"#;
        match parse_codex_usage_limit_event(failed, 4)
            .unwrap()
            .unwrap()
            .kind
        {
            NormalizedEventKind::RateLimit { resets_at, .. } => {
                assert_eq!(resets_at, Some(1790000000))
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn other_failures_and_lines_are_not_usage_limits() {
        for line in [
            r#"{"method":"turn/failed","params":{"error":{"message":"boom"}}}"#,
            r#"{"method":"turn/completed","params":{"turn":{"status":"completed"}}}"#,
            r#"{"method":"item/agentMessage/delta","params":{"delta":"usage limit"}}"#,
            r#"{"method":"turn/failed","params":{"error":{"message":"bad","codexErrorInfo":"badRequest"}}}"#,
        ] {
            assert_eq!(
                parse_codex_usage_limit_event(line, 1).unwrap(),
                None,
                "{line}"
            );
        }
    }
}
