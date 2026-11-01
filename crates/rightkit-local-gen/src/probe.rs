//! `<engine> local-model …`: DEBUG-BUILD-ONLY probe of the machine-wide local-image
//! model-server lock (`crate::machine`). It takes (or reports) the same lock a job
//! takes before it starts `sd-server`, but starts no server and loads no model, so the end-to-end
//! suite can test lock behaviour (holder, busy, yield, stale takeover) without a model. A "server"
//! here is a harmless child of this same binary that only sleeps (`sleep`), so the pid, start
//! time and executable checks the lock performs on a real server are real.
//!
//! ```text
//! <engine> local-model acquire [--model <id>] [--hold-secs <n>] [--fake-server]
//!                                     [--busy-secs <n>] [--yield-to-waiters]
//! <engine> local-model status
//! <engine> local-model identity --pid <n>          # alive, start time, executable
//! <engine> local-model sleep --secs <n> [--stop-file <path>]
//! <engine> local-model spawn-orphan --secs <n> [--stop-file <path>]
//! <engine> local-model tied-owner [--model <id>] [--hold-secs <n>]   # unix only
//! <engine> local-model memory-launch [--setting <mode>] [--total-gb <n>]
//! ```
//! `memory-launch` prints the memory mode a job would use and the exact extra `sd-server`
//! arguments (`crate::memory::select`: `<PREFIX>_LOCAL_IMAGE_MEMORY_MODE` wins over
//! `--setting`, then `auto` from `--total-gb`, else this machine). It starts nothing.
//! `acquire` honours `<PREFIX>_LOCAL_MODEL_WAIT_SECS` and `<PREFIX>_MACHINE_LOCK_DIR` exactly as a
//! job does; on a busy machine it exits 1 with `{"error":{"code":"local_model_busy",…}}`. As a
//! holder it behaves like the desktop worker: `--fake-server` spawns the sleeping child and
//! records it (pid, start time, executable) like `SdServer`; the holder is `busy` for
//! `--busy-secs` (default: the whole hold) and `idle` after; with `--yield-to-waiters` it asks
//! the lock's own yield rule every 250 ms, and on a yield kills its fake server, releases the
//! lock and exits 0. Without a waiter it holds for `--hold-secs`, unloads and releases.
//! `spawn-orphan` starts a sleeper that outlives this process (reparented, so a stale-holder
//! scenario can leave a live server behind a dead holder) and prints its pid.
//! `tied-owner` plays the model-server OWNER: it takes the machine lock, then starts the sleeper
//! as production starts sd-server (an owned command with `kill_on_parent_exit(true)`), records it
//! as the lock's server, prints `{owner_pid, server_pid}` and holds. A scenario `kill -9`s the
//! owner and asserts the sleeper is gone within the bound and the lock can be taken over.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rightkit_child_process::{OwnedChild, OwnedCommand};
use std::time::{Duration, Instant};

use crate::machine::{self, AcquireError, HolderState, MachineLock};
use serde_json::{json, Value};

