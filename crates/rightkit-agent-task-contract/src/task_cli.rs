//! The task command line: hand a task to the resident engine's task journal.
//!
//! * `submit --kind KIND [--params JSON] --json DOCUMENT` (or `--stdin`): a task envelope or a voice hand-off.
//! * `ask TEXT [--wait] [--key KEY]`: a direct `agent.request`.
//! * `act TEXT [--wait] [--key KEY]`: a direct `agent.act`; needs the agent-actions switch.
//! * `status ID`, `result ID`, `cancel ID`, `events ID [--after N]`.
//!
//! Every command forwards to the engine method of the same name through [`TaskHost::call`]. Nothing
//! is queued or stored on this side. A voice hand-off is recognised by its `mode` field, checked and
//! mapped with the voice module, then sent as a task envelope. `ask` builds a direct request from
//! this computer: the text is the intent quote and this device is the target. `--wait` follows the
//! task to its end.
//!
//! Output: a success prints the engine's answer on stdout and exits 0. Exit 2 means the engine
//! refused the request (or the client refused it before sending): a receipt with a `reason` and
//! `code` on stdout. Exit 3 means unknown: `unreachable` (no engine answered; nothing ran) or
//! `unknown` (sent, but no answer came back; for submit, resend once with the same idempotency key).

