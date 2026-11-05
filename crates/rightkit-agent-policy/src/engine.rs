use crate::capabilities::{is_file_write, is_read_only, CapabilityClass};
use crate::deny_globs::DenyGlobSet;
use crate::pdp::{PolicyDecisionPoint, PolicyRequest};
use crate::types::{PermissionDecision, PermissionMode, PermissionVerdict, WorkspaceTrust};

/// Outcome of one automated-review pass over a pending `Ask` decision
/// (FM-031).
///
/// `Defer` is not "no opinion, so allow" -- it is "no opinion, so keep
/// asking a human." There is no outcome variant that lets an absent,
/// erroring, or undecided reviewer produce a silent `Allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// The reviewer actively approves the pending action.
    Approve,
    /// The reviewer actively rejects the pending action.
    Reject,
    /// The reviewer could not reach a judgment. Always falls through to the
    /// original human-fallback `Ask` -- this is the fail-closed leg.
    Defer,
}

/// Automated review hook for prompt-class (`Ask`) permission decisions
/// (FM-031).
///
/// Port of san's `internal/agent/permission.go:22-42,147-155` `PermReviewFunc`
/// pattern: some `Ask`-tier decisions are first offered to an automated
/// reviewer so a routine, low-risk prompt can resolve without blocking on a
/// human, while anything the reviewer is unsure about (or any decision that
/// already resolved to `Deny`/`Allow` on its own terms) is untouched. The
/// contract is fail-closed by construction:
///
/// - Only `Ask` decisions are ever offered to the reviewer -- `Deny` is never
///   relaxed, and an already-silent `Allow` is never re-litigated.
/// - `ReviewOutcome::Defer` -- including "no reviewer attached" via
///   [`PermissionEngine::has_reviewer`] returning `false` -- always leaves
///   the decision at `Ask`, so a missing or indecisive reviewer degrades to
///   the same human-fallback prompt as if FM-031 had never been wired in.
/// - codex `core/src/guardian/` is the secondary reference for the review
///   lifecycle shape (offer, judge, fall through) this trait follows.
pub trait PermissionReviewer: Send + Sync {
    /// Judge one pending `Ask` decision. `target_path` is the same path the
    /// engine evaluated the deny-globs against, when the call has one.
    fn review(&self, cls: CapabilityClass, target_path: Option<&str>) -> ReviewOutcome;

    /// Correlate a review with the complete policy request when one exists.
    ///
    /// The legacy two-argument hook remains the required method so existing
    /// reviewers keep compiling. The default implementation forwards to it;
    /// reviewers that need request-level correlation (command, attributes,
    /// and tool identity) override this method. This preserves the fail-closed
    /// behavior of an omitted request or an undecided review.
    fn review_with_request(
        &self,
        cls: CapabilityClass,
        target_path: Option<&str>,
        request: Option<&PolicyRequest>,
    ) -> ReviewOutcome {
        let _ = request;
        self.review(cls, target_path)
    }
}

impl<F> PermissionReviewer for F
where
    F: Fn(CapabilityClass, Option<&str>) -> ReviewOutcome + Send + Sync,
{
    fn review(&self, cls: CapabilityClass, target_path: Option<&str>) -> ReviewOutcome {
        self(cls, target_path)
    }
}

/// Evaluates a tool call against policy (spec §13).
///
/// Combines the mode policy, a destructive-action escalation, the workspace-trust
/// clamp, and the path deny-globs — **most-restrictive wins**. Pure + deterministic.
///
/// ABAC layer (FM-026): when a [`PolicyDecisionPoint`] is attached via
/// [`Self::with_pdp`] / [`Self::set_pdp`], [`Self::evaluate_with_request`] consults
/// it **before** the mode policy and before any `RuleStore` (the `permission_rules`
/// seam at `engine.rs:166`). The PDP is first-match-wins via
/// [`PolicyDecisionPoint::from_yaml_like`]; a matching rule produces a verdict
/// with `policy_source: "abac:<rule-id>"`. Hard denials (`trust:locked`,
/// `deny-glob:*`) remain the backstop and are evaluated before ABAC so a
/// permissive ABAC rule cannot bypass a secret-path block.
pub struct PermissionEngine {
    deny: DenyGlobSet,
    pdp: Option<PolicyDecisionPoint>,
    reviewer: Option<Box<dyn PermissionReviewer>>,
}

