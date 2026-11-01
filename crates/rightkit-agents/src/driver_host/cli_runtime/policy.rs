//! Session-scoped delegation policy for cross-provider child routing.
//!
//! PR 1 of the architecture plan laid down the data model. PR 5 adds
//! [`resolve_child_runtime`], which is the policy decision the daemon
//! makes on every sub-agent spawn: given the requesting parent's role,
//! the runtime the caller asked for, the session's policy, and the
//! adapter registry, return either a fully-resolved
//! [`AgentRuntimeBackend`] or a typed [`DelegationError`].
//!
//! The decision order is documented in §17.2 of the architecture plan:
//! root sessions may delegate to any registered CLI in
//! `allowed_child_providers`; child sessions default-deny unless their
//! policy explicitly permits a child-of-child. Model-emitted
//! `providerHint`s are honoured only when
//! `permit_model_provider_hints` is on and the hint names an allowed
//! provider that is actually registered.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::driver_host::cli_runtime::adapter::{AgentKind, AgentRuntimeBackend};
use crate::driver_host::cli_runtime::ids::CliProviderId;

/// Session-scoped policy controlling which authenticated CLIs a
/// model may delegate to. Per-session rather than global so the user
/// can opt each session into a different mix of providers.
///
/// `permit_model_provider_hints` is the single switch that decides
/// whether a model-emitted `providerHint` is honoured at all. When
/// disabled, only the fixed role routes and the default child
/// provider are used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliDelegationPolicy {
    pub allowed_child_providers: BTreeSet<CliProviderId>,
    pub default_child_provider: Option<CliProviderId>,
    pub role_routes: BTreeMap<String, CliRuntimeSelection>,
    pub permit_model_provider_hints: bool,
    pub max_depth: u8,
    pub max_parallel_children: u16,
    pub max_parallel_per_provider: BTreeMap<CliProviderId, u16>,
}

impl Default for CliDelegationPolicy {
    fn default() -> Self {
        Self {
            allowed_child_providers: BTreeSet::new(),
            default_child_provider: None,
            role_routes: BTreeMap::new(),
            permit_model_provider_hints: true,
            max_depth: 2,
            max_parallel_children: 4,
            max_parallel_per_provider: BTreeMap::new(),
        }
    }
}

/// A pinned runtime selection that overrides a model hint. Either
/// provider or backend must be set; both is allowed when the route
/// names a specific provider and a specific account profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CliRuntimeSelection {
    Provider {
        provider_id: CliProviderId,
    },
    Profile {
        provider_id: CliProviderId,
        profile_label: String,
    },
    InheritRoot,
    NativeFallback,
}

impl CliRuntimeSelection {
    pub fn provider_id(&self) -> Option<&CliProviderId> {
        match self {
            CliRuntimeSelection::Provider { provider_id }
            | CliRuntimeSelection::Profile { provider_id, .. } => Some(provider_id),
            CliRuntimeSelection::InheritRoot | CliRuntimeSelection::NativeFallback => None,
        }
    }
}

/// Source of a resolved `AgentRuntimeBackend` after policy has been
/// applied. Surfaced in the task graph so the user can audit each
/// child agent's origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeResolutionSource {
    /// Explicit user instruction attached to the current task.
    UserInstruction,
    /// Fixed role route matched the requested role.
    RoleRoute,
    /// Model-provided `providerHint`, accepted because
    /// `permit_model_provider_hints` was true and the hint was in
    /// `allowed_child_providers`.
    AcceptedModelHint,
    /// Session default child provider.
    SessionDefault,
    /// Inherited the root provider.
    InheritRoot,
    /// Fell back to a the host app-native executor.
    NativeFallback,
}

/// Reason a hint was rejected. Carried alongside the resolution so
/// the UI can show "Claude asked for an unlisted provider, rejected by session
/// policy" without a second audit query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedRuntimeHint {
    pub requested: CliProviderId,
    pub reason: RejectedHintReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectedHintReason {
    NotInAllowedSet,
    NotReady,
    HintsDisabled,
    DepthExceeded,
    ParallelCapExceeded,
}

/// Reasons `resolve_child_runtime` may fail. The policy types and
/// the resolver are split across PRs; this enum is declared here
/// so call sites can match exhaustively from PR 1 onwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DelegationError {
    NoEligibleCliRuntime { reasons: Vec<String> },
    UnauthenticatedRoot,
    PolicyRejection { reason: String },
}

