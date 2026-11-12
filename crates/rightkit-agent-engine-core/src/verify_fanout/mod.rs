//! PL-007 — post-edit verification fan-out.
//!
//! After successful workspace mutation, run applicable bounded verifiers
//! concurrently (LSP diagnostics, typecheck, lint, format-check, static/AST,
//! dependency/config validation, cheap targeted tests) into one normalized
//! finding schema; dedupe, discard stale findings, enforce per-verifier &
//! aggregate budgets, keep *unavailable* distinct from *clean* and from
//! *failed*, surface actionable failures to the originating agent
//! pre-completion, and degrade cleanly.
//!
//! Action `ADAPT_TO_RUST` (`E→B`); donor: Pi (canon names the mechanism
//! family "Lens": change detection, concurrent diagnostics, turn-boundary
//! injection, stale-result handling), layered read-only over CG-B01
//! (`rightkit_agent_coding_tools::lsp_tool`) and FM-179 (`crate::post_edit` receipts).
//! Per-leg donor lineage is documented at the top of each submodule; the
//! lane WIRING report carries the full citation + the donor-search receipt.
//!
//! The engine's live post-dispatch gate (`engine_loop_dispatch.rs`) calls
//! [`run_verify_fanout`] after successful workspace mutation. It never runs a
//! completion decision itself —
//! it produces the typed evidence (`FanoutReport`, injection block) the gate
//! consumes.

mod budget;
pub mod collection;
pub mod finding;
pub mod injection;
pub mod pending;
pub mod profile;
pub mod staleness;
pub mod verifiers;

pub use budget::VerifyBudgets;
pub use collection::{collect_mutations, MutationCollection, ReceiptBinding, ReceiptBindingError};
pub use finding::{
    FindingCategory, FindingFixability, FindingRange, FindingSeverity, VerifiedFinding,
};
pub use injection::{
    actionable_findings, aggregate_status, has_required_gap, is_infrastructure_unverified,
    observation_fields, post_dispatch_action, render_followup_injection, required_gap_lines,
    uncovered_required_paths, PostDispatchAction,
};
pub use pending::PendingVerification;
pub use profile::{resolve_profile, VerificationProfile};
pub use verifiers::{
    default_command_verifiers, AggregateStatus, CommandVerifierSpec, VerifierId, VerifierOutcome,
    VerifierRequirement, VerifierStatus,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
use rightkit_agent_coding_tools::{ToolContext, ToolResult, WorkspaceMutation};

/// Sort, dedupe, and absolute-resolve a list of mutations the engine just
/// collected. This is the single seam where `path` strings become typed
/// `WorkspaceMutation`s the rest of the diagnostic pipeline reads. Read-only
/// turns never call this.
pub(crate) fn normalize_workspace_mutations(
    workspace_root: &Path,
    mutations: Vec<WorkspaceMutation>,
) -> Vec<WorkspaceMutation> {
    let mut normalized: Vec<WorkspaceMutation> = Vec::with_capacity(mutations.len());
    for m in mutations {
        let absolute = if m.path.is_absolute() {
            m.path.clone()
        } else {
            workspace_root.join(&m.path)
        };
        let canonical = absolute;
        if !normalized
            .iter()
            .any(|existing| existing.path == canonical && existing.kind == m.kind)
        {
            normalized.push(WorkspaceMutation {
                path: canonical,
                kind: m.kind,
                previous_content: m.previous_content,
            });
        }
    }
    normalized.sort_by(|a, b| a.path.cmp(&b.path));
    normalized
}

/// Tool-context inputs shared by every verifier unit.
#[derive(Clone, Debug)]
pub(crate) struct FanoutToolContext {
    pub session_id: String,
    pub workspace_root: PathBuf,
    pub permission_mode: PermissionMode,
    pub workspace_trust: WorkspaceTrust,
}

impl FanoutToolContext {
    pub(crate) fn tool_context(&self) -> ToolContext {
        ToolContext::new(
            self.session_id.clone(),
            self.workspace_root.clone(),
            self.permission_mode,
            self.workspace_trust,
        )
    }
}

/// Everything one fan-out run needs. `receipt_binding` selects the FM-179
/// leg (see `collection::ReceiptBinding`): `Enforced` fails closed on
/// unbound mutation evidence; `Unchecked` consumes the already-bound
/// mutation records the live Membrane path produces.
pub struct FanoutRequest<'a> {
    pub session_id: &'a str,
    pub workspace_root: &'a Path,
    pub permission_mode: PermissionMode,
    pub workspace_trust: WorkspaceTrust,
    /// The turn's tool results — the change-detection source (Pi's
    /// `turn_end` `toolResults` leg).
    pub turn_results: &'a HashMap<String, ToolResult>,
    pub receipt_binding: ReceiptBinding<'a>,
    /// Configured external verifiers (typecheck / lint / format-check /
    /// static / cheap targeted tests). Empty = LSP + structured-config only.
    pub command_verifiers: Vec<CommandVerifierSpec>,
    pub budgets: VerifyBudgets,
}

