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
