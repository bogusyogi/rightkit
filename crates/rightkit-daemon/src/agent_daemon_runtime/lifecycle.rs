//! The single daemon lifecycle authority used by every operator surface.
//!
//! This is the Rust port of prime-agent's `cli/daemon-command.ts` command
//! surface.  The donor has one client command dispatcher for start, attach,
//! detach, stop, restart, update, scheduling, and process listing.  the host app
//! keeps that same boundary: CLI, TUI, desktop, and extensions submit a
//! [`LifecycleCommand`] to one [`LifecycleAuthority`] instance rather than
//! implementing their own lifecycle decisions.
//!
//! The authority deliberately owns policy and serialization, while the
//! integration owner supplies the existing daemon operations through
//! [`LifecycleBackend`].  This keeps the already-shipped `launch`, `shutdown`,
//! ownership, scheduler, and HTTP session paths as the only effectors; this
//! module is not a second daemon or a second process supervisor.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};

/// The operator surface that issued a lifecycle request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleSurface {
    Cli,
    Tui,
    Desktop,
    Extension,
}

/// Foreground/background selection from the donor's `start` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    Foreground,
    Background,
}

/// One normalized lifecycle command.  Parsing is intentionally separate from
/// execution so all surfaces can use the same command vocabulary and then
/// submit the resulting value to the same authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleCommand {
    Start { mode: LaunchMode },
    Attach { client_id: String },
    Detach { client_id: Option<String> },
    Stop { force: bool },
    Restart,
    Update { artifact: String },
    Schedule(ScheduleCommand),
    ProcessList { reap: bool, force: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleCommand {
    List {
        include_inactive: bool,
    },
    Add {
        session_id: String,
        schedule: String,
        prompt: String,
    },
    Cancel {
        job_id: String,
    },
}

/// Parse the lifecycle portion of a daemon command.  The accepted spellings
/// mirror prime-agent's `start`, `attach`, `detach`, `shutdown`, `restart`,
/// `cron schedule`, and `ps` surface while using the host app's names for the
/// daemon process.
pub fn parse_lifecycle_command(args: &[&str]) -> Result<LifecycleCommand, String> {
    let Some(command) = args.first().copied() else {
        return Err("missing daemon lifecycle command".to_string());
    };

    match command {
        "start" => {
            let mut mode = LaunchMode::Background;
            for arg in &args[1..] {
                match *arg {
                    "--foreground" | "--no-detach" => mode = LaunchMode::Foreground,
                    "--background" | "-d" => mode = LaunchMode::Background,
                    other => return Err(format!("unknown start option: {other}")),
                }
            }
            Ok(LifecycleCommand::Start { mode })
        }
        "attach" => one_value(args, "attach <client-id>", |client_id| {
            LifecycleCommand::Attach { client_id }
        }),
        "detach" => {
            if args.len() > 2 {
                return Err("Usage: daemon detach [client-id]".to_string());
            }
            Ok(LifecycleCommand::Detach {
                client_id: args.get(1).map(|value| (*value).to_string()),
            })
        }
        "stop" | "shutdown" => {
            let mut force = false;
            for arg in &args[1..] {
                match *arg {
                    "--force" | "-f" => force = true,
                    other => return Err(format!("unknown stop option: {other}")),
                }
            }
            Ok(LifecycleCommand::Stop { force })
        }
        "restart" => {
            if args.len() != 1 {
                return Err("Usage: daemon restart".to_string());
            }
            Ok(LifecycleCommand::Restart)
        }
        "update" => one_value(args, "update <artifact>", |artifact| {
            LifecycleCommand::Update { artifact }
        }),
        "schedule" | "cron" => parse_schedule_command(&args[1..]),
        "ps" | "process-list" | "processes" => {
            let mut reap = false;
            let mut force = false;
            for arg in &args[1..] {
                match *arg {
                    "--reap" | "reap" => reap = true,
                    "--force" | "-f" => force = true,
                    other => return Err(format!("unknown ps option: {other}")),
                }
            }
            Ok(LifecycleCommand::ProcessList { reap, force })
        }
        other => Err(format!("unknown daemon lifecycle command: {other}")),
    }
}

fn one_value(
    args: &[&str],
    usage: &str,
    make: impl FnOnce(String) -> LifecycleCommand,
) -> Result<LifecycleCommand, String> {
    if args.len() != 2 || args[1].trim().is_empty() {
        return Err(format!("Usage: daemon {usage}"));
    }
    Ok(make(args[1].to_string()))
}

fn parse_schedule_command(args: &[&str]) -> Result<LifecycleCommand, String> {
    let subcommand = args.first().copied().unwrap_or("list");
    match subcommand {
        "list" => {
            let mut include_inactive = false;
            for arg in &args[1..] {
                match *arg {
                    "--all" | "-a" => include_inactive = true,
                    other => return Err(format!("unknown schedule list option: {other}")),
                }
            }
            Ok(LifecycleCommand::Schedule(ScheduleCommand::List {
                include_inactive,
            }))
        }
        "add" | "schedule" => {
            let separator = args.iter().position(|arg| *arg == "--").ok_or_else(|| {
                "Usage: daemon schedule add <session-id> <schedule> -- <prompt>".to_string()
            })?;
            if separator < 3 {
                return Err(
                    "Usage: daemon schedule add <session-id> <schedule> -- <prompt>".to_string(),
                );
            }
            let prompt = args[separator + 1..].join(" ").trim().to_string();
            if prompt.is_empty() {
                return Err(
                    "Usage: daemon schedule add <session-id> <schedule> -- <prompt>".to_string(),
                );
            }
            Ok(LifecycleCommand::Schedule(ScheduleCommand::Add {
                session_id: args[1].to_string(),
                schedule: args[2..separator].join(" ").trim().to_string(),
                prompt,
            }))
        }
        "cancel" | "delete" | "remove" => {
            if args.len() != 2 || args[1].trim().is_empty() {
                return Err("Usage: daemon schedule cancel <job-id>".to_string());
            }
            Ok(LifecycleCommand::Schedule(ScheduleCommand::Cancel {
                job_id: args[1].to_string(),
            }))
        }
        other => Err(format!("unknown schedule command: {other}")),
    }
}

/// Coarse lifecycle state owned by the authority.  These are durable-state
/// decisions, not UI projections; surfaces only receive snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Restarting,
    Updating,
    Failed,
}

