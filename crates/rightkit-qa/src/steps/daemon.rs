//! The `start_daemon` and `stop_daemon` steps: run the app's engine as a long-lived listener (a
//! daemon host), tracked by the scenario, and stop it bounded.
//!
//! `start_daemon` launches `<engine> <command...> [<port_flag> 0] <args...>` through the same
//! isolated-data-directory guard as every engine launch, with the engine environment (the world
//! marker and ports included), the data-directory variable and the step's `env`, and holds stdin open
//! (closing stdin is the stop signal). It waits (bounded) for one JSON line on stdout,
//! `{"event": "<ready_event>", "port": N}`, and fails with the engine's typed error line
//! (`{"error": {"code", "message"}}`) and its stderr if the process exits first. The process is
//! registered with the scenario's tracker, so it is inside the job and the orphan check sees it. Its
//! whole stdout and stderr go to `<id>-daemon.stdout.log` and `<id>-daemon.stderr.log` in the log
//! directory, where `secret_absent` scans them.
//!
//! `start_daemon` fields: `id` (default `daemon`), `command` (default `["serve"]`), `args`, `env`
//! (strings; the step's own `env` comes last and can override), `env_remove` (names removed from the
//! inherited environment first), `port_flag` (default `--port`; the step adds `<flag> 0` unless
//! `args` already names it; `""` adds nothing), `ready_event` (default `listening`), `scheme`
//! (`http` default, or `https`, for `${<id>_url}`), `listen_timeout_s` (default 20, at most 120),
//! `public_port` (a second plain-HTTP listener: `0` picks a free loopback port and passes
//! `<public_flag> <port>`; `public_flag` defaults to `--public-port`), `secret_files` (a table of
//! environment variable to secret value: each value is written to an owner-only file in
//! `<data dir>/e2e-secrets-<id>/` and the variable is set to the file's path; the value is held, so
//! `engine_log` with `forbid_held` proves it never reached the log), `hold_files` (files whose lines
//! are held as secrets, for example a private key a plugin step wrote).
//!
//! TLS is the app's: a plugin step writes the test certificate files, `env` names them for the
//! engine (`POSTRIGHT_DAEMON_TLS_CERT = "${tls.cert_path}"`), and `hold_files` holds the key.
//!
//! Counts as an engine start of the scenario (`start_daemon` is its own declaration, so it is allowed).
//!
//! Sets `${<id>_url}` (`<scheme>://localhost:<port>`), `${<id>_port}`, `${<id>_pid}` and, with
//! `public_port`, `${<id>_public_url}` and `${<id>_public_port}`. Result `{ listening, port, pid,
//! bind, url, public_url }`.
//!
//! `stop_daemon` fields: `id`, `wait_s` (default 30, at most 120). It closes stdin and waits bounded
//! for the process to exit; one that does not is killed and the step fails. Result `{ code,
//! stdout }` (the daemon's stdout lines after the ready line, at most 20). A daemon still running at
//! scenario teardown is stopped the same way and reported. A process the daemon left running after it
//! exited is recorded as a survivor before it is ended, and the scenario is `errored`. `stop_daemon`
//! unsets `${<id>_port}` and `${<id>_base_url}` (also when the stop fails).
use super::{only, scenario_path, string_table, strings, text, uint};
use crate::check::select;
use crate::engine::EngineTarget;
use crate::process::{settle_exit, terminate_bounded, Tracker};
use crate::scenario::Ctx;
use crate::userdata::{check_engine_launch, EngineLaunch};
use crate::util::{err, tail, Error, Result};
use rightkit_child_process::{OwnedChild, OwnedCommand, WaitOutcome};
use rightkit_fs::private::{ensure_private_dir, write_private_file};
use serde_json::{json, Map, Value};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const DEFAULT_ID: &str = "daemon";
const LISTEN_WAIT: Duration = Duration::from_secs(20);
const STOP_WAIT: Duration = Duration::from_secs(30);

