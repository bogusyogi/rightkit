//! Step-plugin protocol v1 (ADR-0001 item 4, ruling D-b): how an app adds its own step kinds to a
//! rightkit-qa scenario without owning the loop.
//!
//! The runner owns the scenario: it starts the app's plugin binary once, inside the scenario's
//! tracker (so inside the job), sends it `setup`, one `step` per app step, `teardown` and
//! `shutdown`, and executes everything it asks to have started. The plugin supplies behaviour for
//! step kinds and nothing else; it cannot choose journeys, steps, expectations or verdicts.
//!
//! # Wire format
//!
//! JSON lines over the plugin's stdio, one JSON object per line, at most [`FRAME_LIMIT`] bytes.
//! **stdout is the protocol channel and nothing else**: a line that is not a JSON object is a
//! protocol violation. The plugin logs to stderr, which the runner captures in full to
//! `plugin.stderr.log` in the scenario's log directory (where `secret_absent` scans it).
//! Every object has a `type`. Fields not named here are ignored.
//!
//! Runner to plugin, plugin to runner, in order:
//!
//! 1. `hello`, runner first: `{type, protocol: 1, runner, runner_version, scenario}`. The plugin
//!    answers `hello`: `{type, protocol: 1, name, version, kinds: [..]}`. `kinds` are the step kinds
//!    it implements: lower-case letters, digits and `_`, none a runner built-in
//!    ([`crate::steps::BUILTIN_KINDS`]) or a core step kind ([`RESERVED_KINDS`], [`RESERVED_PREFIXES`]).
//! 2. Requests, runner to plugin, each with a fresh integer `id`:
//!    `setup` `{scenario: {name, tier, target, requires, tokens}, data_dir, log_dir, vars}`,
//!    `step` `{kind, params, vars}` (`params` already has `${...}` resolved, `vars` is the
//!    scenario's variables), `teardown` `{vars}`, `shutdown` `{}`.
//! 3. The plugin answers each request with exactly one of
//!    `result` `{id, value, set_vars, hold}` (`value` is what the step's `[[step.expect]]` checks run on and
//!    what `save` stores; `set_vars` is an optional object of scenario variables to set; `hold` is an
//!    optional object of label to secret value, or array of secret values, that the runner holds so
//!    `engine_log` with `forbid_held` proves they never reached the log) or
//!    `error` `{id, message}` (the step failed: an ordinary failed step).
//! 4. While a request is open the plugin may send any number of `spawn` requests
//!    `{id, op, ..}` (the plugin's own integer ids); the runner executes each (see
//!    [`crate::steps::spawn`]: `start`, `write`, `close_stdin`, `read`, `pid`, `wait`, `kill`) and
//!    answers `spawn_result` `{id, value}` or `{id, error}`. Requests run one at a time, so the plugin
//!    waits for each `spawn_result` before it continues.
//!
//! # What the runner does with a plugin message
//!
//! It reads `type`, `id`, `value`, `set_vars`, `hold` and `message`, and nothing else. A `verdict`, `status`,
//! `ok`, `pass` or `passed` field, at the top level of any message, changes nothing: whether a step
//! passes is decided by the runner, from `result` versus `error` and the scenario's own `expect`
//! checks. A plugin that crashes, closes its stdout, answers late, answers the wrong request, sends a
//! line that is not a JSON object, an unknown `type`, or `set_vars` naming a runner variable
//! ([`RESERVED_VARS`]) makes the step *errored*: the error text carries [`ERRORED_MARK`], and
//! [`is_errored`] tells a caller to report `errored` rather than `failed`. The plugin is then dead:
//! every later call is errored too, and teardown only kills it.
//!
//! # The plugin may not start processes
//!
//! A child the plugin starts itself is not registered with the scenario's tracker, and on Unix it gets
//! its own process group, so containment would not reach it. Long-lived services are built-in steps
//! (`start_daemon`); a one-off child goes through a `spawn` request. The survivor scan of the
//! supervisor finds a plugin that spawned directly, and the scenario fails.
//!
//! # Plugin side
//!
//! [`StepPlugin`] and [`serve_stdio`] are the other end of this protocol, for an app's plugin
//! binary: implement the trait, call `serve_stdio(&mut plugin)` from `main`, never print to stdout.
use crate::process::{settle_exit, terminate_bounded, RunnerOwner, Tracker};
use crate::steps::spawn::{SpawnEnv, SpawnTable};
use crate::util::{Error, Result};
use rightkit_child_process::{OwnedChild, OwnedCommand, WaitOutcome};
use rightkit_sidecar::{read_json_line, write_json_line_flush};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The protocol version this module speaks.
pub const PROTOCOL: u64 = 1;
/// Largest message either side sends.
pub const FRAME_LIMIT: usize = 8 * 1024 * 1024;
/// Marks an error text as `errored` (an infrastructure failure of the plugin) rather than `failed`.
/// Searched for anywhere in the text, because a step runner prefixes its own context.
pub const ERRORED_MARK: &str = "[errored]";
/// The environment variable the plugin process finds set to the protocol version.
pub const ENV_PROTOCOL: &str = "RIGHTKIT_QA_STEP_PLUGIN";
/// Longest the plugin may take to answer `hello`, unless the spec says otherwise.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest one request may stay open, unless the step names `timeout_s`.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(120);
const SHUTDOWN_REPLY: Duration = Duration::from_secs(10);
const SHUTDOWN_EXIT_WAIT: Duration = Duration::from_secs(5);
/// Longest `error` message kept.
const MESSAGE_MAX: usize = 4000;

