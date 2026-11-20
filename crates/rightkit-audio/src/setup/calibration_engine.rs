//! Run a host's recognition engine as a bounded child over a retained PCM file.
//!
//! The host decides which binary, which sub-command, which environment and which limits; this
//! module owns the process discipline: the child is tied to the parent process (it never outlives
//! the app), its stdout is capped, a hung child is killed at the timeout, a non-zero exit is a
//! failure, and the JSON on stdout is decoded into the host's result type.
//!
//! The result type is the host's (`T: DeserializeOwned`): the wire format belongs to the engine.

use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;

/// How to launch the engine child.
pub struct EngineRun<'a> {
    /// The engine executable, already resolved by the host.
    pub engine_bin: &'a Path,
    /// First argument, e.g. `"calibrate"`; the PCM path follows it.
    pub subcommand: &'a str,
    /// Environment variables set on the child.
    pub env: &'a [(&'a str, &'a str)],
    /// Upper bound on the child's run time; exists so a hung child cannot block Stop forever.
    pub timeout: Duration,
    /// Most stdout bytes kept; more is an error.
    pub stdout_cap: usize,
}

/// Run `engine_bin <subcommand> <pcm>` and decode its stdout as JSON.
///
/// Error texts keep the `start recognition calibration:` prefixes existing hosts match on.
pub fn run_engine_calibration<T: DeserializeOwned>(
    run: &EngineRun<'_>,
    pcm: &Path,
) -> Result<T, String> {
    let mut owned = rightkit_child_process::OwnedCommand::new(run.engine_bin);
    owned.command_mut().arg(run.subcommand).arg(pcm);
    for (name, value) in run.env {
        owned.command_mut().env(name, value);
    }
    // The owner ties the child to this process: it never outlives the app if the app dies.
    owned.kill_on_parent_exit(true);
    let options = rightkit_child_process::CaptureOptions::default()
        .timeout(run.timeout)
        .stdout_cap(run.stdout_cap);
    let output =
        rightkit_child_process::run_command_capture(owned, None, &options).map_err(|error| {
            format!("start recognition calibration: spawn recognition calibration: {error}")
        })?;
    if output.timed_out {
        let detail = String::from_utf8_lossy(&output.stderr.bytes);
        let detail = detail.trim();
        return Err(if detail.is_empty() {
            "start recognition calibration: recognition calibration timed out".to_string()
        } else {
            format!(
                "start recognition calibration: recognition calibration timed out; stderr: {detail}"
            )
        });
    }
    if output.stdout.truncated {
        return Err(format!(
            "start recognition calibration: recognition calibration stdout exceeded {} bytes",
            run.stdout_cap
        ));
    }
    if !output.status.success() {
        return Err("recognition calibration process failed".to_string());
    }
    serde_json::from_slice(&output.stdout.bytes)
        .map_err(|error| format!("decode recognition calibration result: {error}"))
}
