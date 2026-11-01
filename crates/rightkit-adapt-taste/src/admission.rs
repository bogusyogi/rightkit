//! Gate 1 — Adapt proposal eligibility.
//!
//! Decides whether evidence may form a Taste candidate. Single-source
//! admission policy: category taxonomy, rule shape, duplicate detection via
//! scoped identity, deterministic safety quarantine (origin, authority
//! effects, contradictions), and versioned policy bans. Passing this gate
//! grants no authority at the Cortex or context gates.

use crate::authority::{self, AuthorityEffect, AuthorityResult, StoredRule};
use crate::canonical::{canonical_object, normalize_text, sha256_canonical, sha256_hex};
use crate::model_boundary::{ModelExtractionProposal, ModelProposalError};
use crate::record::{normalize_category, PreferenceRecordV1, RecordClass, RuleKey};
use crate::scope::ScopeDimensions;
use std::collections::BTreeMap;

/// Minimal imperative-starter set for durable sentence shape.
const IMPERATIVE_STARTERS: &[&str] = &[
    "always", "never", "use", "prefer", "run", "avoid", "stop", "do", "ensure", "require", "must",
    "should", "keep", "check", "verify", "commit", "write", "read", "apply", "follow", "skip",
    "limit", "default",
];
const MIN_RULE_CHARS: usize = 15;

fn explicit_label_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static VALUE: OnceLock<regex::Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        regex::Regex::new(r"(?i)^\s*(?:decision|locked|constraint|invariant|rule)\s*:")
            .expect("static Gate 1 label expression")
    })
}

fn explicit_temporal_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static VALUE: OnceLock<regex::Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        regex::Regex::new(r"(?i)^\s*(?:going forward|from now on|henceforth)\b[\s,:.!-]*")
            .expect("static Gate 1 temporal expression")
    })
}

fn explicit_correction_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static VALUE: OnceLock<regex::Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)^\s*(?:no,?\s+that's\s+(?:wrong|not right|not what|not how)|wrong|incorrect|correction\s*:)[\s,:.!-]*",
        )
        .expect("static Gate 1 correction expression")
    })
}

fn polite_correction_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static VALUE: OnceLock<regex::Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        regex::Regex::new(r"(?i)^\s*please\s+stop\b")
            .expect("static Gate 1 polite correction expression")
    })
}

fn explicit_label_body_matches(body: &str, evidence: &str) -> bool {
    let normalized_body = normalize_text(body);
    if normalized_body.chars().count() < MIN_RULE_CHARS {
        return false;
    }
    let normalized_evidence = normalize_text(evidence);
    if !explicit_label_re().is_match(&normalized_evidence) {
        return false;
    }
    let remainder = explicit_label_re()
        .replace(&normalized_evidence, "")
        .to_string();
    !remainder.trim().is_empty() && normalized_body == normalize_text(&remainder)
}

fn first_person_preference_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static VALUE: OnceLock<regex::Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        regex::Regex::new(r"(?i)^\s*i\s+(?:always|never|prefer|avoid|require|must|should|like)\b")
            .expect("static Gate 1 first-person preference expression")
    })
}

pub const GATE1_POLICY_CONTRACT: &str = "adapt.gate1-policy.v1";
pub const GATE1_EXECUTABLE_POLICY_FINGERPRINT_CONTRACT: &str =
    "adapt.gate1-executable-policy-fingerprint.v1";

/// Native policy bans are defined beside the eligibility function that
/// executes them, so the executable-policy fingerprint cannot omit a second
/// policy source maintained elsewhere.
pub const NATIVE_GATE1_POLICY_BANS: &[(&str, &str)] = &[
    (
        "policy-banned:instruction-precedence",
        r"(?i)\b(?:ignore|bypass|override)\b.{0,48}\b(?:system instructions?|policy|safety rules?)\b",
    ),
    (
        "policy-banned:secret-exfiltration",
        r"(?i)\b(?:exfiltrate|leak|upload|publish)\b.{0,48}\b(?:secrets?|credentials?|api[- ]?keys?|tokens?)\b",
    ),
];

/// Compile the exact ban definitions covered by the executable-policy
/// fingerprint. Callers must not construct an independent Gate 1 ban list.
pub fn native_gate1_policy_bans() -> Vec<(String, regex::Regex)> {
    compile_gate1_policy_bans(NATIVE_GATE1_POLICY_BANS)
}