/// A command-line failure: an optional JSON body for stderr and the process exit code.
#[derive(Debug)]
pub struct CliError {
    /// Written to stderr when present.
    pub body: Option<Value>,
    /// The process exit code.
    pub exit: u8,
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self {
            body: Some(serde_json::json!({"error": message})),
            exit: 2,
        }
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

/// Why an engine call failed.
#[derive(Debug)]
pub enum Failure {
    /// Nothing ran: no engine was found or it was not ready or was closing. Safe to retry later.
    Unreachable(String),
    /// The request was sent but no answer arrived (lost link, timeout). The engine may have acted;
    /// retry only with the same idempotency key.
    Unknown(String),
    /// The engine answered with a refusal, with its stable code.
    Refused {
        /// The engine's stable refusal code.
        code: String,
        /// The engine's message.
        message: String,
    },
}

/// What the task command line needs from the application that embeds it.
pub trait TaskHost {
    /// Calls one engine method over the local engine connection.
    fn call(&self, method: &str, params: Value) -> Result<Value, Failure>;
    /// Whether the user has switched agent actions on (only a default for a voice hand-off).
    fn agent_actions_on(&self) -> bool;
    /// This computer's device id, when the application has made one.
    fn local_device_id(&self) -> Option<String>;
    /// A fresh random UUID string.
    fn random_uuid(&self) -> String;
    /// The command name in the help text and the chat labels, such as `app` (labels are `app-cli:ask`).
    fn program_name(&self) -> &str;
    /// The product name in the help text.
    fn product_name(&self) -> &str;
    /// The labels a voice hand-off is mapped with (its chat label and policy reference).
    fn voice_labels(&self) -> VoiceLabels;
    /// The chat session this command runs in, sent as `fromChat` with `agent.act` so the engine
    /// can say who is asking. The application reads it from wherever its chat hosts put it (for
    /// example a session-id environment variable); `None` sends none.
    fn submitter_chat(&self) -> Option<String>;
}

use std::io::Read;
use std::time::{Duration, Instant};

use crate::device::{DEVICE_REF_SCHEMA_VERSION, DevicePlatform, DeviceRef};
use crate::intent::{INTENT_SCHEMA_VERSION, Intent, IntentSource};
use crate::principal::{PRINCIPAL_SCHEMA_VERSION, Principal, PrincipalKind};
use crate::task::{TASK_ENVELOPE_SCHEMA_VERSION, UTTERANCE_REF_SCHEMA_VERSION, UtteranceRef};
use crate::{TaskEnvelope, TaskStatus, VoiceHandoff, VoiceLabels};
use serde_json::{Map, Value, json};

/// The job kind that asks the agent a question. It answers only, with no tools. The engine
/// refuses it until it runs that kind.
pub const AGENT_REQUEST_KIND: &str = "agent.request";
/// The job kind that acts: one bounded agent run over the tools that are switched on, every act approved by the owner. The engine
/// refuses it with `agent_actions_off` unless the `agentActions` setting is on.
pub const AGENT_ACT_KIND: &str = "agent.act";
/// The owner the `ask` envelope names as its origin.
const ASK_OWNER: &str = "owner";
/// Policy reference applied to `ask` tasks.
const ASK_POLICY_REF: &str = "agent-request/v1";
/// Policy reference applied to `act` tasks.
const ACT_POLICY_REF: &str = "agent-act/v1";
/// Pause between polls for `ask --wait`. The engine's events call does not block, so this is the wait.
const WAIT_POLL: Duration = Duration::from_millis(500);
/// How long `ask --wait` follows a task before it answers `unknown` and leaves it running.
const WAIT_LIMIT: Duration = Duration::from_secs(600);

const HELP: &str = "{prog} task submit --kind <kind> [--params <json-object>] --json <envelope|voice-handoff>  (or --stdin)\n\
{prog} task ask <text…> [--wait] [--key <idempotency-key>]  (or --stdin; `{prog} task ask -- <text>` for text that starts with a dash)\n\
{prog} task act <text…> [--wait] [--key <idempotency-key>]  (the same, as an acting agent.act)\n\
{prog} task status <task-id> | result <task-id> | cancel <task-id>\n\
{prog} task events <task-id> [--after N]\n\
{prog} task list  (unsupported: use task.journal)\n\
Each command forwards to the engine method of the same name (task.submit, task.status, task.result,\n\
task.cancel, task.events) over the local engine connection. submit validates a task envelope or a\n\
voice hand-off first. --kind names a job kind the engine runs; a voice hand-off with no\n\
--kind runs agent.request (agent.act while agentActions is on). The engine runs only the kinds it lists in the engine's job-kind list.\n\
act sends one request to the acting agent on this computer as agent.act, with only the {product}\n\
tools that are switched on, each act approved by you in {product} (the task is waiting-user until you answer).\n\
It needs the agentActions setting (off by default). Cancel it with {prog} task cancel <task-id>. act takes\n\
--wait and --key like ask; with --wait it waits up to 10 minutes, so a longer run is unknown here and\n\
keeps going (check it with {prog} task status).\n\
ask sends one question to the agent on this computer as agent.request (answer only, no tools). Without\n\
--wait it prints the accepted receipt with the task_id; check it with status or result. With --wait it\n\
follows the task (lifecycle events on stderr) and prints task.result on stdout when the task ends.\n\
Exit 0: the engine's answer on stdout. Exit 2: refused, with a \"reason\" and \"code\" on stdout.\n\
Exit 3: unknown. \"unreachable\" means no engine answered and nothing ran; \"unknown\" means the request\n\
was sent but no answer came back. With --wait, a task still running after 10 minutes is unknown.\n\
--key <idempotency-key> names the task's idempotency key. Without it, ask mints one and prints it as\n\
\"idempotency_key\" in the receipt. After an unknown receipt, resend with --key set to that key: the\n\
engine resolves the same key to the same task, so the resend does not start a second one.";

/// The help text for a client whose command is `prog` and whose product is `product`.
pub fn help(prog: &str, product: &str) -> String {
    HELP.replace("{prog}", prog).replace("{product}", product)
}

const SUBMIT_USAGE: &str = "submit takes one JSON document, or --stdin (task --help)";
const ASK_USAGE: &str = "ask takes the question as text, or --stdin (task --help)";
const ACT_USAGE: &str = "act takes the request as text, or --stdin (task --help)";
const KEY_USAGE: &str = "--key takes one idempotency key: --key <idempotency-key> (task --help)";

pub fn run(host: &dyn TaskHost, arguments: Vec<String>) -> Result<(), CliError> {
    let mut arguments = arguments;
    if arguments.is_empty() || matches!(arguments[0].as_str(), "help" | "--help" | "-h") {
        println!("{}", help(host.program_name(), host.product_name()));
        return Ok(());
    }
    let command = arguments.remove(0);
    match command.as_str() {
        "submit" => submit(host, arguments),
        "ask" => send_request(host, arguments, AGENT_REQUEST_KIND),
        "act" => send_request(host, arguments, AGENT_ACT_KIND),
        "status" | "result" | "cancel" => task_call(host, &command, arguments),
        "events" => events(host, arguments),
        "list" => list(),
        other => Err(format!("unknown task command: {other} (task --help)").into()),
    }
}

fn submit(host: &dyn TaskHost, arguments: Vec<String>) -> Result<(), CliError> {
    if arguments.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", help(host.program_name(), host.product_name()));
        return Ok(());
    }
    let mut arguments = arguments;
    let kind = take_option(&mut arguments, "--kind")?;
    let params = take_option(&mut arguments, "--params")?;
    let from_stdin = arguments.iter().any(|a| a == "--stdin");
    let positional: Vec<&String> = arguments
        .iter()
        .filter(|a| a.as_str() != "--stdin")
        .collect();
    let text = if from_stdin {
        if !positional.is_empty() {
            return Err(SUBMIT_USAGE.into());
        }
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        buffer
    } else {
        match positional.as_slice() {
            [document] => document.to_string(),
            _ => return Err(SUBMIT_USAGE.into()),
        }
    };

