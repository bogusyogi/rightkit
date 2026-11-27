//! Persistent process supervision for the Claude harness (PL-005).
//!
//! The supervisor is `rightkit_job_queue::ChildSupervisor` with the host's seam around it.
//! `ChildSupervisor` owns the spawn of the vendor process through
//! `rightkit_child_process::OwnedCommand` (so its tree is contained and `terminate_tree` ends
//! it), monotonic generation numbering, the restart ordering (stop the old child first, then
//! boot generation + 1) and a bounded, idempotent stop that reports whether the child was
//! reaped. The Claude Code CLI is kept alive across turns instead of being re-spawned per turn,
//! and every restart advances the generation recorded on the binding. A restart never boots
//! over a process that was not reaped: [`ClaudeHarnessSupervisor::restart`] stops the old child
//! first and refuses when that stop is unconfirmed.
//!
//! The host's part is small. The supervisor never decides the argv: the launch spec arrives
//! already validated by the trust boundary and the delegation policy seam (`workspace`,
//! `delegation`), and an injected [`HarnessSpawner`] turns it into the command to run, so tests
//! can drive the full lifecycle against a scripted process and the daemon can inject the shared
//! stdio command construction ([`stdio_harness_command`]) without this module depending on the
//! daemon binary. Claude `stream-json` says nothing before the first turn, so a generation is
//! ready as soon as it is spawned; the readiness probe is therefore trivial. The child's piped
//! stdin and stdout are handed to `SupervisedStdio`, the [`CliProcessIo`] the runtime reads and
//! writes, and its `shutdown` is the supervisor's `stop`.
//!
//! Linux: `PR_SET_PDEATHSIG` is keyed to the thread that forks. `ChildSupervisor` forks on its
//! own supervision thread, which lives for the whole run (every generation, until `stop` or the
//! child's exit), so a vendor process that the host asked to anchor to itself
//! (`rightkit_agent_coding_tools::shell::set_anchor_children_to_parent`) still dies with the
//! daemon and is not killed early while it is supervised.

use std::io::{self, BufReader, Write};
use std::process::{ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use rightkit_child_process::{OwnedCommand, RestartMode, RestartPolicy};
use rightkit_job_queue::{
    ChildBoot, ChildSession, ChildSupervisor, ChildSupervisorConfig, JobError,
};
use serde_json::Value;

use crate::driver_host::cli_runtime::adapter::{CliLaunchSpec, CliProcessIo, CliRuntimeError};
use crate::driver_host::cli_runtime::transports::process::{
    apply_launch_env, configure_sandbox, sandboxed_program,
};

use super::HarnessError;

/// Injected spawn seam: given the validated launch spec, build the vendor command (stdin and
/// stdout piped, stderr null). The supervisor owns the spawn and the resulting child.
pub type HarnessSpawner =
    Box<dyn Fn(&CliLaunchSpec) -> std::io::Result<OwnedCommand> + Send + Sync>;

/// Identity of a supervised process, recorded on the binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisedProcessInfo {
    pub pid: u32,
    pub generation: u64,
}

