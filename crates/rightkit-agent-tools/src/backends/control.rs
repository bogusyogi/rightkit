//! Tools for one Right Suite app, reached through its in-app control service
//! (`rightkit-qa-driver`'s `embedded` server, a `rightkit-daemon` host named
//! `control-<app>`). Real webview input, DOM/AX query, screenshot, and the app's
//! own typed commands (`command{name,args}`), all over the user-only socket/pipe
//! with the service's per-app request allowlist on the far side.

use crate::exposure::{self, GENERIC_READ_METHODS};
use crate::spec::*;
use rightkit_daemon::{Client, ServiceError, Suite};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The control methods of the in-app server, in the server's own order
/// (`rightkit_qa_driver::embedded::METHODS`). A conformance test asserts this list
/// against the QA crate's dispatch table so the two cannot drift.
pub const CONTROL_METHODS: &[&str] = &[
    "health",
    "click",
    "move",
    "drag",
    "pointer",
    "wheel",
    "key",
    "type",
    "eval",
    "dom",
    "ax",
    "screenshot",
    "set_viewport",
    "focus",
    "command",
    "wait",
    "batch",
    "lease_acquire",
    "lease_release",
    "lease_reconcile",
    "lease_status",
];

/// The `app` label this crate's clients present to services (their allowlist key).
pub const CLIENT_APP: &str = "right-ctl";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPolicy {
    /// Any command the service itself allows (still subject to the hard exposure rules).
    Any,
    /// Only these command names.
    Only(Vec<String>),
}

/// Per-app client-side allowlist: which control methods and app commands agents
/// may use. The service enforces its own allowlist as well; this one is the
/// agent-facing layer on top.
#[derive(Debug, Clone)]
pub struct AppProfile {
    pub app: String,
    /// Service name under `<suite>/runtime/`, default `control-<app>`.
    pub service: String,
    pub methods: Vec<String>,
    pub commands: CommandPolicy,
    /// Sensitive apps (HeardRight) expose no generic read channel.
    pub sensitive: bool,
    /// Dictation apps (HeardRight) also refuse the dictation, delivery and history names
    /// ([`exposure::forbidden_with`]). Other apps keep those words.
    pub dictation: bool,
}

impl AppProfile {
    /// Every control method; any app command.
    pub fn standard(app: &str) -> Self {
        Self {
            app: app.into(),
            service: format!("control-{app}"),
            methods: CONTROL_METHODS.iter().map(|s| s.to_string()).collect(),
            commands: CommandPolicy::Any,
            sensitive: false,
            dictation: false,
        }
    }

    /// An app whose own data must not leave through generic channels: `health`
    /// and an explicit command list only. Commands the hard rules forbid are
    /// dropped from the list.
    pub fn sensitive(app: &str, commands: &[&str]) -> Self {
        let commands = commands
            .iter()
            .filter(|c| exposure::forbidden(c).is_none())
            .map(|c| c.to_string())
            .collect();
        Self {
            app: app.into(),
            service: format!("control-{app}"),
            methods: vec!["health".into(), "command".into()],
            commands: CommandPolicy::Only(commands),
            sensitive: true,
            dictation: false,
        }
    }

    /// Apply the dictation exposure layer to this app: dictation, delivery and history names
    /// are refused, and any listed command they cover is dropped.
    pub fn with_dictation_rules(mut self) -> Self {
        self.dictation = true;
        if let CommandPolicy::Only(names) = &mut self.commands {
            names.retain(|c| exposure::forbidden_with(c, true).is_none());
        }
        self
    }

    /// Whether the hard exposure rules refuse command `name` for this app.
    pub fn command_forbidden(&self, name: &str) -> bool {
        exposure::forbidden_with(name, self.dictation).is_some()
    }

    /// The built-in profile for `app`: HeardRight is sensitive, everything else standard.
    pub fn for_app(app: &str) -> Self {
        match app {
            "heardright" => Self::sensitive(
                "heardright",
                &["health", "version", "list_models", "model_status"],
            )
            .with_dictation_rules(),
            _ => Self::standard(app),
        }
    }

