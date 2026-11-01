//! `host_hooks` runtime registry and dry-run helper.
//!
//! Tracks hooks that the operator has revoked via `host app hooks revoke <id>`
//! for the lifetime of the daemon process, and offers a synchronous dry-run
//! path that exercises matching env-var hook commands without publishing on
//! the event bus.

use crate::agent_hooks::bus::HookOutcome;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::RwLock;

/// Shared, in-memory set of hook IDs the operator has revoked for the current
/// daemon session. Keys are env-var names such as
/// `RIGHTKIT_HOOK_PRE_TOOL_USE`.
#[derive(Default, Debug)]
pub struct HookRegistry {
    revoked: RwLock<HashSet<String>>,
    scoped: RwLock<BTreeMap<String, BTreeSet<String>>>,
    generation: RwLock<u64>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark `id` as revoked for this daemon process. Returns `true` when the
    /// id was not previously revoked.
    pub fn revoke(&self, id: &str) -> bool {
        let mut set = self
            .revoked
            .write()
            .expect("HookRegistry write mutex poisoned");
        set.insert(id.to_string())
    }

    /// Remove a prior revocation for `id`. Returns `true` when the id was
    /// previously revoked.
    pub fn unrevoke(&self, id: &str) -> bool {
        let mut set = self
            .revoked
            .write()
            .expect("HookRegistry write mutex poisoned");
        set.remove(id)
    }

    /// Whether `id` is currently revoked.
    pub fn is_revoked(&self, id: &str) -> bool {
        self.revoked
            .read()
            .expect("HookRegistry read mutex poisoned")
            .contains(id)
    }

    /// Snapshot the revoked identifiers (sorted). Useful for status output.
    pub fn snapshot(&self) -> Vec<String> {
        let set = self
            .revoked
            .read()
            .expect("HookRegistry read mutex poisoned");
        let mut out: Vec<String> = set.iter().cloned().collect();
        out.sort();
        out
    }

    /// Register a hook under a disposable extension scope.
    pub fn register_scoped(&self, scope: &str, id: &str) -> bool {
        let mut scoped = self
            .scoped
            .write()
            .expect("HookRegistry scope mutex poisoned");
        let inserted = scoped
            .entry(scope.to_string())
            .or_default()
            .insert(id.to_string());
        if inserted {
            let mut generation = self
                .generation
                .write()
                .expect("HookRegistry generation mutex poisoned");
            *generation = generation.saturating_add(1);
        }
        inserted
    }

    /// Remove every hook registration owned by `scope`, returning its count.
    pub fn unload_scope(&self, scope: &str) -> usize {
        let removed = self
            .scoped
            .write()
            .expect("HookRegistry scope mutex poisoned")
            .remove(scope)
            .map_or(0, |ids| ids.len());
        if removed > 0 {
            let mut generation = self
                .generation
                .write()
                .expect("HookRegistry generation mutex poisoned");
            *generation = generation.saturating_add(1);
        }
        removed
    }

    pub fn scope_snapshot(&self, scope: &str) -> Vec<String> {
        self.scoped
            .read()
            .expect("HookRegistry scope mutex poisoned")
            .get(scope)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn generation(&self) -> u64 {
        *self
            .generation
            .read()
            .expect("HookRegistry generation mutex poisoned")
    }
}

/// Result of a [`dry_run_hook`] invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDryRunReport {
    /// Whether at least one matching env-var hook actually ran during the
    /// dry-run (i.e. was both configured and not revoked).
    pub ran: bool,
    /// `Proceed` / `Deny` / `Error` synthesized from the hook's exit code.
    pub outcome: HookOutcome,
    /// Env var keys that were run.
    pub invoked: Vec<String>,
    /// Env var keys that exist but were skipped because they were revoked.
    pub revoked: Vec<String>,
    /// True when no env-var hook matched the requested event and matcher.
    pub no_match: bool,
}