pub(crate) fn compile_gate1_policy_bans(
    definitions: &[(&str, &str)],
) -> Vec<(String, regex::Regex)> {
    definitions
        .iter()
        .map(|(reason, pattern)| {
            (
                (*reason).to_string(),
                regex::Regex::new(pattern).expect("native Gate 1 policy regex compiles"),
            )
        })
        .collect()
}

/// Fingerprint the complete source surface that can alter Gate 1 eligibility.
///
/// This deliberately hashes the executable Rust sources instead of copying a
/// parallel policy manifest that could drift. The admission coordinator covers
/// action order, rule-shape rules and bans; its dependencies cover taxonomy,
/// class/identity, scope normalization, origin/effect classification,
/// contradiction semantics, candidate integrity, manifest validation, and the
/// canonical text operations those rules execute. Line endings are normalized
/// so equivalent checkouts agree.
pub fn executable_gate1_policy_sha256() -> String {
    executable_gate1_policy_sha256_for_sources(&[
        ("admission.rs", include_str!("admission.rs")),
        ("authority.rs", include_str!("authority.rs")),
        ("canonical.rs", include_str!("canonical.rs")),
        ("manifest.rs", include_str!("manifest.rs")),
        ("proposal.rs", include_str!("proposal.rs")),
        ("record.rs", include_str!("record.rs")),
        ("scope.rs", include_str!("scope.rs")),
        ("taste.rs", include_str!("taste.rs")),
    ])
}

fn executable_gate1_policy_sha256_for_sources(sources: &[(&str, &str)]) -> String {
    let mut source_digests = sources
        .iter()
        .map(|(name, source)| {
            let normalized = source.replace("\r\n", "\n");
            (
                (*name).to_string(),
                canonical_object([
                    ("name", serde_json::Value::String((*name).to_string())),
                    (
                        "sha256",
                        serde_json::Value::String(sha256_hex(normalized.as_bytes())),
                    ),
                ]),
            )
        })
        .collect::<Vec<_>>();
    source_digests.sort_by(|left, right| left.0.cmp(&right.0));
    sha256_canonical(&canonical_object([
        (
            "contract",
            serde_json::Value::String(GATE1_EXECUTABLE_POLICY_FINGERPRINT_CONTRACT.into()),
        ),
        (
            "sources",
            serde_json::Value::Array(
                source_digests
                    .into_iter()
                    .map(|(_, digest)| digest)
                    .collect(),
            ),
        ),
    ]))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EligibilityDecision {
    Admitted,
    Refused { reason: String },
}

impl EligibilityDecision {
    pub fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted)
    }
}

/// Require a durable sentence shape without a brittle word-count gate.
pub fn rule_shape_valid(body: &str) -> bool {
    let normalized = normalize_text(body);
    if normalized.chars().count() < MIN_RULE_CHARS {
        return false;
    }
    let first_word: String = normalized
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches("'t")
        .to_string();
    if IMPERATIVE_STARTERS.contains(&first_word.as_str()) {
        return true;
    }
    if first_person_preference_re().is_match(&normalized) {
        return true;
    }

    // Labels, temporal markers, and corrections make the directive explicit;
    // validate the text following their prefix so a modal in a factual clause
    // cannot satisfy Gate 1 by itself.
    let remainder = if explicit_label_re().is_match(&normalized) {
        explicit_label_re().replace(&normalized, "").to_string()
    } else if explicit_temporal_re().is_match(&normalized) {
        explicit_temporal_re().replace(&normalized, "").to_string()
    } else if explicit_correction_re().is_match(&normalized) {
        explicit_correction_re()
            .replace(&normalized, "")
            .to_string()
    } else {
        String::new()
    };
    if !remainder.is_empty() {
        let remainder_first_word = remainder
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches("'t");
        if IMPERATIVE_STARTERS.contains(&remainder_first_word)
            || first_person_preference_re().is_match(remainder.trim())
        {
            return true;
        }
    }

    // Conditional directives are durable only when their action is itself a
    // directive; a modal inside the condition remains ordinary factual text.
    if let Some((condition, action)) = normalized.split_once(",") {
        if condition.starts_with("when ") {
            let action_first_word = action.split_whitespace().next().unwrap_or("");
            if IMPERATIVE_STARTERS.contains(&action_first_word)
                || first_person_preference_re().is_match(action.trim())
            {
                return true;
            }
        }
    }
    false
}

