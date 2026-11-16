//! Names and switches the host supplies to the `coding` layer, each fixed once at start-up.
//!
//! The workspace state folder (`<workspace>/<state-dir>/handoff-results`,
//! `<repo>/<state-dir>/worktrees`) comes from `rightkit_agent_policy::set_state_dir_name`.
//! This module holds what is left: the domain that separates a host's stable task ids from every
//! other host's, the label of the roster heuristic in proof reports and the checkpoint resume
//! rule. Without a call each value is the one this crate always used.

use std::sync::OnceLock;

/// Domain string mixed into every [`crate::coding::executed_bakeoff::StableTaskId`], until the
/// host calls [`set_task_id_domain`].
pub const DEFAULT_TASK_ID_DOMAIN: &str = "rightkit-agent/r1b/task-id/v1";

/// Name the roster heuristic reports in roster proofs, until the host calls
/// [`set_roster_router_name`].
pub const DEFAULT_ROSTER_ROUTER_NAME: &str = "rightkit-agent-router (L2 roster heuristic)";

static TASK_ID_DOMAIN: OnceLock<String> = OnceLock::new();
static ROSTER_ROUTER_NAME: OnceLock<String> = OnceLock::new();
static SKELETON_VARIANT: OnceLock<rightkit_repo_graph::SkeletonVariant> = OnceLock::new();
static STRICT_CHECKPOINT_RESUME: OnceLock<bool> = OnceLock::new();

/// Sets the domain string of stable task ids (for example `myapp/r1b/task-id/v1`). Ids derived
/// under another domain differ, so a host that changes it invalidates ids it stored. Call once at
/// start-up; returns `false` (and changes nothing) when one was already set or `domain` is empty.
pub fn set_task_id_domain(domain: &str) -> bool {
    !domain.is_empty() && TASK_ID_DOMAIN.set(domain.to_string()).is_ok()
}

/// The stable task id domain in force.
pub fn task_id_domain() -> &'static str {
    TASK_ID_DOMAIN
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_TASK_ID_DOMAIN)
}

/// Sets the name the roster heuristic reports in roster proofs. Call once at start-up; returns
/// `false` (and changes nothing) when one was already set or `name` is empty.
pub fn set_roster_router_name(name: &str) -> bool {
    !name.is_empty() && ROSTER_ROUTER_NAME.set(name.to_string()).is_ok()
}

/// The roster heuristic name in force.
pub fn roster_router_name() -> &'static str {
    ROSTER_ROUTER_NAME
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_ROSTER_ROUTER_NAME)
}

/// Chooses how a saved checkpoint is applied to a planned graph. `false` (the default) replaces
/// every planned node with the saved one. `true` adopts a saved node only when it is `Done` and
/// the planned node has the same title, role, done-when and dependencies, and drops saved outputs
/// of nodes that did not resume, so a changed plan never inherits stale results. Call once at
/// start-up; returns `false` when one was already set.
pub fn set_strict_checkpoint_resume(strict: bool) -> bool {
    STRICT_CHECKPOINT_RESUME.set(strict).is_ok()
}

/// Whether strict checkpoint resume is in force.
pub fn strict_checkpoint_resume() -> bool {
    STRICT_CHECKPOINT_RESUME.get().copied().unwrap_or(false)
}

/// Chooses the skeletonizer that `handoff_brief` and `repo_map` use. [`SkeletonVariant::Standard`]
/// (the default) keeps the output this crate always produced; [`SkeletonVariant::Extended`]
/// selects the extended skeletonizer of `rightkit-repo-graph`. Call once at start-up; returns
/// `false` (and changes nothing) when one was already set.
///
/// [`SkeletonVariant::Standard`]: rightkit_repo_graph::SkeletonVariant::Standard
/// [`SkeletonVariant::Extended`]: rightkit_repo_graph::SkeletonVariant::Extended
pub fn set_skeleton_variant(variant: rightkit_repo_graph::SkeletonVariant) -> bool {
    SKELETON_VARIANT.set(variant).is_ok()
}

/// The skeletonizer variant in force.
pub fn skeleton_variant() -> rightkit_repo_graph::SkeletonVariant {
    SKELETON_VARIANT.get().copied().unwrap_or_default()
}