/// One running daemon. Closing its stdin is the stop signal.
pub(super) struct Running {
    /// The step's `id`; names the tracker label and the survivors this daemon leaves.
    id: String,
    child: OwnedChild,
    stdin: Option<ChildStdin>,
    /// The plain-HTTP public listener, when `start_daemon` asked for one.
    pub(super) public: Option<SocketAddr>,
    /// Every stdout line after the ready line (a typed error on a failed run).
    stdout: Receiver<String>,
    stderr_log: PathBuf,
}

struct Stopped {
    code: Option<i32>,
    stdout: Vec<String>,
}

fn stderr_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let tail = tail(text.trim(), 800);
    if tail.is_empty() {
        "empty".to_string()
    } else {
        tail
    }
}

impl Running {
    /// Close stdin (the documented stop), wait bounded for exit, and kill and report a daemon that
    /// does not exit.
    fn stop(mut self, tracker: &Tracker, wait: Duration) -> Result<Stopped> {
        let pid = self.child.id();
        drop(self.stdin.take());
        // The leader gets the whole wait to exit on its own after stdin closed. Descendants it left
        // running are recorded as survivors before they are ended (the scenario is `errored`), then
        // the tree is ended and the leader reaped; a leader that does not exit has its tree killed.
        let label = format!("daemon {}", self.id);
        match settle_exit(&mut self.child, &label, tracker, wait) {
            Ok(WaitOutcome::Exited(status)) => {
                tracker.forget(pid);
                let stdout: Vec<String> = self.stdout.try_iter().take(20).collect();
                Ok(Stopped {
                    code: status.code(),
                    stdout,
                })
            }
            other => {
                tracker.forget(pid);
                let note = match other {
                    Err(e) => format!("; {e}"),
                    _ => String::new(),
                };
                Err(Error(format!(
                    "the daemon did not exit within {}s of its stdin closing and was killed (stderr tail: {}){note}",
                    wait.as_secs(),
                    stderr_tail(&self.stderr_log),
                )))
            }
        }
    }
}

fn daemon_id(p: &Map<String, Value>) -> Result<String> {
    let id = text(p, "id")?.unwrap_or_else(|| DEFAULT_ID.to_string());
    if id.is_empty()
        || id.len() > 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return err(format!(
            "daemon id `{id}` must be 1 to 32 letters, digits, `_` or `-`"
        ));
    }
    Ok(id)
}

/// The address the `--bind` argument names (default 127.0.0.1).
fn bind_address(args: &[String]) -> IpAddr {
    let named = args
        .windows(2)
        .find(|pair| pair[0] == "--bind")
        .map(|pair| pair[1].trim_start_matches('[').trim_end_matches(']'));
    match named {
        Some(text) if text.eq_ignore_ascii_case("localhost") => IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(text) => text.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        None => IpAddr::V4(Ipv4Addr::LOCALHOST),
    }
}

/// A free port on `ip`: bind 0, read the port, release it. A port taken in between fails the start
/// with the engine's own bind error.
fn free_port(ip: IpAddr) -> Result<u16> {
    let listener = TcpListener::bind(SocketAddr::new(ip, 0))
        .map_err(|e| Error(format!("find a free public port on {ip}: {e}")))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| Error(format!("read the free public port: {e}")))
}

/// Interpret one stdout line: the port of the ready event, or a remembered typed error.
fn note_stdout_line(
    line: &str,
    ready_event: &str,
    engine_error: &mut Option<String>,
) -> Option<std::result::Result<u16, String>> {
    let value = serde_json::from_str::<Value>(line.trim()).ok()?;
    let event = select(&value, "event").and_then(|e| e.as_str().map(str::to_string));
    if event.as_deref() == Some(ready_event) {
        return Some(
            select(&value, "port")
                .and_then(|p| p.as_u64())
                .and_then(|p| u16::try_from(p).ok())
                .ok_or_else(|| format!("the {ready_event} line has no valid port: {line}")),
        );
    }
    if let Some(code) = select(&value, "error.code").and_then(|c| c.as_str().map(str::to_string)) {
        let message = select(&value, "error.message")
            .and_then(|m| m.as_str().map(str::to_string))
            .unwrap_or_default();
        *engine_error = Some(format!("{code}: {message}"));
    }
    None
}