/// A process row returned by the daemon's one process-list seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub role: String,
    pub state: String,
    pub owned: bool,
}

/// State visible to CLI/TUI/desktop/extension callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleSnapshot {
    pub state: LifecycleState,
    pub generation: u64,
    pub attached_clients: Vec<String>,
}

/// Result of an operation supplied by the existing daemon implementation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendOutcome {
    pub detail: String,
    pub generation: Option<u64>,
    pub processes: Vec<ProcessEntry>,
    /// The resolved `(base_url, token)` for the daemon this outcome reached,
    /// when the backend actually contacted or launched one (currently only
    /// `start`'s background branch). Lets a CLI/TUI caller route its
    /// "ensure the daemon is reachable" step through the shared authority
    /// and still receive the endpoint it needs to make follow-up daemon
    /// calls, without duplicating the resolution logic outside the backend.
    pub endpoint: Option<(String, String)>,
}

/// One backend trait is the only effect boundary.  The integration owner
/// implements it by delegating to the host app's existing launch, HTTP session,
/// shutdown, scheduler, and update/reload paths.
#[async_trait]
pub trait LifecycleBackend: Send + Sync + 'static {
    async fn start(&mut self, mode: LaunchMode) -> Result<BackendOutcome, String>;
    async fn attach(&mut self, client_id: &str) -> Result<BackendOutcome, String>;
    async fn detach(&mut self, client_id: Option<&str>) -> Result<BackendOutcome, String>;
    async fn stop(&mut self, force: bool) -> Result<BackendOutcome, String>;
    async fn restart(&mut self) -> Result<BackendOutcome, String>;
    async fn update(&mut self, artifact: &str) -> Result<BackendOutcome, String>;
    async fn schedule_list(&mut self, include_inactive: bool) -> Result<BackendOutcome, String>;
    async fn schedule_add(
        &mut self,
        session_id: &str,
        schedule: &str,
        prompt: &str,
    ) -> Result<BackendOutcome, String>;
    async fn schedule_cancel(&mut self, job_id: &str) -> Result<BackendOutcome, String>;
    async fn process_list(&mut self, reap: bool, force: bool) -> Result<BackendOutcome, String>;
}

