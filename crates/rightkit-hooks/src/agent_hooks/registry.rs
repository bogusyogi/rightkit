//! `host_hooks` runtime registry and dry-run helper.
//!
//! Tracks hooks that the operator has revoked via `host app hooks revoke <id>`
//! for the lifetime of the daemon process, and offers a synchronous dry-run
//! path that exercises matching configured command hooks without publishing
//! on the event bus.

use crate::agent_hooks::bus::{HookBus, HookOutcome};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::RwLock;

/// Shared, in-memory set of hook IDs the operator has revoked for the current
/// daemon session. Keys are hook ids such as `PreToolUse#0`
/// ([`IdentifiedHook::id`]).
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

/// Result of a [`dry_run_hook_with_adapter`] invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDryRunReport {
    /// Whether at least one matching command hook actually ran during the
    /// dry-run (i.e. was both configured and not revoked).
    pub ran: bool,
    /// `Proceed` / `Deny` / `Error` synthesized from the hook's exit code.
    pub outcome: HookOutcome,
    /// Ids of the hooks that were run.
    pub invoked: Vec<String>,
    /// Ids of hooks that exist but were skipped because they were revoked.
    pub revoked: Vec<String>,
    /// True when no command hook matched the requested event and matcher.
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
    /// `invoked` names the ids of the hooks that ran.
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
    /// or the process never produced a confirmed result
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

/// One configured hook with the identity operators revoke it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifiedHook {
    /// `<Event>#<n>`: `n` counts that event's entries in execution order (config key, project
    /// file, then `RIGHTKIT_HOOKS_JSON`). It changes when the configuration changes.
    pub id: String,
    pub event: String,
    /// `config`, `project` or `env-json`.
    pub origin: &'static str,
    pub spec: crate::agent_hooks::chain::HookSpec,
}

impl HookBus {
    /// Every configured hook for `event`, identified, in execution order.
    pub fn hooks_for(&self, event: &str) -> Result<Vec<IdentifiedHook>, String> {
        let mut labelled = self.configured_specs_labelled(event)?;
        labelled.extend(
            crate::agent_hooks::chain::env_hook_specs(event)?
                .into_iter()
                .map(|spec| ("env-json", spec)),
        );
        Ok(labelled
            .into_iter()
            .enumerate()
            .map(|(n, (origin, spec))| IdentifiedHook {
                id: format!("{event}#{n}"),
                event: event.to_string(),
                origin,
                spec,
            })
            .collect())
    }

    /// Every configured hook of every event in the taxonomy (`GET /hooks/list`).
    pub fn inventory(&self) -> Result<Vec<IdentifiedHook>, String> {
        let mut all = Vec::new();
        for event in crate::agent_hooks::chain::HOOK_EVENT_NAMES {
            all.extend(self.hooks_for(event)?);
        }
        Ok(all)
    }
}

/// Dry-run the command hooks of `event_name` without publishing on the hook bus. `hooks` is
/// `bus.hooks_for(event_name)`. The optional `matcher` is a tool name tested against each hook's
/// own matcher. Revoked hooks are skipped. The first deny wins, then an error, else proceed.
///
/// If no command hook applies, the report has `ran = false`, `outcome = HookOutcome::Proceed`,
/// and `no_match = true`.
pub async fn dry_run_hook_with_adapter(
    event_name: &str,
    matcher: Option<&str>,
    registry: Option<&HookRegistry>,
    hooks: &[IdentifiedHook],
    adapter: &dyn rightkit_sandbox::execution::ProcessLaunchAdapter,
    context: &rightkit_sandbox::execution::ProcessLaunchContext,
) -> HookDryRunReport {
    let candidates: Vec<&IdentifiedHook> = hooks
        .iter()
        .filter(|hook| hook.event == event_name)
        .filter(|hook| hook.spec.kind == crate::agent_hooks::chain::HookKind::Command)
        .filter(|hook| hook.spec.matches(matcher))
        .collect();
    if candidates.is_empty() {
        return HookDryRunReport::no_match();
    }
    let mut revoked = Vec::new();
    let mut invoked = Vec::new();
    let mut outcome = HookOutcome::Proceed;
    for hook in candidates {
        if registry.is_some_and(|reg| reg.is_revoked(&hook.id)) {
            revoked.push(hook.id.clone());
            continue;
        }
        let result =
            run_dry_run_command(hook.spec.command.clone(), event_name, adapter, context).await;
        invoked.push(hook.id.clone());
        match (&outcome, result) {
            (HookOutcome::Deny(_), _) => {}
            (_, deny @ HookOutcome::Deny(_)) => outcome = deny,
            (HookOutcome::Proceed, error @ HookOutcome::Error(_)) => outcome = error,
            _ => {}
        }
    }
    HookDryRunReport {
        ran: !invoked.is_empty(),
        outcome,
        invoked,
        revoked,
        no_match: false,
    }
}

async fn run_dry_run_command(
    command: String,
    event_name: &str,
    adapter: &dyn rightkit_sandbox::execution::ProcessLaunchAdapter,
    context: &rightkit_sandbox::execution::ProcessLaunchContext,
) -> HookOutcome {
    // Unit tests retain deterministic shell semantics when exercising the registry; daemon
    // callers always provide a required governed context.
    #[cfg(windows)]
    let (program, argv) = ("cmd".to_string(), vec!["/C".to_string(), command]);
    #[cfg(not(windows))]
    let (program, argv) = ("sh".to_string(), vec!["-c".to_string(), command]);
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program,
        argv,
        cwd: context.workspace_root.clone(),
        context: context.clone(),
        env: vec![(
            super::host_env_name("RIGHTKIT_HOOK_EVENT").to_string(),
            event_name.to_string(),
        )],
    };
    match adapter.spawn(spec).await {
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