fn exited_before_ready(
    status: Option<std::process::ExitStatus>,
    engine_error: Option<String>,
) -> String {
    let how = match status {
        Some(status) => format!("exited with {status}"),
        None => "closed its stdout but did not exit".to_string(),
    };
    match engine_error {
        Some(error) => format!("the daemon {how} before it was ready; the engine said {error}"),
        None => format!("the daemon {how} before it was ready and printed no typed error"),
    }
}

/// Wait (bounded) for the ready line; fail when the process exits first.
fn await_ready(
    child: &mut OwnedChild,
    lines: &Receiver<String>,
    ready_event: &str,
    wait: Duration,
) -> std::result::Result<u16, String> {
    let deadline = Instant::now() + wait;
    let mut engine_error: Option<String> = None;
    loop {
        match lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if let Some(port) = note_stdout_line(&line, ready_event, &mut engine_error) {
                    return port;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let until = Instant::now() + Duration::from_secs(5);
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if Instant::now() < until => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        _ => break None,
                    }
                };
                return Err(exited_before_ready(status, engine_error));
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                while let Ok(line) = lines.try_recv() {
                    if let Some(port) = note_stdout_line(&line, ready_event, &mut engine_error) {
                        return port;
                    }
                }
                return Err(exited_before_ready(Some(status), engine_error));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("cannot poll the daemon process: {e}")),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the daemon printed no `{ready_event}` line within {}s",
                wait.as_secs()
            ));
        }
    }
}

/// Kill a daemon that failed to start and describe why, with its stderr.
fn startup_failure(
    child: &mut OwnedChild,
    tracker: &Tracker,
    reason: &str,
    stderr_log: &Path,
) -> Error {
    let pid = child.id();
    let (_, note) = terminate_bounded(child);
    tracker.forget(pid);
    Error(format!(
        "{reason} (stderr: {}){}",
        stderr_tail(stderr_log),
        note.map(|n| format!("; {n}")).unwrap_or_default()
    ))
}

/// Everything `launch` needs from the scenario.
struct Launch<'a> {
    engine: &'a EngineTarget,
    data_dir: &'a Path,
    log_dir: &'a Path,
    tracker: &'a Tracker,
    id: &'a str,
    /// Set once the engine process exists, so the caller counts the start even when it then fails.
    started: &'a Cell<bool>,
    argv: &'a [String],
    step_env: &'a [(String, String)],
    env_remove: &'a [String],
    ready_event: &'a str,
    wait: Duration,
}

