// RIGHTKIT-SERIOUS: rightkit-daemon 0.3.0 has no crash-safe reload/replacement handoff saga; kept to hand a live daemon to its replacement (report B-2)
//! Crash-safe daemon update/restart handoff.
//!
//! This ports the state and handoff portions of jcode's
//! `crates/jcode-app-core/src/server/{reload.rs,reload_state.rs}`.  A reload is
//! a single ordered saga: persist recovery intent, fence new mutations, drain
//! the old runtime, launch the replacement with the same endpoint, wait for a
//! ready marker, and only then resume.  Failures are recorded and sent through
//! the supplied rollback hook; callers never silently continue after an
//! incomplete handoff.
//!
//! The process and HTTP details remain RightKit-owned.  [`ReloadHooks`] is the
//! one integration seam for those existing daemon operations, and
//! [`spawn_replacement_for_request`] preserves the existing [`super::launch::DaemonLaunchSpec`]
//! rather than introducing another launch protocol.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::launch::DaemonLaunchSpec;
use super::migrate::{activate_staged_path, rollback_activation, ActivationFence};

const RELOAD_STATE_FILE: &str = "daemon.reload.json";
const RELOAD_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The stages visible to a client waiting for daemon replacement.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReloadPhase {
    Starting,
    Draining,
    Handoff,
    SocketReady,
    Failed,
}

/// Durable marker for a single reload request.  The request id and hash bind
/// the replacement to the update that initiated it; a stale marker cannot be
/// accepted as readiness for a different request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReloadState {
    pub request_id: String,
    pub hash: String,
    pub phase: ReloadPhase,
    pub pid: u32,
    pub timestamp: String,
    pub socket_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Private marker storage.  The path is injectable so tests and auxiliary
/// daemons never touch the primary user's runtime directory.
#[derive(Clone, Debug)]
pub struct ReloadStore {
    path: PathBuf,
}

impl ReloadStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn default_for_user() -> Option<Self> {
        let root = rightkit_agent_config::state_root()?;
        Some(Self::new(root.join(RELOAD_STATE_FILE)))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Option<ReloadState> {
        serde_json::from_slice(&fs::read(&self.path).ok()?).ok()
    }

    pub fn save(&self, state: &ReloadState) -> Result<(), String> {
        self.path
            .parent()
            .ok_or_else(|| "reload marker has no parent directory".to_string())?;
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("encode reload marker: {error}"))?;
        rightkit_fs::atomic_write_with(
            &self.path,
            rightkit_fs::AtomicOptions {
                create_parents: true,
                ..rightkit_fs::AtomicOptions::default()
            },
            |f| std::io::Write::write_all(f, &bytes),
        )
        .map_err(|error| format!("publish reload marker {}: {error}", self.path.display()))
    }

    pub fn clear(&self) {
        let _ = fs::remove_file(&self.path);
    }

    pub fn recent(&self, max_age: Duration) -> Option<ReloadState> {
        let state = self.load()?;
        let modified = fs::metadata(&self.path).ok()?.modified().ok()?;
        if SystemTime::now().duration_since(modified).ok()? <= max_age {
            Some(state)
        } else {
            self.clear();
            None
        }
    }

    pub fn write_starting(&self, request: &ReloadRequest) -> Result<(), String> {
        self.save(&ReloadState::for_request(
            request,
            ReloadPhase::Starting,
            None,
        ))
    }

    pub fn write_phase(
        &self,
        request: &ReloadRequest,
        phase: ReloadPhase,
        detail: Option<String>,
    ) -> Result<(), String> {
        self.save(&ReloadState::for_request(request, phase, detail))
    }

    /// Called by the replacement daemon after it has rebound the endpoint.
    /// It accepts only the request currently in Starting/Draining/Handoff, so
    /// an old or unrelated process cannot publish readiness over a failed run.
    pub fn publish_socket_ready(&self, request_id: &str, hash: &str) -> Result<bool, String> {
        let Some(existing) = self.load() else {
            return Ok(false);
        };
        if existing.request_id != request_id
            || existing.hash != hash
            || !matches!(
                existing.phase,
                ReloadPhase::Starting | ReloadPhase::Draining | ReloadPhase::Handoff
            )
        {
            return Ok(false);
        }
        self.save(&ReloadState {
            request_id: existing.request_id,
            hash: existing.hash,
            phase: ReloadPhase::SocketReady,
            pid: std::process::id(),
            timestamp: now_timestamp(),
            socket_path: existing.socket_path,
            detail: existing.detail,
        })?;
        Ok(true)
    }
}