impl HookDryRunReport {
    fn no_match() -> Self {
        Self {
            ran: false,
            outcome: HookOutcome::Proceed,
            invoked: Vec::new(),
            revoked: Vec::new(),
            no_match: true,
        }
    }

    /// H8 admission decision for this report. Delegates to
    /// [`HookAdmissionV1::from_execution`] — the single admission authority —
    /// so a dry-run and a live daemon dispatch are judged by the same rule.
    pub fn decide_admission(&self) -> HookAdmissionV1 {
        HookAdmissionV1::from_execution(self.ran, !self.revoked.is_empty(), self.invoked.clone())
    }
}

/// H8 hook admission outcome: whether a hook-sourced signal may be treated as
/// evidence, or must be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAdmissionV1 {
    /// A real host command actually ran (spawned and produced a result).
    /// `invoked` names the env-var-keyed hooks that ran.
    Admitted { invoked: Vec<String> },
    /// No real host execution backs this event; nothing is admitted.
    Rejected { reason: HookRejectionReasonV1 },
}

/// Why a hook-sourced signal was rejected by H8 admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookRejectionReasonV1 {
    /// The operator revoked this hook id for the current session.
    Revoked,
    /// No matching hook actually ran: unknown event, no configured command,
    /// empty env var, or the process never produced a confirmed result
    /// (spawn denial, I/O failure, or timeout). The event firing alone is
    /// never treated as proof of execution.
    NoRealExecution,
}

impl HookAdmissionV1 {
    /// The single H8 admission authority. Every hook-sourced admission
    /// decision — dry-run report or live daemon dispatch alike — must route
    /// through this function; do not duplicate this branching elsewhere.
    ///
    /// `ran` must reflect genuine host execution (a process that was
    /// actually spawned and returned a confirmed result), never merely "the
    /// event was published" or "a command string was configured". A hook
    /// firing with no real execution behind it (`ran == false`) is always
    /// rejected, regardless of `revoked`, so it can never manufacture
    /// evidence.
    pub fn from_execution(ran: bool, revoked: bool, invoked: Vec<String>) -> Self {
        if revoked {
            return HookAdmissionV1::Rejected {
                reason: HookRejectionReasonV1::Revoked,
            };
        }
        if !ran {
            return HookAdmissionV1::Rejected {
                reason: HookRejectionReasonV1::NoRealExecution,
            };
        }
        HookAdmissionV1::Admitted { invoked }
    }

    /// True when this decision admits hook-sourced evidence.
    pub fn is_admitted(&self) -> bool {
        matches!(self, HookAdmissionV1::Admitted { .. })
    }
}

