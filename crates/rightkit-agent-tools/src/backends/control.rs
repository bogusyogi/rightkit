//! Tools for one Right Suite app, reached through its in-app control service
//! (`rightkit-control`'s `embedded` server, a `rightkit-daemon` host named
//! `control-<app>`). Real webview input, DOM/AX query, screenshot, and the app's
//! own typed commands (`command{name,args}`), all over the user-only socket/pipe
//! with the service's per-app request allowlist on the far side.

use crate::exposure::{self, GENERIC_READ_METHODS};
use crate::spec::*;
use rightkit_daemon::{Client, ServiceError, Suite};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// The control methods of the in-app server (`rightkit_control::embedded::METHODS`).
pub const CONTROL_METHODS: &[&str] = &[
    "health",
    "click",
    "move",
    "drag",
    "wheel",
    "key",
    "type",
    "eval",
    "dom",
    "ax",
    "screenshot",
    "command",
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
        }
    }

    /// The built-in profile for `app`: HeardRight is sensitive, everything else standard.
    pub fn for_app(app: &str) -> Self {
        match app {
            "heardright" => Self::sensitive(
                "heardright",
                &["health", "version", "list_models", "model_status"],
            ),
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
            "Real click in the webview (viewport CSS pixels, origin top-left).",
            Effect::Act,
            json!({"type": "object", "properties": {"x": num("x"), "y": num("y"), "button": {"type": "string", "enum": ["left", "right"]}, "count": {"type": "integer"}}, "required": ["x", "y"]}),
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
            "Screenshot of the webview (password and secret elements are masked). Returns {path,bytes,masked}.",
            Effect::Capture,
            json!({"type": "object", "properties": {"mask": {"type": "array"}}}),
        ),
        "command" => (
            "Run one of the app's own typed commands by name; `args` is the command's argument string.",
            Effect::Act,
            json!({"type": "object", "properties": {"name": {"type": "string"}, "args": {"type": "string"}}, "required": ["name"]}),
        ),
        _ => ("", Effect::Act, json!({"type": "object"})),
    }
}

struct ControlTool {
    spec: ToolSpec,
    suite: Suite,
    profile: AppProfile,
    method: String,
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

    fn call(&self, _ctx: &CallCtx, args: Value) -> Result<Value, ToolError> {
        let params = if self.method == "command" {
            let name = args["name"].as_str().unwrap_or_default();
            let allowed = match &self.profile.commands {
                CommandPolicy::Any => true,
                CommandPolicy::Only(l) => l.iter().any(|c| c == name),
            };
            if !allowed || exposure::forbidden(name).is_some() {
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
        let client = Client::connect_with(
            &self.suite,
            &self.profile.service,
            CLIENT_APP,
            Some(Duration::from_secs(30)),
        )
        .map_err(map_err)?;
        client.call(&self.method, params).map_err(map_err)
    }
}

/// Tools for `profile` on `suite`: one per enabled control method, named `<app>.<method>`.
pub fn control_tools(suite: &Suite, profile: &AppProfile) -> Vec<Arc<dyn Tool>> {
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
            }) as Arc<dyn Tool>
        })
        .collect()
}