/// Step kinds the core executor owns. A plugin may not claim them. Kept in step with the `exec`
/// match in `scenario.rs`; a kind missing here is still shadowed by the core arm (core kinds are tried
/// first), it just would not be refused at `hello`.
pub const RESERVED_KINDS: [&str; 15] = [
    "cli",
    "engine_seed",
    "cold_cli",
    "restart",
    "kill",
    "parity",
    "write_wav",
    "write_file",
    "file_stat",
    "wav_stats",
    "png_size",
    "ffprobe",
    "set",
    "sleep",
    "assert_no_orphans",
];
/// Step-kind families the core executor owns.
pub const RESERVED_PREFIXES: [&str; 4] = ["ui_", "browser_", "sidecar_", "mock_"];
/// Scenario variables the runner sets; a plugin may not overwrite them (nor `build`, see
/// [`is_build_var`]).
pub const RESERVED_VARS: [&str; 4] = ["data_dir", "evidence_dir", "app_root", "engine"];

/// `build` and `build.*` are the runner's record of the binaries it built (`${build.<role>}`); no
/// plugin `set_vars` and no scenario `save` may repoint them.
pub fn is_build_var(name: &str) -> bool {
    name == "build" || name.starts_with("build.")
}

/// An infrastructure failure of the plugin, reported `errored`.
pub fn errored(what: impl Into<String>) -> Error {
    Error(format!("{ERRORED_MARK} step plugin {}", what.into()))
}

/// Whether `e` is an infrastructure failure (a plugin crash, hang or protocol violation) that the
/// caller reports `errored` instead of `failed`.
pub fn is_errored(e: &Error) -> bool {
    e.0.contains(ERRORED_MARK)
}

/// How to start an app's plugin.
#[derive(Debug, Clone)]
pub struct PluginSpec {
    pub binary: PathBuf,
    pub args: Vec<String>,
    /// Added to the plugin's environment after the world's.
    pub env: Vec<(String, String)>,
    pub hello_timeout: Duration,
}

impl PluginSpec {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            args: Vec::new(),
            env: Vec::new(),
            hello_timeout: HELLO_TIMEOUT,
        }
    }
}

/// What a plugin handed back for one request: the step's value and the variables to set. Nothing
/// else in the plugin's message reaches the caller.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    pub value: Value,
    pub set_vars: BTreeMap<String, Value>,
    /// Secrets the step registered (label, value), in the order sent. The runner adds them to the
    /// scenario's held secrets, the set `engine_log` `forbid_held` checks.
    pub hold: Vec<(String, String)>,
}

/// One line read from the plugin: a message, or the reason the stream broke.
enum Frame {
    Msg(Value),
    Bad(String),
}

fn spawn_reader(reader: Box<dyn Read + Send>) -> Receiver<Frame> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            match read_json_line::<_, Value>(&mut reader, &mut buf, FRAME_LIMIT) {
                Ok(Some(v)) => {
                    if tx.send(Frame::Msg(v)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = tx.send(Frame::Bad(e.to_string()));
                    break;
                }
            }
        }
    });
    rx
}