    let (kind_label, envelope) = match parse(host, &text) {
        Ok(parsed) => parsed,
        Err(reason) => {
            println!(
                "{}",
                json!({"method": "task.submit", "status": "refused", "validated": false, "reason": reason})
            );
            return Err(CliError {
                body: None,
                exit: 2,
            });
        }
    };
    let detail = json!({
        "validated": true,
        "kind": kind_label,
        "idempotency_key": envelope.idempotency_key,
        "target_device": envelope.target_device.device_id,
    });

    // The envelope carries `kind` since schema 2; `--kind` fills it in and may not contradict it.
    let from_envelope = Some(envelope.kind.clone()).filter(|k| !k.is_empty());
    if let (Some(flag), Some(own)) = (&kind, &from_envelope)
        && flag != own
    {
        return refuse(
            "task.submit",
            "--kind differs from the envelope's kind; give one or make them match",
            detail,
        );
    }
    // A voice hand-off with no --kind is an agent request (a voice hand-off names no kind), or an acting
    // agent while the owner has turned `agentActions` on. The act is still approved act by act.
    let default_kind = (kind_label == "voice-handoff").then(|| {
        if host.agent_actions_on() {
            AGENT_ACT_KIND.to_string()
        } else {
            AGENT_REQUEST_KIND.to_string()
        }
    });
    let Some(kind) = kind.or(from_envelope).or(default_kind) else {
        return refuse(
            "task.submit",
            "submit needs --kind <kind>; the engine runs only the kinds in its job-kind list",
            detail,
        );
    };
    let params = match params {
        None if kind == AGENT_REQUEST_KIND => agent_request_params(),
        None if kind == AGENT_ACT_KIND => agent_act_params(),
        None => Value::Object(Map::new()),
        Some(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value @ Value::Object(_)) => value,
            _ => return refuse("task.submit", "--params must be a JSON object", detail),
        },
    };

    let answer = forward_submit(host, &envelope, &kind, params, detail)?;
    println!("{answer}");
    Ok(())
}

/// Sends a checked envelope to `task.submit` with its job kind and params, and returns the
/// ACCEPTANCE receipt. The engine takes the envelope with `task_id` empty, plus `kind` and `params`.
fn forward_submit(
    host: &dyn TaskHost,
    envelope: &TaskEnvelope,
    kind: &str,
    params: Value,
    detail: Value,
) -> Result<Value, CliError> {
    let mut body = serde_json::to_value(envelope)
        .map_err(|error| format!("cannot encode the task envelope: {error}"))?;
    let mut params = params;
    // The chat this runs in, so the engine can say who is asking (an explicit `fromChat` wins).
    // Only `agent.act` takes it (the bridge verifies and removes it, `agent_act::stamp_submitter`);
    // every other kind's params refuse unknown fields.
    if kind == AGENT_ACT_KIND
        && let Value::Object(map) = &mut params
        && !map.contains_key("fromChat")
        && let Some(chat) = host
            .submitter_chat()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    {
        map.insert("fromChat".to_string(), Value::String(chat));
    }
    if let Value::Object(map) = &mut body {
        map.insert("kind".to_string(), json!(kind));
        map.insert("params".to_string(), params);
    }
    host.call("task.submit", body)
        .map_err(|failure| failed("task.submit", failure, detail))
}

/// The params `agent.request` takes when none are given.
fn agent_request_params() -> Value {
    json!({"schema_version": 1, "reply": "text"})
}

