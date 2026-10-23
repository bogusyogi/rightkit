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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Tracker;
    use std::io::PipeReader;
    use std::io::PipeWriter;

    struct Fixture {
        dir: PathBuf,
        tracker: Tracker,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("rkqa-plugin-{}", crate::util::new_id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self {
                dir,
                tracker: Tracker::new(),
            }
        }

        fn env(&self) -> SpawnEnv<'_> {
            SpawnEnv {
                tracker: &self.tracker,
                engine: None,
                data_dir: &self.dir,
                log_dir: &self.dir,
                extra_env: &[],
                ui_binary: None,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.tracker.kill_all();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const LONG: Duration = Duration::from_secs(20);

    /// A runner-side plugin whose far end is the `serve` loop of `plugin`, on real OS pipes.
    fn pair<P: StepPlugin + Send + 'static>(plugin: P) -> Plugin {
        let (from_runner, to_plugin) = std::io::pipe().unwrap();
        let (from_plugin, to_runner) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let mut plugin = plugin;
            let _ = serve(&mut plugin, from_runner, to_runner);
        });
        Plugin::attach(Box::new(from_plugin), Box::new(to_plugin), "scn", LONG).unwrap()
    }

    /// A runner-side plugin whose far end is a hand-written script speaking raw lines.
    fn raw_attach(
        script: impl FnOnce(&mut BufReader<PipeReader>, &mut PipeWriter) + Send + 'static,
    ) -> Result<Plugin> {
        let (from_runner, to_plugin) = std::io::pipe().unwrap();
        let (from_plugin, to_runner) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(from_runner);
            let mut writer = to_runner;
            script(&mut reader, &mut writer);
        });
        Plugin::attach(
            Box::new(from_plugin),
            Box::new(to_plugin),
            "scn",
            Duration::from_secs(5),
        )
    }

    fn raw_pair(
        script: impl FnOnce(&mut BufReader<PipeReader>, &mut PipeWriter) + Send + 'static,
    ) -> Plugin {
        raw_attach(script).unwrap()
    }

    fn line(reader: &mut BufReader<PipeReader>) -> Value {
        let mut text = String::new();
        reader.read_line(&mut text).unwrap();
        serde_json::from_str(text.trim()).unwrap_or(Value::Null)
    }

    fn say(writer: &mut PipeWriter, text: &str) {
        writer.write_all(text.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
    }

    const HELLO_OK: &str =
        r#"{"type":"hello","protocol":1,"name":"raw","version":"1","kinds":["alpha","beta"]}"#;

    fn no_vars() -> BTreeMap<String, Value> {
        BTreeMap::new()
    }

    fn params(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[cfg(unix)]
    fn echo_cmd() -> (&'static str, Vec<&'static str>) {
        ("cat", vec![])
    }

    #[cfg(windows)]
    fn echo_cmd() -> (&'static str, Vec<&'static str>) {
        ("findstr", vec!["^"])
    }

    /// A plugin built on the SDK: records what it was told, echoes steps, fails or crashes on demand,
    /// and drives the runner's spawn API.
    #[derive(Default)]
    struct Fake {
        long: Option<String>,
        seen_setup: bool,
    }

    impl StepPlugin for Fake {
        fn name(&self) -> String {
            "fake".into()
        }
        fn kinds(&self) -> Vec<String> {
            [
                "echo",
                "fail",
                "crash",
                "spawn_probe",
                "spawn_kill",
                "spawn_leave",
                "set_reserved",
                "set_build",
            ]
            .iter()
            .map(|k| k.to_string())
            .collect()
        }
        fn setup(
            &mut self,
            _cx: &mut PluginCx<'_>,
            req: &SetupRequest,
        ) -> std::result::Result<StepOutput, String> {
            self.seen_setup = true;
            Ok(StepOutput::value(json!({
                "scenario": req.scenario["name"],
                "data_dir": req.data_dir.to_string_lossy(),
            }))
            .with_var("project_dir", json!("proj")))
        }
        fn step(
            &mut self,
            cx: &mut PluginCx<'_>,
            kind: &str,
            params: &Map<String, Value>,
            vars: &Map<String, Value>,
        ) -> std::result::Result<StepOutput, String> {
            match kind {
                "echo" => Ok(StepOutput::value(json!({
                    "params": params,
                    "vars": vars,
                    "setup_seen": self.seen_setup,
                }))),
                "fail" => Err("nope".into()),
                "crash" => panic!("the fake plugin crashes on purpose"),
                "set_reserved" => {
                    Ok(StepOutput::value(Value::Null).with_var("data_dir", json!("x")))
                }
                "set_build" => {
                    Ok(StepOutput::value(Value::Null)
                        .with_var("build.app", json!("/elsewhere/app")))
                }
                "spawn_probe" => {
                    let (program, args) = echo_cmd();
                    let (child, pid) = cx.spawn_start(json!({
                        "program": program, "args": args, "stdin": true, "label": "echo"
                    }))?;
                    cx.spawn_write(&child, "hello\n")?;
                    cx.spawn_close_stdin(&child)?;
                    let read = cx.spawn_read(&child, "line", 10_000)?;
                    let waited = cx.spawn_wait(&child, 10_000)?;
                    let pid_again = cx.spawn_pid(&child)?;
                    let (long, long_pid) = cx.spawn_start(json!({
                        "program": program, "args": args, "stdin": true, "label": "long"
                    }))?;
                    self.long = Some(long.clone());
                    let not_done = cx.spawn_wait(&long, 100)?;
                    Ok(StepOutput::value(json!({
                        "pid": pid, "pid_again": pid_again, "read": read, "waited": waited,
                        "long": long, "long_pid": long_pid, "long_wait": not_done,
                    })))
                }
                "spawn_kill" => {
                    let long = self.long.clone().ok_or("no long child")?;
                    let killed = cx.spawn_kill(&long)?;
                    let again = cx.spawn_kill(&long)?;
                    Ok(StepOutput::value(json!({"killed": killed, "again": again})))
                }
                "spawn_leave" => {
                    let (program, args) = echo_cmd();
                    let (_, pid) = cx.spawn_start(json!({
                        "program": program, "args": args, "stdin": true, "label": "left"
                    }))?;
                    Ok(StepOutput::value(json!({"pid": pid})))
                }
                other => Err(format!("unknown kind {other}")),
            }
        }
    }

    #[test]
    fn setup_step_and_teardown_round_trip_through_the_sdk() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut p = pair(Fake::default());
        assert_eq!(p.name(), "fake");
        assert!(p.handles("echo") && !p.handles("cli"));

        let mut body = Map::new();
        body.insert("scenario".into(), json!({"name": "demo", "tier": "fast"}));
        body.insert("data_dir".into(), json!("/data"));
        body.insert("log_dir".into(), json!("/log"));
        body.insert("vars".into(), json!({}));
        let setup = p.setup(&env, body, LONG).unwrap();
        assert_eq!(setup.value["scenario"], "demo");
        assert_eq!(setup.set_vars.get("project_dir"), Some(&json!("proj")));

        let mut vars = no_vars();
        vars.insert("project_dir".into(), json!("proj"));
        let echoed = p
            .step(
                &env,
                "echo",
                &params(json!({"a": 1, "b": ["x"]})),
                &vars,
                LONG,
            )
            .unwrap();
        assert_eq!(echoed.value["params"], json!({"a": 1, "b": ["x"]}));
        assert_eq!(echoed.value["vars"]["project_dir"], "proj");
        assert_eq!(echoed.value["setup_seen"], true);

        // An ordinary plugin failure is a failed step, and the plugin stays usable.
        let failed = p
            .step(&env, "fail", &params(json!({})), &vars, LONG)
            .unwrap_err();
        assert_eq!(failed.0, "nope");
        assert!(!is_errored(&failed));
        assert!(p
            .step(&env, "echo", &params(json!({})), &vars, LONG)
            .is_ok());

        assert!(p.teardown(&env, &vars, LONG).is_ok());
        assert_eq!(p.shutdown(&env), Vec::<String>::new());
        assert!(p
            .step(&env, "echo", &params(json!({})), &vars, LONG)
            .is_err());
    }

    #[test]
    fn verdict_fields_in_plugin_messages_are_ignored() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut p = raw_pair(|r, w| {
            let _ = line(r);
            say(w, HELLO_OK);
            let first = line(r);
            let id = first["id"].clone();
            // A result that claims to pass, with every verdict-shaped field set.
            say(
                w,
                &json!({"type": "result", "id": id, "value": {"x": 1}, "verdict": "passed",
                        "status": "passed", "ok": true, "pass": true, "passed": true,
                        "set_vars": {"made": 7}})
                .to_string(),
            );
            let second = line(r);
            let id = second["id"].clone();
            // An error that claims to be fine is still an error.
            say(
                w,
                &json!({"type": "error", "id": id, "message": "boom", "verdict": "passed",
                        "ok": true, "status": "passed"})
                .to_string(),
            );
            let third = line(r);
            let id = third["id"].clone();
            // A result that claims failure is still a result: the value is data for `expect`.
            say(
                w,
                &json!({"type": "result", "id": id, "value": null, "verdict": "failed",
                        "status": "failed", "ok": false, "pass": false})
                .to_string(),
            );
        });
        let first = p
            .step(&env, "alpha", &params(json!({})), &no_vars(), LONG)
            .unwrap();
        assert_eq!(first.value, json!({"x": 1}));
        assert_eq!(first.set_vars.get("made"), Some(&json!(7)));
        let second = p
            .step(&env, "alpha", &params(json!({})), &no_vars(), LONG)
            .unwrap_err();
        assert_eq!(second.0, "boom");
        assert!(!is_errored(&second));
        let third = p
            .step(&env, "alpha", &params(json!({})), &no_vars(), LONG)
            .unwrap();
        assert_eq!(third.value, Value::Null);
        assert!(third.set_vars.is_empty());
    }

    #[test]
    fn a_plugin_crash_is_errored_and_the_plugin_stays_dead() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut p = pair(Fake::default());
        let e = p
            .step(&env, "crash", &params(json!({})), &no_vars(), LONG)
            .unwrap_err();
        assert!(is_errored(&e), "{}", e.0);
        assert!(p.dead_reason().is_some());
        let again = p
            .step(&env, "echo", &params(json!({})), &no_vars(), LONG)
            .unwrap_err();
        assert!(is_errored(&again), "{}", again.0);
        // Shutting a dead plugin down reports nothing extra and does not hang.
        assert_eq!(p.shutdown(&env), Vec::<String>::new());
    }

    #[test]
    fn protocol_violations_are_errored() {
        let fx = Fixture::new();
        let env = fx.env();
        let step = |p: &mut Plugin| p.step(&env, "alpha", &params(json!({})), &no_vars(), LONG);

        // The answer carries another request's id.
        let mut p = raw_pair(|r, w| {
            let _ = line(r);
            say(w, HELLO_OK);
            let _ = line(r);
            say(w, r#"{"type":"result","id":999,"value":1}"#);
        });
        assert!(is_errored(&step(&mut p).unwrap_err()));

        // A line that is not JSON on the protocol channel.
        let mut p = raw_pair(|r, w| {
            let _ = line(r);
            say(w, HELLO_OK);
            let _ = line(r);
            say(w, "this is a log line, not a message");
        });
        assert!(is_errored(&step(&mut p).unwrap_err()));

        // JSON that is not an object.
        let mut p = raw_pair(|r, w| {
            let _ = line(r);
            say(w, HELLO_OK);
            let _ = line(r);
            say(w, "[1,2,3]");
        });
        assert!(is_errored(&step(&mut p).unwrap_err()));

        // A message type the protocol does not have.
        let mut p = raw_pair(|r, w| {
            let _ = line(r);
            say(w, HELLO_OK);
            let id = line(r)["id"].clone();
            say(
                w,
                &json!({"type": "verdict", "id": id, "value": "passed"}).to_string(),
            );
        });
        assert!(is_errored(&step(&mut p).unwrap_err()));

        // A plugin that sets a variable the runner owns.
        let mut p = pair(Fake::default());
        let e = p
            .step(&env, "set_reserved", &params(json!({})), &no_vars(), LONG)
            .unwrap_err();
        assert!(is_errored(&e) && e.0.contains("data_dir"), "{}", e.0);
    }

    /// `build` and `build.*` hold the runner's record of the binaries it built: a plugin that
    /// tries to repoint them is errored.
    #[test]
    fn a_plugin_that_sets_the_build_variable_is_errored() {
        assert!(is_build_var("build") && is_build_var("build.app"));
        assert!(!is_build_var("builder") && !is_build_var("rebuild.app"));
        let env_fx = Fixture::new();
        let env = env_fx.env();
        let mut p = pair(Fake::default());
        let e = p
            .step(&env, "set_build", &params(json!({})), &no_vars(), LONG)
            .unwrap_err();
        assert!(is_errored(&e) && e.0.contains("build.app"), "{}", e.0);
    }

    /// A command that prints the value of `RKQA_PROBE_HOME` on stdout / stderr.
    #[cfg(unix)]
    fn probe_cmd(stream: &str) -> (&'static str, Vec<String>) {
        let redirect = if stream == "stderr" { " >&2" } else { "" };
        (
            "sh",
            vec!["-c".into(), format!("echo $RKQA_PROBE_HOME{redirect}")],
        )
    }

    #[cfg(windows)]
    fn probe_cmd(stream: &str) -> (&'static str, Vec<String>) {
        let redirect = if stream == "stderr" { " 1>&2" } else { "" };
        (
            "cmd",
            vec!["/C".into(), format!("echo %RKQA_PROBE_HOME%{redirect}")],
        )
    }

    /// N4: a program the plugin spawns runs with the world workspace's environment, and a step
    /// `env` cannot repoint one of those variables.
    #[test]
    fn a_spawned_program_sees_the_worlds_isolation_env_and_cannot_repoint_it() {
        let fx = Fixture::new();
        let world = vec![("RKQA_PROBE_HOME".to_string(), "/world/home".to_string())];
        let env = SpawnEnv {
            extra_env: &world,
            ..fx.env()
        };
        let mut table = SpawnTable::default();
        let req = |v: Value| v.as_object().cloned().unwrap();
        let (program, args) = probe_cmd("stdout");
        let started = table
            .handle(
                &env,
                &req(json!({"op": "start", "program": program, "args": args, "label": "probe"})),
            )
            .unwrap();
        let child = started["child"].as_str().unwrap().to_string();
        let read = table
            .handle(
                &env,
                &req(json!({"op": "read", "child": child, "until": "line", "timeout_ms": 10000})),
            )
            .unwrap();
        assert_eq!(
            read["text"].as_str().unwrap().trim(),
            "/world/home",
            "{read}"
        );

        // Another value for the same variable (any letter case) is refused; the same value is not.
        for name in ["RKQA_PROBE_HOME", "rkqa_probe_home"] {
            let e = table
                .handle(
                    &env,
                    &req(json!({"op": "start", "program": program, "args": args,
                        "env": {name: "/real/profile"}})),
                )
                .unwrap_err();
            assert!(e.contains("override the world's isolated value"), "{e}");
        }
        table
            .handle(
                &env,
                &req(json!({"op": "start", "program": program, "args": args,
                    "env": {"RKQA_PROBE_HOME": "/world/home"}})),
            )
            .unwrap();
        table.cleanup(&fx.tracker);
    }

    /// N4: the UI binary is started only by the runner's launch path, never by `spawn`.
    #[test]
    fn spawn_refuses_the_ui_binary() {
        let fx = Fixture::new();
        let ui = std::env::current_exe().unwrap();
        let env = SpawnEnv {
            ui_binary: Some(ui.as_path()),
            ..fx.env()
        };
        let mut table = SpawnTable::default();
        let e = table
            .handle(
                &env,
                json!({"op": "start", "program": ui.to_string_lossy()})
                    .as_object()
                    .unwrap(),
            )
            .unwrap_err();
        assert!(e.contains("UI binary"), "{e}");
        assert!(fx.tracker.pids().is_empty(), "nothing was started");
    }

    /// N4: the plugin process itself starts with the world workspace's environment.
    #[test]
    fn the_plugin_process_gets_the_worlds_isolation_env() {
        let fx = Fixture::new();
        let world = vec![("RKQA_PROBE_HOME".to_string(), "/world/home".to_string())];
        let env = SpawnEnv {
            extra_env: &world,
            ..fx.env()
        };
        let (program, args) = probe_cmd("stderr");
        let mut spec = PluginSpec::new(program);
        spec.args = args;
        spec.hello_timeout = Duration::from_secs(5);
        // It is not a plugin, so the start fails; what it printed to stderr is what it saw.
        assert!(Plugin::start(&spec, &env, "scn").is_err());
        let log = std::fs::read_to_string(fx.dir.join("plugin.stderr.log")).unwrap();
        assert_eq!(log.trim(), "/world/home");
    }

    #[test]
    fn a_bad_hello_is_errored() {
        for hello in [
            r#"{"type":"hello","protocol":2,"name":"x","kinds":[]}"#,
            r#"{"type":"hello","protocol":1,"kinds":[]}"#,
            r#"{"type":"hello","protocol":1,"name":"x"}"#,
            r#"{"type":"result","id":0}"#,
            // A plugin may not take a runner built-in, a core kind or a core family.
            r#"{"type":"hello","protocol":1,"name":"x","kinds":["http"]}"#,
            r#"{"type":"hello","protocol":1,"name":"x","kinds":["cli"]}"#,
            r#"{"type":"hello","protocol":1,"name":"x","kinds":["ui_click"]}"#,
            r#"{"type":"hello","protocol":1,"name":"x","kinds":["Bad Name"]}"#,
        ] {
            let text = hello.to_string();
            let result = raw_attach(move |r, w| {
                let _ = line(r);
                say(w, &text);
            });
            let e = match result {
                Ok(_) => panic!("accepted the hello {hello}"),
                Err(e) => e,
            };
            assert!(is_errored(&e), "{hello}: {}", e.0);
        }
    }

    #[test]
    fn a_plugin_that_never_answers_times_out_errored() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut p = raw_pair(|r, w| {
            let _ = line(r);
            say(w, HELLO_OK);
            let _ = line(r);
            // Hold the pipe open without answering.
            std::thread::sleep(Duration::from_secs(5));
        });
        let started = Instant::now();
        let e = p
            .step(
                &env,
                "alpha",
                &params(json!({})),
                &no_vars(),
                Duration::from_millis(300),
            )
            .unwrap_err();
        assert!(is_errored(&e) && e.0.contains("in time"), "{}", e.0);
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(p.dead_reason().is_some());
    }

    #[test]
    fn spawn_is_started_and_tracked_by_the_runner_with_stdin_stdout_pid_wait_and_kill() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut p = pair(Fake::default());
        let r = p
            .step(&env, "spawn_probe", &params(json!({})), &no_vars(), LONG)
            .unwrap()
            .value;

        // stdin write, stdout read, pid and wait on a child that ran to its end.
        assert_eq!(r["read"]["text"], "hello", "{r}");
        assert_eq!(r["read"]["eof"], false);
        assert_eq!(r["waited"]["exited"], true, "{r}");
        assert_eq!(r["waited"]["code"], 0, "{r}");
        assert_eq!(r["pid"], r["pid_again"]);
        let pid = r["pid"].as_u64().unwrap() as u32;
        let long_pid = r["long_pid"].as_u64().unwrap() as u32;
        assert_ne!(pid, long_pid);

        // The runner, not the plugin, owns the child: it is in the scenario's tracker while it
        // lives, the reaped one is gone, and a wait that times out says so.
        assert_eq!(r["long_wait"]["exited"], false);
        let tracked = fx.tracker.pids();
        assert!(tracked.contains(&long_pid), "{tracked:?}");
        assert!(!tracked.contains(&pid), "{tracked:?}");
        assert!(crate::process::is_alive(long_pid));
        assert!(p.live_spawned().contains(&long_pid));

        // The runner captured the first child's stdout in full, where the secret scan looks.
        let log = std::fs::read_dir(&fx.dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("spawn-echo-"))
                    .unwrap_or(false)
                    && p.to_string_lossy().ends_with(".stdout.log")
            })
            .expect("the spawn stdout log");
        assert!(std::fs::read_to_string(log).unwrap().contains("hello"));

        // kill ends the tree, drops it from the tracker, and a second kill is not an error.
        let k = p
            .step(&env, "spawn_kill", &params(json!({})), &no_vars(), LONG)
            .unwrap()
            .value;
        assert_eq!(k["killed"]["exited"], true, "{k}");
        assert_eq!(k["again"]["exited"], true, "{k}");
        assert!(!fx.tracker.pids().contains(&long_pid));
        assert!(!crate::process::is_alive(long_pid));
        assert_eq!(p.shutdown(&env), Vec::<String>::new());
    }

    #[test]
    fn a_child_left_running_at_shutdown_is_killed_and_reported() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut p = pair(Fake::default());
        let r = p
            .step(&env, "spawn_leave", &params(json!({})), &no_vars(), LONG)
            .unwrap()
            .value;
        let pid = r["pid"].as_u64().unwrap() as u32;
        assert!(fx.tracker.pids().contains(&pid));
        let problems = p.shutdown(&env);
        assert!(
            problems
                .iter()
                .any(|m| m.contains("still running at teardown")),
            "{problems:?}"
        );
        assert!(!crate::process::is_alive(pid));
        assert!(fx.tracker.pids().is_empty());
    }

    #[test]
    fn spawn_requests_are_validated_by_the_runner() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut table = SpawnTable::default();
        let req = |v: Value| v.as_object().cloned().unwrap();

        // No engine configured: `engine: true` has nothing to start.
        let e = table
            .handle(&env, &req(json!({"op": "start", "engine": true})))
            .unwrap_err();
        assert!(e.contains("no engine binary"), "{e}");
        // Neither engine nor program.
        assert!(table.handle(&env, &req(json!({"op": "start"}))).is_err());
        // An unknown op and an unknown child.
        assert!(table
            .handle(&env, &req(json!({"op": "dance"})))
            .unwrap_err()
            .contains("unknown spawn op"));
        assert!(table
            .handle(&env, &req(json!({"op": "pid", "child": "c9"})))
            .unwrap_err()
            .contains("no spawned child"));

        // The engine binary is only started with `engine: true`, which applies the data-dir guard.
        let engine =
            crate::engine::EngineTarget::new("some-engine-binary").with_data_dir_env("E_DATA");
        let guarded = SpawnEnv {
            engine: Some(&engine),
            ..fx.env()
        };
        let e = table
            .handle(
                &guarded,
                &req(json!({"op": "start", "program": "some-engine-binary"})),
            )
            .unwrap_err();
        assert!(e.contains("engine: true"), "{e}");

        // A child started without `stdin: true` cannot be written to.
        let (program, args) = echo_cmd();
        let started = table
            .handle(
                &env,
                &req(json!({"op": "start", "program": program, "args": args, "label": "quiet"})),
            )
            .unwrap();
        let child = started["child"].as_str().unwrap().to_string();
        let e = table
            .handle(
                &env,
                &req(json!({"op": "write", "child": child, "data": "x"})),
            )
            .unwrap_err();
        assert!(e.contains("stdin is not open"), "{e}");
        let problems = table.cleanup(&fx.tracker);
        assert!(problems.iter().all(|m| !m.is_empty()));
        assert!(fx.tracker.pids().is_empty());
    }

    /// An ordinary process is not a plugin: it never says `hello`, the start fails errored, and the
    /// process it started has been killed and forgotten (it was inside the tracker the whole time).
    #[test]
    fn a_process_that_is_not_a_plugin_fails_errored_and_leaves_nothing_behind() {
        let fx = Fixture::new();
        let env = fx.env();
        let (program, args) = echo_cmd();
        let mut spec = PluginSpec::new(program);
        spec.args = args.iter().map(|a| a.to_string()).collect();
        spec.hello_timeout = Duration::from_secs(5);
        let e = match Plugin::start(&spec, &env, "scn") {
            Ok(_) => panic!("an echo process was accepted as a plugin"),
            Err(e) => e,
        };
        assert!(is_errored(&e), "{}", e.0);
        assert!(fx.tracker.alive().is_empty(), "{:?}", fx.tracker.alive());
        assert!(fx.dir.join("plugin.stderr.log").exists());
    }

    /// Oracle item 9: a plugin that cannot be started is `errored`, not `failed`.
    #[test]
    fn a_missing_plugin_binary_is_a_start_error_and_errored() {
        let fx = Fixture::new();
        let env = fx.env();
        let spec = PluginSpec::new(fx.dir.join("no-such-plugin"));
        let e = match Plugin::start(&spec, &env, "scn") {
            Ok(_) => panic!("started a binary that does not exist"),
            Err(e) => e,
        };
        assert!(e.0.contains("failed to start"), "{}", e.0);
        assert!(
            is_errored(&e),
            "a plugin that never started is errored: {}",
            e.0
        );
        assert!(fx.tracker.pids().is_empty());
    }

    /// PostRight gap: `hold` on a plugin reply reaches the runner, from the SDK and from raw lines.
    #[test]
    fn a_reply_carries_the_secrets_the_step_asks_the_runner_to_hold() {
        struct Holder;
        impl StepPlugin for Holder {
            fn name(&self) -> String {
                "holder".into()
            }
            fn kinds(&self) -> Vec<String> {
                vec!["alpha".into()]
            }
            fn step(
                &mut self,
                _cx: &mut PluginCx<'_>,
                _kind: &str,
                _params: &Map<String, Value>,
                _vars: &Map<String, Value>,
            ) -> std::result::Result<StepOutput, String> {
                Ok(StepOutput::value(json!(1)).with_hold("api token", "token-value-0123456789"))
            }
        }
        let fx = Fixture::new();
        let env = fx.env();
        let mut p = pair(Holder);
        let reply = p
            .step(&env, "alpha", &params(json!({})), &no_vars(), LONG)
            .unwrap();
        assert_eq!(
            reply.hold,
            vec![(
                "api token".to_string(),
                "token-value-0123456789".to_string()
            )]
        );

        // Raw wire: an object of label to secret, and an array of secrets.
        let mut p = raw_pair(|r, w| {
            let _ = line(r);
            say(w, HELLO_OK);
            let _ = line(r);
            say(
                w,
                r#"{"type":"result","id":1,"value":7,"hold":{"key a":"secret-a"}}"#,
            );
            let _ = line(r);
            say(
                w,
                r#"{"type":"result","id":2,"value":8,"hold":["secret-b","secret-c"]}"#,
            );
            let _ = line(r);
            say(
                w,
                r#"{"type":"result","id":3,"value":9,"hold":{"key d":5}}"#,
            );
        });
        let step = |p: &mut Plugin| p.step(&env, "alpha", &params(json!({})), &no_vars(), LONG);
        assert_eq!(
            step(&mut p).unwrap().hold,
            vec![("key a".to_string(), "secret-a".to_string())]
        );
        let values: Vec<String> = step(&mut p)
            .unwrap()
            .hold
            .into_iter()
            .map(|(_, v)| v)
            .collect();
        assert_eq!(values, vec!["secret-b", "secret-c"]);
        // A secret that is not a string breaks the protocol.
        assert!(is_errored(&step(&mut p).unwrap_err()));
    }

    /// Oracle item 1 (spawn): a child that exits 0 and leaves a grandchild running. The grandchild
    /// is recorded as a survivor before the sweep ends it, so the scenario cannot pass.
    #[test]
    fn a_spawned_child_that_exits_leaving_a_grandchild_is_recorded_as_a_survivor() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut table = SpawnTable::default();
        let req = |v: Value| v.as_object().cloned().unwrap();
        let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
            ("cmd", vec!["/C", "start /B ping -n 60 127.0.0.1"])
        } else {
            ("sh", vec!["-c", "sleep 60 & echo $!"])
        };
        let started = table
            .handle(
                &env,
                &req(json!({"op": "start", "program": program, "args": args, "label": "leaver"})),
            )
            .unwrap();
        let child = started["child"].as_str().unwrap().to_string();
        let mut grandchild: Option<u32> = None;
        if cfg!(unix) {
            let read = table
                .handle(
                    &env,
                    &req(
                        json!({"op": "read", "child": child, "until": "line", "timeout_ms": 10000}),
                    ),
                )
                .unwrap();
            grandchild = read["text"].as_str().and_then(|t| t.trim().parse().ok());
            assert!(grandchild.is_some(), "{read}");
        }
        let waited = table
            .handle(
                &env,
                &req(json!({"op": "wait", "child": child, "timeout_ms": 20000})),
            )
            .unwrap();
        assert_eq!(waited["exited"], true, "{waited}");
        assert_eq!(waited["code"], 0, "{waited}");
        let recorded = fx.tracker.recorded();
        assert!(
            !recorded.is_empty(),
            "the grandchild must be recorded as a survivor"
        );
        assert!(
            recorded
                .iter()
                .all(|(_, label)| label.contains("spawn leaver")),
            "{recorded:?}"
        );
        if let Some(pid) = grandchild {
            assert!(recorded.iter().any(|(p, _)| *p == pid), "{recorded:?}");
        }
        assert!(
            fx.tracker.alive().len() >= recorded.len(),
            "teardown reads them as survivors"
        );
        // The sweep still ended them.
        let deadline = Instant::now() + Duration::from_secs(5);
        while recorded.iter().any(|(p, _)| crate::process::is_alive(*p))
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(recorded.iter().all(|(p, _)| !crate::process::is_alive(*p)));
        assert_eq!(table.cleanup(&fx.tracker), Vec::<String>::new());
    }

    /// The same for a child nobody waited for: teardown cleanup records before it sweeps.
    #[test]
    fn a_spawned_child_that_exited_unwaited_leaving_a_grandchild_is_recorded_at_cleanup() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut table = SpawnTable::default();
        let req = |v: Value| v.as_object().cloned().unwrap();
        let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
            // The leader launches the grandchild, then blocks reading stdin: it exits only when
            // the test closes stdin below, so its exit is a thing the test causes and observes.
            (
                "cmd",
                vec!["/C", "start /B ping -n 60 127.0.0.1 >NUL & findstr x"],
            )
        } else {
            ("sh", vec!["-c", "sleep 60 & cat >/dev/null"])
        };
        let started = table
            .handle(
                &env,
                &req(json!({"op": "start", "program": program, "args": args, "label": "unwaited", "stdin": true})),
            )
            .unwrap();
        let pid = started["pid"].as_u64().unwrap() as u32;
        let child = started["child"].as_str().unwrap().to_string();
        assert!(
            crate::process::is_alive(pid),
            "the leader is blocked on stdin, not exited yet"
        );
        table
            .handle(&env, &req(json!({"op": "close_stdin", "child": child})))
            .unwrap();
        // Observe the exit (nobody calls `wait`): bounded poll on the leader pid.
        let deadline = Instant::now() + Duration::from_secs(20);
        while crate::process::is_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            !crate::process::is_alive(pid),
            "the leader (pid {pid}) did not exit after its stdin closed"
        );
        let problems = table.cleanup(&fx.tracker);
        assert!(problems.is_empty(), "{problems:?}");
        let recorded = fx.tracker.recorded();
        assert!(
            !recorded.is_empty(),
            "the grandchild must be recorded as a survivor"
        );
        assert!(
            recorded
                .iter()
                .all(|(_, label)| label.contains("spawn unwaited")),
            "{recorded:?}"
        );
    }

    /// A child killed on request, tree and all, is a deliberate kill: nothing is recorded.
    #[test]
    fn killing_a_running_spawned_child_records_no_survivor() {
        let fx = Fixture::new();
        let env = fx.env();
        let mut table = SpawnTable::default();
        let req = |v: Value| v.as_object().cloned().unwrap();
        let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
            ("ping", vec!["-n", "60", "127.0.0.1"])
        } else {
            ("sleep", vec!["60"])
        };
        let started = table
            .handle(
                &env,
                &req(json!({"op": "start", "program": program, "args": args, "label": "long"})),
            )
            .unwrap();
        let child = started["child"].as_str().unwrap().to_string();
        let killed = table
            .handle(&env, &req(json!({"op": "kill", "child": child})))
            .unwrap();
        assert_eq!(killed["exited"], true, "{killed}");
        assert!(
            fx.tracker.recorded().is_empty(),
            "{:?}",
            fx.tracker.recorded()
        );
    }

    /// Oracle item 1 (plugin): a plugin that exits 0 after `shutdown` leaving a grandchild running.
    #[cfg(unix)]
    #[test]
    fn a_grandchild_a_plugin_leaves_after_a_clean_shutdown_is_recorded_as_a_survivor() {
        let fx = Fixture::new();
        let env = fx.env();
        let script = r#"read hello
echo '{"type":"hello","protocol":1,"name":"sh","version":"1","kinds":["alpha"]}'
sleep 60 &
echo "$!" >&2
read request
echo '{"type":"result","id":1,"value":null}'"#;
        let mut spec = PluginSpec::new("/bin/sh");
        spec.args = vec!["-c".to_string(), script.to_string()];
        spec.hello_timeout = Duration::from_secs(10);
        let mut p = Plugin::start(&spec, &env, "scn").expect("the shell plugin starts");
        let problems = p.shutdown(&env);
        assert!(problems.is_empty(), "{problems:?}");
        let stderr = std::fs::read_to_string(fx.dir.join("plugin.stderr.log")).unwrap();
        let grandchild: u32 = stderr
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("stderr: {stderr}"));
        let recorded = fx.tracker.recorded();
        assert!(
            recorded
                .iter()
                .any(|(pid, label)| *pid == grandchild && label.contains("step plugin")),
            "{recorded:?}"
        );
        assert!(fx.tracker.alive().iter().any(|(pid, _)| *pid == grandchild));
    }

    /// Oracle item 4: an engine process started through `spawn` with `engine: true` is handed to the
    /// scenario's engine count, where it is an undeclared start and the scenario is errored.
    #[cfg(unix)]
    #[test]
    fn spawned_engines_are_counted_through_the_spawn_environment() {
        let fx = Fixture::new();
        let engine = crate::engine::EngineTarget::new("/bin/sh").with_data_dir_env("E_DATA");
        let env = SpawnEnv {
            engine: Some(&engine),
            ..fx.env()
        };
        let mut table = SpawnTable::default();
        let req = |v: Value| v.as_object().cloned().unwrap();
        for n in 1..=3 {
            table
                .handle(
                    &env,
                    &req(json!({"op": "start", "engine": true, "args": ["-c", "exit 0"], "label": format!("eng{n}")})),
                )
                .unwrap();
        }
        // A plain program is not an engine start.
        table
            .handle(
                &env,
                &req(json!({"op": "start", "program": "true", "label": "plain"})),
            )
            .unwrap();
        let launches = table.take_engine_launches();
        assert_eq!(launches.len(), 3, "{launches:?}");
        assert!(table.take_engine_launches().is_empty(), "drained once");
        let mut session = crate::engine::EngineSession::new(None);
        session.note_scenario_sidecar();
        for note in &launches {
            match note {
                crate::steps::spawn::EngineNote::Started { what, cold_reason } => {
                    session.note_spawned_engine(what, *cold_reason, None)
                }
                crate::steps::spawn::EngineNote::Refused(why) => session.note_engine_refusal(why),
            }
        }
        let starts = session.starts();
        assert!(starts.total() >= 4, "total {}", starts.total());
        assert!(
            !starts.errored().is_empty(),
            "three undeclared spawns are errored"
        );
        table.cleanup(&fx.tracker);
    }

    fn drain_into(table: &mut SpawnTable, session: &mut crate::engine::EngineSession) {
        for note in table.take_engine_launches() {
            match note {
                crate::steps::spawn::EngineNote::Started { what, cold_reason } => {
                    session.note_spawned_engine(&what, cold_reason, None)
                }
                crate::steps::spawn::EngineNote::Refused(why) => session.note_engine_refusal(&why),
            }
        }
    }

    /// A spawn of the engine that carries a valid `cold_reason` is a declared cold start: counted
    /// and allowed the way a declared `cold_cli` is. Without it the same spawn stays undeclared.
    #[cfg(unix)]
    #[test]
    fn a_declared_engine_spawn_counts_as_a_declared_cold_start_and_an_undeclared_one_is_errored() {
        let fx = Fixture::new();
        let engine = crate::engine::EngineTarget::new("/bin/sh").with_data_dir_env("E_DATA");
        let env = SpawnEnv {
            engine: Some(&engine),
            ..fx.env()
        };
        let req = |v: Value| v.as_object().cloned().unwrap();
        let mut table = SpawnTable::default();
        let mut session = crate::engine::EngineSession::new(None);
        table
            .handle(
                &env,
                &req(json!({"op": "start", "engine": true, "cold_reason": "role-change", "args": ["-c", "exit 0"], "label": "declared"})),
            )
            .unwrap();
        drain_into(&mut table, &mut session);
        let st = session.starts();
        assert_eq!((st.total(), st.allowed()), (1, 1));
        assert_eq!(st.cold_launches, 1);
        assert_eq!(st.declared_cold, 1);
        assert_eq!(st.cold_reasons["role-change"], 1);
        assert_eq!(st.violation(), None);
        assert!(st.errored().is_empty());

        table
            .handle(
                &env,
                &req(json!({"op": "start", "engine": true, "args": ["-c", "exit 0"], "label": "undeclared"})),
            )
            .unwrap();
        drain_into(&mut table, &mut session);
        let st = session.starts();
        assert_eq!(st.spawned_engines, 1);
        assert!(
            !st.errored().is_empty(),
            "an undeclared spawn is still errored"
        );
        table.cleanup(&fx.tracker);
    }

    /// An unknown `cold_reason` refuses the spawn (nothing starts) and the scenario is errored.
    /// `reason` is not a wire name: it is ignored, so the start stays undeclared.
    #[test]
    fn an_unknown_cold_reason_on_an_engine_spawn_is_refused_and_errored() {
        let fx = Fixture::new();
        let engine =
            crate::engine::EngineTarget::new("some-engine-binary").with_data_dir_env("E_DATA");
        let env = SpawnEnv {
            engine: Some(&engine),
            ..fx.env()
        };
        let req = |v: Value| v.as_object().cloned().unwrap();
        let mut table = SpawnTable::default();
        let mut session = crate::engine::EngineSession::new(None);
        let e = table
            .handle(
                &env,
                &req(json!({"op": "start", "engine": true, "cold_reason": "because"})),
            )
            .unwrap_err();
        assert!(
            e.contains("cold reason `because`") && e.contains("store-lock"),
            "{e}"
        );
        // Not an engine spawn: the declaration means nothing, so it is refused too.
        let e = table
            .handle(
                &env,
                &req(json!({"op": "start", "program": "x", "cold_reason": "startup"})),
            )
            .unwrap_err();
        assert!(e.contains("needs `engine: true`"), "{e}");
        assert!(fx.tracker.pids().is_empty(), "nothing was started");
        drain_into(&mut table, &mut session);
        assert_eq!(session.starts().crashes.len(), 2);
        assert!(!session.starts().errored().is_empty());
    }

    /// SDK: `spawn_start_engine` puts `engine: true` and `cold_reason` on the wire.
    #[test]
    fn the_sdk_spawn_start_engine_sends_the_cold_reason() {
        let (from_runner, to_plugin) = std::io::pipe().unwrap();
        let (from_plugin, to_runner) = std::io::pipe().unwrap();
        let seen = std::thread::spawn(move || {
            let mut reader = BufReader::new(from_runner);
            let mut writer = to_runner;
            let mut buf = Vec::new();
            let mut cx = PluginCx {
                reader: &mut reader,
                writer: &mut writer,
                buf: &mut buf,
                next_spawn: &mut 0,
            };
            // The runner end below answers; this end only needs to send.
            cx.spawn_start_engine(Some("startup"), json!({"label": "e"}))
        });
        let mut runner = BufReader::new(from_plugin);
        let mut text = String::new();
        runner.read_line(&mut text).unwrap();
        let sent: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(sent["op"], "start");
        assert_eq!(sent["engine"], true);
        assert_eq!(sent["cold_reason"], "startup");
        let mut to_plugin = to_plugin;
        say(
            &mut to_plugin,
            r#"{"type":"spawn_result","id":1,"value":{"child":"c1","pid":7}}"#,
        );
        assert_eq!(seen.join().unwrap(), Ok(("c1".to_string(), 7)));
    }
}