/// Build the vendor command exactly as `StdioProcessIo::spawn` does: sandboxed program, cleared
/// environment plus the launch's own, the Linux parent-death hook (when the host asked for its
/// children to be anchored to it) and the sandbox hook, stdin and stdout piped, stderr null. The
/// daemon injects this (or a wrapper of it) as the [`HarnessSpawner`].
pub fn stdio_harness_command(launch: &CliLaunchSpec) -> io::Result<OwnedCommand> {
    let (program, argv) = sandboxed_program(launch).map_err(runtime_error_to_io)?;
    let mut command = rightkit_child_process::command_std(program);
    command
        .args(argv)
        .current_dir(&launch.cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    apply_launch_env(&mut command, launch);
    // Linux: the vendor program dies with the daemon. Registered before the sandbox hook; the
    // supervisor forks on its supervision thread, which outlives the generations it boots.
    #[cfg(target_os = "linux")]
    if rightkit_agent_coding_tools::shell::anchor_children_to_parent() {
        rightkit_agent_coding_tools::shell::parent_death::kill_on_parent_exit_std(&mut command);
    }
    configure_sandbox(&mut command, launch).map_err(runtime_error_to_io)?;
    Ok(OwnedCommand::from_command(command))
}

fn runtime_error_to_io(error: CliRuntimeError) -> io::Error {
    io::Error::other(format!("{}: {}", error.code, error.message))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The stdio of one ready generation, as the runtime's [`CliProcessIo`]. The supervisor owns the
/// child itself; `shutdown` asks it to stop.
#[derive(Clone)]
struct SupervisedStdio {
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    supervisor: Arc<ChildSupervisor>,
    pid: u32,
    generation: u64,
    /// Windows only: the engine's shell job, nested over the child's own job for its
    /// active-process limit. RightKit's job owns the tree; this one only caps it. `None` when the
    /// host chose `host_options::set_windows_job_best_effort` and the job could not be assigned.
    #[cfg(windows)]
    windows_job: Arc<Mutex<Option<rightkit_agent_coding_tools::sandbox::WindowsJob>>>,
}

impl CliProcessIo for SupervisedStdio {
    fn write_line(&self, line: &str) -> Result<(), CliRuntimeError> {
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| CliRuntimeError::new("cli_process_lock", "stdin lock poisoned"))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|error| CliRuntimeError::new("cli_process_write", error.to_string()))
    }

    fn read_event(&self) -> Result<Option<Value>, CliRuntimeError> {
        let mut stdout = self
            .stdout
            .lock()
            .map_err(|_| CliRuntimeError::new("cli_process_lock", "stdout lock poisoned"))?;
        // Vendor CLIs are trusted and emit large frames, so the line is uncapped.
        let mut buffer = Vec::new();
        rightkit_sidecar::read_json_line::<_, Value>(
            &mut *stdout,
            &mut buffer,
            rightkit_sidecar::UNCAPPED,
        )
        .map_err(|error| match error {
            rightkit_sidecar::FrameError::Io(error) => {
                CliRuntimeError::new("cli_process_read", error.to_string())
            }
            error => CliRuntimeError::new("cli_process_invalid_json", error.to_string()),
        })
    }

    fn shutdown(&self) -> Result<(), CliRuntimeError> {
        let report = self.supervisor.stop();
        // Closing the limit job afterwards ends anything still in it, whatever `stop` said.
        #[cfg(windows)]
        self.windows_job
            .lock()
            .map_err(|_| CliRuntimeError::new("cli_process_lock", "job lock poisoned"))?
            .take();
        if report.is_reaped() {
            Ok(())
        } else {
            Err(CliRuntimeError::new(
                "cli_process_shutdown",
                format!(
                    "vendor process {} (generation {}) was not reaped: {:?}",
                    self.pid, self.generation, report.outcome
                ),
            ))
        }
    }

    fn clone_box(&self) -> Box<dyn CliProcessIo> {
        Box::new(self.clone())
    }
}

/// Owns the lifetime of one persistent Claude Code process.
pub struct ClaudeHarnessSupervisor {
    inner: Arc<ChildSupervisor>,
    /// The launch the spawn closure reads (`ChildSupervisorConfig`'s closure takes no argument).
    launch: Arc<Mutex<Option<CliLaunchSpec>>>,
    /// The stdio of the ready generation, stored by the `on_ready` hook.
    session: Arc<Mutex<Option<SupervisedStdio>>>,
    /// Set once a graceful `stop` ran; a subsequent plain `start` is refused (call `restart`,
    /// which re-arms the supervisor).
    stopped: AtomicBool,
}