impl PermissionEngine {
    /// Create a new engine with the default deny-glob lists.
    pub fn new() -> Self {
        Self {
            deny: DenyGlobSet::new(),
            pdp: None,
            reviewer: None,
        }
    }

    /// Create a new engine with a custom [`DenyGlobSet`].
    pub fn with_deny_globs(deny: DenyGlobSet) -> Self {
        Self {
            deny,
            pdp: None,
            reviewer: None,
        }
    }

    /// Create a new engine with an attached ABAC PDP.
    pub fn with_pdp(pdp: PolicyDecisionPoint) -> Self {
        Self {
            deny: DenyGlobSet::new(),
            pdp: Some(pdp),
            reviewer: None,
        }
    }

    /// Attach or replace the ABAC PDP on this engine.
    pub fn set_pdp(&mut self, pdp: PolicyDecisionPoint) {
        self.pdp = Some(pdp);
    }

    /// Remove any attached PDP.
    pub fn clear_pdp(&mut self) {
        self.pdp = None;
    }

    /// Borrow the attached PDP, if any.
    pub fn pdp(&self) -> Option<&PolicyDecisionPoint> {
        self.pdp.as_ref()
    }

    /// Attach or replace the automated review hook (FM-031).
    ///
    /// See [`PermissionReviewer`] for the fail-closed contract: attaching a
    /// reviewer can only ever resolve an `Ask` faster (to `Allow` or `Deny`),
    /// never relax a `Deny`, and `ReviewOutcome::Defer` always preserves the
    /// original human-fallback `Ask`.
    pub fn set_reviewer(&mut self, reviewer: impl PermissionReviewer + 'static) {
        self.reviewer = Some(Box::new(reviewer));
    }

    /// Remove any attached review hook. Decisions revert to unconditional
    /// human-fallback `Ask` -- the same fail-closed default as if no reviewer
    /// had ever been attached.
    pub fn clear_reviewer(&mut self) {
        self.reviewer = None;
    }

    /// Whether an automated review hook is currently attached.
    pub fn has_reviewer(&self) -> bool {
        self.reviewer.is_some()
    }

    /// Evaluate a tool call and return the verdict.
    ///
    /// This is the baseline evaluation without an ABAC request. When a PDP is
    /// attached, use [`Self::evaluate_with_request`] to consult ABAC before the
    /// mode policy (ABAC first, then `RuleStore` / mode). The baseline path is
    /// preserved for callers that do not have a [`PolicyRequest`].
    ///
    /// # Arguments
    /// - `cls` — the capability class of the tool being invoked
    /// - `mode` — current session permission mode
    /// - `trust` — current workspace trust level
    /// - `target_path` — the file/directory path the tool will access, if any
    /// - `destructive` — caller-supplied flag indicating the specific call is destructive
    pub fn evaluate(
        &self,
        cls: CapabilityClass,
        mode: PermissionMode,
        trust: WorkspaceTrust,
        target_path: Option<&str>,
        destructive: bool,
    ) -> PermissionVerdict {
        self.evaluate_with_request(None, cls, mode, trust, target_path, destructive)
    }

