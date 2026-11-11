//! Consumer-facing commands of the `rightkit-qa` binary that are not scenario runs (rightkit#430):
//!
//! * `rightkit-qa install [--to DIR] [--json]` puts this executable, and `rightkit-qa-interactive`
//!   when it sits beside it, at a stable root (default `<managed root>/agent-bin`) with a receipt
//!   that names each file's version and sha256, so a consumer's script calls them by one path on
//!   both hosts.
//! * `rightkit-qa exec [--timeout-s N] [--cwd DIR] [--max-output-bytes N] [--json] -- <program> [args]`
//!   runs one program to completion in an owned process tree that is killed at the deadline.
//! * `rightkit-qa proc kill-tree --pid N --expect-command TEXT [--json]` terminates a process tree,
//!   but only when the pid's command line still contains `TEXT` (a reused pid is not killed).
//!
//! * `rightkit-qa control call --service NAME --method M [--params JSON] [--token-file FILE]
//!   [--app right-qa]` is a command-line client of an embedded control server that is already
//!   running (rightkit#752): one order in, the server's JSON reply out. The token file defaults to
//!   `RIGHTKIT_QA_CONTROL_TOKEN_FILE`. `@rightkit/qa` wraps it for Node (`controlCall`).
//!
//! Every command prints one JSON document with `--json` and exits with a code a script can branch
//! on: 0 done, 1 the program failed or timed out, 2 usage or setup error, 3 refused.

use crate::process::{fingerprint, is_alive, kill_tree, run_owned, RunOptions, Tracker};
use crate::util::{err, sha256_hex, Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const INSTALLED: [&str; 2] = ["rightkit-qa", "rightkit-qa-interactive"];

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn emit(json_out: bool, value: &Value, text: &str) {
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
    } else {
        println!("{text}");
    }
}

// ------------------------------------------------------------------------------------ install

