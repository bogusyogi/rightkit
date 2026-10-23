//! Child processes a step plugin asks the runner to start (`spawn` messages of the step-plugin
//! protocol, see [`crate::plugin`]).
//!
//! The plugin never starts a process itself: on Unix a child of the plugin would get its own process
//! group and escape the scenario's tracking, and a daemon the plugin started would be invisible to the
//! runner's survivor scan. A child it needs goes through here, so the runner spawns it through
//! `rightkit-child-process` (process group or Job Object), registers it with the scenario's
//! [`Tracker`], captures its full stdout and stderr into the scenario's log directory (where
//! `secret_absent` scans them), and kills the tree on request or at teardown.
//!
//! Operations (`op` of a `spawn` message), all answered with a `spawn_result` carrying `value` or `error`:
//!
//! * `start`: `engine` (true: the scenario's engine binary, which goes through the isolated-data-dir
//!   launch guard and gets the engine environment) or `program`; `cold_reason` (with `engine: true`
//!   only: declares the start as a cold launch of that reason, one of [`crate::engine::COLD_REASONS`],
//!   validated as `cold_cli`'s `cold_reason` is; an unknown reason refuses the spawn and the scenario
//!   is `errored`); `args`, `env` (strings), `cwd`,
//!   `stdin` (true: a pipe the plugin can `write` to; default null), `label` (names the logs). The
//!   engine binary can only be started with `engine: true`, never by `program`. Result
//!   `{child, pid, stdout_log, stderr_log}`.
//! * `write`: `child`, `data` (UTF-8 text) or `base64`. Result `{written}`.
//! * `close_stdin`: `child`. Result `{closed}`.
//! * `read`: `child`, `until` (`line` default, `any`, `eof`), `timeout_ms` (default 10000). Result
//!   `{text, eof, timed_out}`. `line` returns one line without its line ending, `any` whatever is
//!   buffered, `eof` everything up to the end of the stream.
//! * `pid`: `child`. Result `{pid}`.
//! * `wait`: `child`, `timeout_ms` (default 30000). Result `{exited, code}`.
//! * `kill`: `child`. Terminates the child's tree (bounded). Result `{exited, code, note}`.
//!
//! Counting and survivors. A `start` with `engine: true` is an engine start of the scenario that no
//! declaration covers (unless it carries a valid `cold_reason`, which makes it a declared cold start): it is drained by the step layer ([`SpawnTable::take_engine_launches`]) into the
//! scenario's engine-start count as an undeclared start, which makes the scenario `errored`. A child
//! that exited on its own (seen by `wait`, `kill` after exit, or teardown) and left descendants running
//! has them recorded as survivors ([`Tracker::record_survivor`]) before the sweep ends them, so the
//! scenario is `errored` rather than passing on evidence the sweep destroyed. A `kill` of a child
//! that is still running ends its whole tree on request and records nothing.
use crate::engine::parse_cold_reason;
use crate::engine::EngineTarget;
use crate::process::{record_survivors_if_exited, terminate_bounded, Tracker};
use crate::userdata::{check_engine_launch, EngineLaunch};
use rightkit_child_process::{OwnedChild, OwnedCommand};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Most live children one plugin may hold at once.
pub const MAX_CHILDREN: usize = 32;
const READ_DEFAULT_MS: u64 = 10_000;
const WAIT_DEFAULT_MS: u64 = 30_000;
/// Longest any single `read` or `wait` blocks the scenario.
const BLOCK_MAX_MS: u64 = 600_000;

/// What spawning needs from the scenario, borrowed so a plugin call and the scenario's own fields can
/// be used together.
pub struct SpawnEnv<'a> {
    /// The scenario's tracker: every child is registered here, so it is inside the job and the
    /// orphan check sees it.
    pub tracker: &'a Tracker,
    /// The scenario's engine target, for `engine: true`.
    pub engine: Option<&'a EngineTarget>,
    pub data_dir: &'a Path,
    /// Where captured stdout and stderr go (the scenario's evidence log directory).
    pub log_dir: &'a Path,
    /// Added to a `program` child's environment before the request's own `env` (the world marker
    /// and ports). An engine child already carries them through the engine target.
    pub extra_env: &'a [(String, String)],
    /// The scenario's UI/app binary. Only the runner's own launch path starts it, so a spawn
    /// refuses it (it would run on whatever profile the plugin's environment names).
    pub ui_binary: Option<&'a Path>,
}

/// Whether two paths name the same file: canonical paths when both resolve, else literal equality.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

