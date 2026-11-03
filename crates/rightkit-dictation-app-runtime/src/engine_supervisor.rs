// Shell-side supervisor for the `rightkit-dictation-engine` sidecar binary — module hub.
//
// The resident native shell is the host app shell. It owns the
// tray, global hotkeys, pill/mini-clipboard/hub window lifecycle, and the
// engine supervision. The supervision itself is RightKit's
// `rightkit_job_queue::EngineSupervisor` (feature `supervisor`): this
// supervisor spawns the `rightkit-dictation-engine` binary after onboarding completes,
// speaks the RightKit stdio contract with it (`EngineFrame` as the request
// params / response result / event body), and bridges the request/response
// cycle into the existing Tauri runtime + IPC commands.
//
// Crash recovery: if the sidecar exits unexpectedly, RightKit's restart policy
// restarts it on the next `start_dictation` request (`RestartMode::OnDemand`); that is
// the release behaviour (a long dictation on a wedged sidecar would be a UI freeze).
// Hot restart (`RestartMode::Automatic`, bounded backoff) is test-mode only: its switch,
// `RIGHTKIT_SIDECAR_AUTORESTART=1`, is read through `dev_var!`, which release builds
// compile out. A spent restart budget is retried after `SIDECAR_ABANDON_COOLDOWN`.
//
// Module layout: `supervisor` owns `EngineSupervisor`/`SupervisorEvent` and
// the the app policy around the RightKit supervisor, `wire` owns the
// frame<->envelope mapping and the method allowlist, `protocol` owns
// frame->event translation, `env` owns the sidecar environment whitelist,
// `paths` owns sidecar/ffmpeg path resolution + engine-binary telemetry, and
// `final_wait`/`revocation` own the shell watchdog's reply bounds.

mod env;
mod final_wait;
mod paths;
mod protocol;
mod revocation;
mod routing;
mod supervisor;
mod wire;

// No `pub(crate) use mod::*` globs: nothing resolves submodule internals
// through this root — siblings use `super::module::name` paths and the test
// module imports `super::protocol::frame_to_engine_event` directly. Only the
// explicit `pub use` surface below is consumed (in-crate and by src-tauri via
// `pub use rightkit_dictation_app_runtime::engine_supervisor`).

#[cfg(feature = "test-mode")]
pub use env::pass_live_eval_env_for_test;
#[cfg(feature = "test-mode")]
pub use env::{l3_env_keys_for_test, strip_l3_env_for_test};
pub use final_wait::{
    final_transcript_timeout, FINAL_TRANSCRIPT_TIMEOUT_MAX, FINAL_TRANSCRIPT_TIMEOUT_MIN,
};
pub use paths::resolve_engine_bin;
#[cfg(feature = "test-mode")]
pub use paths::resolve_engine_override_for_test;
pub use protocol::build_request;
pub use revocation::{classify_revocation_reply, RevocationOutcome};
pub use supervisor::{
    advertises_live_l3, build_sidecar_command_for_test, l3_policy_value, EngineSupervisor,
    SupervisorEvent,
};

/// Whether the engine behind a dictation start holds none of the shell's pushed state (Pro flag,
/// vocabulary, AI-polish policy): this start spawned it (`start_result` is the engine version,
/// not "already-running"), or it was respawned after the pushes were prepared (the generation
/// moved). The shell then sends its config and vocabulary before it starts recording.
pub fn needs_snapshot_push(start_result: &str, generation_seen: u64, generation_now: u64) -> bool {
    start_result != "already-running" || generation_now != generation_seen
}

/// Runs a task on a named thread unless the previous run is still in flight. The shell uses it
/// to bring the engine back after a restart without stacking one rewarm per settings change.
pub struct SingleFlight(std::sync::atomic::AtomicBool);

impl SingleFlight {
    pub const fn new() -> Self {
        Self(std::sync::atomic::AtomicBool::new(false))
    }

    /// Start `task` on a thread named `name`. Returns false, running nothing, when a run is
    /// already in flight (or the thread could not start).
    pub fn spawn(&'static self, name: &str, task: impl FnOnce() + Send + 'static) -> bool {
        use std::sync::atomic::Ordering::{AcqRel, Release};
        if self.0.swap(true, AcqRel) {
            return false;
        }
        let spawned = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                task();
                self.0.store(false, Release);
            });
        if spawned.is_err() {
            self.0.store(false, Release);
            return false;
        }
        true
    }
}

impl Default for SingleFlight {
    fn default() -> Self {
        Self::new()
    }
}