fn print(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn error(code: &str, message: &str, details: Value) -> Value {
    json!({"error": {"code": code, "message": message, "details": details}})
}

fn usage(message: &str) -> i32 {
    print(&error("bad_usage", message, Value::Null));
    2
}

#[derive(Default)]
struct Args {
    model: Option<String>,
    hold: Duration,
    busy: Option<Duration>,
    fake_server: bool,
    yield_to_waiters: bool,
    secs: u64,
    pid: Option<u32>,
    stop_file: Option<PathBuf>,
    setting: Option<String>,
    total_gb: Option<f64>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let secs = |value: Option<&String>| -> Result<u64, String> {
            value
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| format!("{arg} takes whole seconds"))
        };
        match arg.as_str() {
            "--model" => parsed.model = it.next().cloned(),
            // The test harness appends `--input <json>` to every command; the probe takes none.
            "--input" => {
                it.next();
            }
            "--hold-secs" => parsed.hold = Duration::from_secs(secs(it.next())?),
            "--busy-secs" => parsed.busy = Some(Duration::from_secs(secs(it.next())?)),
            "--secs" => parsed.secs = secs(it.next())?,
            "--pid" => parsed.pid = Some(secs(it.next())? as u32),
            "--stop-file" => parsed.stop_file = it.next().map(PathBuf::from),
            "--setting" => parsed.setting = it.next().cloned(),
            "--total-gb" => {
                parsed.total_gb = Some(
                    it.next()
                        .and_then(|v| v.parse::<f64>().ok())
                        .ok_or_else(|| "--total-gb takes a number".to_string())?,
                )
            }
            "--fake-server" => parsed.fake_server = true,
            "--yield-to-waiters" => parsed.yield_to_waiters = true,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok(parsed)
}

/// The engine word that routes to [`run`]; the fake servers re-enter the binary through it.
pub const PROBE_WORD: &str = "local-model";

/// Run the probe with `args` (the words after [`PROBE_WORD`]) against the data folder `data_dir`.
/// Answers `unknown_command` in a release build.
pub fn run(args: &[String], data_dir: &Path) -> i32 {
    if !cfg!(debug_assertions) {
        print(&error(
            "unknown_command",
            "unknown command: local-model",
            Value::Null,
        ));
        return 2;
    }
    let Some(command) = args.first().map(String::as_str) else {
        return usage("usage: local-model acquire|status|identity|sleep|spawn-orphan");
    };
    let parsed = match parse(&args[1..]) {
        Ok(parsed) => parsed,
        Err(message) => return usage(&message),
    };
    match command {
        "status" => {
            print(&machine::report());
            0
        }
        "acquire" => acquire(parsed, data_dir),
        "identity" => identity(parsed),
        "sleep" => {
            sleep(&parsed);
            0
        }
        "spawn-orphan" => spawn_orphan(parsed),
        "tied-owner" => tied_owner(parsed, data_dir),
        "memory-launch" => memory_launch(parsed),
        _ => usage("usage: local-model acquire|status|identity|sleep|spawn-orphan|tied-owner"),
    }
}

/// Sleeps `--secs` seconds, or until `--stop-file` exists. The harmless stand-in for `sd-server`.
fn sleep(args: &Args) {
    let until = Instant::now() + Duration::from_secs(args.secs);
    // Stop when asked, and also when the stop file's folder is gone: the scenario that owned
    // this sleeper has ended and removed its scratch, so nobody can ask any more.
    let stopped = |f: &PathBuf| f.exists() || f.parent().is_some_and(|dir| !dir.exists());
    while Instant::now() < until && !args.stop_file.as_ref().is_some_and(stopped) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn sleeper(secs: u64, stop_file: Option<&PathBuf>) -> std::io::Result<OwnedChild> {
    sleeper_command(secs, stop_file).spawn()
}

fn sleeper_command(secs: u64, stop_file: Option<&PathBuf>) -> OwnedCommand {
    let mut owned =
        OwnedCommand::new(std::env::current_exe().unwrap_or_else(|_| PathBuf::from("engine")));
    let command = owned.command_mut();
    command.args([PROBE_WORD, "sleep", "--secs", &secs.to_string()]);
    if let Some(stop) = stop_file {
        command.arg("--stop-file").arg(stop);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    owned
}

/// The orphan must outlive the command that started it. A QA runner kills the whole process
/// group (Unix) or job (Windows) of the command it ran, so the orphan leaves both; inside a job
/// that denies breakaway, RightKit's detached spawn falls back to staying in the job.
fn detached_sleeper(secs: u64, stop_file: Option<&PathBuf>) -> std::io::Result<OwnedChild> {
    sleeper_command(secs, stop_file).spawn_uncontained_detached()
}

/// Start a sleeper that is NOT a child of the caller once this process exits: the tree is
/// dropped without waiting, so the OS reparents it, exactly like a server whose holder died.
fn spawn_orphan(args: Args) -> i32 {
    match detached_sleeper(args.secs.max(1), args.stop_file.as_ref()) {
        Ok(child) => {
            print(&json!({"pid": child.id()}));
            0
        }
        Err(e) => {
            print(&error("spawn_failed", &e.to_string(), Value::Null));
            1
        }
    }
}

fn memory_launch(args: Args) -> i32 {
    let env = crate::host::env_var(crate::memory::ENV);
    let total = args.total_gb.or_else(crate::memory::total_memory_gb);
    match crate::memory::select(env.as_deref(), args.setting.as_deref(), total) {
        Ok(selection) => {
            print(&json!({
                "requested": selection.requested,
                "source": selection.source,
                "mode": selection.mode.as_str(),
                "total_gb": selection.total_gb,
                "flags": selection.flags,
                "estimated_peak_gb": selection.estimated_peak_gb,
                "estimate": true,
            }));
            0
        }
        Err(message) => {
            print(&error("invalid_request", &message, Value::Null));
            1
        }
    }
}

fn identity(args: Args) -> i32 {
    let Some(pid) = args.pid else {
        return usage("identity needs --pid <n>");
    };
    let started = machine::process_started(pid);
    print(&json!({
        "pid": pid,
        "alive": started.is_some(),
        "started": started,
        "exe": machine::process_executable(pid).map(|p| p.to_string_lossy().into_owned()),
    }));
    0
}

#[cfg(not(unix))]
fn tied_owner(_args: Args, _data_dir: &Path) -> i32 {
    usage("tied-owner is unix only (Windows ties the server to its owner with a Job Object)")
}

#[cfg(unix)]
fn tied_owner(args: Args, data_dir: &Path) -> i32 {
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| "tied-model".to_string());
    let hold = if args.hold.is_zero() {
        Duration::from_secs(60)
    } else {
        args.hold
    };
    let data_dir = data_dir.to_path_buf();
    let lock = match MachineLock::acquire(
        &model,
        &data_dir,
        None,
        machine::wait_budget(),
        |_| {},
        || false,
    ) {
        Ok(lock) => lock,
        Err(other) => {
            print(&error(
                "local_model_lock_error",
                &other.message(),
                Value::Null,
            ));
            return 1;
        }
    };
    // Started exactly as `SdServer::start` starts the model server: an owned command with
    // `kill_on_parent_exit(true)` (rightkit-child-process). Spawned from the main thread, which
    // lives as long as the server (Linux ties the child to the spawning thread).
    let mut command = sleeper_command(hold.as_secs() + 60, None);
    command.kill_on_parent_exit(true);
    let mut server = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            print(&error("spawn_failed", &e.to_string(), Value::Null));
            return 1;
        }
    };
    // Recorded as production records it: the server is the lock's server (the kill target).
    let observed = machine::process_executable(server.id()).unwrap_or_default();
    lock.set_server(server.id(), None, &observed);
    print(&json!({
        "status": "started",
        "owner_pid": std::process::id(),
        "server_pid": server.id(),
    }));
    let until = Instant::now() + hold;
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(250));
    }
    // A normal end: take the tree down, then release (unload first, then release).
    let _ = server.terminate_tree();
    drop(lock);
    0
}