    pub fn with_methods(mut self, methods: &[&str]) -> Self {
        self.methods = methods.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn with_commands(mut self, policy: CommandPolicy) -> Self {
        self.commands = policy;
        self
    }

    /// Allowlist patterns (see `Toolbox::allow`) that grant exactly this profile.
    pub fn allow_patterns(&self) -> Vec<String> {
        let mut out = vec![];
        for m in &self.methods {
            if m == "command" {
                match &self.commands {
                    CommandPolicy::Any => out.push(format!("{}.command:*", self.app)),
                    CommandPolicy::Only(names) => {
                        out.extend(names.iter().map(|n| format!("{}.command:{n}", self.app)))
                    }
                }
            } else {
                out.push(format!("{}.{m}", self.app));
            }
        }
        out
    }

    fn method_enabled(&self, m: &str) -> bool {
        self.methods.iter().any(|x| x == m)
            && !(self.sensitive && GENERIC_READ_METHODS.contains(&m))
    }
}

fn num(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}

/// JSON schema + description of one control method.
fn method_schema(method: &str) -> (&'static str, Effect, Value) {
    match method {
        "health" => ("Is the app's control server up? Returns its status.", Effect::Observe, json!({"type": "object", "properties": {}})),
        "click" => (
            "Real click in the webview (viewport CSS pixels, origin top-left). Optional paceMs spaces the native events (default 0).",
            Effect::Act,
            json!({"type": "object", "properties": {"x": num("x"), "y": num("y"), "button": {"type": "string", "enum": ["left", "right"]}, "count": {"type": "integer"}, "paceMs": {"type": "integer"}}, "required": ["x", "y"]}),
        ),
        "move" => ("Move the pointer to x,y (hover).", Effect::Act, json!({"type": "object", "properties": {"x": num("x"), "y": num("y")}, "required": ["x", "y"]})),
        "drag" => (
            "Drag from (x1,y1) to (x2,y2).",
            Effect::Act,
            json!({"type": "object", "properties": {"x1": num("x1"), "y1": num("y1"), "x2": num("x2"), "y2": num("y2"), "steps": {"type": "integer"}}, "required": ["x1", "y1", "x2", "y2"]}),
        ),
        "wheel" => ("Scroll wheel at x,y by dx,dy.", Effect::Act, json!({"type": "object", "properties": {"x": num("x"), "y": num("y"), "dx": num("dx"), "dy": num("dy")}, "required": ["x", "y", "dx", "dy"]})),
        "key" => ("Press a key chord such as `cmd+k` or `enter`.", Effect::Act, json!({"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"]})),
        "type" => ("Type text into the focused element with real key events.", Effect::Act, json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})),
        "eval" => ("Evaluate JavaScript in the webview and return the result.", Effect::Act, json!({"type": "object", "properties": {"js": {"type": "string"}}, "required": ["js"]})),
        "dom" => ("Query DOM nodes by CSS selector.", Effect::Observe, json!({"type": "object", "properties": {"selector": {"type": "string"}}, "required": ["selector"]})),
        "ax" => ("Accessibility tree of the app window.", Effect::Observe, json!({"type": "object", "properties": {}})),
        "screenshot" => (
            "Screenshot of the webview (password and secret elements are masked). Returns {path,bytes,masked}, or with inline=true {bytes,masked,encoding:\"base64\",data} and no file.",
            Effect::Capture,
            json!({"type": "object", "properties": {"mask": {"type": "array"}, "inline": {"type": "boolean"}}}),
        ),
        "wait" => (
            "Wait in the page, in ONE call, until a JavaScript function body returns something truthy (or timeoutMs, default 10000). Returns {matched,value|last,error,elapsedMs}. Prefer this to polling eval.",
            Effect::Act,
            json!({"type": "object", "properties": {"js": {"type": "string"}, "timeoutMs": {"type": "integer"}, "intervalMs": {"type": "integer"}}, "required": ["js"]}),
        ),
        "batch" => (
            "Run many control steps in ONE call and stop at the first failure: steps=[{method,params}] with any of this app's methods except batch. Returns {completed,results,stepMs} or {ok:false,failedStep,error,results}. The way to drive an app: one call per step is slow.",
            Effect::Act,
            json!({"type": "object", "properties": {"steps": {"type": "array", "items": {"type": "object", "properties": {"method": {"type": "string"}, "params": {"type": "object"}}, "required": ["method"]}}}, "required": ["steps"]}),
        ),
        "command" => (
            "Run one of the app's own typed commands by name; `args` is the command's argument string.",
            Effect::Act,
            json!({"type": "object", "properties": {"name": {"type": "string"}, "args": {"type": "string"}}, "required": ["name"]}),
        ),
        "pointer" => (
            "One raw pointer step in the webview (viewport CSS pixels, origin top-left): phase down, drag, up or move at x,y. Optional button, modifiers and paceMs (native-event spacing, default 0).",
            Effect::Act,
            json!({"type": "object", "properties": {"phase": {"type": "string", "enum": ["down", "drag", "up", "move"]}, "x": num("x"), "y": num("y"), "button": {"type": "string", "enum": ["left", "right"]}, "modifiers": {"type": "array"}, "paceMs": {"type": "integer"}}, "required": ["phase", "x", "y"]}),
        ),
        "set_viewport" => (
            "Resize the webview viewport to width x height CSS pixels without activating the window; returns the size read back after settling (optional settleMs bound).",
            Effect::Act,
            json!({"type": "object", "properties": {"width": {"type": "integer"}, "height": {"type": "integer"}, "settleMs": {"type": "integer"}}, "required": ["width", "height"]}),
        ),
        "focus" => ("Focus the app window.", Effect::Act, json!({"type": "object", "properties": {}})),
        "lease_acquire" => (
            "Acquire the server's input lease; returns {leaseEpoch,nextInputSequence,fencedEpoch}. Optional reconcile (delivered|lost) first settles an unacknowledged input of a previous lease. The leaseEpoch is the ownership token lease_release and lease_reconcile must present.",
            Effect::Act,
            json!({"type": "object", "properties": {"reconcile": {"type": "string", "enum": ["delivered", "lost"]}}}),
        ),
        "lease_release" => (
            "Release the input lease held under leaseEpoch; a stale epoch is refused by the server.",
            Effect::Act,
            json!({"type": "object", "properties": {"leaseEpoch": {"type": "integer"}}, "required": ["leaseEpoch"]}),
        ),
        "lease_reconcile" => (
            "Reconcile the input lease held under leaseEpoch: decision delivered counts its unacknowledged input as delivered, lost as never landed.",
            Effect::Act,
            json!({"type": "object", "properties": {"leaseEpoch": {"type": "integer"}, "decision": {"type": "string", "enum": ["delivered", "lost"]}}, "required": ["leaseEpoch", "decision"]}),
        ),
        "lease_status" => ("Snapshot of the server's input lease (holder epoch and sequencing state).", Effect::Observe, json!({"type": "object", "properties": {}})),
        _ => ("", Effect::Act, json!({"type": "object"})),
    }
}