/// `rightkit-qa install`.
pub fn install(args: &[String]) -> Result<i32> {
    let json_out = has(args, "--json");
    let to = match flag(args, "--to") {
        Some(t) => PathBuf::from(t),
        None => crate::storage::managed_root(None).join("agent-bin"),
    };
    if !to.is_absolute() {
        return err(format!("--to {} is not an absolute path", to.display()));
    }
    let me =
        std::env::current_exe().map_err(|e| Error(format!("cannot find this executable: {e}")))?;
    let me = crate::util::canonical_plain(&me)
        .map_err(|e| Error(format!("cannot resolve {}: {e}", me.display())))?;
    let from_dir = me
        .parent()
        .ok_or_else(|| Error(format!("{} has no folder", me.display())))?
        .to_path_buf();
    std::fs::create_dir_all(&to)
        .map_err(|e| Error(format!("cannot create {}: {e}", to.display())))?;
    let version = env!("CARGO_PKG_VERSION");
    let mut files: Vec<Value> = Vec::new();
    for base in INSTALLED {
        let name = exe_name(base);
        let source = if base == "rightkit-qa" {
            me.clone()
        } else {
            from_dir.join(&name)
        };
        if !source.is_file() {
            if base == "rightkit-qa" {
                return err(format!("{} is not a file", source.display()));
            }
            continue;
        }
        let dest = to.join(&name);
        let bytes = std::fs::read(&source)
            .map_err(|e| Error(format!("cannot read {}: {e}", source.display())))?;
        let sha = sha256_hex(&bytes);
        let same_file = crate::util::canonical_plain(&dest)
            .map(|d| d == source)
            .unwrap_or(false);
        let already = std::fs::read(&dest)
            .map(|b| sha256_hex(&b) == sha)
            .unwrap_or(false);
        if !same_file && !already {
            publish(&source, &dest, &sha)?;
        }
        files.push(json!({
            "name": name,
            "path": dest.display().to_string(),
            "source": source.display().to_string(),
            "sha256": sha,
            "size": bytes.len(),
            "replaced": !same_file && !already,
        }));
    }
    let receipt = json!({
        "schema": "rightkit.qa.install/1",
        "version": version,
        "root": to.display().to_string(),
        "files": files,
    });
    let receipt_path = to.join("rightkit-qa.install.json");
    std::fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&receipt).unwrap_or_default(),
    )
    .map_err(|e| Error(format!("cannot write {}: {e}", receipt_path.display())))?;
    let text = files
        .iter()
        .map(|f| {
            format!(
                "{} {} ({})",
                f["name"].as_str().unwrap_or(""),
                version,
                f["path"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    emit(
        json_out,
        &receipt,
        &format!("installed at {}:\n{text}", to.display()),
    );
    Ok(0)
}

/// Copy `source` over `dest` through a temporary name and verify the hash, so a reader never sees a
/// half-written executable. Keeps the executable bit; a macOS signature stays valid on a copy.
fn publish(source: &Path, dest: &Path, sha: &str) -> Result<()> {
    let Some(file_name) = dest.file_name().and_then(|n| n.to_str()) else {
        return err(format!("{} has no usable file name", dest.display()));
    };
    let tmp = dest.with_file_name(format!("{file_name}.{}.tmp", std::process::id()));
    std::fs::copy(source, &tmp).map_err(|e| {
        Error(format!(
            "cannot copy {} to {}: {e}",
            source.display(),
            tmp.display()
        ))
    })?;
    let copied =
        std::fs::read(&tmp).map_err(|e| Error(format!("cannot read {}: {e}", tmp.display())))?;
    if sha256_hex(&copied) != sha {
        let _ = std::fs::remove_file(&tmp);
        return err(format!(
            "the copy {} does not match {}; nothing was installed",
            tmp.display(),
            source.display()
        ));
    }
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error(format!(
            "cannot put {} in place (is it running?): {e}",
            dest.display()
        ))
    })
}

// ------------------------------------------------------------------------------------ exec

/// `rightkit-qa exec`.
pub fn exec(args: &[String]) -> Result<i32> {
    let json_out = has(args, "--json");
    let split = args.iter().position(|a| a == "--");
    let (own, program) = match split {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (args, &args[args.len()..]),
    };
    let Some(prog) = program.first() else {
        return err("usage: rightkit-qa exec [--timeout-s N] [--cwd DIR] [--max-output-bytes N] [--json] -- <program> [args...]");
    };
    let timeout = match flag(own, "--timeout-s") {
        Some(t) => Some(Duration::from_secs(t.parse::<u64>().map_err(|_| {
            Error(format!("--timeout-s {t:?} is not a number of seconds"))
        })?)),
        None => None,
    };
    let max_output = match flag(own, "--max-output-bytes") {
        Some(t) => t
            .parse::<usize>()
            .map_err(|_| Error(format!("--max-output-bytes {t:?} is not a number")))?,
        None => usize::MAX,
    };
    let opts = RunOptions {
        cwd: flag(own, "--cwd").map(PathBuf::from),
        timeout,
        label: "rightkit-qa exec".to_string(),
        ..RunOptions::default()
    };
    let started = Instant::now();
    let tracker = Tracker::new();
    let out = run_owned(prog, &program[1..], &opts, &tracker)?;
    let stdout = crate::util::tail(&out.stdout, max_output);
    let stderr = crate::util::tail(&out.stderr, max_output);
    let code = if out.timed_out { None } else { out.code };
    let doc = json!({
        "schema": "rightkit.qa.exec/1",
        "program": prog,
        "args": &program[1..],
        "pid": out.pid,
        "exit_code": code,
        "timed_out": out.timed_out,
        "duration_ms": started.elapsed().as_millis() as u64,
        "stdout": stdout,
        "stderr": stderr,
    });
    if json_out {
        emit(true, &doc, "");
    } else {
        print!("{stdout}");
        eprint!("{stderr}");
    }
    Ok(if out.timed_out { 1 } else { code.unwrap_or(1) })
}

// ------------------------------------------------------------------------------------ proc

/// `rightkit-qa proc kill-tree`.
pub fn proc_cmd(args: &[String]) -> Result<i32> {
    let json_out = has(args, "--json");
    if args.first().map(String::as_str) != Some("kill-tree") {
        return err("usage: rightkit-qa proc kill-tree --pid N --expect-command TEXT [--json]");
    }
    let pid: u32 = flag(args, "--pid")
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| Error("--pid N is required".into()))?;
    let Some(expect) = flag(args, "--expect-command").filter(|e| !e.is_empty()) else {
        return err("--expect-command TEXT is required: a pid can be reused, so the process is killed only when its command line still contains TEXT");
    };
    if !is_alive(pid) {
        emit(
            json_out,
            &json!({"schema": "rightkit.qa.proc/1", "pid": pid, "action": "kill-tree", "was_running": false, "running": false}),
            &format!("pid {pid} is not running"),
        );
        return Ok(0);
    }
    let Some(command) = fingerprint(pid) else {
        emit(
            json_out,
            &json!({"schema": "rightkit.qa.proc/1", "pid": pid, "action": "kill-tree", "refused": "no command line to check the identity against on this host"}),
            &format!("pid {pid}: cannot verify its identity on this host; refused"),
        );
        return Ok(3);
    };
    if !command.contains(&expect) {
        emit(
            json_out,
            &json!({"schema": "rightkit.qa.proc/1", "pid": pid, "action": "kill-tree", "refused": "command line does not contain the expected text", "command": command}),
            &format!("pid {pid} runs {command:?}, which does not contain {expect:?}; refused"),
        );
        return Ok(3);
    }
    kill_tree(pid);
    let running = is_alive(pid);
    emit(
        json_out,
        &json!({"schema": "rightkit.qa.proc/1", "pid": pid, "action": "kill-tree", "was_running": true, "running": running, "command": command}),
        &format!(
            "pid {pid} {}",
            if running {
                "is still running"
            } else {
                "terminated"
            }
        ),
    );
    Ok(if running { 1 } else { 0 })
}

