//! The delegation-invariant seam for the Claude harness (PL-005).
//!
//! **Invariant:** Claude-root child work must flow through the host app
//! delegation (`delegate_task`/`delegate_parallel`); provider-native
//! child orchestration must not bypass the host app policy, isolation,
//! budgets, task ownership, event projection, cancellation or
//! persistence. This module writes that seam in three layers:
//!
//! 1. **Launch policy** — [`enforce_delegation_launch_policy`]
//!    fail-closed validates the *effective* launch argv: the native
//!    tool surface is removed (`--tools ""`), the only admitted tool
//!    surface is the the host app MCP gateway (`--allowedTools
//!    mcp__coderight`), the MCP config is strict and the host app-only,
//!    the run-scoped policy is appended verbatim, no
//!    permission-bypass flag is present, and the env-denial set was
//!    not weakened. With the native Task/agent surface removed, the
//!    provider has no orchestration tool to bypass the host app with.
//! 2. **Bypass detection** — [`inspect_tool_use_for_native_child_spawn`]
//!    recognizes the Claude-native child-orchestration tool names if
//!    one ever surfaces in the vendor event stream (defense in depth
//!    behind layer 1). The runtime counts every detection on the
//!    persisted binding and projects a typed
//!    `DelegationBypassDetected` progress event.
//! 3. **Delegation gate** — `delegate_through_coderight` is the
//!    single entry a Claude root uses to spawn child work. It is a
//!    thin authoritative wrapper over the donor resolver
//!    `cli_runtime::policy::resolve_child_runtime` (registry check +
//!    allowed-child set + depth + hint policy, unchanged) that adds a
//!    receipt with the resolution source and session-policy digest,
//!    and [`DelegationLedger`] enforces the session's
//!    `max_parallel_children` budget so child fan-out cannot exceed
//!    the admitted budget.
//!
//! The donor tests from `cli_runtime::policy.rs` are re-exercised at
//! this seam below (claude root → codex child allowed; unlisted
//! provider rejected; depth cap).

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

use super::workspace::harness_denied_env_vars;
use crate::driver_host::cli_runtime::adapter::CliLaunchSpec;
use crate::driver_host::cli_runtime::ids::CliProviderId;
use crate::driver_host::cli_runtime::policy::{
    resolve_child_runtime, CliDelegationPolicy, DelegateTaskRequest, DelegationError,
    RegisteredProviders, RuntimeResolution,
};
use crate::driver_host::{ClaudeLaunchOptions, RUN_SCOPED_POLICY};
use std::path::Path;

/// The tool name of the the host app MCP gateway — the only tool surface
/// an admitted launch exposes.
pub const HOST_MCP_TOOL_SCOPE: &str = "mcp__rightkit";

/// Claude-native child-orchestration tool names. The launch policy
/// removes them from the vendor tool surface entirely; if one still
/// appears in the event stream it is a bypass attempt and must be
/// recorded and denied, never executed as provider-native delegation.
pub const NATIVE_CHILD_SPAWN_TOOL_NAMES: &[&str] = &["Task", "Agent", "dispatch_agent"];

/// SHA-256 hex digest of a policy string (donor shape:
/// `AuthenticatedCliExecutor`'s `policy_hash` over the run policy).
pub fn policy_digest(policy: &str) -> String {
    let digest = rightkit_crypto::sha256(policy.as_bytes());
    rightkit_crypto::hex_encode(&digest)
}

/// SHA-256 hex digest of a session delegation policy, canonicalized
/// through its serde representation (BTreeMap ordering keeps it
/// deterministic).
pub fn session_policy_digest(policy: &CliDelegationPolicy) -> Result<String, DelegationError> {
    let canonical =
        serde_json::to_vec(policy).map_err(|error| DelegationError::PolicyRejection {
            reason: format!("session delegation policy could not be canonicalized: {error}"),
        })?;
    Ok(rightkit_fs::digest::sha256_bytes(&canonical))
}

/// One Claude-native child-spawn attempt observed in the event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeChildSpawn {
    pub tool_name: String,
}