impl ReloadState {
    fn for_request(request: &ReloadRequest, phase: ReloadPhase, detail: Option<String>) -> Self {
        Self {
            request_id: request.request_id.clone(),
            hash: request.hash.clone(),
            phase,
            pid: std::process::id(),
            timestamp: now_timestamp(),
            socket_path: request.launch.socket_path.clone(),
            detail,
        }
    }
}

fn now_timestamp() -> String {
    // chrono is NOT a daemon dependency; match the binary's existing
    // `now_iso` convention (see api_attention.rs) rather than adding one.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("unix:{secs}")
}

/// A normalized reload request.  `launch` is RightKit's existing detached
/// launch contract, including the endpoint and exact daemon arguments.
#[derive(Clone, Debug)]
pub struct ReloadRequest {
    pub request_id: String,
    pub hash: String,
    pub launch: DaemonLaunchSpec,
    /// Optional staged install artifact.  When present, the reload saga owns
    /// its atomic publish boundary; process handoff remains DL-014-owned.
    pub activation: Option<ActivationRequest>,
}

#[derive(Clone, Debug)]
pub struct ActivationRequest {
    pub staged_path: PathBuf,
    pub target_path: PathBuf,
}

impl ReloadRequest {
    pub fn new(
        request_id: impl Into<String>,
        hash: impl Into<String>,
        launch: DaemonLaunchSpec,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            hash: hash.into(),
            launch,
            activation: None,
        }
    }

    pub fn with_activation(
        mut self,
        staged_path: impl Into<PathBuf>,
        target_path: impl Into<PathBuf>,
    ) -> Self {
        self.activation = Some(ActivationRequest {
            staged_path: staged_path.into(),
            target_path: target_path.into(),
        });
        self
    }
}

/// Readiness result used by command/UI consumers while waiting for a handoff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReloadWaitStatus {
    Ready,
    Waiting { pid: Option<u32> },
    Failed(Option<String>),
    Idle,
}

/// Inspect marker state plus the live endpoint probe.  The endpoint callback
/// is supplied by the existing RightKit HTTP/transport layer.
pub fn inspect_wait_status(
    store: &ReloadStore,
    max_age: Duration,
    last_known_pid: Option<u32>,
    endpoint_ready: bool,
    listener_live: bool,
) -> ReloadWaitStatus {
    if let Some(state) = store.recent(max_age) {
        return match state.phase {
            ReloadPhase::SocketReady => ReloadWaitStatus::Ready,
            ReloadPhase::Failed => ReloadWaitStatus::Failed(state.detail),
            ReloadPhase::Starting | ReloadPhase::Draining | ReloadPhase::Handoff => {
                if process_alive(state.pid) {
                    ReloadWaitStatus::Waiting {
                        pid: Some(state.pid),
                    }
                } else {
                    ReloadWaitStatus::Failed(Some(format!(
                        "reload process {} exited before becoming ready",
                        state.pid
                    )))
                }
            }
        };
    }
    if endpoint_ready || listener_live {
        return ReloadWaitStatus::Ready;
    }
    if let Some(pid) = last_known_pid {
        if process_alive(pid) {
            return ReloadWaitStatus::Waiting { pid: Some(pid) };
        }
    }
    ReloadWaitStatus::Idle
}

