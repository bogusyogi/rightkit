//! Detached job runners. A job keeps running after the caller exits because
//! `<engine> job run <id>` (the host engine binary, `std::env::current_exe`) is spawned as its own process; it holds the
//! job lock, so the sidecar and other engine processes never double-run it.

use std::process::Stdio;

/// Local sessions stay in their detached owner when the CLI's wait expires.
/// Unrouted callers (the legacy generate path) keep this signature.
pub fn wait_local_job(
    ctx: &crate::Context,
    input: &serde_json::Value,
) -> crate::GenResult<serde_json::Value> {
    wait_local_job_routed(ctx, input, None)
}

/// [`wait_local_job`] carrying the engine's routing record, so a routed local job keeps its explanation.
pub fn wait_local_job_routed(
    ctx: &crate::Context,
    input: &serde_json::Value,
    route: Option<&crate::routing::EngineRoute>,
) -> crate::GenResult<serde_json::Value> {
    if let Some(timeout) = input.get("wait_timeout_s").filter(|v| !v.is_null()) {
        let valid = timeout.as_f64().is_some_and(|t| t.is_finite() && t >= 0.0);
        if !valid {
            return Err(crate::GenError::new(
                "invalid_input",
                "wait_timeout_s must be a non-negative number of seconds",
            ));
        }
    }
    let submitted =
        crate::api::generate_routed_with(ctx, &crate::providers::registry(), input, false, route)?;
    let id = submitted["job_id"].as_str().unwrap_or_default();
    // The lease goes first: a runner that starts before it could see "no caller" and run free.
    // It is held for the whole wait; a caller that dies without releasing it ends the job.
    let _lease = crate::lease::Lease::attach(ctx, id, "wait");
    spawn_job_runner(ctx, id);
    let mut wait = serde_json::json!({"job_id": id});
    if let Some(timeout) = input.get("wait_timeout_s").filter(|v| !v.is_null()) {
        wait["timeout_s"] = timeout.clone();
    }
    let mut result = crate::api::job_wait_for_runner(ctx, &wait)?;
    if wait.get("timeout_s").is_none() {
        result.as_object_mut().map(|o| o.remove("timed_out"));
    }
    Ok(result)
}

/// Start the detached runner for `job_id`. The runner is pinned to the CALLER's data folder
/// explicitly (not left to whatever the environment happens to say), so `job status|cancel`
/// from that folder always sees the job it runs.
pub fn spawn_job_runner(ctx: &crate::Context, job_id: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let stderr_file = job_log(ctx, job_id).stderr_file().ok();
    let mut owned = rightkit_child_process::OwnedCommand::new(exe);
    {
        let cmd = owned.command_mut();
        cmd.args(["job", "run", job_id])
            .env(crate::host::env_name("APP_DATA"), &ctx.data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        #[cfg(unix)]
        cmd.stderr(stderr_file.map(Stdio::from).unwrap_or_else(Stdio::null));
    }
    #[cfg(windows)]
    {
        // Allowlist only the log handle: the runner must not inherit this process's stdout/stderr
        // pipes, or `generate` would not return until the job finished.
        let stderr = stderr_file
            .map(|file| {
                rightkit_child_process::WindowsStdio::Handle(
                    std::os::windows::io::OwnedHandle::from(file),
                )
            })
            .unwrap_or(rightkit_child_process::WindowsStdio::Null);
        owned.inherit_handles_only([]);
        owned.windows_spawn_config(rightkit_child_process::WindowsSpawnConfig {
            stdin: rightkit_child_process::WindowsStdio::Null,
            stdout: rightkit_child_process::WindowsStdio::Null,
            stderr,
            inherit_environment: true,
            raw_arguments: Vec::new(),
        });
    }
    // Detached and uncontained (breakaway, or the job-safe fallback inside a no-breakaway job).
    // The returned handle is dropped: a detached child is never terminated on drop.
    let _ = owned.spawn_uncontained_detached();
    // The resident desktop sidecar holds the lease of every local job it starts (no-op elsewhere).
    crate::lease::keep(ctx, job_id);
}

/// The runner's stderr carries NDJSON progress events. It is captured through the owner's `rightkit_logs::DetachedLog`:
/// the child appends raw stderr to a private raw file under `<data>/locks/job-stderr/`, and `drain_job_logs` later moves
/// it, scrubbed and rotated, into `<data>/logs/jobs/<job_id>.log` (the raw, unscrubbed file never sits in a collected
/// folder).
fn job_log(ctx: &crate::Context, job_id: &str) -> rightkit_logs::DetachedLog {
    let sink = std::sync::Arc::new(
        rightkit_logs::RotatingTextWriter::new(
            ctx.job_logs_dir().join(format!("{job_id}.log")),
            1 << 20,
        )
        .with_generations(3),
    );
    rightkit_logs::DetachedLog::new(sink)
        .with_raw_path(job_raw_dir(ctx).join(format!("{job_id}.raw")))
}

fn job_raw_dir(ctx: &crate::Context) -> std::path::PathBuf {
    ctx.locks_dir().join("job-stderr")
}

/// Move every detached runner's captured stderr into its scrubbed log. A job that is no longer recoverable has exited, so
/// its raw file is finished (drained whole and removed); a live or resumable one is drained up to its last full line.
/// Called at sidecar start and by every CLI invocation, like `resume_recoverable`.
pub fn drain_job_logs(ctx: &crate::Context, live: &std::collections::HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(job_raw_dir(ctx)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let (Some("raw"), Some(id)) = (
            path.extension().and_then(|e| e.to_str()),
            path.file_stem().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        let log = job_log(ctx, id);
        let _ = if live.contains(id) {
            log.drain().map(|_| ())
        } else {
            log.finish().map(|_| ())
        };
    }
}

/// Resume every recoverable job (§3 Jobs): called at sidecar start and by
/// every CLI invocation, so interrupted work continues on the next start.
/// A job store that cannot be opened is returned, never treated as "no jobs".
pub fn resume_recoverable() -> crate::GenResult<()> {
    let ctx = crate::Context::production();
    let recoverable = crate::api::recoverable_jobs(&ctx)?;
    drain_job_logs(&ctx, &recoverable.iter().cloned().collect());
    for id in recoverable {
        spawn_job_runner(&ctx, &id);
    }
    Ok(())
}