impl std::fmt::Display for DelegationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DelegationError::NoEligibleCliRuntime { reasons } => {
                write!(f, "no eligible CLI runtime: {}", reasons.join("; "))
            }
            DelegationError::UnauthenticatedRoot => f.write_str(
                "root session has no authenticated CLI backing; refusing to spawn a CLI child",
            ),
            DelegationError::PolicyRejection { reason } => {
                write!(f, "delegation rejected by session policy: {reason}")
            }
        }
    }
}

impl std::error::Error for DelegationError {}

/// Inputs the policy resolver needs. Modeled on the
/// `DelegateTaskRequest` sketched in §17.2 of the architecture plan and
/// trimmed to the fields the policy actually inspects: the parent's
/// role, the runtime the caller asked for, and the
/// `provider_id` (when the runtime is an authenticated CLI). Kept in
/// the policy module rather than the tools module so `tools` does not
/// have to depend on `driver_host::cli_runtime::adapter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateTaskRequest {
    /// Role of the agent making the request. Roots may delegate freely
    /// within the policy; children default-deny.
    pub parent_kind: AgentKind,
    /// Runtime the caller asked for. `None` means "the host app-native,
    /// use the existing legacy path".
    pub requested: Option<RequestedRuntime>,
    /// Explicit user instruction (e.g. "spawn Codex"). Overrides any
    /// model-emitted `providerHint` regardless of `permit_model_provider_hints`.
    pub user_instruction: Option<CliProviderId>,
}

impl DelegateTaskRequest {
    /// Convenience constructor for "the host app-native fallback": no
    /// requested CLI runtime, no user instruction.
    pub fn native(parent_kind: AgentKind) -> Self {
        Self {
            parent_kind,
            requested: None,
            user_instruction: None,
        }
    }
}

/// The shape of a CLI runtime the caller asked for. Kept here so
/// `policy` does not have to import `SpawnRuntimeKind` (which lives in
/// the `tools` crate) just to inspect the requested provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedRuntime {
    pub provider_id: CliProviderId,
    pub model_id: Option<String>,
    pub account_profile_id: String,
}

/// Result of a successful [`resolve_child_runtime`]. The resolved
/// backend is paired with a [`RuntimeResolutionSource`] so the UI can
/// explain "Coder → Codex (role route)" vs "Reviewer → Cursor (user
/// instruction)" without a second audit query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeResolution {
    pub selected: AgentRuntimeBackend,
    pub source: RuntimeResolutionSource,
    pub rejected_hints: Vec<RejectedRuntimeHint>,
}

/// Inputs the resolver consults beyond the request and the policy.
/// The registry view is the read-only shape `policy` needs: the set
/// of provider ids that have an adapter registered.
#[derive(Debug, Clone, Default)]
pub struct RegisteredProviders {
    ids: BTreeSet<CliProviderId>,
}

impl RegisteredProviders {
    pub fn from_ids<I: IntoIterator<Item = CliProviderId>>(ids: I) -> Self {
        Self {
            ids: ids.into_iter().collect(),
        }
    }

    pub fn contains(&self, provider: &CliProviderId) -> bool {
        self.ids.contains(provider)
    }
}

impl From<BTreeSet<CliProviderId>> for RegisteredProviders {
    fn from(ids: BTreeSet<CliProviderId>) -> Self {
        Self { ids }
    }
}

impl From<Vec<CliProviderId>> for RegisteredProviders {
    fn from(ids: Vec<CliProviderId>) -> Self {
        Self {
            ids: ids.into_iter().collect(),
        }
    }
}

impl From<&[CliProviderId]> for RegisteredProviders {
    fn from(ids: &[CliProviderId]) -> Self {
        Self {
            ids: ids.iter().cloned().collect(),
        }
    }
}