/// Spawn the engine through the guard and tracker, drain its stdout and stderr into the log
/// directory, and wait for the ready line. The process is gone, forgotten and described on failure.
fn launch(l: &Launch) -> Result<(Running, u16)> {
    let launch_env = l.engine.env_for(Some(l.data_dir), l.step_env);
    // The same guard the engine's own launches pass: no isolated data directory, no launch.
    check_engine_launch(&EngineLaunch {
        binary: &l.engine.binary,
        args: l.argv,
        data_dir: Some(l.data_dir),
        data_dir_env: l.engine.data_dir_env.as_deref(),
        data_dir_arg: l.engine.data_dir_arg.as_deref(),
        forbidden_args: &l.engine.forbidden_args,
        env: &launch_env,
        refusal_probe: l.engine.allow_no_data_dir_for_refusal_probe,
    })
    .map_err(|e| Error(format!("rightkit-qa refused the daemon launch: {e}")))?;
    let stderr_log = l.log_dir.join(format!("{}-daemon.stderr.log", l.id));
    let stdout_log = l.log_dir.join(format!("{}-daemon.stdout.log", l.id));
    let open = |path: &Path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| Error(format!("create {}: {e}", path.display())))
    };
    let stderr = open(&stderr_log)?;
    let mut cmd = Command::new(&l.engine.binary);
    cmd.args(l.argv);
    for name in l.env_remove {
        cmd.env_remove(name);
    }
    for (k, v) in &launch_env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr));
    let mut owned = OwnedCommand::from_command(cmd);
    owned.windows_hide();
    let mut child = owned
        .spawn()
        .map_err(|e| Error(format!("spawn {}: {e}", l.engine.binary.display())))?;
    let pid = child.id();
    l.started.set(true);
    l.tracker.register(pid, &format!("daemon {}", l.id));
    let stdin = child.take_stdin();
    let Some(stdout) = child.take_stdout() else {
        return Err(startup_failure(
            &mut child,
            l.tracker,
            "the daemon has no stdout",
            &stderr_log,
        ));
    };
    // Drain stdout for the process lifetime on a thread: a daemon that never prints cannot hang the
    // scenario, a closed pipe cannot make the engine's later print panic, and every byte reaches the
    // log whether or not a step reads the lines.
    let (sender, lines) = channel::<String>();
    let log_path = stdout_log.clone();
    std::thread::spawn(move || {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok();
        let mut reader = BufReader::new(stdout);
        loop {
            let mut raw = Vec::new();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(file) = log.as_mut() {
                        let _ = file.write_all(&raw);
                    }
                    let line = String::from_utf8_lossy(&raw).trim().to_string();
                    // A dropped receiver only ends the line feed; the log keeps filling.
                    let _ = sender.send(line);
                }
            }
        }
    });
    let port = match await_ready(&mut child, &lines, l.ready_event, l.wait) {
        Ok(port) => port,
        Err(reason) => {
            return Err(startup_failure(&mut child, l.tracker, &reason, &stderr_log));
        }
    };
    Ok((
        Running {
            id: l.id.to_string(),
            child,
            stdin,
            public: None,
            stdout: lines,
            stderr_log,
        },
        port,
    ))
}