/// Poll the same marker/endpoint seam used by CLI, TUI, desktop, and
/// extension callers.  No caller invents a second reload wait loop.
pub async fn await_handoff(
    store: &ReloadStore,
    max_age: Duration,
    last_known_pid: Option<u32>,
    mut endpoint_ready: impl FnMut() -> bool,
    mut listener_live: impl FnMut() -> bool,
) -> ReloadWaitStatus {
    let mut pid = last_known_pid;
    loop {
        let status = inspect_wait_status(store, max_age, pid, endpoint_ready(), listener_live());
        match status {
            ReloadWaitStatus::Waiting { pid: observed } => {
                pid = observed;
                tokio::time::sleep(RELOAD_POLL_INTERVAL).await;
            }
            other => return other,
        }
    }
}

/// Prepare the replacement command with detached stdio.  Keeping the exact
/// `DaemonLaunchSpec` here makes update/restart use the normal launch path.
pub fn replacement_command(spec: &DaemonLaunchSpec) -> Command {
    replacement_command_for_request(spec, None)
}

/// [`replacement_command`] plus the reload identity, stamped as environment so
/// the child can publish readiness for exactly this request.
pub fn replacement_command_for_request(
    spec: &DaemonLaunchSpec,
    identity: Option<(&str, &str)>,
) -> Command {
    let mut command = rightkit_child_process::command_std(&spec.executable);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    if let Some((request_id, hash)) = identity {
        command.env(RELOAD_REQUEST_ID_ENV, request_id);
        command.env(RELOAD_HASH_ENV, hash);
    }
    command
}

/// Env vars the coordinator stamps on the replacement so it can publish its
/// own readiness. Without these the replacement has no way to know it was
/// launched by a reload, `publish_socket_ready` is never called, and
/// `wait_ready` times out and rolls back a perfectly healthy replacement.
pub const RELOAD_REQUEST_ID_ENV: &str = "RIGHTKIT_RELOAD_REQUEST_ID";
pub const RELOAD_HASH_ENV: &str = "RIGHTKIT_RELOAD_HASH";

/// Spawn the replacement daemon. `identity` carries the reload request this
/// replacement is fulfilling, so it can publish `SocketReady` for exactly that
/// request once it is listening; `None` is an ordinary launch.
pub fn spawn_replacement_for_request(
    spec: &DaemonLaunchSpec,
    identity: Option<(&str, &str)>,
) -> std::io::Result<Child> {
    // The replacement must outlive this daemon, so it breaks away from the
    // daemon's kill-on-close containment job; fall back to a plain spawn when
    // an outer job forbids breakaway.
    #[cfg(windows)]
    let child = {
        use std::os::windows::process::CommandExt as _;
        let mut command = replacement_command_for_request(spec, identity);
        command.creation_flags(
            rightkit_agent_tools::coding_tools::no_window::CREATE_NO_WINDOW
                | rightkit_child_process::CREATE_BREAKAWAY_FROM_JOB,
        );
        match command.spawn() {
            Ok(child) => child,
            Err(_) => replacement_command_for_request(spec, identity).spawn()?,
        }
    };
    #[cfg(not(windows))]
    let child = replacement_command_for_request(spec, identity).spawn()?;
    // DL-013: publish ownership immediately after spawn.  Startup recovery
    // re-checks exact PID/start identity before adopting or terminating it.
    if let Some(path) = super::recovery::default_journal_path() {
        super::recovery::record_worker_state(&path, child.id(), true);
    }
    Ok(child)
}