impl ClaudeHarnessSupervisor {
    /// Build a supervisor around the spawn seam. The only fallible step is the restart policy
    /// (automatic restart is off: the runtime decides when to restart); its literal values are
    /// valid, and an invalid one would surface here as an error rather than a panic.
    pub fn new(spawner: HarnessSpawner) -> Result<Self, HarnessError> {
        let restart = RestartPolicy::new(
            RestartMode::Disabled,
            0,
            std::time::Duration::from_secs(60),
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
        )
        .map_err(|error| {
            HarnessError::InvalidState(format!("supervisor restart policy: {error}"))
        })?;
        let launch: Arc<Mutex<Option<CliLaunchSpec>>> = Arc::new(Mutex::new(None));
        let session: Arc<Mutex<Option<SupervisedStdio>>> = Arc::new(Mutex::new(None));
        // The hook needs the supervisor that owns it (for `SupervisedStdio::shutdown`), which
        // exists only after the config is consumed; a weak slot avoids a reference cycle.
        let slot: Arc<OnceLock<Weak<ChildSupervisor>>> = Arc::new(OnceLock::new());

        let spawn_launch = Arc::clone(&launch);
        let spawn = move || {
            let launch = lock(&spawn_launch)
                .clone()
                .ok_or_else(|| io::Error::other("no launch spec"))?;
            spawner(&launch)
        };
        // Claude stream-json says nothing before the first turn: ready means spawned.
        let probe = |_: &mut ChildBoot| Ok(true);
        let ready_session = Arc::clone(&session);
        let ready_slot = Arc::clone(&slot);
        let on_ready = move |child: ChildSession| {
            let ChildSession {
                generation,
                pid,
                stdio,
            } = child;
            let stdin = stdio.stdin.ok_or_else(|| {
                JobError::supervisor("cli_process_stdin", "child did not expose stdin")
            })?;
            let stdout = stdio.stdout.ok_or_else(|| {
                JobError::supervisor("cli_process_stdout", "child did not expose stdout")
            })?;
            let supervisor = ready_slot
                .get()
                .and_then(Weak::upgrade)
                .ok_or_else(|| JobError::supervisor("down", "supervisor is gone"))?;
            // The supervisor does not assign the engine's process-limit job (`StdioProcessIo`
            // does, after spawn). The child is already contained by RightKit's own job; this
            // second job only adds the active-process limit. Nesting can be refused: the launch
            // then fails (the boot fails and the child is stopped), unless the host chose
            // `host_options::set_windows_job_best_effort`, which runs the child without the cap.
            #[cfg(windows)]
            let windows_job = {
                let limit = rightkit_agent_coding_tools::sandbox::shell_job()
                    .and_then(|job| job.assign_process_id(pid).map(|()| job));
                match limit {
                    Ok(job) => Arc::new(Mutex::new(Some(job))),
                    Err(error) if crate::driver_host::host_options::windows_job_best_effort() => {
                        tracing::warn!(%error, pid, "vendor CLI runs without the engine's process-limit job");
                        Arc::new(Mutex::new(None))
                    }
                    Err(error) => {
                        return Err(JobError::supervisor("cli_process_job", error.to_string()))
                    }
                }
            };
            *lock(&ready_session) = Some(SupervisedStdio {
                stdin: Arc::new(Mutex::new(stdin)),
                stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
                supervisor,
                pid,
                generation,
                #[cfg(windows)]
                windows_job,
            });
            Ok(())
        };

        let config = ChildSupervisorConfig::new("claude-harness", spawn, probe)
            .with_on_ready(on_ready)
            .with_restart(restart);
        let inner = Arc::new(ChildSupervisor::new(config));
        slot.get_or_init(|| Arc::downgrade(&inner));
        Ok(Self {
            inner,
            launch,
            session,
            stopped: AtomicBool::new(false),
        })
    }

    fn info(&self) -> Result<SupervisedProcessInfo, HarnessError> {
        let status = self.inner.status();
        let pid = status
            .pid
            .ok_or_else(|| HarnessError::InvalidState("vendor process has no pid".to_string()))?;
        Ok(SupervisedProcessInfo {
            pid,
            generation: status.generation,
        })
    }