/// The complete output of one fan-out run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FanoutReport {
    /// Every path a turn mutation touched (all kinds, post-normalization).
    pub mutated_paths: Vec<PathBuf>,
    /// Paths handed to verifiers (created/modified/rename targets).
    pub verified_paths: Vec<PathBuf>,
    /// One outcome per verifier unit, in deterministic submission order.
    pub outcomes: Vec<VerifierOutcome>,
    /// The normalized, deduplicated, stale-discarded findings.
    pub findings: Vec<VerifiedFinding>,
    /// How many findings were dropped because their content hash no longer
    /// matches the file (Pi's stale-result handling leg).
    pub discarded_stale: usize,
    /// Non-empty only when receipt binding failed: the run was fenced and
    /// nothing was verified (fail closed per the FM-179 contract).
    pub binding_errors: Vec<ReceiptBindingError>,
    /// True when the verifier-unit budget forced units out of the run.
    pub budget_exhausted: bool,
}

impl FanoutReport {
    pub fn is_fenced(&self) -> bool {
        !self.binding_errors.is_empty()
    }
}

/// Collapse cross-verifier duplicates: same problem (path, range, category,
/// whitespace-normalized message) reported more than once is one finding.
/// The most severe occurrence wins; a content-hash binding is adopted from
/// whichever occurrence carries one.
fn dedupe_findings(findings: Vec<VerifiedFinding>) -> Vec<VerifiedFinding> {
    let mut kept: Vec<VerifiedFinding> = Vec::new();
    let mut index_by_key: HashMap<(PathBuf, Option<FindingRange>, FindingCategory, String), usize> =
        HashMap::new();
    for finding in findings {
        let key = finding.dedupe_key();
        match index_by_key.get(&key) {
            Some(&index) => {
                let existing = &mut kept[index];
                if finding.severity > existing.severity {
                    existing.severity = finding.severity;
                }
                if existing.content_hash.is_none() {
                    existing.content_hash = finding.content_hash.clone();
                }
                if matches!(existing.fixability, FindingFixability::Manual)
                    && !matches!(finding.fixability, FindingFixability::Manual)
                {
                    existing.fixability = finding.fixability;
                }
            }
            None => {
                index_by_key.insert(key, kept.len());
                kept.push(finding);
            }
        }
    }
    kept
}

/// Run the post-edit verification fan-out for one turn's mutations.
///
/// Order of operations (all legs of the canon row):
/// 1. Change detection over the turn's tool results, under the requested
///    receipt-binding policy; a binding failure fences the run (fail closed,
///    nothing verified, `FanoutReport::binding_errors` populated).
/// 2. Mutation normalization through the shared post-edit seam.
/// 3. Verifier units assembled by applicability and run concurrently under
///    the budgets (bounded per unit + aggregate; tri-state outcomes).
/// 4. Findings normalized into one schema, deduplicated, and bound to
///    content hashes; findings whose hash no longer matches the file are
///    discarded as stale.
pub async fn run_verify_fanout(request: FanoutRequest<'_>) -> FanoutReport {
    let collection = collect_mutations(request.turn_results, request.receipt_binding);
    let binding_errors = collection.binding_errors.clone();
    if collection.is_fenced() {
        return FanoutReport {
            binding_errors,
            ..FanoutReport::default()
        };
    }

    let normalized = normalize_workspace_mutations(request.workspace_root, collection.mutations);
    let mutated_paths: Vec<PathBuf> = normalized.iter().map(|m| m.path.clone()).collect();
    let verified_paths: Vec<PathBuf> = normalized
        .iter()
        .filter(|m| {
            matches!(
                m.kind,
                rightkit_agent_coding_tools::WorkspaceMutationKind::Created
                    | rightkit_agent_coding_tools::WorkspaceMutationKind::Modified
                    | rightkit_agent_coding_tools::WorkspaceMutationKind::Renamed { .. }
            )
        })
        .map(|m| m.path.clone())
        .collect();

    let tool_ctx = FanoutToolContext {
        session_id: request.session_id.to_string(),
        workspace_root: request.workspace_root.to_path_buf(),
        permission_mode: request.permission_mode,
        workspace_trust: request.workspace_trust,
    };
    // Read-only discovery preflight: resolve the project profile (strict
    // `<state-dir>/verification.json`, Python project roots/interpreters,
    // provisioned typecheckers) before any unit executes. A malformed config
    // becomes a required configuration failure below; it never silently
    // defaults.
    let profile = profile::resolve_profile(request.workspace_root, &verified_paths);
    let mut specs = request.command_verifiers;
    specs.extend(profile.commands.iter().cloned());
    let units = verifiers::build_units(
        &verified_paths,
        &tool_ctx,
        &specs,
        &profile,
        &request.budgets,
    );
    let mut outcomes = verifiers::fan_out_bounded(units, &request.budgets).await;
    if let Some(config_error) = &profile.config_error {
        outcomes.push(VerifierOutcome {
            verifier: VerifierId::profile(),
            subject: Some(profile::verification_config_path(request.workspace_root)),
            status: VerifierStatus::Failed {
                reason: config_error.clone(),
            },
            findings: Vec::new(),
            truncated: false,
            requirement: VerifierRequirement::Required,
            covered_paths: Vec::new(),
        });
    }

    let budget_exhausted = outcomes.iter().any(|o| o.verifier.0 == VerifierId::BUDGET);
    let raw_findings: Vec<VerifiedFinding> = outcomes
        .iter()
        .flat_map(|outcome| outcome.findings.iter().cloned())
        .collect();
    let deduplicated = dedupe_findings(raw_findings);
    let (findings, discarded_stale) = staleness::discard_stale_findings(deduplicated);

    FanoutReport {
        mutated_paths,
        verified_paths,
        outcomes,
        findings,
        discarded_stale,
        binding_errors,
        budget_exhausted,
    }
}