struct ControlTool {
    spec: ToolSpec,
    suite: Suite,
    profile: AppProfile,
    method: String,
    /// One connection shared by every tool of the profile (connecting costs a handshake per call).
    client: Arc<Mutex<Option<Arc<Client>>>>,
}

fn map_err(e: ServiceError) -> ToolError {
    ToolError {
        code: e.code,
        message: e.message,
        retryable: e.retryable,
    }
}

impl Tool for ControlTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn subject(&self, args: &Value) -> Option<String> {
        (self.method == "command").then(|| args["name"].as_str().unwrap_or("").to_string())
    }

    fn call(&self, ctx: &CallCtx, args: Value) -> Result<Value, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::new(ERR_CANCELLED, "cancelled before dispatch"));
        }
        if self.method == "batch" {
            check_batch(&self.profile, &args)?;
        }
        let params = if self.method == "command" {
            let name = args["name"].as_str().unwrap_or_default();
            let allowed = match &self.profile.commands {
                CommandPolicy::Any => true,
                CommandPolicy::Only(l) => l.iter().any(|c| c == name),
            };
            if !allowed || self.profile.command_forbidden(name) {
                return Err(ToolError::new(
                    ERR_NOT_ALLOWED,
                    format!("command `{name}` is not exposed for {}", self.profile.app),
                ));
            }
            let a = match &args["args"] {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            json!({"name": name, "args": a})
        } else {
            args
        };
        // `batch` and `wait` run longer than one step: give them the time they ask for.
        let long = match self.method.as_str() {
            "wait" => Some(
                Duration::from_millis(args_timeout(&params, "timeoutMs", 10_000))
                    + Duration::from_secs(10),
            ),
            "batch" => Some(Duration::from_secs(
                30 * params["steps"]
                    .as_array()
                    .map_or(1, |s| s.len().clamp(1, 20)) as u64,
            )),
            _ => None,
        };
        if let Some(t) = long {
            let client =
                Client::connect_with(&self.suite, &self.profile.service, CLIENT_APP, Some(t))
                    .map_err(map_err)?;
            return client.call(&self.method, params).map_err(map_err);
        }
        let client = {
            let mut slot = self.client.lock().unwrap_or_else(|e| e.into_inner());
            match slot.as_ref() {
                Some(c) => c.clone(),
                None => {
                    let c = Arc::new(
                        Client::connect_with(
                            &self.suite,
                            &self.profile.service,
                            CLIENT_APP,
                            Some(Duration::from_secs(30)),
                        )
                        .map_err(map_err)?,
                    );
                    *slot = Some(c.clone());
                    c
                }
            }
        };
        match client.call(&self.method, params) {
            Ok(v) => Ok(v),
            Err(e) => {
                // Reconnect next time: the connection may be the thing that failed.
                *self.client.lock().unwrap_or_else(|e| e.into_inner()) = None;
                Err(map_err(e))
            }
        }
    }
}