fn valid_kind(kind: &str) -> std::result::Result<(), String> {
    if kind.is_empty()
        || kind.len() > 64
        || !kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(format!(
            "declared the step kind {kind:?}: kinds are 1 to 64 lower-case letters, digits or `_`"
        ));
    }
    if crate::steps::BUILTIN_KINDS.contains(&kind) {
        return Err(format!(
            "declared the step kind `{kind}`, which is a runner built-in"
        ));
    }
    if RESERVED_KINDS.contains(&kind) || RESERVED_PREFIXES.iter().any(|p| kind.starts_with(*p)) {
        return Err(format!(
            "declared the step kind `{kind}`, which the core executor owns"
        ));
    }
    Ok(())
}

fn valid_var_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "${}".contains(c))
}

/// A running step plugin, as the runner sees it.
pub struct Plugin {
    name: String,
    version: String,
    kinds: BTreeSet<String>,
    writer: Option<Box<dyn Write + Send>>,
    rx: Receiver<Frame>,
    child: Option<OwnedChild>,
    next_id: u64,
    /// Why the plugin can no longer be called (set once, never cleared).
    dead: Option<String>,
    exit_seen: bool,
    spawns: SpawnTable,
    stderr_log: Option<PathBuf>,
    /// The scenario's tracker, for a plugin started by [`Plugin::start`]: what the plugin leaves
    /// running when it exits is recorded here before it is ended.
    tracker: Option<Tracker>,
}

impl Plugin {
    fn new(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        child: Option<OwnedChild>,
    ) -> Self {
        Self {
            name: String::new(),
            version: String::new(),
            kinds: BTreeSet::new(),
            writer: Some(writer),
            rx: spawn_reader(reader),
            child,
            next_id: 0,
            dead: None,
            exit_seen: false,
            spawns: SpawnTable::default(),
            stderr_log: None,
            tracker: None,
        }
    }

