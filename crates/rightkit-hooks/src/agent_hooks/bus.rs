//! The hook bus — publishes lifecycle events to subscribers.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Host-owned process boundary for hook execution. Embedders can inject a
/// governed launcher; the default remains local for compatibility.
#[async_trait::async_trait]
pub trait ProcessLaunchAdapter: Send + Sync {
    async fn run(
        &self,
        command: String,
        envs: Vec<(&'static str, String)>,
        context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Result<std::process::Output, String>;
}

struct LocalProcessLaunchAdapter;

/// Bound on retained hook output; the streams are still drained to EOF so a
/// blocked writer never stalls the hook child.
const HOOK_STREAM_CAPTURE_LIMIT: u64 = 256 * 1024;

/// Stops the hook's process tree when the awaiting future is dropped (the caller's timeout or a
/// cancelled turn): the blocking runner polls the flag and terminates the tree.
struct CancelOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Runs the hook through RightKit's bounded runner on a blocking thread: owned process tree, hidden
/// console on Windows, null stdin, first `HOOK_STREAM_CAPTURE_LIMIT` bytes of each stream kept and
/// the rest drained, exit status (code and signal) typed.
fn run_hook_blocking(
    cmd: &str,
    envs: &[(&'static str, String)],
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<std::process::Output, String> {
    #[cfg(windows)]
    let (sh, flag) = ("cmd", "/C");
    #[cfg(not(windows))]
    let (sh, flag) = ("sh", "-c");
    let mut owned = rightkit_child_process::OwnedCommand::new(sh);
    owned
        .command_mut()
        .arg(flag)
        .arg(cmd)
        .envs(envs.iter().map(|(k, v)| (*k, v.as_str())));
    // Providers R46: no credential, daemon token or licence locator reaches a hook process.
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(rightkit_llm::agent_providers::host_credentials::is_credential_env_name)
        {
            owned.command_mut().env_remove(name);
        }
    }
    let options = rightkit_child_process::StreamOptions::new()
        .stdout_tail_cap(0)
        .stderr_tail_cap(0);
    let limit = HOOK_STREAM_CAPTURE_LIMIT as usize;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let output = rightkit_child_process::run_streaming(
        owned,
        &options,
        &|| cancel.load(std::sync::atomic::Ordering::SeqCst),
        &mut |chunk: &[u8]| {
            let room = limit.saturating_sub(stdout.len());
            stdout.extend_from_slice(&chunk[..room.min(chunk.len())]);
        },
        &mut |line: &str| {
            let room = limit.saturating_sub(stderr.len());
            if room > 0 {
                let bytes = line.as_bytes();
                stderr.extend_from_slice(&bytes[..room.min(bytes.len())]);
                if stderr.len() < limit {
                    stderr.push(10u8);
                }
            }
        },
    )
    .map_err(|error| error.to_string())?;
    if output.cancelled {
        return Err("hook cancelled".to_string());
    }
    Ok(std::process::Output {
        status: output.status,
        stdout,
        stderr,
    })
}

#[async_trait::async_trait]
impl ProcessLaunchAdapter for LocalProcessLaunchAdapter {
    async fn run(
        &self,
        cmd: String,
        envs: Vec<(&'static str, String)>,
        _context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Result<std::process::Output, String> {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _stop_on_drop = CancelOnDrop(cancel.clone());
        tokio::task::spawn_blocking(move || run_hook_blocking(&cmd, &envs, &cancel))
            .await
            .map_err(|error| error.to_string())?
    }
}

/// Outcome of a blocking `PreToolUse` hook execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Exit 0 or no hook configured — proceed with the tool call.
    Proceed,
    /// Exit 2 — the hook explicitly denied the call. The string is the hook's stdout.
    Deny(String),
    /// Exit 1 — non-blocking error; logged but execution proceeds.
    Error(String),
}

/// Outcome of the blocking `PreToolUse` hook, including its only permitted
/// mutation: replacement of JSON arguments for the same tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreToolUseOutcome {
    Proceed,
    Transform(serde_json::Value),
    Deny(String),
    /// A hook asked for operator approval. `updated_input` carries any
    /// composed mutation so approval binds the post-mutation payload.
    Ask {
        reason: String,
        updated_input: Option<serde_json::Value>,
    },
    Error(String),
}

/// Full result of the `PreToolUse` chain: the outcome plus hook-provided
/// context to feed back to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct PreToolUseReport {
    pub outcome: PreToolUseOutcome,
    pub additional_context: Vec<String>,
}

/// A lifecycle event published on the hook bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HookEvent {
    SessionStart {
        session_id: String,
    },
    SessionEnd {
        session_id: String,
    },
    PreToolUse {
        session_id: String,
        tool: String,
    },
    PostToolUse {
        session_id: String,
        tool: String,
        is_error: bool,
    },
    UserPromptSubmit {
        session_id: String,
    },
    Compaction {
        session_id: String,
    },
    /// Fires before context compaction starts.
    PreCompact {
        session_id: String,
    },
    Stop {
        session_id: String,
    },
    /// Fires when a sub-agent session stops (mirrors Claude Code's SubagentStop hook).
    SubagentStop {
        session_id: String,
        role: String,
        success: bool,
    },
    /// Fires when the model produces a completed assistant message (notification to the operator).
    Notification {
        session_id: String,
        #[serde(default)]
        message: String,
    },
    /// Fires before a commit is executed. Carries the staged diff so that
    /// hook commands (typically the auditor) can inspect it and deny the
    /// commit by exiting with code 2.
    PreCommit {
        session_id: String,
        /// The staged diff (`git diff --cached`) at the time the hook fires.
        #[serde(default)]
        diff: String,
    },
}

impl HookEvent {
    /// The session this event belongs to.
    pub fn session_id(&self) -> &str {
        match self {
            HookEvent::SessionStart { session_id }
            | HookEvent::SessionEnd { session_id }
            | HookEvent::PreToolUse { session_id, .. }
            | HookEvent::PostToolUse { session_id, .. }
            | HookEvent::UserPromptSubmit { session_id }
            | HookEvent::Compaction { session_id }
            | HookEvent::PreCompact { session_id }
            | HookEvent::Stop { session_id }
            | HookEvent::SubagentStop { session_id, .. }
            | HookEvent::Notification { session_id, .. }
            | HookEvent::PreCommit { session_id, .. } => session_id,
        }
    }

    /// Short event name used for filtering/logging.
    pub fn name(&self) -> &'static str {
        match self {
            HookEvent::SessionStart { .. } => "SessionStart",
            HookEvent::SessionEnd { .. } => "SessionEnd",
            HookEvent::PreToolUse { .. } => "PreToolUse",
            HookEvent::PostToolUse { .. } => "PostToolUse",
            HookEvent::UserPromptSubmit { .. } => "UserPromptSubmit",
            HookEvent::Compaction { .. } => "Compaction",
            HookEvent::PreCompact { .. } => "PreCompact",
            HookEvent::Stop { .. } => "Stop",
            HookEvent::SubagentStop { .. } => "SubagentStop",
            HookEvent::Notification { .. } => "Notification",
            HookEvent::PreCommit { .. } => "PreCommit",
        }
    }
}

/// A subscription handle — receive events as they are published.
pub struct HookSubscription {
    rx: broadcast::Receiver<HookEvent>,
}

impl HookSubscription {
    /// Await the next event. Returns `None` if the bus is closed or events were lagged out.
    pub async fn recv(&mut self) -> Option<HookEvent> {
        loop {
            match self.rx.recv().await {
                Ok(ev) => return Some(ev),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// The hook bus. Clone-able; all clones share the same broadcast channel.
#[derive(Clone)]
pub struct HookBus {
    tx: broadcast::Sender<HookEvent>,
    launcher: std::sync::Arc<dyn ProcessLaunchAdapter>,
    launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
    /// Hooks from the `[hooks]` config key and the project hook file; shared by every clone.
    configured: std::sync::Arc<std::sync::RwLock<crate::agent_hooks::chain::ConfiguredHooks>>,
}

impl HookBus {
    /// Create a new hook bus with the given buffer capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self {
            tx,
            launcher: std::sync::Arc::new(LocalProcessLaunchAdapter),
            launch_context: rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: std::env::current_dir()
                    .unwrap_or_default()
                    .display()
                    .to_string(),
                sandbox_profile: None,
                sandbox_required: false,
                network_required: false,
            },
            configured: Default::default(),
        }
    }

    pub fn with_process_launcher(
        capacity: usize,
        launcher: std::sync::Arc<dyn ProcessLaunchAdapter>,
        launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self {
            tx,
            launcher,
            launch_context,
            configured: Default::default(),
        }
    }

    /// Install the hooks of the `[hooks]` config key and the project hook file (rule 58). The
    /// host passes the project file only for a trusted workspace
    /// ([`crate::agent_hooks::chain::load_project_hook_file`]). Every clone of this bus sees the change.
    pub fn set_configured_hooks(&self, hooks: crate::agent_hooks::chain::ConfiguredHooks) {
        *self
            .configured
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = hooks;
    }

    pub(crate) fn configured_specs_labelled(
        &self,
        event: &str,
    ) -> Result<Vec<(&'static str, crate::agent_hooks::chain::HookSpec)>, String> {
        self.configured
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .specs_labelled(event)
    }

    /// Every hook for `event`, in execution order: the `[hooks]` key, the project file, then the
    /// `RIGHTKIT_HOOKS_JSON` entries. An invalid surface is an error (fail closed for
    /// `PreToolUse`).
    pub fn specs_for(
        &self,
        event: &str,
    ) -> Result<Vec<crate::agent_hooks::chain::HookSpec>, String> {
        let mut specs = self
            .configured
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .specs(event)?;
        specs.extend(crate::agent_hooks::chain::env_hook_specs(event)?);
        Ok(specs)
    }

    /// Subscribe to future events.
    pub fn subscribe(&self) -> HookSubscription {
        HookSubscription {
            rx: self.tx.subscribe(),
        }
    }

    /// Publish an event. Returns the number of subscribers that received it.
    pub fn publish(&self, event: HookEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Current number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Run the ordered `PreToolUse` hook chain.
    ///
    /// Hooks come from the `[hooks]` config key, the project hook file and the
    /// `RIGHTKIT_HOOKS_JSON` entries, in that order ([`specs_for`](Self::specs_for)).
    /// With none configured this returns `Proceed`.
    ///
    /// Semantics (see `chain`): exit 0 = ok (optional typed JSON), exit 2 =
    /// deny with stderr as reason, any deny wins, mutations compose in order,
    /// and timeouts, signal kills and invalid configuration fail closed.
    pub async fn run_pre_tool_use(
        &self,
        session_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> PreToolUseOutcome {
        self.run_pre_tool_use_report(session_id, tool_name, args)
            .await
            .outcome
    }

    /// Like [`run_pre_tool_use`](Self::run_pre_tool_use) but also returns the
    /// hooks' `additionalContext` for the model.
    pub async fn run_pre_tool_use_report(
        &self,
        session_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> PreToolUseReport {
        use crate::agent_hooks::chain::{run_chain, HookDecision};
        let specs = match self.specs_for("PreToolUse") {
            Ok(specs) => specs,
            Err(error) => {
                return PreToolUseReport {
                    outcome: PreToolUseOutcome::Deny(format!(
                        "PreToolUse hook configuration invalid (fail closed): {error}"
                    )),
                    additional_context: Vec::new(),
                };
            }
        };
        if specs.is_empty() {
            return PreToolUseReport {
                outcome: PreToolUseOutcome::Proceed,
                additional_context: Vec::new(),
            };
        }
        let report = run_chain(
            &self.launcher,
            &self.launch_context,
            &specs,
            "PreToolUse",
            session_id,
            Some(tool_name),
            Some(args),
            &[],
        )
        .await;
        let outcome = match report.decision {
            Some(HookDecision::Deny) => PreToolUseOutcome::Deny(
                report
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Tool denied by PreToolUse hook".into()),
            ),
            Some(HookDecision::Ask) => PreToolUseOutcome::Ask {
                reason: report
                    .reason
                    .clone()
                    .unwrap_or_else(|| "PreToolUse hook requires approval".into()),
                updated_input: report.updated_input.clone(),
            },
            _ => match report.updated_input.clone() {
                Some(updated) => PreToolUseOutcome::Transform(updated),
                None if !report.errors.is_empty() => {
                    PreToolUseOutcome::Error(report.errors.join("; "))
                }
                None => PreToolUseOutcome::Proceed,
            },
        };
        PreToolUseReport {
            outcome,
            additional_context: report.additional_context,
        }
    }

    /// Run the configured chain for any event in the taxonomy
    /// (`chain::HOOK_EVENT_NAMES`).
    /// `target` is the matcher subject (tool name) when the event has one.
    pub async fn run_event_hooks(
        &self,
        event: &str,
        session_id: &str,
        target: Option<&str>,
        input: Option<&serde_json::Value>,
        extra_envs: &[(&'static str, String)],
    ) -> crate::agent_hooks::chain::ChainReport {
        let specs = match self.specs_for(event) {
            Ok(specs) => specs,
            Err(error) => {
                let mut report = crate::agent_hooks::chain::ChainReport::default();
                if crate::agent_hooks::chain::fails_closed(event) {
                    report.decision = Some(crate::agent_hooks::chain::HookDecision::Deny);
                    report.reason = Some(format!(
                        "{event} hook configuration invalid (fail closed): {error}"
                    ));
                } else {
                    report.errors.push(error);
                }
                return report;
            }
        };
        crate::agent_hooks::chain::run_chain(
            &self.launcher,
            &self.launch_context,
            &specs,
            event,
            session_id,
            target,
            input,
            extra_envs,
        )
        .await
    }

    /// True when any `PreToolUse` hook is configured (or the configuration is
    /// invalid, which fails closed). Callers use this to keep hook-governed
    /// calls on the serial approval path.
    pub fn has_pre_tool_use_hooks(&self) -> bool {
        match self.specs_for("PreToolUse") {
            Ok(specs) => !specs.is_empty(),
            Err(_) => true,
        }
    }

    /// Run the blocking `Stop` hooks (the `Stop` entries of the configured chain). A deny (exit
    /// 2, `decision` `block`/`deny`, or `continue:false`) keeps the agent going with the
    /// reason.
    ///
    /// This intentionally does not change `publish(HookEvent::Stop)`, because the
    /// daemon hook runner's command/HTTP/MCP subscribers are fire-and-forget.
    pub async fn run_blocking_stop(
        &self,
        session_id: &str,
        reason: &str,
        consecutive_blocks: usize,
    ) -> HookOutcome {
        self.run_blocking_stop_event("Stop", session_id, reason, consecutive_blocks)
            .await
    }

    /// Like [`run_blocking_stop`](Self::run_blocking_stop) for an explicit
    /// event: `"Stop"` or `"SubagentStop"`.
    pub async fn run_blocking_stop_event(
        &self,
        event: &str,
        session_id: &str,
        reason: &str,
        consecutive_blocks: usize,
    ) -> HookOutcome {
        let extras = vec![
            ("RIGHTKIT_HOOK_REASON", reason.to_string()),
            (
                "RIGHTKIT_HOOK_CONSECUTIVE_BLOCKS",
                consecutive_blocks.to_string(),
            ),
        ];
        self.run_blocking_event(
            event,
            session_id,
            extras,
            "Completion denied by blocking Stop hook",
            "Blocking Stop hook exited with error (exit 1)",
            // Rule 54: a blocking Stop hook that cannot run still proceeds, with an `Error`.
            false,
        )
        .await
    }

    /// Run the blocking PreCommit hooks (WS-3).
    ///
    /// Runs the `PreCommit` entries of the configured chain. With none configured
    /// this is a no-op returning `HookOutcome::Proceed` — the gate is
    /// **off by default**.
    ///
    /// The staged diff is passed to the hook via the `RIGHTKIT_HOOK_DIFF`
    /// environment variable so the command (typically an auditor wrapper) can
    /// inspect it and decide whether to allow the commit.
    ///
    /// Exit semantics (same as `run_pre_tool_use`):
    ///   0  → `Proceed` — commit is allowed
    ///   1  → `Error(stdout)` — non-blocking error; logged, commit proceeds
    ///   2  → `Deny(stdout)` — commit is blocked; stdout is the reason
    ///   other → treated as `Error`
    ///   timeout or cannot start → `Deny` (rule 54: an auditor gate that cannot run blocks)
    pub async fn run_pre_commit(&self, session_id: &str, diff: &str) -> HookOutcome {
        self.run_blocking_event(
            "PreCommit",
            session_id,
            vec![("RIGHTKIT_HOOK_DIFF", diff.to_string())],
            "Commit denied by PreCommit auditor gate",
            "PreCommit hook exited with error (exit 1)",
            // Rule 54: an auditor gate that cannot run (timeout, cannot start) blocks the commit.
            // Exit 1 is the hook's own verdict and stays a non-blocking error.
            true,
        )
        .await
    }

    /// Shared blocking-event driver: runs the configured chain for `event`. Any deny wins;
    /// otherwise chain errors are reported as `Error`.
    async fn run_blocking_event(
        &self,
        event: &str,
        session_id: &str,
        extras: Vec<(&'static str, String)>,
        default_deny: &'static str,
        default_error: &'static str,
        unrunnable_blocks: bool,
    ) -> HookOutcome {
        let _ = default_error;
        let report = self
            .run_event_hooks(event, session_id, None, None, &extras)
            .await;
        if report.is_denied() {
            return HookOutcome::Deny(report.reason.unwrap_or_else(|| default_deny.to_string()));
        }
        if unrunnable_blocks && !report.unrunnable.is_empty() {
            return HookOutcome::Deny(format!(
                "{event} hook could not run (blocked): {}",
                report.unrunnable.join("; ")
            ));
        }
        if !report.errors.is_empty() {
            return HookOutcome::Error(report.errors.join("; "));
        }
        HookOutcome::Proceed
    }
}

impl Default for HookBus {
    fn default() -> Self {
        Self::new(256)
    }
}