// ------------------------------------------------------------------------------------ control

/// `rightkit-qa control call`.
pub fn control_cmd(args: &[String]) -> Result<i32> {
    if args.first().map(String::as_str) != Some("call") {
        return err("usage: rightkit-qa control call --service NAME --method M [--params JSON] [--token-file FILE] [--app right-qa]");
    }
    let service =
        flag(args, "--service").ok_or_else(|| Error("--service NAME is required".into()))?;
    let method = flag(args, "--method").ok_or_else(|| Error("--method M is required".into()))?;
    let params: Value = match flag(args, "--params") {
        Some(p) => {
            serde_json::from_str(&p).map_err(|e| Error(format!("--params is not JSON: {e}")))?
        }
        None => json!({}),
    };
    let app = flag(args, "--app").unwrap_or_else(|| "right-qa".to_string());
    let token_file = flag(args, "--token-file")
        .or_else(|| std::env::var(crate::control::token::ENV_AUTH_FILE).ok())
        .ok_or_else(|| {
            Error(format!(
                "--token-file (or {}) is required: the control server refuses an order with no token",
                crate::control::token::ENV_AUTH_FILE
            ))
        })?;
    let token = crate::control::token::read_file(Path::new(&token_file))?;
    let suite = rightkit_roots::Suite::per_user_default()
        .map_err(|e| Error(format!("cannot resolve the suite root: {e}")))?;
    let client = rightkit_daemon::Client::connect(&suite, &service, &app)
        .map_err(|e| Error(format!("cannot connect to {service}: {e}")))?;
    let reply = client.call(
        &method,
        crate::control::token::with_token(params, Some(&token)),
    );
    match reply {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            Ok(0)
        }
        Err(e) => {
            println!("{}", json!({"error": e.to_string()}));
            Ok(1)
        }
    }
}