    /// Start the plugin binary through the scenario's tracker and complete the `hello` exchange. The
    /// plugin gets the world environment (so the survivor scan finds anything it leaves), then
    /// [`ENV_PROTOCOL`], then the spec's own `env`; its stderr is captured to `plugin.stderr.log`.
    /// On any failure the process is killed and forgotten before the error returns.
    pub fn start(spec: &PluginSpec, env: &SpawnEnv, scenario: &str) -> Result<Plugin> {
        let stderr_log = env.log_dir.join("plugin.stderr.log");
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_log)
            .map_err(|e| {
                errored(format!(
                    "could not start: create {}: {e}",
                    stderr_log.display()
                ))
            })?;
        // windowless: handed to OwnedCommand::from_command below, which hides the console
        let mut cmd = Command::new(&spec.binary);
        cmd.args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr));
        for (k, v) in env.extra_env {
            cmd.env(k, v);
        }
        cmd.env(ENV_PROTOCOL, PROTOCOL.to_string());
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut owned = OwnedCommand::from_command(cmd);
        owned.windows_hide();
        // A plugin that cannot be started is an infrastructure failure, not a failed step: the
        // scenario is `errored`.
        let mut child = owned
            .spawn()
            .map_err(|e| errored(format!("failed to start ({}): {e}", spec.binary.display())))?;
        let pid = child.id();
        env.tracker
            .register_owned(pid, "step plugin", RunnerOwner::Plugin);
        let (stdin, stdout) = (child.take_stdin(), child.take_stdout());
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            let _ = terminate_bounded(&mut child);
            env.tracker.forget(pid);
            return Err(errored("has no stdin or stdout pipe"));
        };
        let mut plugin = Plugin::new(Box::new(stdout), Box::new(stdin), Some(child));
        plugin.stderr_log = Some(stderr_log);
        plugin.tracker = Some(env.tracker.clone());
        match plugin.hello(scenario, spec.hello_timeout) {
            Ok(()) => Ok(plugin),
            Err(e) => {
                plugin.abort(env);
                Err(e)
            }
        }
    }

    /// A plugin on an existing pair of streams (no process): the runner side of the same protocol,
    /// for hosts that embed a plugin and for fixtures.
    pub fn attach(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        scenario: &str,
        hello_timeout: Duration,
    ) -> Result<Plugin> {
        let mut plugin = Plugin::new(reader, writer, None);
        plugin.hello(scenario, hello_timeout)?;
        Ok(plugin)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// The step kinds the plugin declared.
    pub fn kinds(&self) -> Vec<String> {
        self.kinds.iter().cloned().collect()
    }

    pub fn handles(&self, kind: &str) -> bool {
        self.kinds.contains(kind)
    }

    /// Why the plugin can no longer be called, when it cannot.
    pub fn dead_reason(&self) -> Option<&str> {
        self.dead.as_deref()
    }

    /// Pids of spawned children not yet reaped.
    pub fn live_spawned(&self) -> Vec<u32> {
        self.spawns.live_pids()
    }

    /// Engine processes the plugin started with `spawn` `engine: true` since the last call, one
    /// description each. The step layer counts them as undeclared engine starts of the scenario.
    pub fn take_engine_launches(&mut self) -> Vec<crate::steps::spawn::EngineNote> {
        self.spawns.take_engine_launches()
    }

    /// Record that the plugin can no longer be used and describe why, with its exit status and
    /// stderr tail when they are known.
    fn die(&mut self, why: impl Into<String>) -> Error {
        let mut why = why.into();
        // The plugin is finished either way: give it a moment to exit on its own, then end the
        // tree (descendants too, before the leader is reaped).
        if let Some(child) = self.child.as_mut() {
            // What a plugin that already exited left running is recorded before it is ended.
            let ended = match self.tracker.as_ref() {
                Some(tracker) => {
                    settle_exit(child, "step plugin", tracker, Duration::from_millis(300))
                }
                None => child.wait_then_kill(Duration::from_millis(300)),
            };
            match ended {
                Ok(WaitOutcome::Exited(status)) => {
                    why.push_str(&format!(" (exit status {status})"));
                }
                Ok(WaitOutcome::Terminated(_)) => {
                    why.push_str(" (it was still running and was killed)");
                }
                Err(e) => why.push_str(&format!(" (could not stop it: {e})")),
            }
            self.exit_seen = true;
        }
        if let Some(log) = &self.stderr_log {
            let text = std::fs::read_to_string(log).unwrap_or_default();
            let tail = crate::util::tail(text.trim(), 600);
            if !tail.is_empty() {
                why.push_str(&format!("; stderr tail: {tail}"));
            }
        }
        self.dead = Some(why.clone());
        errored(why)
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let sent = match self.writer.as_mut() {
            Some(w) => write_json_line_flush(w, message, FRAME_LIMIT).map_err(|e| e.to_string()),
            None => Err("its stdin is closed".to_string()),
        };
        sent.map_err(|why| self.die(format!("could not be written to: {why}")))
    }

    fn recv(&mut self, deadline: Instant) -> Result<Map<String, Value>> {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(left) {
            Ok(Frame::Msg(Value::Object(m))) => Ok(m),
            Ok(Frame::Msg(_)) => Err(self.die("sent a message that is not a JSON object")),
            Ok(Frame::Bad(why)) => Err(self.die(format!("broke the protocol on stdout: {why}"))),
            Err(RecvTimeoutError::Timeout) => Err(self.die("did not answer in time")),
            Err(RecvTimeoutError::Disconnected) => {
                Err(self.die("closed its stdout (it crashed or exited)"))
            }
        }
    }

    fn hello(&mut self, scenario: &str, timeout: Duration) -> Result<()> {
        self.send(&json!({
            "type": "hello",
            "protocol": PROTOCOL,
            "runner": "rightkit-qa",
            "runner_version": env!("CARGO_PKG_VERSION"),
            "scenario": scenario,
        }))?;
        let msg = self.recv(Instant::now() + timeout)?;
        match Self::read_hello(&msg) {
            Ok((name, version, kinds)) => {
                self.name = name;
                self.version = version;
                self.kinds = kinds;
                Ok(())
            }
            Err(why) => Err(self.die(why)),
        }
    }

    fn read_hello(
        msg: &Map<String, Value>,
    ) -> std::result::Result<(String, String, BTreeSet<String>), String> {
        if msg.get("type").and_then(Value::as_str) != Some("hello") {
            return Err("did not answer `hello` first".to_string());
        }
        let protocol = msg.get("protocol").and_then(Value::as_u64);
        if protocol != Some(PROTOCOL) {
            return Err(format!(
                "speaks protocol {protocol:?}, this runner speaks {PROTOCOL}"
            ));
        }
        let name = msg
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| "sent a `hello` without a `name`".to_string())?
            .to_string();
        let version = msg
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let declared = msg
            .get("kinds")
            .and_then(Value::as_array)
            .ok_or_else(|| "sent a `hello` without a `kinds` array".to_string())?;
        let mut kinds = BTreeSet::new();
        for item in declared {
            let kind = item
                .as_str()
                .ok_or_else(|| "sent a `kinds` entry that is not a string".to_string())?;
            valid_kind(kind)?;
            kinds.insert(kind.to_string());
        }
        Ok((name, version, kinds))
    }

    /// Answer one `spawn` request: run it, tracked, and send the `spawn_result`.
    fn serve_spawn(&mut self, env: &SpawnEnv, msg: &Map<String, Value>) -> Result<()> {
        let Some(id) = msg
            .get("id")
            .filter(|v| v.is_u64() || v.is_string())
            .cloned()
        else {
            return Err(self.die("sent a `spawn` request without an `id`"));
        };
        let reply = match self.spawns.handle(env, msg) {
            Ok(value) => json!({"type": "spawn_result", "id": id, "value": value}),
            Err(error) => json!({"type": "spawn_result", "id": id, "error": error}),
        };
        self.send(&reply)
    }

    fn parse_reply(&mut self, msg: &Map<String, Value>) -> Result<Reply> {
        let value = msg.get("value").cloned().unwrap_or(Value::Null);
        let mut set_vars = BTreeMap::new();
        match msg.get("set_vars") {
            None | Some(Value::Null) => {}
            Some(Value::Object(vars)) => {
                for (name, v) in vars {
                    if !valid_var_name(name) {
                        return Err(self.die(format!(
                            "tried to set the scenario variable {name:?}, which is not a usable name"
                        )));
                    }
                    if RESERVED_VARS.contains(&name.as_str()) || is_build_var(name) {
                        return Err(
                            self.die(format!("tried to set `{name}`, a variable the runner owns"))
                        );
                    }
                    set_vars.insert(name.clone(), v.clone());
                }
            }
            Some(_) => return Err(self.die("sent `set_vars` that is not an object")),
        }
        let hold = match msg.get("hold") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Object(held)) => {
                let mut out = Vec::new();
                for (label, v) in held {
                    let Some(secret) = v.as_str() else {
                        return Err(self.die(format!("sent `hold.{label}` that is not a string")));
                    };
                    out.push((label.clone(), secret.to_string()));
                }
                out
            }
            Some(Value::Array(held)) => {
                let mut out = Vec::new();
                for (n, v) in held.iter().enumerate() {
                    let Some(secret) = v.as_str() else {
                        return Err(self.die(format!("sent `hold[{n}]` that is not a string")));
                    };
                    out.push((format!("plugin hold {}", n + 1), secret.to_string()));
                }
                out
            }
            Some(_) => return Err(self.die("sent `hold` that is neither an object nor an array")),
        };
        Ok(Reply {
            value,
            set_vars,
            hold,
        })
    }

    /// One request and its answer, serving `spawn` requests in between. `error` is an ordinary
    /// failure; everything that breaks the protocol is errored.
    fn call(
        &mut self,
        env: &SpawnEnv,
        kind: &str,
        mut body: Map<String, Value>,
        timeout: Duration,
    ) -> Result<Reply> {
        if let Some(why) = &self.dead {
            return Err(errored(format!("is not running ({why})")));
        }
        self.next_id += 1;
        let id = self.next_id;
        body.insert("type".into(), json!(kind));
        body.insert("id".into(), json!(id));
        self.send(&Value::Object(body))?;
        let deadline = Instant::now() + timeout;
        loop {
            let msg = self.recv(deadline)?;
            match msg.get("type").and_then(Value::as_str) {
                Some("spawn") => self.serve_spawn(env, &msg)?,
                Some(answer @ ("result" | "error")) => {
                    if msg.get("id").and_then(Value::as_u64) != Some(id) {
                        return Err(self.die(format!(
                            "answered with a `{answer}` whose id is not the open request {id}"
                        )));
                    }
                    if answer == "error" {
                        let text = msg
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("the plugin reported an error without a message");
                        let text: String = text.chars().take(MESSAGE_MAX).collect();
                        return Err(Error(text));
                    }
                    return self.parse_reply(&msg);
                }
                Some(other) => {
                    return Err(self.die(format!(
                        "sent a `{other}` message while request {id} was open"
                    )))
                }
                None => return Err(self.die("sent a message without a `type`")),
            }
        }
    }

    /// `setup`: `body` is `{scenario, data_dir, log_dir, vars}`.
    pub fn setup(
        &mut self,
        env: &SpawnEnv,
        body: Map<String, Value>,
        timeout: Duration,
    ) -> Result<Reply> {
        self.call(env, "setup", body, timeout)
    }

    /// One app step. `params` has `${...}` resolved; `vars` is the scenario's variables.
    pub fn step(
        &mut self,
        env: &SpawnEnv,
        kind: &str,
        params: &Map<String, Value>,
        vars: &BTreeMap<String, Value>,
        timeout: Duration,
    ) -> Result<Reply> {
        let mut body = Map::new();
        body.insert("kind".into(), json!(kind));
        body.insert("params".into(), Value::Object(params.clone()));
        body.insert("vars".into(), json!(vars));
        self.call(env, "step", body, timeout)
    }

    pub fn teardown(
        &mut self,
        env: &SpawnEnv,
        vars: &BTreeMap<String, Value>,
        timeout: Duration,
    ) -> Result<Reply> {
        let mut body = Map::new();
        body.insert("vars".into(), json!(vars));
        self.call(env, "teardown", body, timeout)
    }

    /// Kill the process at once and forget it (the failure path of `start`).
    fn abort(&mut self, env: &SpawnEnv) {
        self.writer = None;
        if let Some(mut child) = self.child.take() {
            let pid = child.id();
            let _ = terminate_bounded(&mut child);
            env.tracker.forget(pid);
        }
        let _ = self.spawns.cleanup(env.tracker);
    }

    /// Ask the plugin to stop, close its stdin, wait bounded for it to exit (killing it when it does
    /// not), and kill any child it left running. Returns the problems found; empty means a clean stop.
    pub fn shutdown(&mut self, env: &SpawnEnv) -> Vec<String> {
        let mut problems = Vec::new();
        if self.dead.is_none() {
            if let Err(e) = self.call(env, "shutdown", Map::new(), SHUTDOWN_REPLY) {
                problems.push(format!("step plugin shutdown: {}", e.0));
            }
        }
        self.writer = None;
        if let Some(mut child) = self.child.take() {
            let pid = child.id();
            // A plugin that was already ended by `die` needs no wait; otherwise it gets a few seconds
            // to exit after its stdin closed, and the tree is killed when it does not.
            let clean = self.exit_seen
                || match settle_exit(&mut child, "step plugin", env.tracker, SHUTDOWN_EXIT_WAIT) {
                    Ok(WaitOutcome::Exited(_)) => true,
                    Ok(WaitOutcome::Terminated(_)) => false,
                    Err(_) => {
                        let (_, note) = terminate_bounded(&mut child);
                        problems.extend(note);
                        false
                    }
                };
            if !clean {
                problems.push(format!(
                    "the step plugin (pid {pid}) did not exit after shutdown and was killed"
                ));
            }
            env.tracker.forget(pid);
        }
        problems.extend(self.spawns.cleanup(env.tracker));
        if self.dead.is_none() {
            self.dead = Some("shut down".to_string());
        }
        problems
    }
}