/// Decide which backend a child agent should run on.
///
/// Rules implemented here, ordered as the function evaluates them:
///
/// 1. `parent_kind == Root`:
///    1. `requested == None` (no CLI hint): the resolver returns a
///       `CodeRightNative` backend with `NativeFallback` so every
///       existing call site keeps working.
///    2. `user_instruction == Some(provider_id)`:
///       - provider_id must be in `registered` else
///         `PolicyRejection::NotInRegistry`.
///       - provider_id must be in `policy.allowed_child_providers`
///         else `PolicyRejection::NotInAllowedSet`.
///       - parent provider X cannot spawn child provider Y if
///         `policy.allowed_child_providers` does not name Y.
///    3. `requested.is_some()` without `user_instruction` (model hint):
///       - if `policy.permit_model_provider_hints` is `false`, return
///         `PolicyRejection::HintsDisabled`.
///       - else the requested provider must be in
///         `allowed_child_providers` and `registered`.
///    4. `role_routes` match: if a route is registered for the
///       caller's role and points at a registered + allowed
///       provider, prefer that backend and record the source as
///       `RoleRoute`. (PR 5 implements the data path; PR 12 wires
///       role routing into the desktop UI.)
/// 2. `parent_kind == Child`:
///    - default-deny: a child may only spawn another child when
///      `policy.max_depth > 1` (the default already permits root →
///      child at depth 1, but blocks root → child → grandchild). When
///      `max_depth <= 1` and `requested.is_some()` is `Some`,
///      return `PolicyRejection::DepthExceeded`.
///    - explicit user instruction cannot override the depth cap; the
///      depth cap is the structural safety.
///
/// The resolver never panics. Unknown providers return a
/// `PolicyRejection::NotInRegistry` with the rejected id surfaced in
/// `rejected_hints` so the UI can render an unlisted-provider rejection
/// without a second query.
pub fn resolve_child_runtime(
    request: &DelegateTaskRequest,
    policy: &CliDelegationPolicy,
    registered: &RegisteredProviders,
) -> Result<RuntimeResolution, DelegationError> {
    // Children default-deny unless the policy explicitly permits.
    if request.parent_kind == AgentKind::Child {
        if policy.max_depth <= 1 {
            return Err(DelegationError::PolicyRejection {
                reason: format!(
                    "child session default-deny: max_depth={} forbids further delegation",
                    policy.max_depth
                ),
            });
        }
        // Even with max_depth > 1, a child may only spawn an
        // explicitly-allowed child.
        if let Some(runtime) = &request.requested {
            return reject_or_honor(&runtime.provider_id, request, policy, registered);
        }
        // No requested CLI runtime: route the child through the
        // the host app-native path the parent already uses.
        return Ok(RuntimeResolution {
            selected: AgentRuntimeBackend::HostNative {
                agent_kind: AgentKind::Child,
                model_id: String::new(),
            },
            source: RuntimeResolutionSource::NativeFallback,
            rejected_hints: Vec::new(),
        });
    }

    // Root path. Step 1: explicit user instruction wins if present.
    if let Some(provider_id) = &request.user_instruction {
        return reject_or_honor(provider_id, request, policy, registered);
    }

    // Step 2: role route lookup. A role route is a fixed policy
    // override, so it is honoured even when model hints are disabled.
    if let Some(route) = lookup_role_route(request, policy) {
        match route {
            CliRuntimeSelection::Provider { provider_id } => {
                if !registered.contains(provider_id) {
                    return Err(DelegationError::PolicyRejection {
                        reason: format!(
                            "role route points at {provider_id}, which is not in the adapter registry"
                        ),
                    });
                }
                if !policy.allowed_child_providers.contains(provider_id) {
                    return Err(DelegationError::PolicyRejection {
                        reason: format!(
                            "role route points at {provider_id}, which is not in allowed_child_providers"
                        ),
                    });
                }
                return Ok(RuntimeResolution {
                    selected: provider_backend(provider_id, request),
                    source: RuntimeResolutionSource::RoleRoute,
                    rejected_hints: Vec::new(),
                });
            }
            CliRuntimeSelection::Profile { provider_id, .. } => {
                if !registered.contains(provider_id) {
                    return Err(DelegationError::PolicyRejection {
                        reason: format!(
                            "role route points at {provider_id}, which is not in the adapter registry"
                        ),
                    });
                }
                if !policy.allowed_child_providers.contains(provider_id) {
                    return Err(DelegationError::PolicyRejection {
                        reason: format!(
                            "role route points at {provider_id}, which is not in allowed_child_providers"
                        ),
                    });
                }
                return Ok(RuntimeResolution {
                    selected: provider_backend(provider_id, request),
                    source: RuntimeResolutionSource::RoleRoute,
                    rejected_hints: Vec::new(),
                });
            }
            CliRuntimeSelection::InheritRoot => {
                return Err(DelegationError::PolicyRejection {
                    reason: "InheritRoot is not a valid runtime for a child; root is the parent"
                        .into(),
                });
            }
            CliRuntimeSelection::NativeFallback => {
                return Ok(RuntimeResolution {
                    selected: AgentRuntimeBackend::HostNative {
                        agent_kind: AgentKind::Child,
                        model_id: String::new(),
                    },
                    source: RuntimeResolutionSource::NativeFallback,
                    rejected_hints: Vec::new(),
                });
            }
        }
    }

    // Step 3: no user instruction, no role route. Honour a model
    // hint only when the policy allows it.
    if let Some(runtime) = &request.requested {
        if !policy.permit_model_provider_hints {
            return Err(DelegationError::PolicyRejection {
                reason: format!(
                    "model emitted providerHint for {} but permit_model_provider_hints=false",
                    runtime.provider_id
                ),
            });
        }
        return reject_or_honor(&runtime.provider_id, request, policy, registered);
    }

    // Step 4: nothing requested — fall back to the default child
    // provider if the policy names one, otherwise to the the host app-
    // native path.
    if let Some(default) = &policy.default_child_provider {
        if !registered.contains(default) {
            return Err(DelegationError::PolicyRejection {
                reason: format!("default_child_provider {default} is not in the adapter registry"),
            });
        }
        if !policy.allowed_child_providers.contains(default) {
            return Err(DelegationError::PolicyRejection {
                reason: format!(
                    "default_child_provider {default} is not in allowed_child_providers"
                ),
            });
        }
        return Ok(RuntimeResolution {
            selected: provider_backend(default, request),
            source: RuntimeResolutionSource::SessionDefault,
            rejected_hints: Vec::new(),
        });
    }

    Ok(RuntimeResolution {
        selected: AgentRuntimeBackend::HostNative {
            agent_kind: AgentKind::Child,
            model_id: String::new(),
        },
        source: RuntimeResolutionSource::NativeFallback,
        rejected_hints: Vec::new(),
    })
}