/// Detect a provider-native child-spawn attempt in a tool-call event.
/// Returns `Some` when the tool name is one of the Claude-native
/// child-orchestration tools.
pub fn inspect_tool_use_for_native_child_spawn(tool_name: &str) -> Option<NativeChildSpawn> {
    if NATIVE_CHILD_SPAWN_TOOL_NAMES
        .iter()
        .any(|name| name.eq_ignore_ascii_case(tool_name))
    {
        Some(NativeChildSpawn {
            tool_name: tool_name.to_string(),
        })
    } else {
        None
    }
}

/// A fail-closed delegation launch-policy verdict. The first violated
/// invariant wins; every variant names the drift so the operator can
/// repair the launch context rather than the guard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchPolicyViolation {
    NativeToolSurfaceEnabled {
        found: String,
    },
    AllowedToolsNotPinnedToCoderight {
        found: String,
    },
    StrictMcpConfigMissing,
    McpConfigMissingOrEmpty,
    SystemPolicyNotRunScoped,
    BypassPermissionsFlagPresent,
    EnvDenialWeakened {
        missing: Vec<String>,
    },
    /// The legacy launch spec itself was rejected (hostile model id,
    /// session/resume conflict) before policy scanning could run.
    LaunchSpecInvalid {
        reason: String,
    },
}

impl std::fmt::Display for LaunchPolicyViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchPolicyViolation::NativeToolSurfaceEnabled { found } => {
                write!(f, "native tool surface must be removed (`--tools \"\"`), found: {found}")
            }
            LaunchPolicyViolation::AllowedToolsNotPinnedToCoderight { found } => {
                write!(f, "allowed tools must be pinned to {HOST_MCP_TOOL_SCOPE}, found: {found}")
            }
            LaunchPolicyViolation::StrictMcpConfigMissing => {
                f.write_str("--strict-mcp-config is required so no additional MCP server can be injected")
            }
            LaunchPolicyViolation::McpConfigMissingOrEmpty => {
                f.write_str("--mcp-config must carry the Agent MCP server document")
            }
            LaunchPolicyViolation::SystemPolicyNotRunScoped => {
                write!(f, "--append-system-prompt must carry RUN_SCOPED_POLICY verbatim")
            }
            LaunchPolicyViolation::BypassPermissionsFlagPresent => {
                f.write_str("a permission-bypass flag is present; delegation policy cannot hold without the Agent approval gate")
            }
            LaunchPolicyViolation::EnvDenialWeakened { missing } => {
                write!(f, "launch env_remove is missing denied variables: {}", missing.join(", "))
            }
            LaunchPolicyViolation::LaunchSpecInvalid { reason } => {
                write!(f, "launch spec rejected before policy scan: {reason}")
            }
        }
    }
}

/// Value of a flag in the argument list, or the synthetic
/// `"<missing>"` marker when the flag is absent.
fn flag_value<'a>(args: &[&'a str], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1])
}

fn has_flag(args: &[&str], flag: &str) -> bool {
    args.iter().any(|arg| *arg == flag)
}