// ---------------------------------------------------------------------------- plugin side

/// What the runner sent with `setup`.
#[derive(Debug, Clone, Default)]
pub struct SetupRequest {
    pub scenario: Value,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub vars: Map<String, Value>,
}

impl SetupRequest {
    fn from_message(msg: &Map<String, Value>) -> Self {
        let path = |key: &str| {
            msg.get(key)
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_default()
        };
        Self {
            scenario: msg.get("scenario").cloned().unwrap_or(Value::Null),
            data_dir: path("data_dir"),
            log_dir: path("log_dir"),
            vars: msg
                .get("vars")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        }
    }
}

/// A plugin's answer to `setup`, `step` or `teardown`: the value and variables to set.
#[derive(Debug, Clone, Default)]
pub struct StepOutput {
    pub value: Value,
    pub set_vars: BTreeMap<String, Value>,
    /// Secrets this step made or saw (label, value). The runner holds them, so `engine_log` with
    /// `forbid_held` proves none reached the engine log.
    pub hold: Vec<(String, String)>,
}

impl StepOutput {
    pub fn value(value: Value) -> Self {
        Self {
            value,
            set_vars: BTreeMap::new(),
            hold: Vec::new(),
        }
    }

    /// Register a secret for `engine_log` `forbid_held` (see [`StepOutput::hold`]).
    pub fn with_hold(mut self, label: &str, secret: &str) -> Self {
        self.hold.push((label.to_string(), secret.to_string()));
        self
    }