    /// Evaluate with an optional ABAC [`PolicyRequest`].
    ///
    /// When `request` is `Some` and a PDP is attached, the PDP is consulted
    /// **before** the mode policy (and thus before the `permission_rules`
    /// `RuleStore` seam at `engine.rs:166` which sits above this engine). This
    /// satisfies FM-026's invariant: *ABAC first, glob second; `policy_source:
    /// "abac:<rule-id>"`; `from_yaml_like` config; first-match-wins*.
    ///
    /// Hard denials (`trust:locked`, `deny-glob:*`) are still the backstop and
    /// are evaluated before ABAC so a permissive ABAC rule cannot launder a
    /// secret-path write.
    pub fn evaluate_with_request(
        &self,
        request: Option<&PolicyRequest>,
        cls: CapabilityClass,
        mode: PermissionMode,
        trust: WorkspaceTrust,
        target_path: Option<&str>,
        destructive: bool,
    ) -> PermissionVerdict {
        // 1. Locked workspace: nothing executes, not even reads.
        if trust == WorkspaceTrust::Locked {
            return PermissionVerdict::new(
                PermissionDecision::Deny,
                "workspace is locked — no tool execution",
                "trust:locked",
            );
        }

        // 2. Deny-globs are the hard backstop — a permissive mode must not override a
        //    secret/VCS path block.
        if let Some(path) = target_path.filter(|p| !p.is_empty()) {
            if is_file_write(cls) {
                if let Some(wm) = self.deny.first_write_match(path) {
                    return PermissionVerdict::new(
                        PermissionDecision::Deny,
                        format!("'{}' matches write deny-glob '{}'", path, wm),
                        "deny-glob:write",
                    );
                }
            }
            if is_read_only(cls) {
                if let Some(rm) = self.deny.first_read_match(path) {
                    return PermissionVerdict::new(
                        PermissionDecision::Deny,
                        format!("'{}' matches read deny-glob '{}'", path, rm),
                        "deny-glob:read",
                    );
                }
            }
        }

        // 2b. ABAC PDP — consult before mode / RuleStore seam (FM-026).
        // Hard denials above remain the backstop; ABAC is first-match-wins via
        // `from_yaml_like` and surfaces `policy_source: "abac:<rule-id>"`.
        if let Some(req) = request {
            if let Some(pdp) = &self.pdp {
                if let Some(rule) = pdp.matched_rule(req) {
                    let source = format!("abac:{}", rule.id);
                    let mut effect = rule.effect;
                    let mut reason = format!(
                        "{:?} by ABAC rule '{}' (tool '{}') -> {:?}",
                        cls, rule.id, rule.tool_match, rule.effect
                    );
                    // Rule 51: an ABAC Allow never relaxes the untrusted-workspace clamp. Only
                    // Bypass, the explicit opt-out, passes it.
                    if effect == PermissionDecision::Allow
                        && mode != PermissionMode::Bypass
                        && matches!(trust, WorkspaceTrust::Untrusted | WorkspaceTrust::Sandboxed)
                        && !is_read_only(cls)
                    {
                        effect = PermissionDecision::Ask;
                        reason.push_str(
                            "; the workspace is not trusted, so approval is still required",
                        );
                    }
                    return PermissionVerdict::new(effect, reason, source);
                }
            }
        }

        // 3. Base decision from the mode policy.
        let (mut decision, mut source) = mode_base(cls, mode);

        // 4. Destructive escalation: a dangerous op never gets a silent auto-allow
        //    outside bypass.
        if destructive && decision == PermissionDecision::Allow && mode != PermissionMode::Bypass {
            decision = PermissionDecision::Ask;
            source = "destructive:escalated";
        }

        // 4b. Test-file protection: a write to a test/spec file must never be
        //     silently auto-applied (an agent could otherwise rewrite tests to
        //     make them "pass"). Escalate any auto-allow to Ask outside Bypass.
        if is_file_write(cls)
            && decision == PermissionDecision::Allow
            && mode != PermissionMode::Bypass
        {
            if let Some(path) = target_path.filter(|p| !p.is_empty()) {
                if is_test_file(path) {
                    decision = PermissionDecision::Ask;
                    source = "test-file:protected";
                }
            }
        }

        // 5. Trust clamp: untrusted/sandboxed can only TIGHTEN — an effectful
        //    auto-allow becomes ask ("read-only until approved"). Trusted leaves the
        //    mode decision intact. Bypass is the explicit opt-out (sandbox/disposable
        //    / --yes), so it overrides the clamp.
        if mode != PermissionMode::Bypass
            && matches!(trust, WorkspaceTrust::Untrusted | WorkspaceTrust::Sandboxed)
            && !is_read_only(cls)
            && decision == PermissionDecision::Allow
        {
            decision = PermissionDecision::Ask;
            source = if trust == WorkspaceTrust::Untrusted {
                "trust:untrusted"
            } else {
                "trust:sandboxed"
            };
        }

        // 6. Automated review of prompt-class Ask decisions (FM-031). Only ever
        //    offered when the decision so far is `Ask` -- a `Deny` reached
        //    above (locked, deny-glob, ABAC) is terminal and never reaches the
        //    reviewer, and an `Allow` is never re-litigated either. `Defer`
        //    (including no reviewer attached) leaves `decision`/`source`
        //    untouched, so the fail-closed human-fallback Ask is always the
        //    default outcome.
        if decision == PermissionDecision::Ask {
            if let Some(reviewer) = &self.reviewer {
                match reviewer.review_with_request(cls, target_path, request) {
                    ReviewOutcome::Approve => {
                        decision = PermissionDecision::Allow;
                        source = "review:auto-approved";
                    }
                    ReviewOutcome::Reject => {
                        decision = PermissionDecision::Deny;
                        source = "review:auto-rejected";
                    }
                    ReviewOutcome::Defer => {}
                }
            }
        }

        PermissionVerdict::new(decision, describe(decision, cls, mode), source)
    }
}