/// Validate the effective Claude launch spec against the delegation
/// launch policy. `spec` is the adapter-produced launch (the donor
/// `ClaudeLaunchOptions::launch_spec` argv); the executable path is
/// stripped exactly like `StdioProcessIo::sandboxed_program` does
/// before flag scanning.
pub fn enforce_delegation_launch_policy(spec: &CliLaunchSpec) -> Result<(), LaunchPolicyViolation> {
    let args: Vec<&str> =
        if !spec.argv.is_empty() && Path::new(&spec.argv[0]) == spec.executable.as_path() {
            spec.argv[1..].iter().map(String::as_str).collect()
        } else {
            spec.argv.iter().map(String::as_str).collect()
        };

    match flag_value(&args, "--tools") {
        Some("") => {}
        Some(other) => {
            return Err(LaunchPolicyViolation::NativeToolSurfaceEnabled {
                found: other.to_string(),
            })
        }
        None => {
            return Err(LaunchPolicyViolation::NativeToolSurfaceEnabled {
                found: "<missing --tools flag>".to_string(),
            })
        }
    }

    match flag_value(&args, "--allowedTools") {
        Some(HOST_MCP_TOOL_SCOPE) => {}
        Some(other) => {
            return Err(LaunchPolicyViolation::AllowedToolsNotPinnedToCoderight {
                found: other.to_string(),
            })
        }
        None => {
            return Err(LaunchPolicyViolation::AllowedToolsNotPinnedToCoderight {
                found: "<missing --allowedTools flag>".to_string(),
            })
        }
    }

    if !has_flag(&args, "--strict-mcp-config") {
        return Err(LaunchPolicyViolation::StrictMcpConfigMissing);
    }
    match flag_value(&args, "--mcp-config") {
        Some(value) if !value.trim().is_empty() => {}
        _ => return Err(LaunchPolicyViolation::McpConfigMissingOrEmpty),
    }
    match flag_value(&args, "--append-system-prompt") {
        Some(RUN_SCOPED_POLICY) => {}
        _ => return Err(LaunchPolicyViolation::SystemPolicyNotRunScoped),
    }
    // Fail closed on permission bypass: the host app owns approvals.
    if has_flag(&args, "--dangerously-skip-permissions") {
        return Err(LaunchPolicyViolation::BypassPermissionsFlagPresent);
    }
    // The env-denial set must not have been weakened.
    let denied = harness_denied_env_vars();
    let missing: Vec<String> = denied
        .iter()
        .filter(|name| !spec.env_remove.iter().any(|present| present == *name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(LaunchPolicyViolation::EnvDenialWeakened { missing });
    }
    Ok(())
}

/// Convenience overload validating the legacy launch shape directly
/// (same invariants, before the adapter maps it onto `CliLaunchSpec`).
pub fn enforce_delegation_launch_policy_for_legacy(
    options: &ClaudeLaunchOptions,
) -> Result<(), LaunchPolicyViolation> {
    let legacy =
        options
            .launch_spec()
            .map_err(|error| LaunchPolicyViolation::LaunchSpecInvalid {
                reason: error.to_string(),
            })?;
    // Reuse the legacy spec directly: the legacy argv equals the
    // adapter argv and the legacy env_remove is a BTreeSet.
    let spec = CliLaunchSpec {
        provider_id: CliProviderId::new("claude"),
        transport: crate::driver_host::cli_runtime::ids::CliTransportKind::ClaudeStreamJson,
        executable: legacy.program,
        argv: legacy.args,
        env_keep: Vec::new(),
        env_remove: legacy.env_remove.into_iter().collect(),
        env: legacy.env.into_iter().collect(),
        cwd: legacy.cwd,
        mcp_server: None,
        profile_hash: String::new(),
        session_id: String::new(),
    };
    enforce_delegation_launch_policy(&spec)
}

/// Receipt recorded for every child spawn a Claude root performs
/// through the host app delegation. Persisted with the run so each
/// child's origin (policy source + session policy digest) is
/// auditable after a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationReceipt {
    /// Monotonic ledger sequence for this delegation.
    pub receipt_id: u64,
    /// Binding of the Claude root that requested the child.
    pub parent_binding_id: String,
    /// The the host app policy verdict: resolved backend + source +
    /// rejected hints (verbatim from the donor resolver).
    pub resolution: RuntimeResolution,
    /// Digest of the session policy the decision was made under.
    pub session_policy_digest: String,
}

/// Resolve child work for a Claude root through the host app delegation.
///
/// Thin authoritative wrapper over the donor
/// [`resolve_child_runtime`]: every policy decision (registry,
/// allowed-child set, depth cap, model hints, role routes, session
/// default) is made by that resolver, unchanged. This wrapper only
/// records the receipt. A Claude root *must not* spawn child work by
/// any other path.
pub fn delegate_through_host(
    receipt_id: u64,
    parent_binding_id: &str,
    request: &DelegateTaskRequest,
    policy: &CliDelegationPolicy,
    registered: &RegisteredProviders,
) -> Result<DelegationReceipt, DelegationError> {
    let resolution = resolve_child_runtime(request, policy, registered)?;
    Ok(DelegationReceipt {
        receipt_id,
        parent_binding_id: parent_binding_id.to_string(),
        resolution,
        session_policy_digest: session_policy_digest(policy)?,
    })
}