pub(super) fn start(ctx: &mut Ctx, p: &Map<String, Value>) -> Result<Value> {
    only(
        p,
        "start_daemon",
        &[
            "id",
            "command",
            "args",
            "env",
            "env_remove",
            "port_flag",
            "ready_event",
            "scheme",
            "listen_timeout_s",
            "public_port",
            "public_flag",
            "secret_files",
            "hold_files",
        ],
    )?;
    let id = daemon_id(p)?;
    if ctx.steps.daemons.contains_key(&id) {
        return err(format!("daemon `{id}` is already running: stop it first"));
    }
    let engine = ctx
        .engine
        .clone()
        .ok_or_else(|| Error("start_daemon needs the engine binary".into()))?;
    if !engine.binary.exists() {
        return err(format!(
            "engine binary not found at {}",
            engine.binary.display()
        ));
    }
    let mut argv = if p.get("command").is_some_and(|v| !v.is_null()) {
        strings(p, "command")?
    } else {
        vec!["serve".to_string()]
    };
    if argv.is_empty() {
        return err("`command` must not be empty");
    }
    let user_args = strings(p, "args")?;
    let port_flag = text(p, "port_flag")?.unwrap_or_else(|| "--port".to_string());
    if !port_flag.is_empty() && !user_args.contains(&port_flag) {
        argv.push(port_flag);
        argv.push("0".to_string());
    }
    argv.extend(user_args);
    let ready_event = text(p, "ready_event")?.unwrap_or_else(|| "listening".to_string());
    let scheme = text(p, "scheme")?.unwrap_or_else(|| "http".to_string());
    if scheme != "http" && scheme != "https" {
        return err("`scheme` must be http or https");
    }
    let wait = if p.get("listen_timeout_s").is_some_and(|v| !v.is_null()) {
        Duration::from_secs(uint(p, "listen_timeout_s")?.unwrap_or(20).clamp(1, 120))
    } else {
        LISTEN_WAIT
    };

    let mut step_env: Vec<(String, String)> = Vec::new();
    let mut public: Option<SocketAddr> = None;
    if let Some(wanted) = uint(p, "public_port")? {
        let flag = text(p, "public_flag")?.unwrap_or_else(|| "--public-port".to_string());
        if argv.contains(&flag) {
            return err("give the public listener as `public_port`, not as an `args` entry");
        }
        let ip = bind_address(&argv);
        let port = if wanted == 0 {
            free_port(ip)?
        } else {
            u16::try_from(wanted).map_err(|_| Error("`public_port` must be 0 to 65535".into()))?
        };
        argv.push(flag);
        argv.push(port.to_string());
        public = Some(SocketAddr::new(ip, port));
    } else if p.get("public_flag").is_some_and(|v| !v.is_null()) {
        return err("`public_flag` needs `public_port`");
    }

    let secrets = string_table(p, "secret_files")?;
    if !secrets.is_empty() {
        let dir = ctx.data_dir.join(format!("e2e-secrets-{id}"));
        ensure_private_dir(&dir).map_err(|e| Error(format!("create {}: {e}", dir.display())))?;
        for (var, value) in &secrets {
            if var.is_empty() || !var.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return err(format!(
                    "`secret_files` key `{var}` must be an environment variable name (letters, digits, `_`)"
                ));
            }
            let path = dir.join(var.to_ascii_lowercase());
            write_private_file(&path, value.as_bytes())
                .map_err(|e| Error(format!("write {}: {e}", path.display())))?;
            ctx.steps.hold(&format!("secret file {var}"), value);
            step_env.push((var.clone(), path.to_string_lossy().into_owned()));
        }
    }
    for file in strings(p, "hold_files")? {
        let path = scenario_path(&ctx.data_dir, &ctx.log_dir, &file)?;
        let content = std::fs::read_to_string(&path)
            .map_err(|e| Error(format!("read {}: {e}", path.display())))?;
        for line in content.lines() {
            let line = line.trim();
            if line.len() >= 16 && !line.starts_with("-----") {
                ctx.steps.hold(&format!("secret in {file}"), line);
            }
        }
    }
    // The step's own `env` comes last, so it can override what the runner set.
    step_env.extend(string_table(p, "env")?);
    let env_remove = strings(p, "env_remove")?;

    let started = Cell::new(false);
    let launched = launch(&Launch {
        engine: &engine,
        data_dir: &ctx.data_dir,
        log_dir: &ctx.log_dir,
        tracker: ctx.tracker,
        id: &id,
        started: &started,
        argv: &argv,
        step_env: &step_env,
        env_remove: &env_remove,
        ready_event: &ready_event,
        wait,
    });
    // The daemon is an engine start of the scenario, counted once the process existed (the step is
    // its own declaration, so it is also allowed).
    if started.get() {
        ctx.engine_session.note_daemon_engine();
    }
    let (mut running, port) = launched?;
    running.public = public;
    let ip = bind_address(&argv);
    let pid = running.child.id();
    let url = format!("{scheme}://localhost:{port}");
    ctx.vars.insert(format!("{id}_url"), json!(url));
    ctx.vars.insert(format!("{id}_port"), json!(port));
    ctx.vars.insert(format!("{id}_pid"), json!(pid));
    let public_url = public.map(|addr| format!("http://localhost:{}", addr.port()));
    if let (Some(addr), Some(u)) = (public, &public_url) {
        ctx.vars.insert(format!("{id}_public_url"), json!(u));
        ctx.vars
            .insert(format!("{id}_public_port"), json!(addr.port()));
    }
    for name in [
        format!("{id}-daemon.stdout.log"),
        format!("{id}-daemon.stderr.log"),
    ] {
        let path = ctx.log_dir.join(&name);
        ctx.artifact(&name, path);
    }
    ctx.steps.daemons.insert(id, running);
    Ok(json!({
        "listening": true,
        "port": port,
        "pid": pid,
        "bind": ip.to_string(),
        "url": url,
        "public_url": public_url,
    }))
}