impl Default for PermissionEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Heuristically identify test/spec files across common ecosystems. Used to
/// gate silent writes — editing tests should always be a deliberate, approved
/// act, not something an autonomous agent does to turn a run green.
pub fn is_test_file(path: &str) -> bool {
    let p = path.replace('\\', "/").to_lowercase();
    if p.contains("/tests/")
        || p.contains("/__tests__/")
        || p.contains("/spec/")
        || p.starts_with("tests/")
        || p.starts_with("test/")
        || p.starts_with("spec/")
    {
        return true;
    }
    let name = p.rsplit('/').next().unwrap_or(&p);
    name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.rs")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.ends_with("_test.ts")
        || name.ends_with("_test.js")
        || name.ends_with("_spec.rb")
        || name.ends_with("_spec.ts")
        || name.ends_with("_spec.js")
        || name.starts_with("test_")
}

/// Compute the base (pre-escalation) decision from mode + capability class.
fn mode_base(cls: CapabilityClass, mode: PermissionMode) -> (PermissionDecision, &'static str) {
    if is_read_only(cls) {
        return (PermissionDecision::Allow, "mode:read-only");
    }

    match mode {
        // Plan: reasons + reads only; network may ask, everything effectful is denied.
        PermissionMode::Plan => {
            if cls == CapabilityClass::Network {
                (PermissionDecision::Ask, "mode:plan")
            } else {
                (PermissionDecision::Deny, "mode:plan")
            }
        }

        PermissionMode::Ask => (PermissionDecision::Ask, "mode:ask"),

        // Accept-edits: file writes auto-apply; shell / network / mcp / etc. still ask.
        PermissionMode::AcceptEdits => {
            if is_file_write(cls) {
                (PermissionDecision::Allow, "mode:accept-edits")
            } else {
                (PermissionDecision::Ask, "mode:accept-edits")
            }
        }

        // Auto-safe: non-destructive mutations auto-apply; destructive / process-control /
        // browser ask. (Shell carries a per-call destructive flag, so it escalates to ask
        // in step 4.)
        PermissionMode::AutoSafe => {
            if matches!(
                cls,
                CapabilityClass::Destructive
                    | CapabilityClass::ProcessControl
                    | CapabilityClass::Browser
                    | CapabilityClass::Media
            ) {
                (PermissionDecision::Ask, "mode:auto-safe")
            } else {
                (PermissionDecision::Allow, "mode:auto-safe")
            }
        }

        PermissionMode::Bypass => (PermissionDecision::Allow, "mode:bypass"),
    }
}

/// Build a human-readable reason string.
fn describe(decision: PermissionDecision, cls: CapabilityClass, mode: PermissionMode) -> String {
    match decision {
        PermissionDecision::Allow => format!("{:?} allowed in {:?} mode", cls, mode),
        PermissionDecision::Ask => format!("{:?} needs approval in {:?} mode", cls, mode),
        PermissionDecision::Deny => format!("{:?} denied in {:?} mode", cls, mode),
    }
}