    pub fn with_var(mut self, name: &str, value: Value) -> Self {
        self.set_vars.insert(name.to_string(), value);
        self
    }
}

/// What a plugin binary implements. Return `Err(message)` for a failed step; the runner turns it
/// into an `error` message. A plugin has no way to say a step passed other than returning a value.
pub trait StepPlugin {
    fn name(&self) -> String;
    fn version(&self) -> String {
        "0".to_string()
    }
    /// The step kinds this plugin implements.
    fn kinds(&self) -> Vec<String>;
    fn setup(
        &mut self,
        _cx: &mut PluginCx<'_>,
        _req: &SetupRequest,
    ) -> std::result::Result<StepOutput, String> {
        Ok(StepOutput::default())
    }
    fn step(
        &mut self,
        cx: &mut PluginCx<'_>,
        kind: &str,
        params: &Map<String, Value>,
        vars: &Map<String, Value>,
    ) -> std::result::Result<StepOutput, String>;
    fn teardown(
        &mut self,
        _cx: &mut PluginCx<'_>,
        _vars: &Map<String, Value>,
    ) -> std::result::Result<StepOutput, String> {
        Ok(StepOutput::default())
    }
}

/// The plugin's handle on the runner during one request: the way to ask for a child process.
pub struct PluginCx<'a> {
    reader: &'a mut dyn BufRead,
    writer: &'a mut dyn Write,
    buf: &'a mut Vec<u8>,
    next_spawn: &'a mut u64,
}