fn args_timeout(params: &Value, key: &str, default: u64) -> u64 {
    params[key].as_u64().unwrap_or(default).min(600_000)
}

/// `batch` is one call carrying many methods, so the profile's limits apply to each step: a
/// method the profile does not enable (a sensitive app's `eval`/`dom`/`ax`/`screenshot`), a
/// nested `batch`, or a `command` the command policy refuses is rejected before anything runs.
fn check_batch(profile: &AppProfile, args: &Value) -> Result<(), ToolError> {
    let steps = args["steps"]
        .as_array()
        .ok_or_else(|| ToolError::new(ERR_INVALID_ARGS, "batch needs a `steps` array"))?;
    for (i, step) in steps.iter().enumerate() {
        let m = step["method"].as_str().unwrap_or_default();
        if m == "batch" || !profile.method_enabled(m) {
            return Err(ToolError::new(
                ERR_NOT_ALLOWED,
                format!("batch step {i}: `{m}` is not exposed for {}", profile.app),
            ));
        }
        if m == "command" {
            let name = step["params"]["name"].as_str().unwrap_or_default();
            let allowed = match &profile.commands {
                CommandPolicy::Any => true,
                CommandPolicy::Only(l) => l.iter().any(|c| c == name),
            };
            if !allowed || profile.command_forbidden(name) {
                return Err(ToolError::new(
                    ERR_NOT_ALLOWED,
                    format!(
                        "batch step {i}: command `{name}` is not exposed for {}",
                        profile.app
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Tools for `profile` on `suite`: one per enabled control method, named `<app>.<method>`.
pub fn control_tools(suite: &Suite, profile: &AppProfile) -> Vec<Arc<dyn Tool>> {
    let client: Arc<Mutex<Option<Arc<Client>>>> = Arc::new(Mutex::new(None));
    CONTROL_METHODS
        .iter()
        .filter(|m| profile.method_enabled(m))
        .map(|m| {
            let (desc, effect, schema) = method_schema(m);
            let description = format!("{desc} (app: {})", profile.app);
            Arc::new(ControlTool {
                spec: ToolSpec::new(format!("{}.{m}", profile.app), description, effect, schema),
                suite: suite.clone(),
                profile: profile.clone(),
                method: m.to_string(),
                client: client.clone(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(methods: &[&str]) -> Value {
        json!({"steps": methods.iter().map(|m| json!({"method": m, "params": {}})).collect::<Vec<_>>()})
    }

    #[test]
    fn standard_profile_may_batch_its_methods_but_not_nest() {
        let p = AppProfile::standard("demo");
        assert!(p.methods.iter().any(|m| m == "batch") && p.methods.iter().any(|m| m == "wait"));
        assert!(check_batch(&p, &steps(&["click", "type", "screenshot", "wait"])).is_ok());
        assert!(check_batch(&p, &steps(&["batch"])).is_err());
        assert!(check_batch(&p, &json!({})).is_err());
    }

    #[test]
    fn the_seven_newer_methods_are_standard_with_schemas() {
        let p = AppProfile::standard("demo");
        for m in [
            "pointer",
            "set_viewport",
            "focus",
            "lease_acquire",
            "lease_release",
            "lease_reconcile",
            "lease_status",
        ] {
            assert!(CONTROL_METHODS.contains(&m), "{m}");
            assert!(p.method_enabled(m), "{m}");
            let (desc, _, schema) = method_schema(m);
            assert!(!desc.is_empty(), "{m}");
            assert_eq!(schema["type"], "object", "{m}");
        }
        assert!(check_batch(
            &p,
            &steps(&["pointer", "set_viewport", "focus", "lease_status"])
        )
        .is_ok());
        // Lease ownership is in the schemas: release/reconcile cannot omit the epoch.
        for m in ["lease_release", "lease_reconcile"] {
            let (_, _, schema) = method_schema(m);
            assert!(
                schema["required"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|v| v == "leaseEpoch")),
                "{m}"
            );
        }
        // A sensitive profile still gets none of them.
        let p = AppProfile::for_app("heardright");
        for m in ["pointer", "set_viewport", "focus", "lease_acquire"] {
            assert!(!p.method_enabled(m), "{m}");
        }
    }

    #[test]
    fn dictation_names_are_refused_only_for_a_dictation_profile() {
        let heard = AppProfile::for_app("heardright");
        assert!(heard.dictation);
        for name in ["history", "delivery_status", "start_dictation"] {
            assert!(heard.command_forbidden(name), "{name}");
        }
        let gen = AppProfile::standard("genright");
        assert!(!gen.dictation);
        for name in ["history", "delivery_status"] {
            assert!(!gen.command_forbidden(name), "{name}");
        }
        assert!(gen.command_forbidden("paste_text"));
        let listed = AppProfile::sensitive("x", &["history", "health"]).with_dictation_rules();
        assert!(
            matches!(&listed.commands, CommandPolicy::Only(l) if l == &vec!["health".to_string()])
        );
    }

    #[test]
    fn a_batch_cannot_smuggle_generic_reads_past_a_sensitive_profile() {
        let p = AppProfile::for_app("heardright");
        for m in ["eval", "dom", "ax", "screenshot", "wait", "batch", "click"] {
            assert!(!p.method_enabled(m), "{m}");
            assert!(check_batch(&p, &steps(&[m])).is_err(), "{m}");
        }
        let ok = json!({"steps": [{"method": "command", "params": {"name": "version"}}]});
        assert!(check_batch(&p, &ok).is_ok());
        let bad = json!({"steps": [{"method": "command", "params": {"name": "transcribe_all"}}]});
        assert!(check_batch(&p, &bad).is_err());
    }
}