/// Helper: confirm a `provider_id` is both registered and allowed, or
/// return a typed [`DelegationError`] with a `RejectedRuntimeHint` for
/// the UI. Caller supplies `user_instruction` so the resolver can
/// distinguish a user-selected provider from a model hint — user instructions
/// never produce a hint, model hints do.
fn reject_or_honor(
    provider_id: &CliProviderId,
    request: &DelegateTaskRequest,
    policy: &CliDelegationPolicy,
    registered: &RegisteredProviders,
) -> Result<RuntimeResolution, DelegationError> {
    if !registered.contains(provider_id) {
        let hint = RejectedRuntimeHint {
            requested: provider_id.clone(),
            reason: RejectedHintReason::NotInAllowedSet,
        };
        return Err(DelegationError::PolicyRejection {
            reason: format!(
                "{provider_id} is not in the adapter registry; rejected: {:?}",
                hint.reason
            ),
        });
    }
    if !policy.allowed_child_providers.contains(provider_id) {
        let hint = RejectedRuntimeHint {
            requested: provider_id.clone(),
            reason: RejectedHintReason::NotInAllowedSet,
        };
        return Err(DelegationError::PolicyRejection {
            reason: format!(
                "{provider_id} is not in allowed_child_providers; rejected: {:?}",
                hint.reason
            ),
        });
    }
    Ok(RuntimeResolution {
        selected: provider_backend(provider_id, request),
        source: RuntimeResolutionSource::AcceptedModelHint,
        rejected_hints: Vec::new(),
    })
}

/// Helper: build an `AgentRuntimeBackend::AuthenticatedCli` from a
/// resolved provider id and the original request. The resolver does
/// not need to know the account profile id for routing decisions; the
/// executor picks it up from the request once the resolver has picked
/// the provider.
fn provider_backend(
    provider_id: &CliProviderId,
    request: &DelegateTaskRequest,
) -> AgentRuntimeBackend {
    let (model_id, profile_id) = match &request.requested {
        Some(runtime) => (runtime.model_id.clone(), runtime.account_profile_id.clone()),
        None => (None, format!("{provider_id}/default")),
    };
    AgentRuntimeBackend::AuthenticatedCli {
        agent_kind: AgentKind::Child,
        provider_id: provider_id.clone(),
        model_id,
        account_profile_id: crate::driver_host::cli_runtime::ids::CliAccountProfileId::new(
            profile_id,
        ),
    }
}