impl PluginCx<'_> {
    /// One `spawn` request and its `spawn_result`. `fields` is an object of the operation's fields.
    pub fn spawn_op(&mut self, op: &str, fields: Value) -> std::result::Result<Value, String> {
        let mut msg = match fields {
            Value::Object(m) => m,
            Value::Null => Map::new(),
            _ => return Err("spawn fields must be an object".to_string()),
        };
        *self.next_spawn += 1;
        let id = *self.next_spawn;
        msg.insert("type".into(), json!("spawn"));
        msg.insert("id".into(), json!(id));
        msg.insert("op".into(), json!(op));
        write_json_line_flush(&mut self.writer, &Value::Object(msg), FRAME_LIMIT)
            .map_err(|e| format!("write the spawn request: {e}"))?;
        let answer = read_json_line::<_, Value>(&mut self.reader, self.buf, FRAME_LIMIT)
            .map_err(|e| format!("read the spawn result: {e}"))?
            .ok_or_else(|| "the runner closed the pipe before answering `spawn`".to_string())?;
        if answer.get("type").and_then(Value::as_str) != Some("spawn_result")
            || answer.get("id").and_then(Value::as_u64) != Some(id)
        {
            return Err(format!("expected the spawn_result for {id}, got {answer}"));
        }
        match answer.get("error").and_then(Value::as_str) {
            Some(error) => Err(error.to_string()),
            None => Ok(answer.get("value").cloned().unwrap_or(Value::Null)),
        }
    }

    /// `start` of the scenario's engine binary (`engine: true`), with the declaration `cold_reason`
    /// (one of [`crate::engine::COLD_REASONS`]) when the scenario's engine budget should count it as a
    /// declared cold start; `None` leaves it undeclared, which makes the scenario `errored`. `fields`
    /// are the other `start` fields (`args`, `env`, `cwd`, `stdin`, `label`). Returns the child id and pid.
    pub fn spawn_start_engine(
        &mut self,
        cold_reason: Option<&str>,
        fields: Value,
    ) -> std::result::Result<(String, u32), String> {
        let mut fields = match fields {
            Value::Object(m) => m,
            Value::Null => Map::new(),
            _ => return Err("spawn fields must be an object".to_string()),
        };
        fields.insert("engine".into(), json!(true));
        if let Some(reason) = cold_reason {
            fields.insert("cold_reason".into(), json!(reason));
        }
        self.spawn_start(Value::Object(fields))
    }

    /// `start`: returns the child id and pid.
    pub fn spawn_start(&mut self, fields: Value) -> std::result::Result<(String, u32), String> {
        let v = self.spawn_op("start", fields)?;
        let child = v
            .get("child")
            .and_then(Value::as_str)
            .ok_or_else(|| "the runner's start result has no `child`".to_string())?
            .to_string();
        let pid = v
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| "the runner's start result has no `pid`".to_string())?;
        Ok((child, pid))
    }

    pub fn spawn_write(&mut self, child: &str, data: &str) -> std::result::Result<Value, String> {
        self.spawn_op("write", json!({"child": child, "data": data}))
    }

    pub fn spawn_close_stdin(&mut self, child: &str) -> std::result::Result<Value, String> {
        self.spawn_op("close_stdin", json!({"child": child}))
    }

    /// `until` is `line`, `any` or `eof`.
    pub fn spawn_read(
        &mut self,
        child: &str,
        until: &str,
        timeout_ms: u64,
    ) -> std::result::Result<Value, String> {
        self.spawn_op(
            "read",
            json!({"child": child, "until": until, "timeout_ms": timeout_ms}),
        )
    }

    pub fn spawn_pid(&mut self, child: &str) -> std::result::Result<u32, String> {
        self.spawn_op("pid", json!({"child": child}))?
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| "the runner's pid result has no `pid`".to_string())
    }

    pub fn spawn_wait(
        &mut self,
        child: &str,
        timeout_ms: u64,
    ) -> std::result::Result<Value, String> {
        self.spawn_op("wait", json!({"child": child, "timeout_ms": timeout_ms}))
    }

    pub fn spawn_kill(&mut self, child: &str) -> std::result::Result<Value, String> {
        self.spawn_op("kill", json!({"child": child}))
    }
}