/// Scoped identity index over canonical rules.
#[derive(Debug, Clone, Default)]
pub struct RuleIndex {
    by_key: BTreeMap<RuleKey, ()>,
    by_id: BTreeMap<String, Vec<RuleKey>>,
}

impl RuleIndex {
    pub fn insert(&mut self, key: RuleKey) {
        self.by_id
            .entry(key.record_id.clone())
            .or_default()
            .push(key.clone());
        self.by_key.insert(key, ());
    }

    pub fn has(&self, key: &RuleKey) -> bool {
        self.by_key.contains_key(key)
    }

    pub fn keys_for_id(&self, record_id: &str) -> Vec<&RuleKey> {
        self.by_id
            .get(record_id)
            .map(|keys| keys.iter().collect())
            .unwrap_or_default()
    }
}

/// Inputs to eligibility for one candidate.
pub struct EligibilityInput<'a> {
    pub operation: &'a str,
    pub rule: &'a str,
    pub category: &'a str,
    pub scope: &'a str,
    pub scope_dimensions_raw: &'a BTreeMap<String, String>,
    pub record_class: &'a str,
    pub origin: authority::Origin,
    pub evidence_text: &'a str,
    pub declared_authority_effect: Option<&'a str>,
    /// Versioned policy bans: `(reason, regex)` evaluated against the rule.
    pub policy_bans: &'a [(String, regex::Regex)],
    pub index: &'a RuleIndex,
    pub stored_rules: &'a [StoredRule],
}

/// Gate 1: decide whether a candidate is eligible to become a Taste proposal.
pub fn evaluate_eligibility(input: &EligibilityInput<'_>) -> EligibilityDecision {
    let operation = input.operation.trim().to_lowercase();
    if !matches!(operation.as_str(), "add" | "update" | "deprecate") {
        return refused(format!("unsupported-action:{}", operation));
    }

    // Non-Taste semantic classes are rejected outright from the Taste lane.
    let class = match RecordClass::parse(input.record_class) {
        Some(c) => c,
        None => return refused(format!("not-taste-class:{}", input.record_class)),
    };

    // Category taxonomy: unknown categories go to review, never active.
    if normalize_category(input.category).is_none() {
        return refused("category-not-allowed".into());
    }

    let body = input.rule.trim();
    if body.is_empty() {
        return refused("rule-empty".into());
    }

    // Fail-closed scope normalization BEFORE any matching/identity decision:
    // malformed dimensions can never silently widen applicability.
    if ScopeDimensions::normalize(input.scope_dimensions_raw).is_err() {
        return refused("scope-malformed".into());
    }

    let key = RuleKey::new(input.scope, body);
    match operation.as_str() {
        "update" | "deprecate" => {
            if !input.index.has(&key) && input.index.keys_for_id(&key.record_id).is_empty() {
                return refused(format!("{operation}-target-missing"));
            }
        }
        _ => {
            if input.index.has(&key) || input.index.has(&RuleKey::new("", &key.record_id)) {
                return refused("rule-duplicate".into());
            }
        }
    }

    for (reason, pattern) in input.policy_bans {
        if pattern.is_match(body) {
            return refused(reason.clone());
        }
    }

    let AuthorityResult {
        admitted, reason, ..
    } = authority::evaluate_origin(input.origin, input.evidence_text);
    if !admitted {
        return refused(reason);
    }

    // The declared effect is transport metadata, not authority. Recompute it
    // from the proposed rule itself, refuse mismatches, and quarantine effects
    // that would expand permission or weaken security. Evidence bytes decide
    // origin authority; a benign excerpt must not launder an unsafe proposal.
    let rule_effect = authority::classify_authority_effect(body);
    let effect_name = match rule_effect {
        AuthorityEffect::Neutral => "neutral",
        AuthorityEffect::Restrictive => "restrictive",
        AuthorityEffect::PermissionExpanding => "permission-expanding",
        AuthorityEffect::SecurityWeakening => "security-weakening",
    };
    if let Some(declared) = input.declared_authority_effect {
        let declared = declared.trim().to_lowercase().replace('_', "-");
        if declared != effect_name {
            return refused(format!(
                "authority-effect-mismatch:{declared}:{effect_name}"
            ));
        }
    }
    match rule_effect {
        AuthorityEffect::PermissionExpanding | AuthorityEffect::SecurityWeakening => {
            return refused(format!("authority-effect:{effect_name}"));
        }
        AuthorityEffect::Neutral | AuthorityEffect::Restrictive => {}
    }

    if !rule_shape_valid(body) && !explicit_label_body_matches(body, input.evidence_text) {
        return refused("rule-invalid-shape".into());
    }

    let conflicts = authority::detect_rule_contradictions(body, input.scope, input.stored_rules);
    if !conflicts.is_empty() {
        return refused("rule-conflict-needs-review".into());
    }

    let _ = class; // validated above
    EligibilityDecision::Admitted
}