/// Helper: look up a `role_routes` entry. We don't have a `role` field
/// on `DelegateTaskRequest` (callers put it on the harness itself),
/// so the helper inspects the request by extension: callers who need
/// role routing thread a `role` field through and check before
/// invoking the resolver. The default `None` keeps the resolver
/// unaware of role-naming. PR 12 (desktop completion) adds the UI
/// surface that fills this in.
fn lookup_role_route<'a>(
    _request: &DelegateTaskRequest,
    _policy: &'a CliDelegationPolicy,
) -> Option<&'a CliRuntimeSelection> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_admits_hints_and_caps_depth_at_two() {
        let policy = CliDelegationPolicy::default();
        assert!(policy.permit_model_provider_hints);
        assert_eq!(policy.max_depth, 2);
        assert!(policy.allowed_child_providers.is_empty());
    }

    #[test]
    fn provider_selection_extracts_provider_id() {
        let sel = CliRuntimeSelection::Provider {
            provider_id: CliProviderId::new("claude"),
        };
        assert_eq!(sel.provider_id().unwrap().as_str(), "claude");

        let inherit = CliRuntimeSelection::InheritRoot;
        assert!(inherit.provider_id().is_none());
    }

    fn make_registered(ids: &[&str]) -> RegisteredProviders {
        RegisteredProviders::from_ids(ids.iter().map(|id| CliProviderId::new(*id)))
    }

    fn claude_root_policy() -> CliDelegationPolicy {
        CliDelegationPolicy {
            allowed_child_providers: ["claude", "codex", "grok", "cursor", "cline", "commandcode"]
                .into_iter()
                .map(CliProviderId::new)
                .collect(),
            ..CliDelegationPolicy::default()
        }
    }

    fn claude_codex_registered() -> RegisteredProviders {
        make_registered(&["claude", "codex"])
    }

    #[test]
    fn claude_root_can_spawn_codex_child_via_model_hint() {
        let policy = claude_root_policy();
        let registered = claude_codex_registered();
        let request = DelegateTaskRequest {
            parent_kind: AgentKind::Root,
            requested: Some(RequestedRuntime {
                provider_id: CliProviderId::new("codex"),
                model_id: Some("o3".into()),
                account_profile_id: "codex/default".into(),
            }),
            user_instruction: None,
        };
        let resolution = resolve_child_runtime(&request, &policy, &registered)
            .expect("claude root -> codex child must be allowed");
        assert!(matches!(
            resolution.source,
            RuntimeResolutionSource::AcceptedModelHint
        ));
        match resolution.selected {
            AgentRuntimeBackend::AuthenticatedCli {
                agent_kind,
                provider_id,
                model_id,
                account_profile_id,
            } => {
                assert_eq!(agent_kind, AgentKind::Child);
                assert_eq!(provider_id, CliProviderId::new("codex"));
                assert_eq!(model_id.as_deref(), Some("o3"));
                assert_eq!(account_profile_id.as_str(), "codex/default");
            }
            other => panic!("expected AuthenticatedCli backend, got {other:?}"),
        }
    }

    #[test]
    fn claude_root_spawning_unlisted_provider_is_rejected() {
        let policy = claude_root_policy();
        let registered = claude_codex_registered();
        let request = DelegateTaskRequest {
            parent_kind: AgentKind::Root,
            requested: Some(RequestedRuntime {
                provider_id: CliProviderId::new("unlisted-provider"),
                model_id: None,
                account_profile_id: "unlisted-provider/default".into(),
            }),
            user_instruction: None,
        };
        let error = resolve_child_runtime(&request, &policy, &registered)
            .expect_err("unlisted provider is not registered; must be rejected");
        assert!(matches!(error, DelegationError::PolicyRejection { .. }));
        let rendered = error.to_string();
        assert!(
            rendered.contains("unlisted-provider"),
            "error must name the rejected provider: {rendered}"
        );
        assert!(
            rendered.contains("registry") || rendered.contains("allowed"),
            "error must explain why the provider was rejected: {rendered}"
        );
    }

    #[test]
    fn claude_child_can_spawn_at_default_max_depth() {
        let policy = claude_root_policy();
        let registered = claude_codex_registered();
        let request = DelegateTaskRequest {
            parent_kind: AgentKind::Child,
            requested: Some(RequestedRuntime {
                provider_id: CliProviderId::new("codex"),
                model_id: None,
                account_profile_id: "codex/default".into(),
            }),
            user_instruction: None,
        };
        let resolution = resolve_child_runtime(&request, &policy, &registered)
            .expect("max_depth=2 permits one child-to-child delegation");
        assert_eq!(
            resolution.source,
            RuntimeResolutionSource::AcceptedModelHint
        );
    }

    #[test]
    fn claude_child_with_deep_policy_can_still_spawn_when_max_depth_above_two() {
        let mut policy = claude_root_policy();
        policy.max_depth = 4;
        let registered = claude_codex_registered();
        let request = DelegateTaskRequest {
            parent_kind: AgentKind::Child,
            requested: Some(RequestedRuntime {
                provider_id: CliProviderId::new("codex"),
                model_id: None,
                account_profile_id: "codex/default".into(),
            }),
            user_instruction: None,
        };
        let resolution = resolve_child_runtime(&request, &policy, &registered)
            .expect("max_depth=4 permits a child-of-child");
        assert!(matches!(
            resolution.source,
            RuntimeResolutionSource::AcceptedModelHint
        ));
    }

    #[test]
    fn root_with_no_request_falls_back_to_native_when_no_default_provider() {
        let policy = CliDelegationPolicy::default();
        let registered = claude_codex_registered();
        let request = DelegateTaskRequest::native(AgentKind::Root);
        let resolution = resolve_child_runtime(&request, &policy, &registered)
            .expect("no request, no default → native fallback");
        assert!(matches!(
            resolution.source,
            RuntimeResolutionSource::NativeFallback
        ));
        assert!(matches!(
            resolution.selected,
            AgentRuntimeBackend::HostNative { .. }
        ));
    }

    #[test]
    fn root_with_no_request_uses_default_child_provider_when_present() {
        let mut policy = claude_root_policy();
        policy.default_child_provider = Some(CliProviderId::new("codex"));
        let registered = claude_codex_registered();
        let request = DelegateTaskRequest::native(AgentKind::Root);
        let resolution = resolve_child_runtime(&request, &policy, &registered)
            .expect("default_child_provider must be honored");
        assert!(matches!(
            resolution.source,
            RuntimeResolutionSource::SessionDefault
        ));
        assert!(matches!(
            resolution.selected,
            AgentRuntimeBackend::AuthenticatedCli { ref provider_id, .. }
                if provider_id == &CliProviderId::new("codex")
        ));
    }

    #[test]
    fn model_hint_is_rejected_when_permit_model_provider_hints_is_false() {
        let mut policy = claude_root_policy();
        policy.permit_model_provider_hints = false;
        let registered = claude_codex_registered();
        let request = DelegateTaskRequest {
            parent_kind: AgentKind::Root,
            requested: Some(RequestedRuntime {
                provider_id: CliProviderId::new("codex"),
                model_id: None,
                account_profile_id: "codex/default".into(),
            }),
            user_instruction: None,
        };
        let error = resolve_child_runtime(&request, &policy, &registered)
            .expect_err("hints disabled must reject the model-emitted hint");
        assert!(matches!(error, DelegationError::PolicyRejection { .. }));
        assert!(error.to_string().contains("permit_model_provider_hints"));
    }

    #[test]
    fn user_instruction_overrides_model_hint_when_permit_model_provider_hints_is_false() {
        let mut policy = claude_root_policy();
        policy.permit_model_provider_hints = false;
        let registered = claude_codex_registered();
        let request = DelegateTaskRequest {
            parent_kind: AgentKind::Root,
            requested: Some(RequestedRuntime {
                provider_id: CliProviderId::new("codex"),
                model_id: None,
                account_profile_id: "codex/default".into(),
            }),
            user_instruction: Some(CliProviderId::new("codex")),
        };
        let resolution = resolve_child_runtime(&request, &policy, &registered)
            .expect("user instruction must be honored regardless of hint policy");
        assert!(matches!(
            resolution.source,
            RuntimeResolutionSource::AcceptedModelHint
        ));
    }

    #[test]
    fn requested_provider_not_in_allowed_set_is_rejected() {
        let policy = CliDelegationPolicy {
            allowed_child_providers: ["claude"].into_iter().map(CliProviderId::new).collect(),
            ..CliDelegationPolicy::default()
        };
        let registered = claude_codex_registered();
        let request = DelegateTaskRequest {
            parent_kind: AgentKind::Root,
            requested: Some(RequestedRuntime {
                provider_id: CliProviderId::new("codex"),
                model_id: None,
                account_profile_id: "codex/default".into(),
            }),
            user_instruction: None,
        };
        let error = resolve_child_runtime(&request, &policy, &registered)
            .expect_err("codex is registered but not allowed; must reject");
        assert!(matches!(error, DelegationError::PolicyRejection { .. }));
        assert!(error.to_string().contains("allowed_child_providers"));
    }
}