struct Held {
    label: String,
    pid: u32,
    owned: Option<OwnedChild>,
    stdin: Option<ChildStdin>,
    out: Option<Receiver<Vec<u8>>>,
    pending: Vec<u8>,
    eof: bool,
    /// `Some(code)` once the child has been reaped.
    exit: Option<Option<i32>>,
}

/// The children one plugin has started.
#[derive(Default)]
pub struct SpawnTable {
    next: u64,
    children: BTreeMap<String, Held>,
    /// Engine processes started with `engine: true` that the scenario's engine count has not seen yet
    /// (a description each). The step layer drains them with [`SpawnTable::take_engine_launches`]
    /// into the scenario's engine-start count, where each is an undeclared start.
    engine_launches: Vec<EngineNote>,
}

/// What a plugin's `engine: true` spawn means for the scenario's engine count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineNote {
    /// The engine was started (`what` names the child). `cold_reason` is the declaration the request
    /// carried, one of [`crate::engine::COLD_REASONS`]; `None` is an undeclared start.
    Started {
        what: String,
        cold_reason: Option<&'static str>,
    },
    /// The request was refused because its `cold_reason` was not valid; nothing started.
    Refused(String),
}

fn str_list(req: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    match req.get(key).filter(|v| !v.is_null()) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("`{key}` must be an array of strings"))
            })
            .collect(),
        Some(_) => Err(format!("`{key}` must be an array of strings")),
    }
}

fn str_pairs(req: &Map<String, Value>, key: &str) -> Result<Vec<(String, String)>, String> {
    match req.get(key).filter(|v| !v.is_null()) {
        None => Ok(Vec::new()),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| {
                v.as_str()
                    .map(|s| (k.clone(), s.to_string()))
                    .ok_or_else(|| format!("`{key}.{k}` must be a string"))
            })
            .collect(),
        Some(_) => Err(format!("`{key}` must be a table of strings")),
    }
}

fn millis(req: &Map<String, Value>, key: &str, default_ms: u64) -> Result<Duration, String> {
    match req.get(key).filter(|v| !v.is_null()) {
        None => Ok(Duration::from_millis(default_ms)),
        Some(v) => v
            .as_u64()
            .map(|ms| Duration::from_millis(ms.min(BLOCK_MAX_MS)))
            .ok_or_else(|| format!("`{key}` must be a non-negative whole number of milliseconds")),
    }
}

/// A log-file-safe label: letters, digits, `_` and `-`, at most 32 characters.
fn clean_label(raw: &str) -> String {
    let label: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    if label.is_empty() {
        "child".to_string()
    } else {
        label
    }
}