/// Returned after every command.  `surface` is provenance only; it never
/// changes semantics, which is what keeps all four consumers on one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleReport {
    pub surface: LifecycleSurface,
    pub command: LifecycleCommand,
    pub snapshot: LifecycleSnapshot,
    pub detail: String,
    pub processes: Vec<ProcessEntry>,
    /// Propagated from [`BackendOutcome::endpoint`]. `None` for every command
    /// that does not resolve a daemon endpoint, and for any idempotent
    /// short-circuit report (see `LifecycleAuthority::idempotent_report`),
    /// which never calls the backend at all.
    pub endpoint: Option<(String, String)>,
}

/// The one serialized lifecycle authority.  Clones are handles to the same
/// backend and state, so a CLI/TUI/desktop/extension clone cannot accidentally
/// create a parallel supervisor.
pub struct LifecycleAuthority<B: LifecycleBackend> {
    backend: Arc<Mutex<B>>,
    state: Arc<RwLock<LifecycleSnapshot>>,
    operation: Arc<Mutex<()>>,
}

impl<B: LifecycleBackend> Clone for LifecycleAuthority<B> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            state: self.state.clone(),
            operation: self.operation.clone(),
        }
    }
}

impl<B: LifecycleBackend> LifecycleAuthority<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
            state: Arc::new(RwLock::new(LifecycleSnapshot {
                state: LifecycleState::Stopped,
                generation: 0,
                attached_clients: Vec::new(),
            })),
            operation: Arc::new(Mutex::new(())),
        }
    }

    pub async fn snapshot(&self) -> LifecycleSnapshot {
        self.state.read().await.clone()
    }

    /// All surfaces call this method.  The operation lock serializes state
    /// transitions and backend calls, while the snapshot remains the only
    /// lifecycle projection sent back to a consumer.
    pub async fn execute(
        &self,
        surface: LifecycleSurface,
        command: LifecycleCommand,
    ) -> Result<LifecycleReport, String> {
        let _operation = self.operation.lock().await;
        let before = self.snapshot().await;

        if let Some(report) = self.idempotent_report(surface, &command, &before) {
            return Ok(report);
        }
        validate_command(&before, &command)?;

        let mut backend = self.backend.lock().await;
        let outcome = match &command {
            LifecycleCommand::Start { mode } => {
                set_state(&self.state, LifecycleState::Starting).await;
                backend.start(*mode).await
            }
            LifecycleCommand::Attach { client_id } => backend.attach(client_id).await,
            LifecycleCommand::Detach { client_id } => backend.detach(client_id.as_deref()).await,
            LifecycleCommand::Stop { force } => {
                set_state(&self.state, LifecycleState::Stopping).await;
                backend.stop(*force).await
            }
            LifecycleCommand::Restart => {
                set_state(&self.state, LifecycleState::Restarting).await;
                backend.restart().await
            }
            LifecycleCommand::Update { artifact } => {
                set_state(&self.state, LifecycleState::Updating).await;
                backend.update(artifact).await
            }
            LifecycleCommand::Schedule(schedule) => match schedule {
                ScheduleCommand::List { include_inactive } => {
                    backend.schedule_list(*include_inactive).await
                }
                ScheduleCommand::Add {
                    session_id,
                    schedule,
                    prompt,
                } => backend.schedule_add(session_id, schedule, prompt).await,
                ScheduleCommand::Cancel { job_id } => backend.schedule_cancel(job_id).await,
            },
            LifecycleCommand::ProcessList { reap, force } => {
                backend.process_list(*reap, *force).await
            }
        };

        match outcome {
            Ok(outcome) => {
                let snapshot = self
                    .commit_success(&command, &before, outcome.generation)
                    .await;
                Ok(LifecycleReport {
                    surface,
                    command,
                    snapshot,
                    detail: outcome.detail,
                    processes: outcome.processes,
                    endpoint: outcome.endpoint,
                })
            }
            Err(error) => {
                if changes_process_state(&command) {
                    set_state(&self.state, LifecycleState::Failed).await;
                }
                Err(error)
            }
        }
    }

    pub async fn execute_cli(&self, command: LifecycleCommand) -> Result<LifecycleReport, String> {
        self.execute(LifecycleSurface::Cli, command).await
    }

    pub async fn execute_tui(&self, command: LifecycleCommand) -> Result<LifecycleReport, String> {
        self.execute(LifecycleSurface::Tui, command).await
    }

    pub async fn execute_desktop(
        &self,
        command: LifecycleCommand,
    ) -> Result<LifecycleReport, String> {
        self.execute(LifecycleSurface::Desktop, command).await
    }

    pub async fn execute_extension(
        &self,
        command: LifecycleCommand,
    ) -> Result<LifecycleReport, String> {
        self.execute(LifecycleSurface::Extension, command).await
    }

    fn idempotent_report(
        &self,
        surface: LifecycleSurface,
        command: &LifecycleCommand,
        snapshot: &LifecycleSnapshot,
    ) -> Option<LifecycleReport> {
        let detail = match command {
            LifecycleCommand::Start { .. } if snapshot.state == LifecycleState::Running => {
                Some("daemon already running".to_string())
            }
            // `Stop` is deliberately never short-circuited from the in-process
            // snapshot: a fresh CLI process starts with `Stopped` while a
            // daemon owned by another process may be alive. The backend
            // observes the real discovery record and is itself idempotent.
            _ => None,
        }?;
        Some(LifecycleReport {
            surface,
            command: command.clone(),
            snapshot: snapshot.clone(),
            detail,
            processes: Vec::new(),
            // No backend call was made on this short-circuit path, so no
            // endpoint was resolved. A caller that needs the endpoint on the
            // idempotent "already running" branch must fall back to its own
            // resolution (see `resolve_daemon_endpoint_via_authority` in
            // `lifecycle_backend.rs`, which does exactly that).
            endpoint: None,
        })
    }

    async fn commit_success(
        &self,
        command: &LifecycleCommand,
        before: &LifecycleSnapshot,
        backend_generation: Option<u64>,
    ) -> LifecycleSnapshot {
        let mut next = before.clone();
        match command {
            LifecycleCommand::Start { .. }
            | LifecycleCommand::Restart
            | LifecycleCommand::Update { .. } => {
                next.state = LifecycleState::Running;
                next.generation =
                    backend_generation.unwrap_or_else(|| before.generation.saturating_add(1));
            }
            LifecycleCommand::Stop { .. } => next.state = LifecycleState::Stopped,
            LifecycleCommand::Attach { client_id } => {
                next.state = LifecycleState::Running;
                if !next.attached_clients.iter().any(|id| id == client_id) {
                    next.attached_clients.push(client_id.clone());
                }
            }
            LifecycleCommand::Detach { client_id } => {
                if let Some(client_id) = client_id {
                    next.attached_clients.retain(|id| id != client_id);
                } else {
                    next.attached_clients.clear();
                }
            }
            LifecycleCommand::Schedule(_) | LifecycleCommand::ProcessList { .. } => {}
        }
        *self.state.write().await = next.clone();
        next
    }
}

