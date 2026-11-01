//! rightkit-child-process (default features, no tokio): runs real processes. The engine binary itself (`--version`) to
//! prove bounded captured output; a shell child with a known exit code and both streams; output truncation caps; a
//! deadline that kills a real long-running child; an owned child whose whole tree is terminated; a restart-policy loop
//! that really respawns a crashing child until the budget is exhausted; health thresholds; process table and usage.
use rightkit_child_process::{
    process_table, process_usage, run, HealthOutcome, HealthPolicy, HealthProbe, HealthTracker, OwnedCommand, ReapOutcome,
    RestartMode, RestartOutcome, RestartPolicy, RestartTracker, RunOptions,
};
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::{Duration, Instant};

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-child-process: {what}"))
    }
}

fn e<T: std::fmt::Display>(what: &str) -> impl FnOnce(T) -> String + '_ {
    move |err| format!("rightkit-child-process: {what}: {err}")
}

/// A shell child that prints one line on each stream and exits with `code`.
fn exiter(code: i32) -> (&'static str, Vec<String>) {
    #[cfg(unix)]
    {
        ("/bin/sh", vec!["-c".into(), format!("echo out-line; echo err-line 1>&2; exit {code}")])
    }
    #[cfg(windows)]
    {
        ("cmd", vec!["/C".into(), format!("echo out-line& echo err-line 1>&2& exit {code}")])
    }
}

/// A child that would run for 30 seconds if nobody stopped it.
fn sleeper() -> (&'static str, Vec<String>) {
    #[cfg(unix)]
    {
        ("/bin/sh", vec!["-c".into(), "sleep 30".into()])
    }
    #[cfg(windows)]
    {
        ("ping", vec!["-n".into(), "30".into(), "127.0.0.1".into()])
    }
}

