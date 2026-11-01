//! Persistent process supervision for the Claude harness (PL-005).
//!
//! Donor lineage: `cli_runtime::transports::process::StdioProcessIo`
//! owns spawning, environment denial, sandboxing and descendant
//! containment; `cli_runtime::recovery::LiveProcessGeneration` owns
//! monotonic restart generations; the executor's `restart_agent`
//! flow owns the shutdown-old-then-respawn ordering. This supervisor
//! composes exactly those pieces around one *persistent* vendor
//! process: the Claude Code CLI is kept alive across turns instead of
//! being re-spawned per turn, and every restart advances the
//! generation recorded on the binding.
//!
//! The supervisor never decides the argv. The launch spec arrives
//! already validated by the trust boundary and the delegation policy
//! seam (`workspace`, `delegation`); spawning itself is delegated to
//! an injected [`HarnessSpawner`] so tests can drive the full
//! lifecycle against a scripted process and the daemon can inject the
//! shared `StdioProcessIo`-based transport without this module
//! depending on the daemon binary (same seam as
//! `cli_runtime::executor::CliDriverHostBridge`).

use std::sync::Mutex;

use crate::driver_host::cli_runtime::adapter::{
    CliLaunchSpec, CliProcessIo, CliRuntimeError, StartedCliProcess,
};
use crate::driver_host::cli_runtime::recovery::LiveProcessGeneration;

use super::HarnessError;

/// Injected spawn seam: given the validated launch spec and the
/// generation the new process should carry, start the vendor process.
pub type HarnessSpawner =
    Box<dyn Fn(&CliLaunchSpec, u64) -> Result<StartedCliProcess, CliRuntimeError> + Send + Sync>;

/// Identity of a supervised process, recorded on the binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisedProcessInfo {
    pub pid: u32,
    pub generation: u64,
}

struct SupervisorState {
    process: Option<StartedCliProcess>,
    generation: LiveProcessGeneration,
    /// Set once a graceful `stop` ran; a subsequent plain `start` is
    /// refused (call `restart`, which re-arms the supervisor).
    stopped: bool,
}

/// Owns the lifetime of one persistent Claude Code process.
pub struct ClaudeHarnessSupervisor {
    spawner: HarnessSpawner,
    state: Mutex<SupervisorState>,
}

impl ClaudeHarnessSupervisor {
    pub fn new(spawner: HarnessSpawner) -> Self {
        Self {
            spawner,
            state: Mutex::new(SupervisorState {
                process: None,
                generation: LiveProcessGeneration::new("claude-harness"),
                stopped: false,
            }),
        }
    }