fn changes_process_state(command: &LifecycleCommand) -> bool {
    matches!(
        command,
        LifecycleCommand::Start { .. }
            | LifecycleCommand::Stop { .. }
            | LifecycleCommand::Restart
            | LifecycleCommand::Update { .. }
    )
}

fn validate_command(
    snapshot: &LifecycleSnapshot,
    command: &LifecycleCommand,
) -> Result<(), String> {
    match command {
        LifecycleCommand::Start { .. }
        | LifecycleCommand::Stop { .. }
        | LifecycleCommand::Restart
        | LifecycleCommand::Update { .. }
        | LifecycleCommand::ProcessList { .. } => Ok(()),
        LifecycleCommand::Attach { client_id } if client_id.trim().is_empty() => {
            Err("cannot attach an empty client id".to_string())
        }
        LifecycleCommand::Attach { .. } if snapshot.state != LifecycleState::Running => {
            Err("cannot attach while daemon is not running".to_string())
        }
        LifecycleCommand::Attach { .. } => Ok(()),
        LifecycleCommand::Detach { .. } => Ok(()),
        LifecycleCommand::Schedule(_) if snapshot.state != LifecycleState::Running => {
            Err("cannot schedule while daemon is not running".to_string())
        }
        LifecycleCommand::Schedule(ScheduleCommand::Add {
            session_id,
            schedule,
            prompt,
        }) if session_id.trim().is_empty()
            || schedule.trim().is_empty()
            || prompt.trim().is_empty() =>
        {
            Err("session id, schedule, and prompt must be non-empty".to_string())
        }
        LifecycleCommand::Schedule(ScheduleCommand::Cancel { job_id })
            if job_id.trim().is_empty() =>
        {
            Err("job id must be non-empty".to_string())
        }
        LifecycleCommand::Schedule(_) => Ok(()),
    }
}