impl SpawnTable {
    /// Run one `spawn` request. An `Err` is the text of the `spawn_result` `error`.
    pub fn handle(&mut self, env: &SpawnEnv, req: &Map<String, Value>) -> Result<Value, String> {
        let op = req
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| "spawn needs a string `op`".to_string())?;
        if op == "start" {
            return self.start(env, req);
        }
        if !["write", "close_stdin", "read", "pid", "wait", "kill"].contains(&op) {
            return Err(format!(
                "unknown spawn op `{op}` (ops: start, write, close_stdin, read, pid, wait, kill)"
            ));
        }
        let id = req
            .get("child")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("spawn {op} needs the string `child` that `start` returned"))?
            .to_string();
        let held = self
            .children
            .get_mut(&id)
            .ok_or_else(|| format!("no spawned child `{id}`"))?;
        match op {
            "write" => held.write(req),
            "close_stdin" => {
                let closed = held.stdin.take().is_some();
                Ok(json!({ "closed": closed }))
            }
            "read" => held.read(req),
            "pid" => Ok(json!({ "pid": held.pid })),
            "wait" => held.wait(env.tracker, req),
            _ => held.kill(env.tracker),
        }
    }

    fn start(&mut self, env: &SpawnEnv, req: &Map<String, Value>) -> Result<Value, String> {
        if self.children.len() >= MAX_CHILDREN {
            return Err(format!(
                "this plugin already holds {MAX_CHILDREN} spawned children"
            ));
        }
        let args = str_list(req, "args")?;
        let step_env = str_pairs(req, "env")?;
        let label = clean_label(req.get("label").and_then(Value::as_str).unwrap_or("child"));
        let want_stdin = req.get("stdin").and_then(Value::as_bool).unwrap_or(false);
        let use_engine = req.get("engine").and_then(Value::as_bool).unwrap_or(false);

        // `cold_reason` (and only that name) declares an engine start; it means nothing otherwise.
        let mut cold_reason: Option<&'static str> = None;
        if let Some(raw) = req.get("cold_reason").filter(|v| !v.is_null()) {
            let refused =
                |why: String| -> String { format!("rightkit-qa refused the spawn: {why}") };
            if !use_engine {
                let why = refused(
                    "`cold_reason` declares an engine start, so it needs `engine: true`"
                        .to_string(),
                );
                self.engine_launches
                    .push(EngineNote::Refused(format!("spawn `{label}`: {why}")));
                return Err(why);
            }
            let parsed = raw
                .as_str()
                .ok_or_else(|| "`cold_reason` must be a string".to_string())
                .and_then(|s| parse_cold_reason(s).map_err(|e| e.0));
            match parsed {
                Ok(reason) => cold_reason = Some(reason),
                Err(e) => {
                    let why = refused(e);
                    self.engine_launches
                        .push(EngineNote::Refused(format!("spawn `{label}`: {why}")));
                    return Err(why);
                }
            }
        }

        let mut cmd;

        let shown: String = if use_engine {
            if req.get("program").is_some_and(|v| !v.is_null()) {
                return Err("give `engine: true` or `program`, not both".to_string());
            }
            let engine = env.engine.ok_or_else(|| {
                "`engine` is true but this scenario has no engine binary".to_string()
            })?;
            if !engine.binary.exists() {
                return Err(format!(
                    "engine binary not found: {}",
                    engine.binary.display()
                ));
            }
            let launch_env = engine.env_for(Some(env.data_dir), &step_env);
            // The same guard the engine's own launches pass: no isolated data directory, no launch.
            check_engine_launch(&EngineLaunch {
                binary: &engine.binary,
                args: &args,
                data_dir: Some(env.data_dir),
                data_dir_env: engine.data_dir_env.as_deref(),
                data_dir_arg: engine.data_dir_arg.as_deref(),
                forbidden_args: &engine.forbidden_args,
                env: &launch_env,
                refusal_probe: engine.allow_no_data_dir_for_refusal_probe,
            })
            .map_err(|e| format!("rightkit-qa refused the spawn: {e}"))?;
            // windowless: handed to OwnedCommand::from_command below, which hides the console
            cmd = Command::new(&engine.binary);
            // The world workspace's isolation first, the engine's own env (data dir, marker) over it.
            for (k, v) in env.extra_env {
                cmd.env(k, v);
            }
            for (k, v) in &launch_env {
                cmd.env(k, v);
            }
            engine.binary.display().to_string()
        } else {
            let program = req.get("program").and_then(Value::as_str).ok_or_else(|| {
                "spawn start needs `engine: true` or a string `program`".to_string()
            })?;
            if let Some(engine) = env.engine {
                if same_file(Path::new(program), &engine.binary) {
                    return Err("the engine binary is started with `engine: true`, which applies the data-directory guard".to_string());
                }
            }
            if let Some(ui) = env.ui_binary {
                if same_file(Path::new(program), ui) {
                    return Err("rightkit-qa refused the spawn: the UI binary is started only by the runner's own launch path (`ui_launch`), never by `spawn`".to_string());
                }
            }
            // The world's isolation (HOME, TMP, APPDATA, XDG, data dir) is not the plugin's to
            // repoint: a step `env` naming one of those with another value would put the child on
            // a real profile.
            for (k, v) in &step_env {
                if let Some((_, world)) = env
                    .extra_env
                    .iter()
                    .find(|(wk, _)| wk.eq_ignore_ascii_case(k))
                {
                    if world != v {
                        return Err(format!(
                            "rightkit-qa refused the spawn: `env` {k} would override the world's isolated value"
                        ));
                    }
                }
            }
            // windowless: handed to OwnedCommand::from_command below, which hides the console
            cmd = Command::new(program);
            for (k, v) in env.extra_env.iter().chain(step_env.iter()) {
                cmd.env(k, v);
            }
            program.to_string()
        };
        cmd.args(&args);
        if let Some(cwd) = req.get("cwd").and_then(Value::as_str) {
            cmd.current_dir(cwd);
        }

        self.next += 1;
        let n = self.next;
        let id = format!("c{n}");
        let stdout_log = env.log_dir.join(format!("spawn-{label}-{n}.stdout.log"));
        let stderr_log = env.log_dir.join(format!("spawn-{label}-{n}.stderr.log"));
        let stdout_file = std::fs::File::create(&stdout_log)
            .map_err(|e| format!("create {}: {e}", stdout_log.display()))?;
        let stderr_file = std::fs::File::create(&stderr_log)
            .map_err(|e| format!("create {}: {e}", stderr_log.display()))?;
        cmd.stdin(if want_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_file));
        let mut owned = OwnedCommand::from_command(cmd);
        owned.windows_hide();
        let mut child = owned.spawn().map_err(|e| format!("spawn {shown}: {e}"))?;
        let pid = child.id();
        env.tracker.register(pid, &format!("spawn {label}"));
        if use_engine {
            // Counted when the process exists, whatever the plugin does with it next.
            self.engine_launches.push(EngineNote::Started {
                what: format!("spawn `{label}` (pid {pid})"),
                cold_reason,
            });
        }
        let stdin = child.take_stdin();
        let out = child.take_stdout().map(|mut pipe| {
            let (tx, rx) = channel::<Vec<u8>>();
            std::thread::spawn(move || {
                let mut file = stdout_file;
                let mut chunk = [0u8; 8192];
                // Every byte goes to the log first, so the secret scan sees the whole stream even
                // when the plugin never reads it.
                while let Ok(read) = pipe.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    let _ = file.write_all(&chunk[..read]);
                    let _ = tx.send(chunk[..read].to_vec());
                }
            });
            rx
        });
        self.children.insert(
            id.clone(),
            Held {
                label,
                pid,
                owned: Some(child),
                stdin,
                out,
                pending: Vec::new(),
                eof: false,
                exit: None,
            },
        );
        Ok(json!({
            "child": id,
            "pid": pid,
            "stdout_log": stdout_log.to_string_lossy(),
            "stderr_log": stderr_log.to_string_lossy(),
        }))
    }

    /// Kill every child still running and forget the rest. A child still running here is a plugin
    /// that did not stop what it started: reported, and killed so nothing outlives the scenario.
    pub fn cleanup(&mut self, tracker: &Tracker) -> Vec<String> {
        let mut problems = Vec::new();
        for (_, mut held) in std::mem::take(&mut self.children) {
            if held.exit.is_some() {
                continue;
            }
            let Some(mut owned) = held.owned.take() else {
                continue;
            };
            // Exited on its own but never waited for: what it left behind is recorded as survivors
            // before the sweep ends it.
            let exited = owned
                .process_handle()
                .and_then(|h| h.wait_exited_timeout(Duration::ZERO))
                .unwrap_or(false);
            if exited {
                // A survivor makes the scenario `errored` through the tracker (`Ctx::finish`).
                record_survivors_if_exited(&owned, &format!("spawn {}", held.label), tracker);
                let _ = owned.terminate_tree();
            } else {
                problems.push(format!(
                    "spawned child `{}` (pid {}) was still running at teardown and was killed",
                    held.label, held.pid
                ));
                let (_, note) = terminate_bounded(&mut owned);
                problems.extend(note);
            }
            tracker.forget(held.pid);
        }
        problems
    }

    /// Engine processes the plugin started since the last call, one description each. The caller
    /// counts them (see [`crate::engine::EngineSession::note_spawned_engine`]).
    pub fn take_engine_launches(&mut self) -> Vec<EngineNote> {
        std::mem::take(&mut self.engine_launches)
    }

    /// The pids of children not yet reaped, for a caller that reports them.
    pub fn live_pids(&self) -> Vec<u32> {
        self.children
            .values()
            .filter(|h| h.exit.is_none())
            .map(|h| h.pid)
            .collect()
    }
}