fn stop_fake(fake: Option<OwnedChild>) {
    if let Some(mut child) = fake {
        let _ = child.terminate_tree();
    }
}

fn acquire(args: Args, data_dir: &Path) -> i32 {
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| "probe-model".to_string());
    let data_dir = data_dir.to_path_buf();
    let wait_started = Instant::now();
    match MachineLock::acquire(
        &model,
        &data_dir,
        None,
        machine::wait_budget(),
        |_| {},
        || false,
    ) {
        Ok(lock) => {
            let waited_ms = wait_started.elapsed().as_millis() as u64;
            // The "server": a sleeping child, recorded exactly as a real one is.
            let fake = if args.fake_server {
                match sleeper(args.hold.as_secs() + 30, None) {
                    Ok(child) => {
                        let exe = machine::process_executable(child.id()).unwrap_or_default();
                        lock.set_server(child.id(), None, &exe);
                        Some(child)
                    }
                    Err(e) => {
                        print(&error("spawn_failed", &e.to_string(), Value::Null));
                        return 1;
                    }
                }
            } else {
                None
            };
            print(&json!({
                "status": "acquired",
                "waited_ms": waited_ms,
                "holder": lock.holder().to_json(),
                "took_over": lock.took_over.as_ref().map(|h| h.to_json()),
                "stale_server": lock.stale_server.as_ref().map(|s| s.to_json()),
                "lock_dir": machine::machine_dir().to_string_lossy(),
            }));
            let begun = Instant::now();
            let busy_until = begun + args.busy.unwrap_or(args.hold);
            let until = begun + args.hold;
            let tick = if args.yield_to_waiters {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(1)
            };
            let mut idle = false;
            let mut yielded = false;
            while Instant::now() < until {
                if !idle && Instant::now() >= busy_until {
                    lock.set_state(HolderState::Idle); // the job is done, the server stays warm
                    idle = true;
                }
                if args.yield_to_waiters && lock.yield_due() {
                    yielded = true;
                    break;
                }
                std::thread::sleep(tick);
            }
            // Unload first, then release: a waiter must never see a free lock with the server still alive.
            stop_fake(fake);
            drop(lock);
            // stderr: stdout stays the single JSON document the harness parses.
            eprintln!("{}", if yielded { "yielded" } else { "released" });
            0
        }
        Err(AcquireError::Busy { holder, waited }) => {
            let message = AcquireError::Busy {
                holder: holder.clone(),
                waited,
            }
            .message();
            let message = message
                .split_once(": ")
                .map(|(_, rest)| rest.to_string())
                .unwrap_or(message);
            print(&error(
                machine::BUSY_CODE,
                &message,
                json!({"holder": holder.to_json(), "waited_secs": waited.as_secs()}),
            ));
            1
        }
        Err(other) => {
            print(&error(
                "local_model_lock_error",
                &other.message(),
                Value::Null,
            ));
            1
        }
    }
}