async fn set_state(state: &Arc<RwLock<LifecycleSnapshot>>, value: LifecycleState) {
    state.write().await.state = value;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct MockBackend {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LifecycleBackend for MockBackend {
        async fn start(&mut self, _mode: LaunchMode) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome {
                detail: "started".into(),
                generation: Some(7),
                ..Default::default()
            })
        }
        async fn attach(&mut self, _client_id: &str) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome::default())
        }
        async fn detach(&mut self, _client_id: Option<&str>) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome::default())
        }
        async fn stop(&mut self, _force: bool) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome {
                detail: "stopped".into(),
                ..Default::default()
            })
        }
        async fn restart(&mut self) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome::default())
        }
        async fn update(&mut self, _artifact: &str) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome::default())
        }
        async fn schedule_list(
            &mut self,
            _include_inactive: bool,
        ) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome::default())
        }
        async fn schedule_add(
            &mut self,
            _session_id: &str,
            _schedule: &str,
            _prompt: &str,
        ) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome::default())
        }
        async fn schedule_cancel(&mut self, _job_id: &str) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome::default())
        }
        async fn process_list(
            &mut self,
            _reap: bool,
            _force: bool,
        ) -> Result<BackendOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(BackendOutcome::default())
        }
    }

    #[test]
    fn parser_keeps_donor_lifecycle_vocabulary() {
        assert_eq!(
            parse_lifecycle_command(&["start", "--foreground"]),
            Ok(LifecycleCommand::Start {
                mode: LaunchMode::Foreground
            })
        );
        assert_eq!(
            parse_lifecycle_command(&["shutdown", "--force"]),
            Ok(LifecycleCommand::Stop { force: true })
        );
        assert_eq!(
            parse_lifecycle_command(&["cron", "add", "job-1", "every 5m", "--", "hello"]),
            Ok(LifecycleCommand::Schedule(ScheduleCommand::Add {
                session_id: "job-1".into(),
                schedule: "every 5m".into(),
                prompt: "hello".into(),
            }))
        );
        assert_eq!(
            parse_lifecycle_command(&["ps"]),
            Ok(LifecycleCommand::ProcessList {
                reap: false,
                force: false
            })
        );
    }

    #[tokio::test]
    async fn all_surface_handles_share_one_authority_and_generation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let authority = LifecycleAuthority::new(MockBackend {
            calls: calls.clone(),
        });

        let started = authority
            .execute_cli(LifecycleCommand::Start {
                mode: LaunchMode::Background,
            })
            .await
            .unwrap();
        assert_eq!(started.snapshot.generation, 7);
        let attached = authority
            .execute_tui(LifecycleCommand::Attach {
                client_id: "tui".into(),
            })
            .await
            .unwrap();
        assert_eq!(attached.snapshot.attached_clients, vec!["tui"]);
        let desktop = authority
            .execute_desktop(LifecycleCommand::ProcessList {
                reap: false,
                force: false,
            })
            .await
            .unwrap();
        let extension = authority
            .execute_extension(LifecycleCommand::Detach { client_id: None })
            .await
            .unwrap();
        assert_eq!(desktop.snapshot.generation, extension.snapshot.generation);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn attach_before_start_is_rejected_without_backend_effect() {
        let calls = Arc::new(AtomicUsize::new(0));
        let authority = LifecycleAuthority::new(MockBackend {
            calls: calls.clone(),
        });
        let error = authority
            .execute_cli(LifecycleCommand::Attach {
                client_id: "cli".into(),
            })
            .await
            .unwrap_err();
        assert!(error.contains("not running"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// FM-129 / AO-02(f): UI/TUI projections must never write daemon
    /// lifecycle state. `LifecycleSnapshot` is structurally a read-only
    /// projection — it has no method that writes back into the authority,
    /// and the authority's real state lives behind a private
    /// `Arc<RwLock<LifecycleSnapshot>>` that only `commit_success` (reached
    /// exclusively through `execute()`) ever mutates. Prove this at runtime:
    /// mutating a local clone obtained from `snapshot()` — exactly what a
    /// UI/TUI surface holds — has zero effect on the authority's own state.
    #[tokio::test]
    async fn fm_129_ui_snapshot_projection_cannot_write_back_lifecycle_state() {
        let calls = Arc::new(AtomicUsize::new(0));
        let authority = LifecycleAuthority::new(MockBackend {
            calls: calls.clone(),
        });

        let mut projection = authority.snapshot().await;
        assert_eq!(projection.state, LifecycleState::Stopped);
        assert_eq!(projection.generation, 0);

        // A UI/TUI surface is free to mutate its own local projection copy...
        projection.state = LifecycleState::Running;
        projection.generation = 999;
        projection.attached_clients.push("phantom-client".into());

        // ...but that local mutation can never reach the authority: there is
        // no setter on `LifecycleSnapshot`, and no public method on
        // `LifecycleAuthority` besides `execute`/`execute_cli`/`execute_tui`/
        // `execute_desktop`/`execute_extension` touches its internal state.
        let after = authority.snapshot().await;
        assert_eq!(after.state, LifecycleState::Stopped);
        assert_eq!(after.generation, 0);
        assert!(after.attached_clients.is_empty());
        // No backend effect was triggered by the local projection mutation.
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn repeated_start_is_idempotent_on_the_shared_state() {
        let calls = Arc::new(AtomicUsize::new(0));
        let authority = LifecycleAuthority::new(MockBackend {
            calls: calls.clone(),
        });
        authority
            .execute_cli(LifecycleCommand::Start {
                mode: LaunchMode::Background,
            })
            .await
            .unwrap();
        let second = authority
            .execute_tui(LifecycleCommand::Start {
                mode: LaunchMode::Foreground,
            })
            .await
            .unwrap();
        assert_eq!(second.detail, "daemon already running");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
