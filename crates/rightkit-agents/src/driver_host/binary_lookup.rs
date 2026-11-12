//! `PATH` lookup for vendor CLI binaries (`crate::discovery::find_binaries`) and the
//! bounded readers for probes that run through the host app's own launch adapter.
//!
//! The probe readers below do not duplicate RightKit: they read stdout and stderr of a
//! `rightkit_sandbox::execution::ManagedChild` that the host app launched itself, which is the host app's own
//! process model. RightKit's `run_bounded` starts its own unconfined child instead.

use std::path::PathBuf;

/// Locate `name` on the process `PATH`, using the host's executable rules (`PATHEXT` on Windows,
/// the executable bit elsewhere). Only absolute candidates are accepted: RightKit's search joins
/// `name` onto every `PATH` entry, so an empty entry would otherwise resolve to a path relative
/// to the working directory.
pub(crate) fn find_on_path(name: &str) -> Option<PathBuf> {
    first_absolute(crate::discovery::find_binaries(name))
}

fn first_absolute(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|path| path.is_absolute())
}

/// Upper bound for a sandboxed `--version` probe. A slow or hung sandboxed
/// launch must not hang provider discovery.
pub(crate) const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Read stdout to EOF and wait for exit, bounded by [`VERSION_PROBE_TIMEOUT`].
/// On timeout the child is cancelled (killed and reaped) so no orphan remains.
/// Returns `(exit_code, stdout_bytes)` or a human-readable error.
pub(crate) async fn collect_version_output(
    child: Box<dyn rightkit_sandbox::execution::ManagedChild>,
) -> Result<(i32, Vec<u8>), String> {
    collect_probe_output(child, VERSION_PROBE_TIMEOUT, "--version").await
}

/// Upper bound for the whole of one discovery request: every probe it runs together must fit,
/// so the HTTP route answers well inside a 30s client timeout.
pub(crate) const DISCOVERY_TOTAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// Read stdout to EOF (stderr is drained and discarded so a chatty child cannot block on a full
/// pipe) and wait for exit, all within `timeout`. On timeout the child is killed and reaped.
pub(crate) async fn collect_probe_output(
    child: Box<dyn rightkit_sandbox::execution::ManagedChild>,
    timeout: std::time::Duration,
    label: &str,
) -> Result<(i32, Vec<u8>), String> {
    collect_probe_streams(child, timeout, label)
        .await
        .map(|(status, stdout, _stderr)| (status, stdout))
}

/// Like [`collect_probe_output`] but also returns stderr (capped at 64 KiB).
pub(crate) async fn collect_probe_streams(
    mut child: Box<dyn rightkit_sandbox::execution::ManagedChild>,
    timeout: std::time::Duration,
    label: &str,
) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
    const STDERR_CAP: usize = 64 * 1024;
    let work = async {
        let mut stdout = child.take_stdout();
        let mut stderr = child.take_stderr();
        let read_stdout = async {
            let mut bytes = Vec::new();
            if let Some(stream) = stdout.as_mut() {
                tokio::io::AsyncReadExt::read_to_end(stream, &mut bytes)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok::<_, String>(bytes)
        };
        let read_stderr = async {
            let mut kept = Vec::new();
            if let Some(stream) = stderr.as_mut() {
                let mut buffer = [0u8; 4096];
                while let Ok(read) = tokio::io::AsyncReadExt::read(stream, &mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    let room = STDERR_CAP.saturating_sub(kept.len());
                    kept.extend_from_slice(&buffer[..read.min(room)]);
                }
            }
            kept
        };
        let (bytes, err_bytes) = tokio::join!(read_stdout, read_stderr);
        let bytes = bytes?;
        let status = child
            .wait()
            .await
            .map_err(|e| format!("probe wait failed: {e:?}"))?;
        Ok::<_, String>((status, bytes, err_bytes))
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.cancel()).await;
            Err(format!("{label} timed out after {}s", timeout.as_secs()))
        }
    }
}

/// Spawn `spec` in the native sandbox and collect stdout and stderr, with spawn and collection
/// together bounded by `timeout`.
///
/// The probe runs on its own task: a sandbox launch that blocks synchronously instead of
/// yielding would otherwise stall the caller's timeout along with it. The caller's deadline
/// (probe bound plus a short kill grace) is enforced from outside that task, which is aborted
/// when it expires.
pub(crate) async fn run_bounded_probe_streams(
    spec: rightkit_sandbox::execution::CommandLaunchSpec,
    timeout: std::time::Duration,
    label: &str,
) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
    let owned_label = label.to_string();
    let mut task = tokio::spawn(probe_streams_inner(spec, timeout, owned_label));
    let outer = timeout + std::time::Duration::from_secs(3);
    match tokio::time::timeout(outer, &mut task).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_error)) => Err(format!("{label} probe task failed: {join_error}")),
        Err(_) => {
            task.abort();
            Err(format!("{label} timed out after {}s", timeout.as_secs()))
        }
    }
}

async fn probe_streams_inner(
    spec: rightkit_sandbox::execution::CommandLaunchSpec,
    timeout: std::time::Duration,
    label: String,
) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
    let label = label.as_str();
    let started = std::time::Instant::now();
    let adapter = rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter;
    let child = match tokio::time::timeout(
        timeout,
        rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec),
    )
    .await
    {
        Ok(Ok(child)) => child,
        Ok(Err(error)) => return Err(format!("{label} spawn failed: {error:?}")),
        Err(_) => return Err(format!("{label} timed out after {}s", timeout.as_secs())),
    };
    let remaining = timeout
        .checked_sub(started.elapsed())
        .unwrap_or(std::time::Duration::from_millis(1));
    collect_probe_streams(child, remaining, label).await
}

/// Spawn `spec` in the native sandbox and collect its stdout, with spawn and collection together
/// bounded by `timeout`. Every discovery-time child (`--version`, `auth status`, ...) goes through
/// here so none can outlive the discovery budget.
pub(crate) async fn run_bounded_probe(
    spec: rightkit_sandbox::execution::CommandLaunchSpec,
    timeout: std::time::Duration,
    label: &str,
) -> Result<(i32, Vec<u8>), String> {
    run_bounded_probe_streams(spec, timeout, label)
        .await
        .map(|(status, stdout, _stderr)| (status, stdout))
}