/// The variable suffixes `start_daemon` sets under the daemon's id, and so `stop_daemon` unsets.
const DAEMON_VAR_SUFFIXES: [&str; 5] = ["url", "port", "pid", "public_url", "public_port"];

/// Remove every variable `start_daemon` set for `id`.
fn clear_daemon_vars(vars: &mut BTreeMap<String, Value>, id: &str) {
    for suffix in DAEMON_VAR_SUFFIXES {
        vars.remove(&format!("{id}_{suffix}"));
    }
}

pub(super) fn stop(ctx: &mut Ctx, p: &Map<String, Value>) -> Result<Value> {
    only(p, "stop_daemon", &["id", "wait_s"])?;
    let id = daemon_id(p)?;
    let wait = match uint(p, "wait_s")? {
        Some(0) => return err("`wait_s` must be at least 1"),
        Some(secs) => Duration::from_secs(secs.min(120)),
        None => STOP_WAIT,
    };
    let daemon = ctx.steps.daemons.remove(&id).ok_or_else(|| {
        Error(format!(
            "no daemon `{id}` is running: add a start_daemon step first"
        ))
    })?;
    let stopped = daemon.stop(ctx.tracker, wait);
    // The daemon is gone either way: every variable `start_daemon` set for it no longer names
    // anything, so a later step that uses one is an unresolved variable, not a connection to
    // something else.
    clear_daemon_vars(&mut ctx.vars, &id);
    let stopped = stopped?;
    Ok(json!({ "code": stopped.code, "stdout": stopped.stdout }))
}

