// `schedule_cron` — register/list/cancel cron-style scheduled prompt records.
//
// Store schedules in a workspace-persisted registry with cron_expr, prompt, id.
// Parse 5-field cron expression into a CronSchedule struct (validate fields).
// The daemon runner consumes due records and submits them into deterministic
// `schedule-<id>` sessions.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const SCHEDULE_ADMIN_TOOLSET: &str = "schedule-admin";

/// Complete schedule trigger vocabulary shared by CLI/daemon adapters.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ScheduleTrigger {
    Once {
        at_unix_ms: i64,
    },
    Cron {
        expression: String,
        time_zone: String,
    },
    Interval {
        every_seconds: u64,
        start_unix_ms: Option<i64>,
    },
    Monitor {
        topic: String,
        filter: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleLifecycleState {
    pub paused: bool,
    pub overlap: String,
    pub misfire: String,
    pub idempotency_key: Option<String>,
    pub retry_limit: u32,
}

impl Default for ScheduleLifecycleState {
    fn default() -> Self {
        Self {
            paused: false,
            overlap: "skip".into(),
            misfire: "skip".into(),
            idempotency_key: None,
            retry_limit: 0,
        }
    }
}

impl ScheduleTrigger {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Once { .. } => Ok(()),
            Self::Cron {
                expression,
                time_zone,
            } if expression.trim().is_empty() || time_zone.trim().is_empty() => {
                Err("cron requires expression and time zone".into())
            }
            Self::Cron { .. } => Ok(()),
            Self::Interval { every_seconds, .. } if *every_seconds == 0 => {
                Err("interval must be positive".into())
            }
            Self::Interval { .. } => Ok(()),
            Self::Monitor { topic, .. } if topic.trim().is_empty() => {
                Err("monitor topic is empty".into())
            }
            Self::Monitor { .. } => Ok(()),
        }
    }

    pub fn next_run_at(&self, after_unix_ms: i64) -> Result<Option<i64>, String> {
        self.validate()?;
        Ok(match self {
            Self::Once { at_unix_ms } => (*at_unix_ms > after_unix_ms).then_some(*at_unix_ms),
            Self::Interval {
                every_seconds,
                start_unix_ms,
            } => {
                let step = (*every_seconds as i64).saturating_mul(1_000);
                let start = start_unix_ms.unwrap_or(after_unix_ms.saturating_add(step));
                if start > after_unix_ms {
                    Some(start)
                } else {
                    Some(
                        start.saturating_add(
                            ((after_unix_ms - start) / step + 1).saturating_mul(step),
                        ),
                    )
                }
            }
            // Cron preview must use daemon timezone rules; refuse rather than
            // claiming that no run exists.
            Self::Cron { .. } => {
                return Err("cron preview is available through the daemon schedule API".into())
            }
            Self::Monitor { .. } => None,
        })
    }
}