fn refused(reason: String) -> EligibilityDecision {
    EligibilityDecision::Refused { reason }
}

/// Convenience: run eligibility for a model-proposed extraction after binding
/// it to qualifying user evidence. The model text is untrusted until this
/// deterministic gate passes.
pub fn evaluate_model_proposal(
    proposal: &ModelExtractionProposal,
    evidence: &[rightkit_agent_transcript::TranscriptEventV1],
    index: &RuleIndex,
    stored_rules: &[StoredRule],
) -> Result<EligibilityDecision, ModelProposalError> {
    // The excerpt proves origin, not semantics. Require model text to be the
    // excerpt itself or the deterministic body after one supported prefix;
    // otherwise selected transcript evidence would launder an unrelated rule.
    if !model_rule_matches_bound_excerpt(&proposal.rule_text, &proposal.bound_evidence_excerpt) {
        return Err(ModelProposalError::UnboundEvidence);
    }
    let expected = crate::canonical::sha256_hex(proposal.bound_evidence_excerpt.as_bytes());
    let all_bound = !proposal.bound_evidence_ids.is_empty()
        && proposal.bound_evidence_ids.iter().all(|id| {
            evidence.iter().any(|event| {
                event.event_id == *id
                    && event.kind == "user_message"
                    && event.role.as_deref() == Some("user")
                    && !event.synthetic
                    && !event.meta
                    && !event.private_reasoning_omitted
                    && !event.redacted
                    && !event.flags.synthetic
                    && !event.flags.meta
                    && !event.flags.private_reasoning_omitted
                    && !event.flags.redacted
                    && crate::canonical::sha256_hex(event.text.as_bytes()) == expected
            })
        });
    if !all_bound {
        return Err(ModelProposalError::UnboundEvidence);
    }
    let empty_bans: Vec<(String, regex::Regex)> = Vec::new();
    let dims = BTreeMap::new();
    Ok(evaluate_eligibility(&EligibilityInput {
        operation: "add",
        rule: &proposal.rule_text,
        category: &proposal.category_hint,
        scope: &proposal.scope_hint,
        scope_dimensions_raw: &dims,
        record_class: "standing_preference",
        origin: authority::Origin::UserTurn,
        evidence_text: &proposal.bound_evidence_excerpt,
        declared_authority_effect: None,
        policy_bans: &empty_bans,
        index,
        stored_rules,
    }))
}

fn model_rule_matches_bound_excerpt(rule: &str, excerpt: &str) -> bool {
    let normalized_rule = normalize_text(rule);
    let normalized_excerpt = normalize_text(excerpt);
    if normalized_rule == normalized_excerpt {
        return true;
    }
    let remainder = if polite_correction_re().is_match(&normalized_excerpt) {
        polite_correction_re()
            .replace(&normalized_excerpt, "stop")
            .to_string()
    } else if explicit_label_re().is_match(&normalized_excerpt) {
        explicit_label_re()
            .replace(&normalized_excerpt, "")
            .to_string()
    } else if explicit_temporal_re().is_match(&normalized_excerpt) {
        explicit_temporal_re()
            .replace(&normalized_excerpt, "")
            .to_string()
    } else if explicit_correction_re().is_match(&normalized_excerpt) {
        explicit_correction_re()
            .replace(&normalized_excerpt, "")
            .to_string()
    } else {
        return false;
    };
    normalized_rule == normalize_text(&remainder)
}

/// Build a candidate record from an admitted proposal. This is the ONLY
/// constructor path out of gate 1, and it stamps provisional influence +
/// candidate lifecycle; nothing admitted here is durably authoritative yet.
pub fn build_candidate(
    rule: &str,
    category: &str,
    class: RecordClass,
    scope: &str,
    dims: ScopeDimensions,
    confidence: f64,
    evidence_ids: Vec<String>,
    now: &str,
) -> Result<PreferenceRecordV1, crate::record::RecordError> {
    PreferenceRecordV1::new_candidate(
        rule,
        category,
        class,
        scope,
        dims,
        confidence,
        evidence_ids,
        now,
    )
}