fn owned(program: &str, args: &[String]) -> OwnedCommand {
    let mut c = OwnedCommand::new(program);
    c.command_mut().args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    c
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let exe = std::env::current_exe().map_err(e("current_exe"))?;

    // The engine binary answers --version; its stdout is captured and parsed.
    let out = run(&exe, ["--version"], &RunOptions::new(Duration::from_secs(20))).map_err(e("run --version"))?;
    ensure(out.status.success() && !out.timed_out, &format!("engine --version exited {:?} timed_out={}", out.status.code(), out.timed_out))?;
    let v: Value = serde_json::from_slice(&out.stdout.bytes).map_err(e("--version JSON"))?;
    ensure(v["version"] == env!("CARGO_PKG_VERSION"), &format!("engine reported {v}"))?;

    // Output caps: the first bytes are kept, the rest counted.
    let capped = run(&exe, ["--version"], &RunOptions::new(Duration::from_secs(20)).stdout_cap(4)).map_err(e("run capped"))?;
    ensure(
        capped.stdout.bytes.len() == 4 && capped.stdout.truncated && capped.stdout.total_bytes > 4,
        &format!("cap result: kept {} truncated {} total {}", capped.stdout.bytes.len(), capped.stdout.truncated, capped.stdout.total_bytes),
    )?;

    // Exit code and both streams of a shell child.
    let (prog, args) = exiter(3);
    let r = run(prog, &args, &RunOptions::new(Duration::from_secs(20))).map_err(e("run exiter"))?;
    ensure(r.status.code() == Some(3) && !r.timed_out, &format!("exiter exit code {:?}", r.status.code()))?;
    ensure(String::from_utf8_lossy(&r.stdout.bytes).trim() == "out-line", "stdout of the shell child")?;
    ensure(String::from_utf8_lossy(&r.stderr.bytes).trim() == "err-line", "stderr of the shell child")?;

    // stdin to a real process (unix: cat).
    #[cfg(unix)]
    {
        let echoed = rightkit_child_process::run_with_stdin("cat", Vec::<String>::new(), b"piped input", &RunOptions::new(Duration::from_secs(20)))
            .map_err(e("run_with_stdin"))?;
        ensure(echoed.stdout.bytes == b"piped input", "cat did not echo stdin")?;
    }

    // A deadline terminates a real long-running child.
    let (prog, args) = sleeper();
    let started = Instant::now();
    let slow = run(prog, &args, &RunOptions::new(Duration::from_millis(400))).map_err(e("run sleeper"))?;
    ensure(slow.timed_out && !slow.status.success(), "a child past its deadline was not reported timed out")?;
    ensure(started.elapsed() < Duration::from_secs(15), &format!("the deadline did not stop the child promptly ({:?})", started.elapsed()))?;

    // An owned child: alive, visible in the process table, then its tree is terminated and reaped.
    let (prog, args) = sleeper();
    let mut child = owned(prog, &args).spawn().map_err(e("spawn owned"))?;
    let pid = child.id();
    ensure(child.try_wait().map_err(e("try_wait"))?.is_none(), "the sleeper exited on its own")?;
    let table = process_table().map_err(e("process_table"))?;
    let me = std::process::id();
    ensure(table.iter().any(|p| p.pid == pid && p.parent_pid == me), "the owned child is not in the process table under this process")?;
    ensure(rightkit_child_process::owned_count() >= 1, "owned_count does not see the owned child")?;
    match child.terminate_tree_bounded(Duration::from_secs(10)).map_err(e("terminate_tree_bounded"))? {
        ReapOutcome::Exited(status) => ensure(!status.success(), "a terminated sleeper reported success")?,
        ReapOutcome::TimedOut => return Err("rightkit-child-process: terminating the owned tree timed out".into()),
    }
    let after = process_table().map_err(e("process_table after"))?;
    ensure(!after.iter().any(|p| p.pid == pid && p.parent_pid == me), "the terminated child is still in the process table")?;

    // Restart policy driving real respawns: a child that always fails is restarted twice, then the budget is exhausted.
    let policy = RestartPolicy::new(RestartMode::Automatic, 2, Duration::from_secs(60), Duration::from_millis(10), Duration::from_millis(40))
        .map_err(e("RestartPolicy::new"))?;
    let mut tracker = RestartTracker::new(policy);
    let epoch = Instant::now();
    let mut spawns = 0u32;
    let mut delays = Vec::new();
    loop {
        let generation = tracker.started(epoch.elapsed());
        let (prog, args) = exiter(7);
        let mut c = owned(prog, &args).spawn().map_err(e("spawn crasher"))?;
        spawns += 1;
        let status = c.wait().map_err(e("wait crasher"))?;
        ensure(status.code() == Some(7), &format!("crasher exit code {:?}", status.code()))?;
        match tracker.exited(generation, epoch.elapsed()) {
            RestartOutcome::Restart(permit) => {
                delays.push(permit.delay());
                std::thread::sleep(permit.delay());
            }
            RestartOutcome::Exhausted => break,
            other => return Err(format!("rightkit-child-process: unexpected restart outcome {other:?}")),
        }
        ensure(spawns < 10, "the restart budget never ran out")?;
    }
    ensure(spawns == 3 && delays.len() == 2 && delays[1] >= delays[0], &format!("spawns {spawns}, delays {delays:?}"))?;

    // Health thresholds.
    let health = HealthPolicy::new(Duration::from_secs(5), 2).map_err(e("HealthPolicy::new"))?;
    let mut tracker = HealthTracker::new(health, Duration::ZERO);
    ensure(tracker.observe(Duration::from_secs(1), HealthProbe::Success) == HealthOutcome::Healthy, "first success is not healthy")?;
    ensure(tracker.observe(Duration::from_secs(2), HealthProbe::Failure) == HealthOutcome::Healthy, "one failure should stay healthy")?;
    ensure(
        tracker.observe(Duration::from_secs(3), HealthProbe::Failure) == HealthOutcome::Unhealthy { consecutive_failures: 2 },
        "two failures should be unhealthy",
    )?;
    let never = HealthTracker::new(health, Duration::ZERO);
    ensure(never.outcome(Duration::from_secs(6)) == HealthOutcome::StartupTimedOut, "startup timeout")?;

    // This process's own resource usage.
    let usage = process_usage().map_err(e("process_usage"))?;
    ensure(usage.resident_bytes > 0 && usage.cpu_seconds >= 0.0, &format!("process_usage {usage:?}"))?;

    Ok(json!({ "engine_version": v["version"], "restart_spawns": spawns, "resident_bytes": usage.resident_bytes, "owned_pid_terminated": pid }))
}