/// The params `agent.act` takes when none are given (the engine's limits apply).
fn agent_act_params() -> Value {
    json!({"schema_version": 1})
}

/// `ask <text…> [--wait]` / `act <text…> [--wait]` (or `--stdin`): one `agent.request` or
/// `agent.act` from this computer. The envelope is built the way a voice hand-off is mapped: the
/// text is the direct intent quote, and this device is both the origin connection and the target.
fn send_request(
    host: &dyn TaskHost,
    arguments: Vec<String>,
    kind: &'static str,
) -> Result<(), CliError> {
    let acting = kind == AGENT_ACT_KIND;
    let (command, usage, chat, policy) = if acting {
        (
            "act",
            ACT_USAGE,
            format!("{}-cli:act", host.program_name()),
            ACT_POLICY_REF,
        )
    } else {
        (
            "ask",
            ASK_USAGE,
            format!("{}-cli:ask", host.program_name()),
            ASK_POLICY_REF,
        )
    };
    if arguments.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", help(host.program_name(), host.product_name()));
        return Ok(());
    }
    // Words after a bare `--` are the text, even when they look like flags.
    let (options, literal) = match arguments.iter().position(|a| a == "--") {
        Some(at) => (arguments[..at].to_vec(), arguments[at + 1..].to_vec()),
        None => (arguments, Vec::new()),
    };
    let (options, key) = take_key(options)?;
    let wait = options.iter().any(|a| a == "--wait");
    let from_stdin = options.iter().any(|a| a == "--stdin");
    let mut words: Vec<String> = options
        .into_iter()
        .filter(|a| a != "--wait" && a != "--stdin")
        .collect();
    if let Some(flag) = words.iter().find(|a| a.starts_with("--")) {
        return Err(format!("unknown {command} option {flag} (task --help)").into());
    }
    words.extend(literal);
    let text = if from_stdin {
        if !words.is_empty() {
            return Err(usage.into());
        }
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        buffer
    } else {
        words.join(" ")
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(usage.into());
    }

    let Some(device_id) = host.local_device_id() else {
        return refuse(
            "task.submit",
            "this computer has no device id yet; open the application on it once, then try again",
            json!({"kind": kind}),
        );
    };
    let utterance_id = host.random_uuid();
    // `--key` names the task for a resend; without it this call mints a key and prints it.
    let idempotency_key = key.unwrap_or_else(|| format!("{command}:{utterance_id}:1"));
    let envelope = TaskEnvelope {
        schema_version: TASK_ENVELOPE_SCHEMA_VERSION,
        task_id: String::new(),
        kind: kind.to_string(),
        conversation_id: utterance_id.clone(),
        utterance: UtteranceRef {
            schema_version: UTTERANCE_REF_SCHEMA_VERSION,
            utterance_id: utterance_id.clone(),
            generation: 1,
        },
        origin: Principal {
            schema_version: PRINCIPAL_SCHEMA_VERSION,
            kind: PrincipalKind::Owner,
            subject: ASK_OWNER.to_string(),
            connection: Some(device_id.clone()),
        },
        context_refs: Vec::new(),
        authority: None,
        intent: Intent {
            schema_version: INTENT_SCHEMA_VERSION,
            source: IntentSource::Direct {
                chat,
                device: device_id.clone(),
                message_id: format!("{utterance_id}#1"),
                quote: text,
            },
        },
        target_device: DeviceRef {
            schema_version: DEVICE_REF_SCHEMA_VERSION,
            device_id: device_id.clone(),
            platform: local_platform(),
            display_name: None,
        },
        idempotency_key: idempotency_key.clone(),
        deadline_ms: None,
        policy_ref: policy.to_string(),
        owner_epoch: 0,
        status: TaskStatus::Accepted,
        checkpoint_ref: None,
        output_refs: Vec::new(),
    };
    let detail = json!({
        "validated": true,
        "kind": kind,
        "idempotency_key": envelope.idempotency_key,
        "target_device": device_id,
    });
    let params = if acting {
        agent_act_params()
    } else {
        agent_request_params()
    };
    let mut answer = forward_submit(host, &envelope, kind, params, detail)?;
    // The receipt always names the key, so a caller can resend it after an unknown outcome.
    if let Value::Object(map) = &mut answer {
        map.entry("idempotency_key")
            .or_insert_with(|| json!(idempotency_key));
    }
    if !wait {
        println!("{answer}");
        return Ok(());
    }
    let task_id = answer
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or("the engine's receipt has no task_id")?
        .to_string();
    eprintln!(
        "{}",
        json!({"accepted": true, "task_id": task_id, "idempotency_key": idempotency_key})
    );
    wait_for(host, &task_id, &idempotency_key)
}