/// The only runtime-specific seam in the reload saga.  Implement this by
/// delegating to the existing daemon state, shutdown, launch, and session
/// recovery paths; do not implement another lifecycle supervisor here.
#[async_trait]
pub trait ReloadHooks: Send + Sync + 'static {
    async fn persist_recovery(&mut self, request: &ReloadRequest) -> Result<(), String>;
    async fn fence_intake(&mut self) -> Result<(), String>;
    async fn drain(&mut self) -> Result<(), String>;
    async fn launch_replacement(&mut self, request: &ReloadRequest) -> Result<(), String>;
    async fn wait_ready(&mut self, request: &ReloadRequest) -> Result<(), String>;
    async fn resume(&mut self, request: &ReloadRequest) -> Result<(), String>;
    async fn rollback(&mut self, request: &ReloadRequest) -> Result<(), String>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReloadError {
    #[error("reload failed during {stage}: {error}")]
    Stage { stage: &'static str, error: String },
    #[error("reload failed during {stage}: {error}; rollback also failed: {rollback}")]
    StageAndRollback {
        stage: &'static str,
        error: String,
        rollback: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadReport {
    pub request_id: String,
    pub hash: String,
    pub phase: ReloadPhase,
}

/// Serializes the update/restart saga and records each durable phase.  The
/// same coordinator handle is shared by all lifecycle surfaces.
pub struct ReloadCoordinator<H: ReloadHooks> {
    hooks: std::sync::Arc<Mutex<H>>,
    store: ReloadStore,
    operation: std::sync::Arc<Mutex<()>>,
}

impl<H: ReloadHooks> Clone for ReloadCoordinator<H> {
    fn clone(&self) -> Self {
        Self {
            hooks: self.hooks.clone(),
            store: self.store.clone(),
            operation: self.operation.clone(),
        }
    }
}

impl<H: ReloadHooks> ReloadCoordinator<H> {
    pub fn new(hooks: H, store: ReloadStore) -> Self {
        Self {
            hooks: std::sync::Arc::new(Mutex::new(hooks)),
            store,
            operation: std::sync::Arc::new(Mutex::new(())),
        }
    }

    pub fn store(&self) -> &ReloadStore {
        &self.store
    }

    pub async fn run(&self, request: ReloadRequest) -> Result<ReloadReport, ReloadError> {
        let _operation = self.operation.lock().await;
        let mut hooks = self.hooks.lock().await;
        let mut activation: Option<ActivationFence> = None;

        let result = async {
            stage("persist recovery", self.store.write_starting(&request))?;
            stage("persist recovery", hooks.persist_recovery(&request).await)?;
            stage("fence intake", hooks.fence_intake().await)?;
            stage(
                "mark draining",
                self.store
                    .write_phase(&request, ReloadPhase::Draining, None),
            )?;
            stage("drain", hooks.drain().await)?;
            stage(
                "mark handoff",
                self.store.write_phase(&request, ReloadPhase::Handoff, None),
            )?;
            if let Some(plan) = &request.activation {
                activation = Some(
                    activate_staged_path(&plan.staged_path, &plan.target_path, &request.request_id)
                        .map_err(|error| ReloadError::Stage {
                            stage: "activate replacement",
                            error,
                        })?,
                );
            }
            stage(
                "launch replacement",
                hooks.launch_replacement(&request).await,
            )?;
            stage("wait for readiness", hooks.wait_ready(&request).await)?;
            stage("resume", hooks.resume(&request).await)?;
            Ok::<ReloadReport, ReloadError>(ReloadReport {
                request_id: request.request_id.clone(),
                hash: request.hash.clone(),
                phase: ReloadPhase::SocketReady,
            })
        }
        .await;

        match result {
            Ok(report) => Ok(report),
            Err(error) => {
                let (stage_name, message) = match &error {
                    ReloadError::Stage { stage, error } => (*stage, error.clone()),
                    ReloadError::StageAndRollback { stage, error, .. } => (*stage, error.clone()),
                };
                let marker_error = self.store.write_phase(
                    &request,
                    ReloadPhase::Failed,
                    Some(format!("{stage_name}: {message}")),
                );
                let activation_error = activation
                    .as_ref()
                    .and_then(|fence| rollback_activation(fence).err());
                let rollback = hooks.rollback(&request).await;
                let rollback = match (rollback, activation_error) {
                    (Err(error), _) => Err(error),
                    (Ok(()), Some(error)) => Err(error),
                    (Ok(()), None) => Ok(()),
                };
                match (rollback, marker_error) {
                    (Err(rollback), _) => Err(ReloadError::StageAndRollback {
                        stage: stage_name,
                        error: message,
                        rollback,
                    }),
                    (_, Err(marker)) => Err(ReloadError::StageAndRollback {
                        stage: stage_name,
                        error: message,
                        rollback: format!("could not persist failed marker: {marker}"),
                    }),
                    (Ok(()), Ok(())) => Err(error),
                }
            }
        }
    }
}

fn stage(stage: &'static str, result: Result<(), String>) -> Result<(), ReloadError> {
    result.map_err(|error| ReloadError::Stage { stage, error })
}

/// Whether process `pid` exists, through RightKit's process identity read (a zombie
/// still exists; on Windows an exited process whose handle is held elsewhere does not).
/// A read that fails for lack of permission counts as alive, as `kill(pid, 0)` did.
pub fn process_alive(pid: u32) -> bool {
    let Some(pid) = std::num::NonZeroU32::new(pid) else {
        return false;
    };
    match rightkit_child_process::AdoptedIdentity::capture(pid) {
        Ok(identity) => identity.is_some(),
        Err(_) => true,
    }
}

/// ra-002: best-effort termination used by [`super::lifecycle_backend`]'s
/// reload rollback when a spawned replacement never becomes ready (never
/// publishes `SocketReady` before the deadline, or exits on its own). This
/// intentionally does not require a pinned `boot_epoch_ms` the way
/// `kill_tree::terminate_pid_pinned` does: the pid here was returned by this
/// process's own immediately-preceding replacement spawn, so there is
/// no discovery-file race window to guard against — the caller already knows
/// exactly which process it is trying to stop.
pub fn terminate_pid_best_effort(pid: u32) -> bool {
    use rightkit_child_process::{AdoptedIdentity, AdoptedSignal, SignalResult};

    let Some(pid) = std::num::NonZeroU32::new(pid) else {
        return false;
    };
    // The identity is captured immediately before the signal, so the pid cannot change
    // hands in between. `Graceful` is SIGTERM on Unix and a terminate on Windows.
    let Ok(Some(identity)) = AdoptedIdentity::capture(pid) else {
        return false;
    };
    matches!(
        identity.signal(AdoptedSignal::Graceful),
        Ok(SignalResult::Delivered)
    )
}

/// Called by a daemon during startup, once its listener is up: if this process
/// was launched as a reload replacement, publish `SocketReady` for that exact
/// request so the coordinator's `wait_ready` observes real readiness instead
/// of timing out. A no-op for every ordinary daemon start.
pub fn publish_socket_ready_if_replacement() -> bool {
    publish_socket_ready_for(reload_identity_from_env(), ReloadStore::default_for_user())
}

/// The reload identity this process was launched with, if any. Absent for an
/// ordinary daemon start.
fn reload_identity_from_env() -> Option<(String, String)> {
    match (
        std::env::var(RELOAD_REQUEST_ID_ENV),
        std::env::var(RELOAD_HASH_ENV),
    ) {
        (Ok(request_id), Ok(hash)) => Some((request_id, hash)),
        // A half-set identity is not a reload: publishing against a guessed
        // request id could satisfy someone else's wait.
        _ => None,
    }
}

/// Resolution half of [`publish_socket_ready_if_replacement`], separated from
/// the environment and filesystem lookups so it can be tested directly.
fn publish_socket_ready_for(
    identity: Option<(String, String)>,
    store: Option<ReloadStore>,
) -> bool {
    let (Some((request_id, hash)), Some(store)) = (identity, store) else {
        return false;
    };
    publish_socket_ready_into(&store, &request_id, &hash)
}

fn publish_socket_ready_into(store: &ReloadStore, request_id: &str, hash: &str) -> bool {
    match store.publish_socket_ready(request_id, hash) {
        Ok(published) => published,
        Err(error) => {
            tracing::warn!(%error, "reload replacement could not publish socket readiness");
            false
        }
    }
}