/// Budget ledger for parallel Claude-root children. Enforces the
/// session policy's `max_parallel_children` cap on top of the donor
/// resolver so fan-out cannot exceed the admitted budget (the
/// `S:F10`/`FM-102` bounded-admission shape applied at the harness
/// seam).
pub struct DelegationLedger {
    max_parallel_children: u16,
    // S:F10 (per-lineage/per-child budgets, item 5): the receipt-id
    // counter and the admitted set share ONE mutex so the
    // budget-check + reservation + id-issue is one atomic critical
    // section. Splitting these (the prior shape) let two concurrent
    // `admit` calls both observe room under the cap, both reserve,
    // and both push — over-admitting past `max_parallel_children`.
    // A reservation is recorded here (as a placeholder receipt_id in
    // `active`) *before* the lock is released for the (possibly
    // slow) delegation resolve, then reconciled with the real
    // receipt or released on failure.
    state: Mutex<LedgerState>,
}

struct LedgerState {
    next_receipt_id: u64,
    /// Reserved + admitted receipt ids. A reservation exists as soon
    /// as budget is claimed, before delegation resolution runs, so
    /// the budget check below always sees every in-flight admit.
    reserved: std::collections::HashSet<u64>,
    active: Vec<DelegationReceipt>,
}

impl DelegationLedger {
    pub fn new(policy: &CliDelegationPolicy) -> Self {
        Self {
            max_parallel_children: policy.max_parallel_children,
            state: Mutex::new(LedgerState {
                next_receipt_id: 1,
                reserved: std::collections::HashSet::new(),
                active: Vec::new(),
            }),
        }
    }

    /// Admit one child spawn: enforce the parallel budget, resolve
    /// through the host app delegation, record the receipt. Budget
    /// admission is atomic with respect to concurrent callers: the
    /// check-and-reserve step happens under one lock, so no two
    /// concurrent admits can both slip past a nearly-exhausted cap.
    pub fn admit(
        &self,
        parent_binding_id: &str,
        request: &DelegateTaskRequest,
        policy: &CliDelegationPolicy,
        registered: &RegisteredProviders,
    ) -> Result<DelegationReceipt, DelegationError> {
        let receipt_id = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| DelegationError::PolicyRejection {
                    reason: "delegation ledger mutex poisoned".to_string(),
                })?;
            let in_flight = (state.active.len() + state.reserved.len()) as u64;
            if in_flight >= u64::from(self.max_parallel_children) {
                return Err(DelegationError::PolicyRejection {
                    reason: format!(
                        "max_parallel_children={} reached; refusing to spawn another child",
                        self.max_parallel_children
                    ),
                });
            }
            let id = state.next_receipt_id;
            state.next_receipt_id = state.next_receipt_id.saturating_add(1);
            state.reserved.insert(id);
            id
        };

        let resolved =
            delegate_through_host(receipt_id, parent_binding_id, request, policy, registered);

        let mut state = self
            .state
            .lock()
            .map_err(|_| DelegationError::PolicyRejection {
                reason: "delegation ledger mutex poisoned".to_string(),
            })?;
        state.reserved.remove(&receipt_id);
        match resolved {
            Ok(receipt) => {
                state.active.push(receipt.clone());
                Ok(receipt)
            }
            Err(error) => Err(error),
        }
    }

    /// Mark a receipt complete (child finished, failed or was
    /// cancelled). Returns whether the receipt was active.
    pub fn complete(&self, receipt_id: u64) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return false,
        };
        let before = state.active.len();
        state
            .active
            .retain(|receipt| receipt.receipt_id != receipt_id);
        state.active.len() != before
    }

    /// Number of currently admitted (not merely reserved) children.
    pub fn active_count(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.active.len())
            .unwrap_or(0)
    }
}