/// Dry-run the matching command env-var hook for `event_name` without
/// publishing on the hook bus. The optional `matcher` is a substring filter on
/// the env var name. `revoked` describes hooks that the operator has already
/// disabled for this session — those keys are skipped, even if their env var
/// is set.
///
/// If no matching command hook exists, the report has `ran = false`,
/// `outcome = HookOutcome::Proceed`, and `no_match = true`.
#[cfg(test)]
pub async fn dry_run_hook(
    event_name: &str,
    matcher: Option<&str>,
    registry: Option<&HookRegistry>,
) -> HookDryRunReport {
    let env_var = match hook_command_env_var(event_name) {
        Some(v) => v,
        None => return HookDryRunReport::no_match(),
    };
    if let Some(m) = matcher {
        if !m.is_empty() && !env_var.contains(m) {
            return HookDryRunReport::no_match();
        }
    }

    if let Some(reg) = registry {
        if reg.is_revoked(&env_var) {
            return HookDryRunReport {
                ran: false,
                outcome: HookOutcome::Proceed,
                invoked: Vec::new(),
                revoked: vec![env_var.clone()],
                no_match: false,
            };
        }
    }

    let value = match crate::agent_hooks::hook_env(&env_var) {
        Some(v) => v,
        None => return HookDryRunReport::no_match(),
    };

    let envs: Vec<(&'static str, String)> = vec![("RIGHTKIT_HOOK_EVENT", event_name.to_string())];
    let outcome = run_hook_command(
        value,
        envs,
        "Hook denied execution",
        "Hook exited with error",
    )
    .await;
    HookDryRunReport {
        ran: true,
        outcome,
        invoked: vec![env_var.clone()],
        revoked: Vec::new(),
        no_match: false,
    }
}

/// Adapter-aware entry point reserved for daemon callers during migration.
pub async fn dry_run_hook_with_adapter(
    event_name: &str,
    matcher: Option<&str>,
    registry: Option<&HookRegistry>,
    adapter: &dyn rightkit_sandbox::execution::ProcessLaunchAdapter,
    context: &rightkit_sandbox::execution::ProcessLaunchContext,
) -> HookDryRunReport {
    // Unit tests retain deterministic shell semantics when exercising the
    // registry; daemon callers always provide a required governed context.
    #[cfg(test)]
    if !context.sandbox_required {
        return dry_run_hook(event_name, matcher, registry).await;
    }

    let env_var = match hook_command_env_var(event_name) {
        Some(v) => v,
        None => return HookDryRunReport::no_match(),
    };
    if matcher.is_some_and(|m| !m.is_empty() && !env_var.contains(m)) {
        return HookDryRunReport::no_match();
    }
    if registry.is_some_and(|reg| reg.is_revoked(&env_var)) {
        return HookDryRunReport {
            ran: false,
            outcome: HookOutcome::Proceed,
            invoked: Vec::new(),
            revoked: vec![env_var],
            no_match: false,
        };
    }
    let Some(command) = crate::agent_hooks::hook_env(&env_var) else {
        return HookDryRunReport::no_match();
    };
    #[cfg(windows)]
    let (program, argv) = ("cmd".to_string(), vec!["/C".to_string(), command]);
    #[cfg(not(windows))]
    let (program, argv) = ("sh".to_string(), vec!["-c".to_string(), command]);
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program,
        argv,
        cwd: context.workspace_root.clone(),
        context: context.clone(),
        env: vec![("RIGHTKIT_HOOK_EVENT".to_string(), event_name.to_string())],
    };
    let outcome = match adapter.spawn(spec).await {
        Ok(mut child) => {
            use tokio::io::AsyncReadExt;
            let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                let mut stdout = Vec::new();
                if let Some(mut stream) = child.take_stdout() {
                    stream.read_to_end(&mut stdout).await.map_err(|_| ())?;
                }
                let code = child.wait().await.map_err(|_| ())?;
                Ok::<_, ()>((stdout, code))
            })
            .await;
            match result {
                Ok(Ok((stdout, code))) => hook_exit_outcome(
                    Some(code),
                    &stdout,
                    "Hook denied execution",
                    "Hook exited with error",
                ),
                Ok(Err(())) => HookOutcome::Error("hook process I/O failed".into()),
                Err(_) => {
                    let _ = child.cancel().await;
                    HookOutcome::Error("hook timed out after 30 seconds".into())
                }
            }
        }
        Err(error) => HookOutcome::Error(format!("hook failed to spawn: {error:?}")),
    };
    HookDryRunReport {
        ran: true,
        outcome,
        invoked: vec![env_var],
        revoked: Vec::new(),
        no_match: false,
    }
}

fn hook_exit_outcome(
    code: Option<i32>,
    stdout: &[u8],
    default_deny: &str,
    default_error: &str,
) -> HookOutcome {
    let message = String::from_utf8_lossy(stdout).trim().to_string();
    match code {
        Some(0) | None => HookOutcome::Proceed,
        Some(2) => HookOutcome::Deny(if message.is_empty() {
            default_deny.into()
        } else {
            message
        }),
        Some(1) => HookOutcome::Error(if message.is_empty() {
            default_error.into()
        } else {
            message
        }),
        Some(other) => HookOutcome::Error(format!(
            "hook exited with unexpected code {other}: {message}"
        )),
    }
}