fn read_frame<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<Map<String, Value>>> {
    let bad = |why: String| std::io::Error::new(std::io::ErrorKind::InvalidData, why);
    match read_json_line::<_, Value>(reader, buf, FRAME_LIMIT) {
        Ok(None) => Ok(None),
        Ok(Some(Value::Object(m))) => Ok(Some(m)),
        Ok(Some(_)) => Err(bad("the runner sent a message that is not an object".into())),
        Err(e) => Err(bad(e.to_string())),
    }
}

fn write_frame<W: Write>(writer: &mut W, value: &Value) -> std::io::Result<()> {
    write_json_line_flush(writer, value, FRAME_LIMIT)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Serve `plugin` on `input`/`output` until the runner sends `shutdown` or closes the pipe.
pub fn serve<P: StepPlugin>(
    plugin: &mut P,
    input: impl Read,
    mut output: impl Write,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(input);
    let mut buf = Vec::new();
    let mut spawn_seq = 0u64;
    let Some(hello) = read_frame(&mut reader, &mut buf)? else {
        return Ok(());
    };
    if hello.get("type").and_then(Value::as_str) != Some("hello")
        || hello.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected a protocol {PROTOCOL} hello first"),
        ));
    }
    write_frame(
        &mut output,
        &json!({
            "type": "hello",
            "protocol": PROTOCOL,
            "name": plugin.name(),
            "version": plugin.version(),
            "kinds": plugin.kinds(),
        }),
    )?;
    loop {
        let Some(msg) = read_frame(&mut reader, &mut buf)? else {
            return Ok(());
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let kind = msg
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if kind == "shutdown" {
            write_frame(
                &mut output,
                &json!({"type": "result", "id": id, "value": null}),
            )?;
            return Ok(());
        }
        let outcome: std::result::Result<StepOutput, String> = {
            let mut cx = PluginCx {
                reader: &mut reader,
                writer: &mut output,
                buf: &mut buf,
                next_spawn: &mut spawn_seq,
            };
            let object = |key: &str| -> Map<String, Value> {
                msg.get(key)
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default()
            };
            match kind.as_str() {
                "setup" => plugin.setup(&mut cx, &SetupRequest::from_message(&msg)),
                "step" => {
                    let step_kind = msg.get("kind").and_then(Value::as_str).unwrap_or("");
                    plugin.step(&mut cx, step_kind, &object("params"), &object("vars"))
                }
                "teardown" => plugin.teardown(&mut cx, &object("vars")),
                other => Err(format!("unknown request type `{other}`")),
            }
        };
        let reply = match outcome {
            Ok(out) => {
                let hold: Map<String, Value> = out
                    .hold
                    .iter()
                    .map(|(label, secret)| (label.clone(), json!(secret)))
                    .collect();
                json!({"type": "result", "id": id, "value": out.value, "set_vars": out.set_vars, "hold": hold})
            }
            Err(message) => json!({"type": "error", "id": id, "message": message}),
        };
        write_frame(&mut output, &reply)?;
    }
}

/// [`serve`] on this process's stdin and stdout. The plugin must not print to stdout itself.
pub fn serve_stdio<P: StepPlugin>(plugin: &mut P) -> std::io::Result<()> {
    serve(plugin, std::io::stdin().lock(), std::io::stdout().lock())
}