impl Held {
    fn write(&mut self, req: &Map<String, Value>) -> Result<Value, String> {
        let data: Vec<u8> = if let Some(encoded) = req.get("base64").and_then(Value::as_str) {
            crate::util::base64_decode(encoded)
                .ok_or_else(|| "`base64` is not valid base64".to_string())?
        } else {
            req.get("data")
                .and_then(Value::as_str)
                .ok_or_else(|| "spawn write needs the string `data` or `base64`".to_string())?
                .as_bytes()
                .to_vec()
        };
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            "the child's stdin is not open (start it with `stdin: true`; `close_stdin` closes it)"
                .to_string()
        })?;
        stdin
            .write_all(&data)
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("write to the child's stdin: {e}"))?;
        Ok(json!({ "written": data.len() }))
    }

    fn read(&mut self, req: &Map<String, Value>) -> Result<Value, String> {
        let until = req.get("until").and_then(Value::as_str).unwrap_or("line");
        if !["line", "any", "eof"].contains(&until) {
            return Err(format!("`until` must be line, any or eof, got `{until}`"));
        }
        let deadline = Instant::now() + millis(req, "timeout_ms", READ_DEFAULT_MS)?;
        loop {
            match until {
                "line" => {
                    if let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = self.pending.drain(..=pos).collect();
                        let text = String::from_utf8_lossy(&line)
                            .trim_end_matches(['\n', '\r'])
                            .to_string();
                        return Ok(json!({ "text": text, "eof": false, "timed_out": false }));
                    }
                }
                "any" if !self.pending.is_empty() => {
                    let all = std::mem::take(&mut self.pending);
                    return Ok(json!({
                        "text": String::from_utf8_lossy(&all),
                        "eof": false,
                        "timed_out": false
                    }));
                }
                _ => {}
            }
            if self.eof {
                let rest = std::mem::take(&mut self.pending);
                let mut text = String::from_utf8_lossy(&rest).into_owned();
                if until == "line" {
                    text = text.trim_end_matches(['\n', '\r']).to_string();
                }
                return Ok(json!({ "text": text, "eof": true, "timed_out": false }));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // A line read leaves a partial line buffered; the others hand over what they have.
                let text = if until == "line" {
                    String::new()
                } else {
                    String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned()
                };
                return Ok(json!({ "text": text, "eof": false, "timed_out": true }));
            }
            match self.out.as_ref() {
                None => self.eof = true,
                Some(rx) => match rx.recv_timeout(left) {
                    Ok(chunk) => self.pending.extend(chunk),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => self.eof = true,
                },
            }
        }
    }

    /// Record the exit, forget the pid and release the process handle (which ends any descendants
    /// the child left behind).
    fn reaped(&mut self, tracker: &Tracker, code: Option<i32>) {
        self.exit = Some(code);
        self.stdin = None;
        tracker.forget(self.pid);
        self.owned = None;
    }

    fn wait(&mut self, tracker: &Tracker, req: &Map<String, Value>) -> Result<Value, String> {
        if let Some(code) = self.exit {
            return Ok(json!({ "exited": true, "code": code }));
        }
        let timeout = millis(req, "timeout_ms", WAIT_DEFAULT_MS)?;
        let owned = self
            .owned
            .as_mut()
            .ok_or_else(|| "the child was already released".to_string())?;
        // Look without reaping: a leader that has exited still pins its process group, so
        // `terminate_tree` can end anything it left behind before the reap.
        let handle = owned
            .process_handle()
            .map_err(|e| format!("wait for the child: {e}"))?;
        match handle.wait_exited_timeout(timeout) {
            Ok(true) => {
                // The leader exited and is not reaped yet: what it left running is recorded as
                // survivors first, then the tree is ended and the leader reaped.
                record_survivors_if_exited(owned, &format!("spawn {}", self.label), tracker);
                let status = owned
                    .terminate_tree()
                    .map_err(|e| format!("reap the child: {e}"))?;
                let code = status.code();
                self.reaped(tracker, code);
                Ok(json!({ "exited": true, "code": code }))
            }
            Ok(false) => Ok(json!({ "exited": false, "code": Value::Null })),
            Err(e) => Err(format!("wait for the child: {e}")),
        }
    }

    fn kill(&mut self, tracker: &Tracker) -> Result<Value, String> {
        if let Some(code) = self.exit {
            return Ok(json!({ "exited": true, "code": code, "note": Value::Null }));
        }
        let owned = self
            .owned
            .as_mut()
            .ok_or_else(|| "the child was already released".to_string())?;
        // A leader that has already exited left its descendants behind on its own: recorded before
        // the kill. A running leader is killed on request, tree and all, which records nothing.
        record_survivors_if_exited(owned, &format!("spawn {}", self.label), tracker);
        let (status, note) = terminate_bounded(owned);
        match status {
            Some(status) => {
                let code = status.code();
                self.reaped(tracker, code);
                Ok(json!({ "exited": true, "code": code, "note": note }))
            }
            None => Err(format!(
                "the child (pid {}) was not reaped after tree termination{}",
                self.pid,
                note.map(|n| format!(": {n}")).unwrap_or_default()
            )),
        }
    }
}

/// The path text a spawn result reports for a log, for tests and callers that read it back.
pub fn log_path(value: &Value, key: &str) -> Option<PathBuf> {
    value.get(key).and_then(Value::as_str).map(PathBuf::from)
}