fn hook_command_env_var(event_name: &str) -> Option<String> {
    let base = match event_name {
        "PreToolUse" => hook_var!("RIGHTKIT_HOOK_PRE_TOOL_USE"),
        "PostToolUse" => hook_var!("RIGHTKIT_HOOK_POST_TOOL_USE"),
        "UserPromptSubmit" => hook_var!("RIGHTKIT_HOOK_USER_PROMPT_SUBMIT"),
        "Notification" => hook_var!("RIGHTKIT_HOOK_NOTIFICATION"),
        "SubagentStop" => hook_var!("RIGHTKIT_HOOK_SUBAGENT_STOP"),
        "Stop" => hook_var!("RIGHTKIT_HOOK_STOP_BLOCKING"),
        "PreCommit" => hook_var!("RIGHTKIT_HOOK_PRE_COMMIT"),
        _ => None,
    };
    base.map(str::to_string)
}

#[cfg(test)]
async fn run_hook_command(
    cmd: String,
    envs: Vec<(&'static str, String)>,
    default_deny: &'static str,
    default_error: &'static str,
) -> HookOutcome {
    #[cfg(windows)]
    let (sh, flag) = ("cmd", "/C");
    #[cfg(not(windows))]
    let (sh, flag) = ("sh", "-c");

    // Shared RightKit runner: hidden console, null stdin, the whole hook tree terminated at the
    // deadline (the previous tokio timeout stopped waiting but left the hook running).
    let result = tokio::task::spawn_blocking(move || {
        let mut owned = rightkit_child_process::OwnedCommand::new(sh);
        owned.command_mut().arg(flag).arg(&cmd);
        for (key, value) in envs {
            owned.command_mut().env(key, value);
        }
        let options = rightkit_child_process::RunOptions::new(std::time::Duration::from_secs(30))
            .stdout_cap(usize::MAX)
            .stderr_cap(0);
        rightkit_child_process::run_command(owned, None, &options)
    })
    .await;

    match result {
        Ok(Ok(output)) if output.timed_out => {
            HookOutcome::Error("hook timed out or panicked".to_string())
        }
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout.bytes)
                .trim()
                .to_string();
            match output.status.code() {
                Some(0) | None => HookOutcome::Proceed,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// These tests mutate the PROCESS-GLOBAL env var `RIGHTKIT_HOOK_NOTIFICATION`; `#[tokio::test]`
    /// runs them on parallel threads, so without serialization one test's `set_var` races another's
    /// read (the 2026-07-05 flake: exit_zero and exit_one interleaving). Every env-touching test
    /// holds this guard for its whole body. Poison-recovering so one panicking test can't wedge the
    /// rest into a false failure.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_guard() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn scoped_registration_unloads_and_advances_generation() {
        let registry = HookRegistry::new();
        assert!(registry.register_scoped("plugin:alpha", "RIGHTKIT_HOOK_PRE_TOOL_USE"));
        assert!(!registry.register_scoped("plugin:alpha", "RIGHTKIT_HOOK_PRE_TOOL_USE"));
        assert_eq!(
            registry.scope_snapshot("plugin:alpha"),
            vec!["RIGHTKIT_HOOK_PRE_TOOL_USE"]
        );
        let registered_generation = registry.generation();
        assert_eq!(registry.unload_scope("plugin:alpha"), 1);
        assert!(registry.generation() > registered_generation);
        assert!(registry.scope_snapshot("plugin:alpha").is_empty());
        assert_eq!(registry.unload_scope("plugin:alpha"), 0);
    }

    #[test]
    fn registry_revoke_is_idempotent() {
        let reg = HookRegistry::new();
        assert!(reg.revoke("RIGHTKIT_HOOK_PRE_TOOL_USE"));
        assert!(reg.is_revoked("RIGHTKIT_HOOK_PRE_TOOL_USE"));
        assert!(!reg.revoke("RIGHTKIT_HOOK_PRE_TOOL_USE"));
        assert!(reg.unrevoke("RIGHTKIT_HOOK_PRE_TOOL_USE"));
        assert!(!reg.is_revoked("RIGHTKIT_HOOK_PRE_TOOL_USE"));
        assert!(!reg.unrevoke("RIGHTKIT_HOOK_PRE_TOOL_USE"));
    }

    #[test]
    fn registry_snapshot_is_sorted() {
        let reg = HookRegistry::new();
        reg.revoke("RIGHTKIT_HOOK_PRE_TOOL_USE");
        reg.revoke("RIGHTKIT_HOOK_NOTIFICATION");
        assert_eq!(
            reg.snapshot(),
            vec![
                "RIGHTKIT_HOOK_NOTIFICATION".to_string(),
                "RIGHTKIT_HOOK_PRE_TOOL_USE".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn dry_run_unknown_event_returns_no_match() {
        let reg = HookRegistry::new();
        let report = dry_run_hook("UnknownEvent", None, Some(&reg)).await;
        assert!(!report.ran);
        assert_eq!(report.outcome, HookOutcome::Proceed);
        assert!(report.no_match);
        assert!(report.invoked.is_empty());
    }

    #[allow(clippy::await_holding_lock)]
    // ENV_LOCK serializes env-var mutation across parallel #[tokio::test]; dry_run_hook reads env synchronously before yielding, so the guard must stay held through .await
    #[tokio::test]
    async fn dry_run_with_no_env_var_returns_no_match() {
        let _env = env_guard();
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
        let reg = HookRegistry::new();
        let report = dry_run_hook("Notification", None, Some(&reg)).await;
        assert!(!report.ran);
        assert!(report.no_match);
        assert!(report.invoked.is_empty());
    }

    #[allow(clippy::await_holding_lock)]
    // ENV_LOCK serializes env-var mutation across parallel #[tokio::test]; dry_run_hook reads env synchronously before yielding, so the guard must stay held through .await
    #[tokio::test]
    async fn dry_run_revoked_hook_skips_invocation() {
        let _env = env_guard();
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", "echo ran && exit 0");
        let reg = HookRegistry::new();
        reg.revoke("RIGHTKIT_HOOK_NOTIFICATION");
        let report = dry_run_hook("Notification", None, Some(&reg)).await;
        assert!(!report.ran);
        assert_eq!(report.outcome, HookOutcome::Proceed);
        assert_eq!(
            report.revoked,
            vec!["RIGHTKIT_HOOK_NOTIFICATION".to_string()]
        );
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    #[allow(clippy::await_holding_lock)]
    // ENV_LOCK serializes env-var mutation across parallel #[tokio::test]; dry_run_hook reads env synchronously before yielding, so the guard must stay held through .await
    #[tokio::test]
    async fn dry_run_exit_zero_returns_proceed() {
        let _env = env_guard();
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", "exit 0");
        let reg = HookRegistry::new();
        let report = dry_run_hook("Notification", None, Some(&reg)).await;
        assert!(report.ran);
        assert_eq!(report.outcome, HookOutcome::Proceed);
        assert_eq!(
            report.invoked,
            vec!["RIGHTKIT_HOOK_NOTIFICATION".to_string()]
        );
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    #[allow(clippy::await_holding_lock)]
    // ENV_LOCK serializes env-var mutation across parallel #[tokio::test]; dry_run_hook reads env synchronously before yielding, so the guard must stay held through .await
    #[tokio::test]
    async fn dry_run_exit_two_returns_deny_with_message() {
        let _env = env_guard();
        // `;` is not a cmd separator on Windows (commands run via `cmd /C`);
        // use the platform-correct chaining operator.
        let cmd = if cfg!(windows) {
            "echo blocked& exit 2"
        } else {
            "echo blocked; exit 2"
        };
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", cmd);
        let reg = HookRegistry::new();
        let report = dry_run_hook("Notification", None, Some(&reg)).await;
        assert!(report.ran);
        match report.outcome {
            HookOutcome::Deny(msg) => assert_eq!(msg, "blocked"),
            other => panic!("expected Deny, got {other:?}"),
        }
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    #[allow(clippy::await_holding_lock)]
    // ENV_LOCK serializes env-var mutation across parallel #[tokio::test]; dry_run_hook reads env synchronously before yielding, so the guard must stay held through .await
    #[tokio::test]
    async fn dry_run_exit_one_returns_error_with_message() {
        let _env = env_guard();
        let cmd = if cfg!(windows) {
            "echo failed& exit 1"
        } else {
            "echo failed; exit 1"
        };
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", cmd);
        let reg = HookRegistry::new();
        let report = dry_run_hook("Notification", None, Some(&reg)).await;
        assert!(report.ran);
        match report.outcome {
            HookOutcome::Error(msg) => assert_eq!(msg, "failed"),
            other => panic!("expected Error, got {other:?}"),
        }
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    /// Negative control for H8 admission: an event that fires with NO real
    /// host execution behind it (here, the injected fault is "no
    /// `RIGHTKIT_HOOK_NOTIFICATION` command configured") must be REJECTED,
    /// never `Admitted`. This is the invariant the integration owner proves —
    /// a hook or serialization event alone must never manufacture evidence.
    /// If `HookDryRunReport::decide_admission` (or the `ran` signal it
    /// trusts) ever regresses to admitting an unexecuted event, this test
    /// fails.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn admission_rejects_hook_event_with_no_real_execution() {
        let _env = env_guard();
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
        let reg = HookRegistry::new();
        let report = dry_run_hook("Notification", None, Some(&reg)).await;
        // Sanity: the event still resolves to a known hook slot, it simply
        // never actually ran — this is the fault under test, not a no-match.
        assert!(
            !report.ran,
            "no host command was configured; nothing executed"
        );

        let admission = report.decide_admission();
        assert_eq!(
            admission,
            HookAdmissionV1::Rejected {
                reason: HookRejectionReasonV1::NoRealExecution
            }
        );
        assert!(!admission.is_admitted());
    }

    /// Positive control paired with the negative control above: a report
    /// where a host command genuinely ran (`ran == true`) is admitted, and
    /// carries forward exactly the env vars that were invoked.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn admission_admits_report_with_confirmed_execution() {
        let _env = env_guard();
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", "exit 0");
        let reg = HookRegistry::new();
        let report = dry_run_hook("Notification", None, Some(&reg)).await;
        assert!(report.ran);
        let admission = report.decide_admission();
        assert_eq!(
            admission,
            HookAdmissionV1::Admitted {
                invoked: vec!["RIGHTKIT_HOOK_NOTIFICATION".to_string()]
            }
        );
        assert!(admission.is_admitted());
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    /// A revoked hook is rejected with the `Revoked` reason, distinct from
    /// `NoRealExecution`, so operators and evidence readers can tell an
    /// operator-suppressed hook apart from one that never fired at all.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn admission_rejects_revoked_hook_with_revoked_reason() {
        let _env = env_guard();
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", "exit 0");
        let reg = HookRegistry::new();
        reg.revoke("RIGHTKIT_HOOK_NOTIFICATION");
        let report = dry_run_hook("Notification", None, Some(&reg)).await;
        assert!(!report.ran);
        let admission = report.decide_admission();
        assert_eq!(
            admission,
            HookAdmissionV1::Rejected {
                reason: HookRejectionReasonV1::Revoked
            }
        );
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }

    #[allow(clippy::await_holding_lock)]
    // ENV_LOCK serializes env-var mutation across parallel #[tokio::test]; dry_run_hook reads env synchronously before yielding, so the guard must stay held through .await
    #[tokio::test]
    async fn dry_run_matcher_filters_env_var_name() {
        let _env = env_guard();
        std::env::set_var("RIGHTKIT_HOOK_NOTIFICATION", "exit 0");
        let reg = HookRegistry::new();
        let report = dry_run_hook("Notification", Some("PRE"), Some(&reg)).await;
        assert!(report.no_match);
        assert!(!report.ran);
        std::env::remove_var("RIGHTKIT_HOOK_NOTIFICATION");
    }
}