    /// Spawn the vendor process at the current generation. Fails when a process is already
    /// running (use [`Self::restart`]).
    pub fn start(&self, launch: &CliLaunchSpec) -> Result<SupervisedProcessInfo, HarnessError> {
        if self.inner.status().pid.is_some() || lock(&self.session).is_some() {
            return Err(HarnessError::AlreadyRunning);
        }
        if self.stopped.load(Ordering::SeqCst) {
            return Err(HarnessError::NotRunning);
        }
        *lock(&self.launch) = Some(launch.clone());
        self.inner
            .start()
            .map_err(|error| HarnessError::InvalidState(error.to_string()))?;
        self.info()
    }

    /// Clone the live process I/O handle for a reader or writer. The supervisor stays the sole
    /// owner of the underlying child; the clone shares the stdio and can only ask it to stop.
    pub fn io(&self) -> Result<Box<dyn CliProcessIo>, HarnessError> {
        let session = lock(&self.session);
        let stdio = session.as_ref().ok_or(HarnessError::NotRunning)?;
        Ok(stdio.clone_box())
    }

    /// The generation of the currently supervised process.
    pub fn generation(&self) -> Result<u64, HarnessError> {
        Ok(self.inner.status().generation)
    }

    /// The pid of the currently supervised process, if any.
    pub fn pid(&self) -> Result<Option<u32>, HarnessError> {
        Ok(self.inner.status().pid)
    }

    /// Record that the vendor process exited on its own (EOF on its stdout). Idempotent; returns
    /// whether a process was live.
    pub fn mark_exited(&self) -> Result<bool, HarnessError> {
        Ok(lock(&self.session).take().is_some())
    }

    /// Stop the old process, advance the generation, spawn a fresh one. The old child is
    /// stopped first through the supervisor's bounded `stop`, and the next generation is booted
    /// (generation + 1, the counter never resets) only when that stop reaped the old child, which
    /// is also the case when none was running. Re-arms the supervisor after a graceful stop. The
    /// restart policy is disabled, so no restart budget is spent either way.
    ///
    /// Callers see:
    /// - old process reaped (or none): the new generation is booted and its [`SupervisedProcessInfo`]
    ///   returned; the launch spec is stored for it.
    /// - old process not reaped: `Err` (`cli_process_shutdown`), nothing is spawned, the stored
    ///   launch spec is left as it was, the stored stdio is dropped and the supervisor is marked
    ///   stopped like after a failed [`Self::stop`]; calling `restart` again retries the stop.
    /// - the new generation fails to boot: `Err(InvalidState)` with the supervisor's reason; the
    ///   old process is already gone.
    pub fn restart(&self, launch: &CliLaunchSpec) -> Result<SupervisedProcessInfo, HarnessError> {
        let report = self.inner.stop();
        // The old child is stopped; drop its stdio before the new one arrives.
        lock(&self.session).take();
        if !report.is_reaped() {
            self.stopped.store(true, Ordering::SeqCst);
            return Err(HarnessError::from(CliRuntimeError::new(
                "cli_process_shutdown",
                format!(
                    "restart refused, the old vendor process was not reaped: {:?}",
                    report.outcome
                ),
            )));
        }
        *lock(&self.launch) = Some(launch.clone());
        self.inner
            .start()
            .map_err(|error| HarnessError::InvalidState(error.to_string()))?;
        self.stopped.store(false, Ordering::SeqCst);
        self.info()
    }

    /// Graceful stop. Idempotent: a second call is a no-op success. Bounded by the supervisor.
    pub fn stop(&self) -> Result<(), HarnessError> {
        let report = self.inner.stop();
        lock(&self.session).take();
        self.stopped.store(true, Ordering::SeqCst);
        if report.is_reaped() {
            Ok(())
        } else {
            Err(HarnessError::from(CliRuntimeError::new(
                "cli_process_shutdown",
                format!("vendor process was not reaped: {:?}", report.outcome),
            )))
        }
    }
}

impl Drop for ClaudeHarnessSupervisor {
    fn drop(&mut self) {
        // The stored stdio holds the supervisor, and the supervisor's hook holds the stored
        // stdio: clear it so the supervisor can drop, and stop the child.
        lock(&self.session).take();
        self.inner.stop();
    }
}