    /// Spawn the vendor process at the current generation. Fails when
    /// a process is already running (use [`Self::restart`]).
    pub fn start(&self, launch: &CliLaunchSpec) -> Result<SupervisedProcessInfo, HarnessError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HarnessError::InvalidState("supervisor mutex poisoned".to_string()))?;
        if state.process.is_some() {
            return Err(HarnessError::AlreadyRunning);
        }
        if state.stopped {
            return Err(HarnessError::NotRunning);
        }
        let generation = state.generation.generation;
        let process = (self.spawner)(launch, generation)?;
        let info = SupervisedProcessInfo {
            pid: process.pid,
            generation,
        };
        state.process = Some(process);
        Ok(info)
    }

    /// Clone the live process I/O handle for a reader or writer.
    /// Mirrors the donor `VendorSessionHandle` construction
    /// (`inner: process.inner.clone_box()`), which keeps the
    /// supervisor the sole owner of the underlying I/O state.
    pub fn io(&self) -> Result<Box<dyn CliProcessIo>, HarnessError> {
        let state = self
            .state
            .lock()
            .map_err(|_| HarnessError::InvalidState("supervisor mutex poisoned".to_string()))?;
        let process = state.process.as_ref().ok_or(HarnessError::NotRunning)?;
        Ok(process.inner.clone_box())
    }

    /// The generation of the currently supervised process.
    pub fn generation(&self) -> Result<u64, HarnessError> {
        let state = self
            .state
            .lock()
            .map_err(|_| HarnessError::InvalidState("supervisor mutex poisoned".to_string()))?;
        Ok(state.generation.generation)
    }

    /// The pid of the currently supervised process, if any.
    pub fn pid(&self) -> Result<Option<u32>, HarnessError> {
        let state = self
            .state
            .lock()
            .map_err(|_| HarnessError::InvalidState("supervisor mutex poisoned".to_string()))?;
        Ok(state.process.as_ref().map(|process| process.pid))
    }

    /// Record that the vendor process exited on its own (EOF on its
    /// stdout). Idempotent; returns whether a process was live.
    pub fn mark_exited(&self) -> Result<bool, HarnessError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HarnessError::InvalidState("supervisor mutex poisoned".to_string()))?;
        Ok(state.process.take().is_some())
    }

    /// Stop the old process, advance the generation, spawn a fresh
    /// one. This is the donor `restart_agent` ordering: shutdown the
    /// existing process first (propagating its error), then start the
    /// replacement. Re-arms the supervisor after a graceful stop.
    pub fn restart(&self, launch: &CliLaunchSpec) -> Result<SupervisedProcessInfo, HarnessError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HarnessError::InvalidState("supervisor mutex poisoned".to_string()))?;
        if let Some(old) = state.process.take() {
            // `StdioProcessIo::shutdown` terminates the whole vendor
            // process tree (Windows job object / process-group kill),
            // so no Claude descendant outlives the binding.
            if let Err(error) = old.inner.shutdown() {
                return Err(HarnessError::from(error));
            }
        }
        let generation = state.generation.restarted();
        let process = (self.spawner)(launch, generation)?;
        let info = SupervisedProcessInfo {
            pid: process.pid,
            generation,
        };
        state.process = Some(process);
        state.stopped = false;
        Ok(info)
    }

    /// Graceful stop. Idempotent: a second call is a no-op success
    /// (donor contract: "shutdown is idempotent; safe to call more
    /// than once" on `CliDriverHostBridge::shutdown`).
    pub fn stop(&self) -> Result<(), HarnessError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HarnessError::InvalidState("supervisor mutex poisoned".to_string()))?;
        if let Some(process) = state.process.take() {
            process.inner.shutdown().map_err(HarnessError::from)?;
        }
        state.stopped = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::claude_harness::test_support::{
        scripted_spawner, ScriptedInner, ScriptedProcessIo,
    };
    use std::sync::Arc;

    fn supervisor_with(lines: Vec<String>) -> (ClaudeHarnessSupervisor, Arc<ScriptedInner>) {
        let scripted = ScriptedProcessIo::new(lines);
        let inner = scripted.inner();
        (
            ClaudeHarnessSupervisor::new(scripted_spawner(&inner)),
            inner,
        )
    }

    fn minimal_launch() -> CliLaunchSpec {
        CliLaunchSpec {
            provider_id: crate::driver_host::cli_runtime::ids::CliProviderId::new("claude"),
            transport: crate::driver_host::cli_runtime::ids::CliTransportKind::ClaudeStreamJson,
            executable: "/usr/local/bin/claude".into(),
            argv: vec!["--print".to_string()],
            env_keep: Vec::new(),
            env_remove: Vec::new(),
            env: Vec::new(),
            cwd: "/work".into(),
            mcp_server: None,
            profile_hash: "profile".into(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        }
    }

    #[test]
    fn supervisor_start_assigns_generation_zero_and_exposes_io() {
        let (supervisor, _inner) = supervisor_with(vec![]);
        let info = supervisor.start(&minimal_launch()).unwrap();
        assert_eq!(info.pid, 4242);
        assert_eq!(info.generation, 0);
        assert_eq!(supervisor.generation().unwrap(), 0);
        assert_eq!(supervisor.pid().unwrap(), Some(4242));
        // The cloned I/O handle shares the scripted state.
        supervisor.io().unwrap().write_line("ping").unwrap();
    }

    #[test]
    fn supervisor_start_twice_is_rejected_as_already_running() {
        let (supervisor, _inner) = supervisor_with(vec![]);
        supervisor.start(&minimal_launch()).unwrap();
        let error = supervisor.start(&minimal_launch()).unwrap_err();
        assert!(matches!(error, HarnessError::AlreadyRunning));
    }

    #[test]
    fn supervisor_restart_shuts_the_old_process_and_increments_generation() {
        let (supervisor, inner) = supervisor_with(vec![]);
        supervisor.start(&minimal_launch()).unwrap();
        let restarted = supervisor.restart(&minimal_launch()).unwrap();
        assert_eq!(restarted.generation, 1);
        assert_eq!(inner.shutdown_count(), 1, "old process must be shut down");
        assert_eq!(supervisor.generation().unwrap(), 1);
        // A second restart advances again.
        let again = supervisor.restart(&minimal_launch()).unwrap();
        assert_eq!(again.generation, 2);
    }

    #[test]
    fn supervisor_stop_is_idempotent_and_rejects_later_io() {
        let (supervisor, inner) = supervisor_with(vec![]);
        supervisor.start(&minimal_launch()).unwrap();
        supervisor.stop().unwrap();
        assert_eq!(inner.shutdown_count(), 1);
        assert!(supervisor.io().is_err(), "io after stop must fail closed");
        // Idempotent second stop.
        supervisor.stop().unwrap();
        assert_eq!(inner.shutdown_count(), 1);
        // A plain start after a graceful stop is refused; restart re-arms.
        assert!(matches!(
            supervisor.start(&minimal_launch()).unwrap_err(),
            HarnessError::NotRunning
        ));
        let restarted = supervisor.restart(&minimal_launch()).unwrap();
        assert_eq!(restarted.generation, 1);
        assert!(supervisor.io().is_ok());
    }

    #[test]
    fn supervisor_mark_exited_is_idempotent() {
        let (supervisor, _inner) = supervisor_with(vec![]);
        assert!(!supervisor.mark_exited().unwrap());
        supervisor.start(&minimal_launch()).unwrap();
        assert!(supervisor.mark_exited().unwrap());
        assert!(!supervisor.mark_exited().unwrap());
        assert!(supervisor.io().is_err());
    }

    #[test]
    fn supervisor_spawn_failure_propagates_as_transport_error() {
        let spawner: HarnessSpawner = Box::new(|_launch, _generation| {
            Err(CliRuntimeError::new(
                "cli_process_spawn",
                "failed to start claude",
            ))
        });
        let supervisor = ClaudeHarnessSupervisor::new(spawner);
        let error = supervisor.start(&minimal_launch()).unwrap_err();
        assert!(
            matches!(error, HarnessError::Transport { ref code, .. } if code == "cli_process_spawn")
        );
    }
}