pub struct ScheduleCronTool {
    schedules: Arc<Mutex<HashMap<String, ScheduleEntry>>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ScheduleEntry {
    id: String,
    cron_expr: String,
    prompt: String,
    #[serde(default = "default_time_zone")]
    time_zone: String,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    toolset: Option<String>,
    #[serde(default)]
    execution_backend: Option<String>,
    #[serde(default)]
    delivery: Vec<DeliveryTarget>,
    #[serde(default)]
    script_allowlist: Vec<String>,
    #[serde(default = "default_job_mode")]
    mode: String,
    #[serde(default)]
    paused: bool,
    /// Maximum runtime in seconds for scriptOnly schedules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_runtime_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trigger: Option<ScheduleTrigger>,
    #[serde(default)]
    lifecycle: ScheduleLifecycleState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryTarget {
    kind: String,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    allow_private_network: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CronSchedule {
    minute: Vec<u8>,
    hour: Vec<u8>,
    day: Vec<u8>,
    month: Vec<u8>,
    weekday: Vec<u8>,
}

impl ScheduleCronTool {
    pub fn new() -> Self {
        Self {
            schedules: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for ScheduleCronTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for ScheduleCronTool {
    fn name(&self) -> &str {
        "schedule_cron"
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        match args.get("operation").and_then(Value::as_str) {
            Some("register" | "cancel") => Some(rightkit_agent_policy::DurableWriteKind::Schedule),
            _ => None,
        }
    }

    fn description(&self) -> &str {
        "Register/list/cancel durable cron-style scheduled prompt records in .rightkit/schedules.json. Ops: register(cron_expr, prompt), list(), cancel(id). Cron: '*/5 * * * *' = every 5min; '0 9 * * *' = 9am daily. The daemon runner claims due records once per minute and submits prompts into schedule-<id> sessions. Fail: invalid cron syntax."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["register", "list", "cancel", "preview"],
                    "description": "Operation to perform"
                },
                "cron_expr": {
                    "type": "string",
                    "description": "5-field cron expression (minute hour day month weekday) for register operation"
                },
                "prompt": {
                    "type": "string",
                    "description": "The prompt text to execute on schedule (register operation)"
                },
                "time_zone": {
                    "type": "string",
                    "description": "Optional timezone for cron matching, e.g. UTC, UTC+05:30, America/New_York, Europe/London, Asia/Kolkata"
                },
                "skills": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional skill ids to attach as schedule metadata"
                },
                "workdir": {
                    "type": "string",
                    "description": "Optional absolute working directory for the scheduled job"
                },
                "toolset": {
                    "type": "string",
                    "description": "Optional toolset profile to record for this scheduled job"
                },
                "execution_backend": {
                    "type": "string",
                    "description": "Optional execution backend id to record for this scheduled job"
                },
                "delivery": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string" },
                            "target": { "type": "string" }
                        },
                        "required": ["kind"]
                    },
                    "description": "Optional delivery targets such as desktop_notification, session_message, telegram_chat, webhook, or file_artifact"
                },
                "mode": {
                    "type": "string",
                    "enum": ["agent", "scriptOnly"],
                    "description": "Scheduled job mode. scriptOnly runs the prompt as a command only when it exactly matches script_allowlist."
                },
                "script_allowlist": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exact command strings permitted when mode is scriptOnly"
                },
                "max_runtime_seconds": {
                    "type": "integer",
                    "description": "Maximum runtime in seconds for scriptOnly schedules; script is killed if it exceeds this limit"
                },
                "id": {
                    "type": "string",
                    "description": "Schedule ID to cancel (cancel operation)"
                },
                "trigger": {
                    "type": "object",
                    "description": "One-shot, cron, interval, or monitor trigger used by preview"
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let operation = match args["operation"].as_str() {
            Some(op) => op,
            None => return ToolResult::error("Missing required argument: operation"),
        };

        match operation {
            "register" => {
                if let Err(message) = require_schedule_admin_for_mutation(ctx) {
                    return ToolResult::error(message);
                }
                if let Err(message) = require_schedule_write_authorization(ctx) {
                    return ToolResult::error(message);
                }
                let cron_expr = args["cron_expr"].as_str().unwrap_or("");
                if cron_expr.trim().is_empty() && args.get("trigger").is_none() {
                    return ToolResult::error("Missing cron_expr or trigger for register");
                }
                let prompt = match args["prompt"].as_str() {
                    Some(p) => p,
                    None => return ToolResult::error("Missing prompt for register"),
                };
                let time_zone = args["time_zone"].as_str().unwrap_or("UTC");
                self.register_schedule(cron_expr, prompt, time_zone, &args, ctx)
            }
            "preview" => {
                let trigger = match args
                    .get("trigger")
                    .cloned()
                    .ok_or_else(|| "Missing trigger for preview".to_string())
                    .and_then(|value| {
                        serde_json::from_value::<ScheduleTrigger>(value).map_err(|e| e.to_string())
                    }) {
                    Ok(value) => value,
                    Err(message) => return ToolResult::error(message),
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or_default();
                match trigger.next_run_at(now) {
                    Ok(Some(next)) => ToolResult::ok(format!("nextRunAtUnixMs={next}")),
                    Ok(None) => ToolResult::ok("no scheduled run"),
                    Err(message) => ToolResult::error(message),
                }
            }
            "list" => self.list_schedules(ctx),
            "cancel" => {
                if let Err(message) = require_schedule_admin_for_mutation(ctx) {
                    return ToolResult::error(message);
                }
                if let Err(message) = require_schedule_write_authorization(ctx) {
                    return ToolResult::error(message);
                }
                let id = match args["id"].as_str() {
                    Some(i) => i,
                    None => return ToolResult::error("Missing id for cancel"),
                };
                self.cancel_schedule(id, ctx)
            }
            other => ToolResult::error(format!("Unknown operation: {}", other)),
        }
    }
}

impl ScheduleCronTool {
    fn register_schedule(
        &self,
        cron_expr: &str,
        prompt: &str,
        time_zone: &str,
        args: &Value,
        ctx: &ToolContext,
    ) -> ToolResult {
        let mut trigger = match args.get("trigger").cloned() {
            Some(value) => match serde_json::from_value::<ScheduleTrigger>(value) {
                Ok(trigger) => trigger,
                Err(error) => return ToolResult::error(format!("Invalid trigger: {error}")),
            },
            None => ScheduleTrigger::Cron {
                expression: cron_expr.to_string(),
                time_zone: time_zone.to_string(),
            },
        };
        if let Err(error) = trigger.validate() {
            return ToolResult::error(error);
        }
        let encoded_expr = match &trigger {
            ScheduleTrigger::Once { at_unix_ms } => format!("once:{at_unix_ms}"),
            ScheduleTrigger::Cron { expression, .. } => expression.clone(),
            ScheduleTrigger::Interval {
                every_seconds,
                start_unix_ms,
            } => {
                format!(
                    "interval:{every_seconds}:{}",
                    start_unix_ms.map(|v| v.to_string()).unwrap_or_default()
                )
            }
            ScheduleTrigger::Monitor { topic, filter } => {
                format!("monitor:{topic}:{}", filter.clone().unwrap_or_default())
            }
        };
        // Validate cron syntax only for cron triggers; all other trigger kinds
        // are encoded in legacy cron_expr for one scheduler consumer.
        match if matches!(trigger, ScheduleTrigger::Cron { .. }) {
            parse_cron(&encoded_expr)
        } else {
            Ok(CronSchedule {
                minute: Vec::new(),
                hour: Vec::new(),
                day: Vec::new(),
                month: Vec::new(),
                weekday: Vec::new(),
            })
        } {
            Ok(_) => {
                let time_zone = match normalize_time_zone(time_zone) {
                    Ok(tz) => tz,
                    Err(e) => return ToolResult::error(format!("Invalid time zone: {e}")),
                };
                if let ScheduleTrigger::Cron {
                    time_zone: trigger_tz,
                    ..
                } = &mut trigger
                {
                    *trigger_tz = time_zone.clone();
                }
                let skills = string_array_arg(args, "skills");
                let workdir = args["workdir"]
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string());
                if let Some(workdir) = &workdir {
                    let path = PathBuf::from(workdir);
                    if !path.is_absolute() {
                        return ToolResult::error("workdir must be an absolute path");
                    }
                    if !path_stays_inside(&path, &ctx.workspace_root) {
                        return ToolResult::error("workdir must stay inside the workspace");
                    }
                }
                let toolset = optional_string_arg(args, "toolset");
                let execution_backend = optional_string_arg(args, "execution_backend");
                let delivery = match delivery_arg(args, ctx) {
                    Ok(delivery) => delivery,
                    Err(e) => return ToolResult::error(e),
                };
                let script_allowlist = string_array_arg(args, "script_allowlist");
                let mode = args["mode"].as_str().unwrap_or("agent");
                if !mode.eq_ignore_ascii_case("agent") && !mode.eq_ignore_ascii_case("scriptOnly") {
                    return ToolResult::error("mode must be agent or scriptOnly");
                }
                let max_runtime_seconds = args["max_runtime_seconds"].as_u64().filter(|v| *v > 0);
                let id = format!(
                    "schedule_{}",
                    uuid::Uuid::new_v4().to_string()[0..8].to_string()
                );
                let entry = ScheduleEntry {
                    id: id.clone(),
                    cron_expr: encoded_expr,
                    prompt: prompt.to_string(),
                    time_zone: time_zone.clone(),
                    skills,
                    workdir,
                    toolset,
                    execution_backend,
                    delivery,
                    script_allowlist,
                    mode: if mode.eq_ignore_ascii_case("scriptOnly") {
                        "scriptOnly".to_string()
                    } else {
                        "agent".to_string()
                    },
                    paused: false,
                    max_runtime_seconds,
                    trigger: Some(trigger),
                    lifecycle: ScheduleLifecycleState::default(),
                };

                let mut schedules = match self.load_effective_schedules(ctx) {
                    Ok(s) => s,
                    Err(e) => return ToolResult::error(e),
                };
                schedules.insert(id.clone(), entry);
                if let Err(e) = save_workspace_schedules(ctx, &schedules) {
                    return ToolResult::error(e);
                }
                *self.schedules.lock().unwrap() = schedules;

                ToolResult::ok(format!(
                    "Registered schedule '{}' with cron expression '{}' in {}",
                    id, cron_expr, time_zone
                ))
            }
            Err(e) => ToolResult::error(format!("Invalid cron expression: {}", e)),
        }
    }

    fn list_schedules(&self, ctx: &ToolContext) -> ToolResult {
        let schedules = match self.load_effective_schedules(ctx) {
            Ok(s) => s,
            Err(e) => return ToolResult::error(e),
        };

        if schedules.is_empty() {
            return ToolResult::ok("No schedules registered");
        }

        let mut output = String::from("Registered schedules:\n");
        for entry in schedules.values() {
            output.push_str(&format!(
                "  [{}] {} -> '{}'\n",
                entry.id, entry.cron_expr, entry.prompt
            ));
            output.push_str(&format!("      time zone: {}\n", entry.time_zone));
        }

        ToolResult::ok(output)
    }

    fn cancel_schedule(&self, id: &str, ctx: &ToolContext) -> ToolResult {
        let mut schedules = match self.load_effective_schedules(ctx) {
            Ok(s) => s,
            Err(e) => return ToolResult::error(e),
        };

        if schedules.remove(id).is_some() {
            if let Err(e) = save_workspace_schedules(ctx, &schedules) {
                return ToolResult::error(e);
            }
            *self.schedules.lock().unwrap() = schedules;
            ToolResult::ok(format!("Cancelled schedule '{}'", id))
        } else {
            ToolResult::error(format!("Schedule '{}' not found", id))
        }
    }

    fn load_effective_schedules(
        &self,
        ctx: &ToolContext,
    ) -> Result<HashMap<String, ScheduleEntry>, String> {
        let mut merged = self.schedules.lock().unwrap().clone();
        for (id, entry) in load_workspace_schedules(ctx)? {
            merged.insert(id, entry);
        }
        Ok(merged)
    }
}
