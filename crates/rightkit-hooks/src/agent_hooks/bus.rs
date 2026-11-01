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
        }
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
    /// Hooks come from `RIGHTKIT_HOOK_PRE_TOOL_USE` (legacy single command,
    /// runs first) followed by the `PreToolUse` entries of
    /// `RIGHTKIT_HOOKS_JSON`. With none configured this returns `Proceed`.
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
        use crate::agent_hooks::chain::{load_hook_specs, run_chain, HookDecision};
        let specs = match load_hook_specs("PreToolUse") {
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

    /// Run the configured `RIGHTKIT_HOOKS_JSON` chain for any event in the
    /// taxonomy (`UserPromptSubmit`, `Stop`, `SubagentStop`, `SessionStart`,
    /// `SessionEnd`, `PreCompact`, `Notification`, `PostToolUse`).
    /// `target` is the matcher subject (tool name) when the event has one.
    pub async fn run_event_hooks(
        &self,
        event: &str,
        session_id: &str,
        target: Option<&str>,
        input: Option<&serde_json::Value>,
        extra_envs: &[(&'static str, String)],
    ) -> crate::agent_hooks::chain::ChainReport {
        let specs = match crate::agent_hooks::chain::load_hook_specs(event) {
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
        match crate::agent_hooks::chain::load_hook_specs("PreToolUse") {
            Ok(specs) => !specs.is_empty(),
            Err(_) => true,
        }
    }

    /// Run the blocking `Stop` hooks: the legacy single command in
    /// `RIGHTKIT_HOOK_STOP_BLOCKING` first, then the `Stop` entries of
    /// `RIGHTKIT_HOOKS_JSON`. A deny from either (exit 2, `decision`
    /// `block`/`deny`, or `continue:false`) keeps the agent going with the
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
    /// event: `"Stop"` (legacy env var plus JSON) or `"SubagentStop"` (JSON
    /// only).
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
            if event == "Stop" {
                hook_var!("RIGHTKIT_HOOK_STOP_BLOCKING")
            } else {
                None
            },
            session_id,
            extras,
            "Completion denied by blocking Stop hook",
            "Blocking Stop hook exited with error (exit 1)",
        )
        .await
    }

    /// Run the blocking PreCommit hooks (WS-3).
    ///
    /// The legacy command in `RIGHTKIT_HOOK_PRE_COMMIT` runs first, then the
    /// `PreCommit` entries of `RIGHTKIT_HOOKS_JSON`. With neither configured
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
    pub async fn run_pre_commit(&self, session_id: &str, diff: &str) -> HookOutcome {
        self.run_blocking_event(
            "PreCommit",
            hook_var!("RIGHTKIT_HOOK_PRE_COMMIT"),
            session_id,
            vec![("RIGHTKIT_HOOK_DIFF", diff.to_string())],
            "Commit denied by PreCommit auditor gate",
            "PreCommit hook exited with error (exit 1)",
        )
        .await
    }

    /// Shared blocking-event driver: legacy env command (if any), then the
    /// JSON chain. Any deny wins; otherwise a legacy error, then chain
    /// errors, are reported as `Error`.
    async fn run_blocking_event(
        &self,
        event: &str,
        legacy_env: Option<&str>,
        session_id: &str,
        extras: Vec<(&'static str, String)>,
        default_deny: &'static str,
        default_error: &'static str,
    ) -> HookOutcome {
        let mut legacy = HookOutcome::Proceed;
        if let Some(cmd) = legacy_env.and_then(crate::agent_hooks::hook_env) {
            let mut envs = vec![
                ("RIGHTKIT_HOOK_EVENT", event.to_string()),
                ("RIGHTKIT_HOOK_SESSION", session_id.to_string()),
            ];
            envs.extend(extras.iter().cloned());
            legacy = run_blocking_command_with_launcher(
                self.launcher.clone(),
                self.launch_context.clone(),
                cmd,
                envs,
                default_deny,
                default_error,
            )
            .await;
            if matches!(legacy, HookOutcome::Deny(_)) {
                return legacy;
            }
        }
        let report = self
            .run_event_hooks(event, session_id, None, None, &extras)
            .await;
        if report.is_denied() {
            return HookOutcome::Deny(report.reason.unwrap_or_else(|| default_deny.to_string()));
        }
        if matches!(legacy, HookOutcome::Error(_)) {
            return legacy;
        }
        if !report.errors.is_empty() {
            return HookOutcome::Error(report.errors.join("; "));
        }
        HookOutcome::Proceed
    }
}

async fn run_blocking_command_with_launcher(
    launcher: std::sync::Arc<dyn ProcessLaunchAdapter>,
    _launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
    cmd: String,
    envs: Vec<(&'static str, String)>,
    default_deny: &'static str,
    default_error: &'static str,
) -> HookOutcome {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        launcher.run(cmd, envs, _launch_context),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            match output.status.code() {
                Some(0) => HookOutcome::Proceed,
                // `code() == None` means no exit evidence — the hook was
                // signal-killed. Report it honestly rather than as a clean
                // exit so a killed gate can never masquerade as Proceed.
                None => {
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    HookOutcome::Error(if stderr.is_empty() {
                        "hook terminated by signal".to_string()
                    } else {
                        format!("hook terminated by signal: {stderr}")
                    })
                }
                Some(2) => HookOutcome::Deny(if stdout.is_empty() {
                    default_deny.to_string()
                } else {
                    stdout
                }),
                Some(1) => HookOutcome::Error(if stdout.is_empty() {
                    default_error.to_string()
                } else {
                    stdout
                }),
                Some(code) => {
                    HookOutcome::Error(format!("hook exited with unexpected code {code}: {stdout}"))
                }
            }
        }
        Ok(Err(e)) => HookOutcome::Error(format!("hook failed to spawn: {e}")),
        Err(_) => HookOutcome::Error("hook timed out or panicked".to_string()),
    }
}

impl Default for HookBus {
    fn default() -> Self {
        Self::new(256)
    }
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_reaches_subscriber() {
        let bus = HookBus::new(16);
        let mut sub = bus.subscribe();
        bus.publish(HookEvent::SessionStart {
            session_id: "s1".into(),
        });
        let ev = sub.recv().await.unwrap();
        assert_eq!(
            ev,
            HookEvent::SessionStart {
                session_id: "s1".into()
            }
        );
    }

    #[tokio::test]
    async fn events_arrive_in_order() {
        let bus = HookBus::new(16);
        let mut sub = bus.subscribe();
        bus.publish(HookEvent::SessionStart {
            session_id: "s".into(),
        });
        bus.publish(HookEvent::PreToolUse {
            session_id: "s".into(),
            tool: "read_file".into(),
        });
        bus.publish(HookEvent::PostToolUse {
            session_id: "s".into(),
            tool: "read_file".into(),
            is_error: false,
        });

        assert_eq!(sub.recv().await.unwrap().name(), "SessionStart");
        assert_eq!(sub.recv().await.unwrap().name(), "PreToolUse");
        assert_eq!(sub.recv().await.unwrap().name(), "PostToolUse");
    }

    #[tokio::test]
    async fn multiple_subscribers_all_receive() {
        let bus = HookBus::new(16);
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);
        bus.publish(HookEvent::Stop {
            session_id: "s".into(),
        });
        assert_eq!(a.recv().await.unwrap().name(), "Stop");
        assert_eq!(b.recv().await.unwrap().name(), "Stop");
    }

    #[test]
    fn event_session_id_accessor() {
        let ev = HookEvent::PreToolUse {
            session_id: "abc".into(),
            tool: "shell".into(),
        };
        assert_eq!(ev.session_id(), "abc");
    }

    #[test]
    fn event_serializes_with_kind_tag() {
        let ev = HookEvent::SessionStart {
            session_id: "s1".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""kind":"sessionStart""#));
    }

    #[tokio::test]
    async fn no_subscribers_publish_returns_zero() {
        let bus = HookBus::new(16);
        let count = bus.publish(HookEvent::Stop {
            session_id: "s".into(),
        });
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn run_blocking_stop_is_noop_without_env() {
        let _guard = hooks_env_test_lock();
        std::env::remove_var("RIGHTKIT_HOOKS_JSON");
        std::env::remove_var("RIGHTKIT_HOOK_STOP_BLOCKING");
        let bus = HookBus::new(16);

        assert_eq!(
            bus.run_blocking_stop("s1", "complete", 0).await,
            HookOutcome::Proceed
        );
    }

    /// Serializes tests that touch `RIGHTKIT_HOOK_*` env vars. Without this
    /// the hooks tests race under default-parallel `cargo test`.
    #[cfg(test)]
    fn hooks_env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct FixedProcessLaunchAdapter {
        stdout: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl ProcessLaunchAdapter for FixedProcessLaunchAdapter {
        async fn run(
            &self,
            _command: String,
            _envs: Vec<(&'static str, String)>,
            _context: rightkit_sandbox::execution::ProcessLaunchContext,
        ) -> Result<std::process::Output, String> {
            #[cfg(unix)]
            use std::os::unix::process::ExitStatusExt;
            #[cfg(windows)]
            use std::os::windows::process::ExitStatusExt;

            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: self.stdout.clone(),
                stderr: Vec::new(),
            })
        }
    }

    #[test]
    fn pre_commit_event_serializes_with_kind_tag() {
        let ev = HookEvent::PreCommit {
            session_id: "s1".into(),
            diff: "diff --git a/foo.rs b/foo.rs\n+line".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""kind":"preCommit""#));
        assert!(json.contains("diff --git"));
    }

    #[test]
    fn pre_commit_event_name_and_session() {
        let ev = HookEvent::PreCommit {
            session_id: "sess-42".into(),
            diff: String::new(),
        };
        assert_eq!(ev.name(), "PreCommit");
        assert_eq!(ev.session_id(), "sess-42");
    }

    #[tokio::test]
    async fn pre_commit_publish_reaches_subscriber() {
        let bus = HookBus::new(16);
        let mut sub = bus.subscribe();
        bus.publish(HookEvent::PreCommit {
            session_id: "s".into(),
            diff: "+new line".into(),
        });
        let ev = sub.recv().await.unwrap();
        assert_eq!(ev.name(), "PreCommit");
        if let HookEvent::PreCommit { diff, .. } = &ev {
            assert_eq!(diff, "+new line");
        } else {
            panic!("expected PreCommit event");
        }
    }

    #[tokio::test]
    async fn run_pre_commit_is_noop_without_env() {
        let _guard = hooks_env_test_lock();
        std::env::remove_var("RIGHTKIT_HOOK_PRE_COMMIT");
        let bus = HookBus::new(16);
        assert_eq!(
            bus.run_pre_commit("s1", "some diff").await,
            HookOutcome::Proceed
        );
    }

    #[tokio::test]
    async fn run_pre_commit_exit_zero_proceeds() {
        let _guard = hooks_env_test_lock();
        std::env::set_var("RIGHTKIT_HOOK_PRE_COMMIT", "exit 0");
        let bus = HookBus::new(16);
        assert_eq!(
            bus.run_pre_commit("s1", "clean diff").await,
            HookOutcome::Proceed
        );
        std::env::remove_var("RIGHTKIT_HOOK_PRE_COMMIT");
    }

    #[tokio::test]
    async fn run_pre_commit_exit_two_denies_with_reason() {
        let _guard = hooks_env_test_lock();
        let cmd = if cfg!(windows) {
            "echo security finding: hardcoded secret& exit 2"
        } else {
            "echo 'security finding: hardcoded secret'; exit 2"
        };
        std::env::set_var("RIGHTKIT_HOOK_PRE_COMMIT", cmd);
        let bus = HookBus::new(16);
        let outcome = bus.run_pre_commit("s1", "+password = 'secret'").await;
        match outcome {
            HookOutcome::Deny(msg) => {
                assert!(
                    msg.contains("security finding"),
                    "deny reason should contain hook stdout: {msg}"
                );
            }
            other => panic!("expected Deny, got {other:?}"),
        }
        std::env::remove_var("RIGHTKIT_HOOK_PRE_COMMIT");
    }

    #[tokio::test]
    async fn run_pre_commit_exit_one_is_error() {
        let _guard = hooks_env_test_lock();
        let cmd = if cfg!(windows) {
            "echo hook crashed& exit 1"
        } else {
            "echo 'hook crashed'; exit 1"
        };
        std::env::set_var("RIGHTKIT_HOOK_PRE_COMMIT", cmd);
        let bus = HookBus::new(16);
        let outcome = bus.run_pre_commit("s1", "diff").await;
        match outcome {
            HookOutcome::Error(msg) => {
                assert!(
                    msg.contains("hook crashed"),
                    "error should contain hook stdout: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        std::env::remove_var("RIGHTKIT_HOOK_PRE_COMMIT");
    }

    #[tokio::test]
    async fn eff002_pre_tool_transform_envelope_is_applied() {
        let _guard = hooks_env_test_lock();
        std::env::set_var("RIGHTKIT_HOOK_PRE_TOOL_USE", "fixture");
        let bus = HookBus::with_process_launcher(
            16,
            std::sync::Arc::new(FixedProcessLaunchAdapter {
                stdout: br#"{"schemaVersion":1,"action":"transform","arguments":{"command":"echo transformed"}}"#.to_vec(),
            }),
            rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: "fixture".into(),
                sandbox_profile: None,
                sandbox_required: false,
                network_required: false,
            },
        );
        let outcome = bus
            .run_pre_tool_use(
                "s1",
                "shell",
                &serde_json::json!({"command":"echo original"}),
            )
            .await;
        assert_eq!(
            outcome,
            PreToolUseOutcome::Transform(serde_json::json!({"command":"echo transformed"}))
        );
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
    }

    #[tokio::test]
    async fn eff002_pre_tool_ordinary_stdout_stays_proceed() {
        let _guard = hooks_env_test_lock();
        let cmd = if cfg!(windows) {
            "echo informational"
        } else {
            "printf informational"
        };
        std::env::set_var("RIGHTKIT_HOOK_PRE_TOOL_USE", cmd);
        assert_eq!(
            HookBus::new(16)
                .run_pre_tool_use("s1", "shell", &serde_json::json!({}))
                .await,
            PreToolUseOutcome::Proceed
        );
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
    }

    #[tokio::test]
    async fn eff002_pre_tool_deny_and_error_remain_typed() {
        let _guard = hooks_env_test_lock();
        let deny = if cfg!(windows) {
            "echo blocked& exit 2"
        } else {
            "echo blocked; exit 2"
        };
        std::env::set_var("RIGHTKIT_HOOK_PRE_TOOL_USE", deny);
        assert!(
            matches!(HookBus::new(16).run_pre_tool_use("s1", "shell", &serde_json::json!({})).await, PreToolUseOutcome::Deny(message) if message.contains("blocked"))
        );
        let error = if cfg!(windows) {
            "echo failed& exit 1"
        } else {
            "echo failed; exit 1"
        };
        std::env::set_var("RIGHTKIT_HOOK_PRE_TOOL_USE", error);
        assert!(
            matches!(HookBus::new(16).run_pre_tool_use("s1", "shell", &serde_json::json!({})).await, PreToolUseOutcome::Error(message) if message.contains("failed"))
        );
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
    }

    #[tokio::test]
    async fn run_pre_commit_passes_diff_via_env() {
        let _guard = hooks_env_test_lock();
        // The hook echoes back the RIGHTKIT_HOOK_DIFF env var and exits 2
        // to verify the diff was actually passed.
        let cmd = if cfg!(windows) {
            "echo %RIGHTKIT_HOOK_DIFF%& exit 2"
        } else {
            "echo \"$RIGHTKIT_HOOK_DIFF\"; exit 2"
        };
        std::env::set_var("RIGHTKIT_HOOK_PRE_COMMIT", cmd);
        let bus = HookBus::new(16);
        let outcome = bus.run_pre_commit("s1", "+leaked_key").await;
        match outcome {
            HookOutcome::Deny(msg) => {
                assert!(
                    msg.contains("+leaked_key"),
                    "deny reason should contain the diff content: {msg}"
                );
            }
            other => panic!("expected Deny, got {other:?}"),
        }
        std::env::remove_var("RIGHTKIT_HOOK_PRE_COMMIT");
    }

    #[test]
    fn full_event_taxonomy_has_bus_events() {
        let s = "s".to_string();
        let events = [
            HookEvent::PreToolUse {
                session_id: s.clone(),
                tool: "t".into(),
            },
            HookEvent::PostToolUse {
                session_id: s.clone(),
                tool: "t".into(),
                is_error: false,
            },
            HookEvent::UserPromptSubmit {
                session_id: s.clone(),
            },
            HookEvent::Stop {
                session_id: s.clone(),
            },
            HookEvent::SubagentStop {
                session_id: s.clone(),
                role: "r".into(),
                success: true,
            },
            HookEvent::SessionStart {
                session_id: s.clone(),
            },
            HookEvent::SessionEnd {
                session_id: s.clone(),
            },
            HookEvent::PreCompact {
                session_id: s.clone(),
            },
            HookEvent::Notification {
                session_id: s.clone(),
                message: String::new(),
            },
        ];
        let names: Vec<&str> = events.iter().map(HookEvent::name).collect();
        assert_eq!(names, crate::agent_hooks::chain::HOOK_EVENT_NAMES);
        assert!(events.iter().all(|e| e.session_id() == "s"));
        let json = serde_json::to_string(&events[7]).unwrap();
        assert!(json.contains(r#""kind":"preCompact""#));
    }

    fn scripted_bus(stdout: &str, code: i32) -> HookBus {
        struct Fixed(String, i32);
        #[async_trait::async_trait]
        impl ProcessLaunchAdapter for Fixed {
            async fn run(
                &self,
                _command: String,
                _envs: Vec<(&'static str, String)>,
                _context: rightkit_sandbox::execution::ProcessLaunchContext,
            ) -> Result<std::process::Output, String> {
                #[cfg(unix)]
                let status = {
                    use std::os::unix::process::ExitStatusExt;
                    std::process::ExitStatus::from_raw(self.1 << 8)
                };
                #[cfg(windows)]
                let status = {
                    use std::os::windows::process::ExitStatusExt;
                    std::process::ExitStatus::from_raw(self.1 as u32)
                };
                Ok(std::process::Output {
                    status,
                    stdout: self.0.clone().into_bytes(),
                    stderr: Vec::new(),
                })
            }
        }
        HookBus::with_process_launcher(
            16,
            std::sync::Arc::new(Fixed(stdout.to_string(), code)),
            rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: "fixture".into(),
                sandbox_profile: None,
                sandbox_required: false,
                network_required: false,
            },
        )
    }

    #[tokio::test]
    async fn pre_tool_ask_outcome_carries_reason_and_post_mutation_input() {
        let _guard = hooks_env_test_lock();
        std::env::set_var("RIGHTKIT_HOOK_PRE_TOOL_USE", "fixture");
        let bus = scripted_bus(
            r#"{"decision":"ask","reason":"confirm","updatedInput":{"command":"echo safe"}}"#,
            0,
        );
        let outcome = bus
            .run_pre_tool_use("s1", "shell", &serde_json::json!({"command":"rm x"}))
            .await;
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
        assert_eq!(
            outcome,
            PreToolUseOutcome::Ask {
                reason: "confirm".into(),
                updated_input: Some(serde_json::json!({"command":"echo safe"})),
            }
        );
    }

    #[tokio::test]
    async fn pre_tool_json_deny_and_allow_and_additional_context() {
        let _guard = hooks_env_test_lock();
        std::env::set_var("RIGHTKIT_HOOK_PRE_TOOL_USE", "fixture");
        let deny = scripted_bus(r#"{"decision":"block","reason":"policy"}"#, 0);
        assert_eq!(
            deny.run_pre_tool_use("s1", "shell", &serde_json::json!({}))
                .await,
            PreToolUseOutcome::Deny("policy".into())
        );
        let allow = scripted_bus(r#"{"decision":"allow","additionalContext":"checked"}"#, 0);
        let report = allow
            .run_pre_tool_use_report("s1", "shell", &serde_json::json!({}))
            .await;
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
        assert_eq!(report.outcome, PreToolUseOutcome::Proceed);
        assert_eq!(report.additional_context, vec!["checked".to_string()]);
    }

    #[tokio::test]
    async fn pre_tool_chain_from_json_env_matches_by_tool_and_denies() {
        let _guard = hooks_env_test_lock();
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
        std::env::set_var(
            crate::agent_hooks::chain::HOOKS_JSON_ENV,
            r#"{"PreToolUse":[{"matcher":"shell","command":"a"},{"matcher":"read_file","command":"b"}]}"#,
        );
        let bus = scripted_bus("", 2);
        let shell = bus
            .run_pre_tool_use("s1", "shell", &serde_json::json!({}))
            .await;
        let other = bus
            .run_pre_tool_use("s1", "write_file", &serde_json::json!({}))
            .await;
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        assert!(matches!(shell, PreToolUseOutcome::Deny(_)));
        assert_eq!(other, PreToolUseOutcome::Proceed);
    }

    #[tokio::test]
    async fn pre_tool_invalid_hooks_json_fails_closed() {
        let _guard = hooks_env_test_lock();
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
        std::env::set_var(crate::agent_hooks::chain::HOOKS_JSON_ENV, "{not json");
        let outcome = HookBus::new(16)
            .run_pre_tool_use("s1", "shell", &serde_json::json!({}))
            .await;
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        assert!(matches!(outcome, PreToolUseOutcome::Deny(m) if m.contains("fail closed")));
    }

    #[tokio::test]
    async fn pre_tool_timeout_fails_closed_via_bus() {
        struct Slow;
        #[async_trait::async_trait]
        impl ProcessLaunchAdapter for Slow {
            async fn run(
                &self,
                _command: String,
                _envs: Vec<(&'static str, String)>,
                _context: rightkit_sandbox::execution::ProcessLaunchContext,
            ) -> Result<std::process::Output, String> {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                Err("unreachable".into())
            }
        }
        let _guard = hooks_env_test_lock();
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
        std::env::set_var(
            crate::agent_hooks::chain::HOOKS_JSON_ENV,
            r#"{"PreToolUse":[{"command":"slow","timeoutSecs":1}]}"#,
        );
        let bus = HookBus::with_process_launcher(
            16,
            std::sync::Arc::new(Slow),
            rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: "fixture".into(),
                sandbox_profile: None,
                sandbox_required: false,
                network_required: false,
            },
        );
        let outcome = bus
            .run_pre_tool_use("s1", "shell", &serde_json::json!({}))
            .await;
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        assert!(matches!(outcome, PreToolUseOutcome::Deny(m) if m.contains("timed out")));
    }

    #[tokio::test]
    async fn run_event_hooks_blocks_user_prompt_submit() {
        let _guard = hooks_env_test_lock();
        std::env::set_var(
            crate::agent_hooks::chain::HOOKS_JSON_ENV,
            r#"{"UserPromptSubmit":[{"command":"gate"}]}"#,
        );
        let bus = scripted_bus(r#"{"decision":"block","reason":"no secrets"}"#, 0);
        let report = bus
            .run_event_hooks("UserPromptSubmit", "s1", None, None, &[])
            .await;
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        assert!(report.is_denied());
        assert_eq!(report.reason.as_deref(), Some("no secrets"));
    }

    #[tokio::test]
    async fn blocking_stop_reads_hooks_json_and_block_keeps_agent_going() {
        let _guard = hooks_env_test_lock();
        std::env::remove_var("RIGHTKIT_HOOK_STOP_BLOCKING");
        std::env::set_var(
            crate::agent_hooks::chain::HOOKS_JSON_ENV,
            r#"{"Stop":[{"command":"gate"}]}"#,
        );
        let blocked = scripted_bus(r#"{"decision":"block","reason":"tests still failing"}"#, 0)
            .run_blocking_stop("s1", "assistant_message_completed", 0)
            .await;
        let stopped = scripted_bus(r#"{"continue":false,"stopReason":"keep going"}"#, 0)
            .run_blocking_stop("s1", "assistant_message_completed", 1)
            .await;
        let allowed = scripted_bus("", 0)
            .run_blocking_stop("s1", "assistant_message_completed", 0)
            .await;
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        assert_eq!(blocked, HookOutcome::Deny("tests still failing".into()));
        assert_eq!(stopped, HookOutcome::Deny("keep going".into()));
        assert_eq!(allowed, HookOutcome::Proceed);
    }

    #[tokio::test]
    async fn blocking_stop_legacy_env_still_works_and_json_adds_to_it() {
        let _guard = hooks_env_test_lock();
        std::env::set_var("RIGHTKIT_HOOK_STOP_BLOCKING", "legacy");
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        let legacy_deny = scripted_bus("", 2).run_blocking_stop("s1", "r", 0).await;
        assert!(matches!(legacy_deny, HookOutcome::Deny(_)));
        // Legacy allows (exit 0), JSON chain denies: deny wins.
        std::env::set_var(
            crate::agent_hooks::chain::HOOKS_JSON_ENV,
            r#"{"Stop":[{"command":"gate"}]}"#,
        );
        let both = scripted_bus(r#"{"decision":"block","reason":"json says no"}"#, 0)
            .run_blocking_stop("s1", "r", 0)
            .await;
        std::env::remove_var("RIGHTKIT_HOOK_STOP_BLOCKING");
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        assert_eq!(both, HookOutcome::Deny("json says no".into()));
    }

    #[tokio::test]
    async fn subagent_stop_uses_json_only_and_can_block() {
        let _guard = hooks_env_test_lock();
        std::env::set_var(
            crate::agent_hooks::chain::HOOKS_JSON_ENV,
            r#"{"SubagentStop":[{"command":"gate"}]}"#,
        );
        let bus = scripted_bus(r#"{"decision":"block","reason":"subagent not done"}"#, 0);
        let sub = bus
            .run_blocking_stop_event("SubagentStop", "s1", "r", 0)
            .await;
        let main = bus.run_blocking_stop_event("Stop", "s1", "r", 0).await;
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        assert_eq!(sub, HookOutcome::Deny("subagent not done".into()));
        assert_eq!(main, HookOutcome::Proceed);
    }

    #[tokio::test]
    async fn pre_commit_reads_hooks_json_and_keeps_legacy() {
        let _guard = hooks_env_test_lock();
        std::env::remove_var("RIGHTKIT_HOOK_PRE_COMMIT");
        std::env::set_var(
            crate::agent_hooks::chain::HOOKS_JSON_ENV,
            r#"{"PreCommit":[{"command":"audit"}]}"#,
        );
        let denied = scripted_bus(r#"{"decision":"deny","reason":"secret in diff"}"#, 0)
            .run_pre_commit("s1", "+key")
            .await;
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        assert_eq!(denied, HookOutcome::Deny("secret in diff".into()));
        std::env::set_var("RIGHTKIT_HOOK_PRE_COMMIT", "legacy");
        let legacy = scripted_bus("", 2).run_pre_commit("s1", "+key").await;
        std::env::remove_var("RIGHTKIT_HOOK_PRE_COMMIT");
        assert!(matches!(legacy, HookOutcome::Deny(_)));
    }

    #[test]
    fn has_pre_tool_use_hooks_reflects_configuration() {
        let _guard = hooks_env_test_lock();
        std::env::remove_var("RIGHTKIT_HOOK_PRE_TOOL_USE");
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
        let bus = HookBus::new(4);
        assert!(!bus.has_pre_tool_use_hooks());
        std::env::set_var(
            crate::agent_hooks::chain::HOOKS_JSON_ENV,
            r#"{"PreToolUse":[{"command":"x"}]}"#,
        );
        assert!(bus.has_pre_tool_use_hooks());
        std::env::set_var(crate::agent_hooks::chain::HOOKS_JSON_ENV, "{bad");
        assert!(bus.has_pre_tool_use_hooks());
        std::env::remove_var(crate::agent_hooks::chain::HOOKS_JSON_ENV);
    }
}