/// Takes `--key <idempotency-key>` out of the ask options. The value is trimmed and must not be
/// empty or look like another flag. A second `--key` is refused.
fn take_key(options: Vec<String>) -> Result<(Vec<String>, Option<String>), CliError> {
    let mut rest = Vec::with_capacity(options.len());
    let mut key: Option<String> = None;
    let mut remaining = options.into_iter();
    while let Some(arg) = remaining.next() {
        if arg != "--key" {
            rest.push(arg);
            continue;
        }
        if key.is_some() {
            return Err("--key is given more than once (task --help)".into());
        }
        match remaining.next() {
            Some(value) if !value.trim().is_empty() && !value.starts_with("--") => {
                key = Some(value.trim().to_string());
            }
            _ => return Err(KEY_USAGE.into()),
        }
    }
    Ok((rest, key))
}

/// `ask --wait`: polls `task.status` until the task is terminal. Each poll also reads the lifecycle
/// events past the cursor (`task.events`, `after`) onto stderr. Then it prints `task.result` on
/// stdout. Exit 0 when the task succeeded, 2 when it ended otherwise, 3 when it is still running
/// after `WAIT_LIMIT`.
fn wait_for(host: &dyn TaskHost, task_id: &str, idempotency_key: &str) -> Result<(), CliError> {
    let started = Instant::now();
    let mut after = 0_i64;
    loop {
        let status = host
            .call("task.status", json!({"taskId": task_id}))
            .map_err(|failure| failed("task.status", failure, json!({"task_id": task_id})))?;
        let events = host
            .call("task.events", json!({"taskId": task_id, "after": after}))
            .map_err(|failure| {
                failed(
                    "task.events",
                    failure,
                    json!({"task_id": task_id, "after": after}),
                )
            })?;
        for event in events
            .get("events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            eprintln!("{event}");
        }
        after = events.get("next").and_then(Value::as_i64).unwrap_or(after);
        if status.get("terminal").and_then(Value::as_bool) == Some(true) {
            let result = host
                .call("task.result", json!({"taskId": task_id}))
                .map_err(|failure| failed("task.result", failure, json!({"task_id": task_id})))?;
            println!("{result}");
            let succeeded = result.get("status").and_then(Value::as_str) == Some("succeeded");
            return if succeeded {
                Ok(())
            } else {
                Err(CliError {
                    body: None,
                    exit: 2,
                })
            };
        }
        if started.elapsed() >= WAIT_LIMIT {
            let reason = "still running after 10 minutes; check it with task status <task-id>";
            println!(
                "{}",
                receipt(
                    "task.status",
                    "unknown",
                    reason,
                    json!({"task_id": task_id, "idempotency_key": idempotency_key}),
                )
            );
            return Err(CliError {
                body: None,
                exit: 3,
            });
        }
        std::thread::sleep(WAIT_POLL);
    }
}

fn local_platform() -> DevicePlatform {
    if cfg!(target_os = "macos") {
        DevicePlatform::Mac
    } else if cfg!(target_os = "windows") {
        DevicePlatform::Windows
    } else {
        DevicePlatform::Linux
    }
}

/// `status`, `result` and `cancel`: one task id, forwarded to the method of the same name.
fn task_call(host: &dyn TaskHost, command: &str, arguments: Vec<String>) -> Result<(), CliError> {
    let method = format!("task.{command}");
    let task_id = single_task_id(command, &arguments)?;
    match host.call(&method, json!({ "taskId": task_id })) {
        Ok(answer) => {
            println!("{answer}");
            Ok(())
        }
        Err(failure) => Err(failed(&method, failure, json!({ "task_id": task_id }))),
    }
}