/// Stop every daemon still running (scenario teardown); each one found running, and each that
/// would not exit, is a problem.
pub(super) fn stop_all(ctx: &mut Ctx) -> Vec<String> {
    let mut problems = Vec::new();
    for (id, daemon) in std::mem::take(&mut ctx.steps.daemons) {
        if let Err(e) = daemon.stop(ctx.tracker, STOP_WAIT) {
            problems.push(format!("daemon `{id}`: {}", e.0));
        }
    }
    problems
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct Fx {
        data: PathBuf,
        logs: PathBuf,
        tracker: Tracker,
        engine: EngineTarget,
    }

    impl Fx {
        /// An "engine" that is `/bin/sh`: the script is the first argument.
        fn new() -> Self {
            let root = crate::storage::app_temp_dir()
                .join(format!("rkqa-daemon-{}", crate::util::new_id()));
            let (data, logs) = (root.join("data"), root.join("logs"));
            std::fs::create_dir_all(&data).unwrap();
            std::fs::create_dir_all(&logs).unwrap();
            Self {
                data,
                logs,
                tracker: Tracker::new(),
                engine: EngineTarget::new("/bin/sh").with_data_dir_env("QA_DAEMON_DATA"),
            }
        }

        fn launch(&self, script: &str, wait_ms: u64) -> Result<(Running, u16)> {
            let argv = vec!["-c".to_string(), script.to_string()];
            launch(&Launch {
                engine: &self.engine,
                data_dir: &self.data,
                log_dir: &self.logs,
                tracker: &self.tracker,
                id: "d1",
                started: &Cell::new(false),
                argv: &argv,
                step_env: &[],
                env_remove: &[],
                ready_event: "listening",
                wait: Duration::from_millis(wait_ms),
            })
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            self.tracker.kill_all();
        }
    }

    const READY: &str = r#"echo '{"event":"listening","port":4242}'"#;

    #[test]
    fn a_daemon_that_prints_ready_is_tracked_and_stops_when_stdin_closes() {
        let fx = Fx::new();
        let script = format!("{READY}; echo bye; cat >/dev/null");
        let (running, port) = fx.launch(&script, 10_000).unwrap();
        assert_eq!(port, 4242);
        assert_eq!(fx.tracker.pids().len(), 1);
        let stopped = running.stop(&fx.tracker, Duration::from_secs(10)).unwrap();
        assert_eq!(stopped.code, Some(0));
        assert!(fx.tracker.pids().is_empty());
        // The daemon's whole stdout is in the log, for the secret scan.
        let log = std::fs::read_to_string(fx.logs.join("d1-daemon.stdout.log")).unwrap();
        assert!(log.contains("listening") && log.contains("bye"), "{log}");
    }

    /// Oracle item 1: a daemon that exits 0 and leaves a grandchild running. The grandchild is
    /// recorded as a survivor before the sweep ends it, so the scenario cannot pass.
    #[test]
    fn a_grandchild_a_daemon_leaves_after_exiting_is_recorded_as_a_survivor() {
        let fx = Fx::new();
        let script = format!("{READY}; sleep 60 & echo $!; cat >/dev/null");
        let (running, _) = fx.launch(&script, 10_000).unwrap();
        let stopped = running.stop(&fx.tracker, Duration::from_secs(10)).unwrap();
        assert_eq!(stopped.code, Some(0));
        let grandchild: u32 = stopped
            .stdout
            .iter()
            .find_map(|l| l.trim().parse().ok())
            .unwrap_or_else(|| panic!("no pid line in {:?}", stopped.stdout));
        let recorded = fx.tracker.recorded();
        assert!(
            recorded
                .iter()
                .any(|(pid, label)| *pid == grandchild && label.contains("daemon d1")),
            "{recorded:?}"
        );
        assert!(
            fx.tracker.alive().iter().any(|(pid, _)| *pid == grandchild),
            "teardown reads it as a survivor"
        );
        assert!(fx.tracker.assert_no_orphans().is_err());
    }

    /// A daemon that stops cleanly records nothing.
    #[test]
    fn a_daemon_that_leaves_nothing_behind_records_no_survivor() {
        let fx = Fx::new();
        let (running, _) = fx
            .launch(&format!("{READY}; cat >/dev/null"), 10_000)
            .unwrap();
        running.stop(&fx.tracker, Duration::from_secs(10)).unwrap();
        assert!(fx.tracker.recorded().is_empty());
        assert!(fx.tracker.alive().is_empty());
    }

    /// Gap: `stop_daemon` unsets every variable `start_daemon` set.
    #[test]
    fn stop_daemon_unsets_the_variables_start_daemon_set() {
        let fx = Fx::new();
        let (running, _) = fx
            .launch(&format!("{READY}; cat >/dev/null"), 10_000)
            .unwrap();
        let root = fx.data.parent().unwrap().to_path_buf();
        let cfg = crate::config::QaConfig::parse(
            "app = \"daemontest\"\n",
            &root.join("rightkit-qa.toml"),
        )
        .unwrap();
        let ws = crate::workspace::create(&root.join("managed"), None, "daemontest").unwrap();
        let mut ctx = Ctx::new(&cfg, None, None, &ws, &fx.tracker, "s").unwrap();
        for suffix in DAEMON_VAR_SUFFIXES {
            ctx.vars.insert(format!("d1_{suffix}"), json!(1));
        }
        ctx.steps.daemons.insert("d1".to_string(), running);
        let mut p = Map::new();
        p.insert("id".into(), json!("d1"));
        p.insert("wait_s".into(), json!(10));
        stop(&mut ctx, &p).unwrap();
        for suffix in DAEMON_VAR_SUFFIXES {
            assert!(!ctx.vars.contains_key(&format!("d1_{suffix}")), "{suffix}");
        }
    }

    #[test]
    fn a_daemon_that_exits_before_ready_fails_with_its_typed_error_and_stderr() {
        let fx = Fx::new();
        let script = r#"echo '{"error":{"code":"bind_failed","message":"port in use"}}'; echo oops >&2; exit 3"#;
        let e = match fx.launch(script, 10_000) {
            Ok(_) => panic!("started a daemon that never listened"),
            Err(e) => e,
        };
        assert!(
            e.0.contains("bind_failed: port in use") && e.0.contains("oops"),
            "{}",
            e.0
        );
        assert!(fx.tracker.pids().is_empty());
    }

    #[test]
    fn a_daemon_that_never_prints_ready_is_killed_after_the_wait() {
        let fx = Fx::new();
        let started = Instant::now();
        let e = match fx.launch("sleep 30", 1_000) {
            Ok(_) => panic!("started a daemon that never listened"),
            Err(e) => e,
        };
        assert!(e.0.contains("no `listening` line within 1s"), "{}", e.0);
        assert!(started.elapsed() < Duration::from_secs(20));
        assert!(fx.tracker.alive().is_empty());
    }

    #[test]
    fn a_daemon_that_ignores_the_stop_signal_is_killed_and_reported() {
        let fx = Fx::new();
        let script = format!("{READY}; sleep 60");
        let (running, _) = fx.launch(&script, 10_000).unwrap();
        let e = running
            .stop(&fx.tracker, Duration::from_secs(1))
            .map(|_| ())
            .unwrap_err();
        assert!(
            e.0.contains("did not exit within 1s") && e.0.contains("was killed"),
            "{}",
            e.0
        );
        assert!(fx.tracker.alive().is_empty());
    }

    #[test]
    fn the_isolated_data_dir_guard_applies_to_the_daemon_launch() {
        let mut fx = Fx::new();
        fx.engine.data_dir_env = None;
        let e = match fx.launch(READY, 1_000) {
            Ok(_) => panic!("launched without an isolated data dir"),
            Err(e) => e,
        };
        assert!(e.0.contains("refused the daemon launch"), "{}", e.0);
        assert!(fx.tracker.pids().is_empty());
    }

    #[test]
    fn ready_lines_and_bind_addresses_are_read_strictly() {
        let mut error = None;
        assert!(matches!(
            note_stdout_line(
                r#"{"event":"listening","port":80}"#,
                "listening",
                &mut error
            ),
            Some(Ok(80))
        ));
        assert!(matches!(
            note_stdout_line(
                r#"{"event":"listening","port":99999}"#,
                "listening",
                &mut error
            ),
            Some(Err(_))
        ));
        assert!(note_stdout_line("noise", "listening", &mut error).is_none());
        assert!(note_stdout_line(r#"{"event":"other"}"#, "listening", &mut error).is_none());
        assert!(note_stdout_line(
            r#"{"error":{"code":"x","message":"y"}}"#,
            "listening",
            &mut error
        )
        .is_none());
        assert_eq!(error.as_deref(), Some("x: y"));
        assert_eq!(bind_address(&[]), IpAddr::V4(Ipv4Addr::LOCALHOST));
        let args = ["--bind".to_string(), "[::1]".to_string()];
        assert_eq!(bind_address(&args), "::1".parse::<IpAddr>().unwrap());
        assert!(daemon_id(&Map::new()).is_ok());
        let mut bad = Map::new();
        bad.insert("id".into(), json!("a/b"));
        assert!(daemon_id(&bad).is_err());
    }
}

/// Runs on every host (the process tests above need `/bin/sh`).
#[cfg(test)]
mod var_tests {
    use super::*;

    #[test]
    fn clearing_a_daemon_removes_every_variable_start_daemon_sets_and_nothing_else() {
        let mut vars = BTreeMap::new();
        // Exactly the names the start path inserts.
        for name in [
            "d1_url",
            "d1_port",
            "d1_pid",
            "d1_public_url",
            "d1_public_port",
            "d2_url",
            "d1_other",
            "d10_url",
        ] {
            vars.insert(name.to_string(), json!(1));
        }
        clear_daemon_vars(&mut vars, "d1");
        let left: Vec<&str> = vars.keys().map(String::as_str).collect();
        assert_eq!(left, vec!["d10_url", "d1_other", "d2_url"]);
    }

    #[test]
    fn the_suffix_list_matches_what_start_daemon_inserts() {
        let source = include_str!("daemon.rs");
        for suffix in DAEMON_VAR_SUFFIXES {
            assert!(
                source.contains(&format!("format!(\"{{id}}_{suffix}\")")),
                "start_daemon does not set {{id}}_{suffix}"
            );
        }
    }
}