fn events(host: &dyn TaskHost, arguments: Vec<String>) -> Result<(), CliError> {
    let mut arguments = arguments;
    let after = match take_option(&mut arguments, "--after")? {
        None => 0,
        Some(text) => text
            .parse::<i64>()
            .map_err(|_| "--after takes a whole number (task --help)".to_string())?,
    };
    let task_id = single_task_id("events", &arguments)?;
    let params = json!({ "taskId": task_id, "after": after });
    match host.call("task.events", params) {
        Ok(answer) => {
            println!("{answer}");
            Ok(())
        }
        Err(failure) => Err(failed(
            "task.events",
            failure,
            json!({ "task_id": task_id, "after": after }),
        )),
    }
}

/// `list` has no engine method. The journal's own summary is `task.journal`.
fn list() -> Result<(), CliError> {
    let reason = "the engine has no task.list; task.journal reports the journal's schema, owner epoch, admission and counts, and task.status or task.events read one task";
    println!(
        "{}",
        receipt("task.list", "unsupported", reason, Value::Null)
    );
    Err(CliError {
        body: None,
        exit: 2,
    })
}

fn single_task_id(command: &str, arguments: &[String]) -> Result<String, CliError> {
    match arguments {
        [id] if !id.trim().is_empty() => Ok(id.clone()),
        _ => Err(format!("usage: task {command} <task-id> (task --help)").into()),
    }
}

/// Removes `name value` from the arguments and returns the value. A flag with no value is an error.
fn take_option(arguments: &mut Vec<String>, name: &str) -> Result<Option<String>, CliError> {
    let Some(index) = arguments.iter().position(|a| a == name) else {
        return Ok(None);
    };
    if index + 1 >= arguments.len() {
        return Err(format!("{name} needs a value (task --help)").into());
    }
    let value = arguments.remove(index + 1);
    arguments.remove(index);
    Ok(Some(value))
}

/// One receipt: `method`, `status`, `reason`, and the extra fields of `detail` (an object or null).
fn receipt(method: &str, status: &str, reason: &str, detail: Value) -> Value {
    let mut answer = json!({"method": method, "status": status, "reason": reason});
    if let (Value::Object(base), Value::Object(extra)) = (&mut answer, detail) {
        base.extend(extra);
    }
    answer
}

/// A refusal the CLI makes before anything is sent: exit 2.
fn refuse(method: &str, reason: &str, detail: Value) -> Result<(), CliError> {
    println!("{}", receipt(method, "refused", reason, detail));
    Err(CliError {
        body: None,
        exit: 2,
    })
}

/// Prints the receipt for an engine failure and returns the exit: 2 for a refusal, 3 for unknown.
fn failed(method: &str, failure: Failure, detail: Value) -> CliError {
    let (status, exit, reason, code) = match failure {
        Failure::Unreachable(message) => ("unreachable", 3, message, None),
        Failure::Unknown(message) => ("unknown", 3, message, None),
        Failure::Refused { code, message } => ("refused", 2, message, Some(code)),
    };
    let mut detail = detail;
    if let (Some(code), Value::Object(map)) = (code, &mut detail) {
        map.insert("code".to_string(), json!(code));
    }
    println!("{}", receipt(method, status, &reason, detail));
    CliError { body: None, exit }
}

/// Reads a voice hand-off (recognised by its `mode` field) or a task envelope, and checks it.
fn parse(host: &dyn TaskHost, text: &str) -> Result<(&'static str, TaskEnvelope), String> {
    let value: Value = serde_json::from_str(text).map_err(|error| format!("not JSON: {error}"))?;
    if value.get("mode").is_some() {
        let handoff: VoiceHandoff = serde_json::from_value(value)
            .map_err(|error| format!("invalid voice hand-off: {error}"))?;
        let envelope = handoff
            .to_task_envelope(&host.voice_labels())
            .map_err(|error| error.to_string())?;
        return Ok(("voice-handoff", envelope));
    }
    let envelope: TaskEnvelope =
        serde_json::from_value(value).map_err(|error| format!("invalid task envelope: {error}"))?;
    if envelope.schema_version != TASK_ENVELOPE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported task envelope schema version {}",
            envelope.schema_version
        ));
    }
    if envelope.idempotency_key.trim().is_empty() {
        return Err("task envelope idempotency_key is empty".to_string());
    }
    if envelope.utterance.utterance_id.trim().is_empty() {
        return Err("task envelope utterance_id is empty".to_string());
    }
    if envelope.target_device.device_id.trim().is_empty() {
        return Err("task envelope target_device.device_id is empty".to_string());
    }
    Ok(("task-envelope", envelope))
}
