//! Deterministic Taste extraction from canonical native transcript events.
//! Only selected external-user transcript events can mint candidates; model,
//! tool, repository, synthetic, meta, private-reasoning, & redacted text cannot.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::authority::{classify_authority_effect, AuthorityEffect};
use crate::canonical::sha256_hex;

pub const TASTE_CANDIDATE_SCHEMA: &str = "adapt.taste-candidate.v1";
pub const COUNTERFACTUAL_SCHEMA: &str = "adapt.taste-counterfactual.v1";
pub const COUNTERFACTUAL_NONE_RECORDED: &str = "none_recorded";
const MAX_CONTEXT_EVENTS: usize = 4;
const MAX_CONTEXT_CHARS: usize = 4_000;
const MAX_TASTE_CANDIDATES_PER_EVENT: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasteContextEventV1 {
    pub event_id: String,
    pub kind: String,
    pub role: Option<String>,
    pub session_id: String,
    pub transcript_id: String,
    pub parser_digest: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub text: String,
    pub classification: String,
    pub synthetic: bool,
    pub meta: bool,
    pub redacted: bool,
    pub is_source: bool,
    pub truncated: bool,
}

/// A correction-derived alternative is evidence, not authority.  The
/// rejected text is retained only when it is present as an exact, visible
/// assistant event in the mined context.  `failure_evidence` stays explicitly
/// `none_recorded`: choosing a valid alternative does not prove a failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasteCounterfactualV1 {
    pub schema_version: String,
    pub status: String,
    pub rejected_alternative: Option<String>,
    pub rejected_alternative_event_id: Option<String>,
    pub rejected_alternative_byte_start: Option<u64>,
    pub rejected_alternative_byte_end: Option<u64>,
    pub rejected_alternative_text_sha256: Option<String>,
    pub replacement: String,
    pub reason: String,
    pub reason_kind: String,
    pub failure_evidence: String,
    pub source_event_id: String,
    pub source_session_id: String,
    pub source_transcript_id: String,
    pub source_transcript_sha256: String,
    pub source_parser_digest: String,
    pub source_evidence_text_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TasteCandidateV1 {
    pub schema_version: String,
    pub candidate_id: String,
    pub rule: String,
    pub category: String,
    pub record_type: String,
    pub scope: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scope_dimensions: BTreeMap<String, String>,
    pub source_event_id: String,
    pub source_session_id: String,
    pub source_transcript_id: String,
    /// Full frozen transcript-prefix digest observed while mining.
    pub source_transcript_sha256: String,
    pub source_parser_digest: String,
    pub source_host: String,
    pub source_byte_start: u64,
    pub source_byte_end: u64,
    pub evidence_text_sha256: String,
    pub evidence_text: String,
    pub context_events: Vec<TasteContextEventV1>,
    pub authority_effect: AuthorityEffect,
    pub confidence: f64,
    pub needs_review: bool,
    pub act_kind: rightkit_agent_transcript::evidence::ActKind,
    pub evidence_class: rightkit_agent_transcript::evidence::EvidenceClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avoided_alternative: Option<String>,
    /// In-memory capability binding every serialized field to evidence that
    /// passed the native transcript parser. This is deliberately neither
    /// public nor deserializable: JSON cannot manufacture authority.
    #[serde(skip)]
    integrity_sha256: String,
}

impl TasteCandidateV1 {
    fn seal_integrity(mut self) -> Self {
        self.integrity_sha256 =
            sha256_hex(&serde_json::to_vec(&self).expect("Taste candidate serializes"));
        self
    }

    pub(crate) fn verify_integrity(&self) -> bool {
        !self.integrity_sha256.is_empty()
            && self.integrity_sha256
                == sha256_hex(&serde_json::to_vec(self).expect("Taste candidate serializes"))
    }
}

/// Project a mined candidate into a source-bound counterfactual record.  The
/// prior assistant event is accepted only when the candidate carries its
/// exact visible context event; otherwise the honest representation is an
/// explicit `none_recorded` value.
pub fn counterfactual_for_candidate(candidate: &TasteCandidateV1) -> TasteCounterfactualV1 {
    let rejected = candidate.avoided_alternative.as_ref().and_then(|text| {
        candidate.context_events.iter().rev().find(|event| {
            event.kind == "assistant_message"
                && event.role.as_deref() == Some("assistant")
                && !event.synthetic
                && !event.meta
                && !event.redacted
                && !event.text.trim().is_empty()
                && event.session_id == candidate.source_session_id
                && event.transcript_id == candidate.source_transcript_id
                && event.parser_digest == candidate.source_parser_digest
                && &event.text == text
        })
    });
    let status = if rejected.is_some() {
        "recorded"
    } else {
        COUNTERFACTUAL_NONE_RECORDED
    };
    TasteCounterfactualV1 {
        schema_version: COUNTERFACTUAL_SCHEMA.into(),
        status: status.into(),
        rejected_alternative: rejected.map(|event| event.text.clone()),
        rejected_alternative_event_id: rejected.map(|event| event.event_id.clone()),
        rejected_alternative_byte_start: rejected.map(|event| event.byte_start),
        rejected_alternative_byte_end: rejected.map(|event| event.byte_end),
        rejected_alternative_text_sha256: rejected.map(|event| sha256_hex(event.text.as_bytes())),
        replacement: candidate.rule.clone(),
        reason: candidate.evidence_text.clone(),
        reason_kind: if candidate.act_kind
            == rightkit_agent_transcript::evidence::ActKind::Correction
        {
            "user_correction"
        } else {
            COUNTERFACTUAL_NONE_RECORDED
        }
        .into(),
        failure_evidence: COUNTERFACTUAL_NONE_RECORDED.into(),
        source_event_id: candidate.source_event_id.clone(),
        source_session_id: candidate.source_session_id.clone(),
        source_transcript_id: candidate.source_transcript_id.clone(),
        source_transcript_sha256: candidate
            .source_transcript_sha256
            .trim_start_matches("sha256:")
            .to_lowercase(),
        source_parser_digest: candidate.source_parser_digest.clone(),
        source_evidence_text_sha256: candidate.evidence_text_sha256.clone(),
    }
}

fn correction_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:^|\b)(?:no,?\s+that(?:'s| is)\s+(?:wrong|not right|not what|not how)|wrong\b|incorrect\b|not quite\b|correction\s*:|please stop\b|stop (?:doing|using|writing|skipping|generating)\b|why (?:did|are) you\b|never .{0,100} again\b|don'?t .{0,100} again\b)",
        )
        .expect("static correction expression")
    })
}

fn explicit_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(
            // Modal words are meaningful only when they open a directive.
            // A modal embedded in a declarative fact ("the build must...")
            // is not evidence of a user preference. Explicit labels,
            // temporal markers, corrections, and first-person preferences
            // remain independently strong signals.
            r"(?i)(?:^|[.!?]\s+)(?:decision|locked|constraint|invariant|rule|correction)\s*:|^\s*(?:always|never|prefer|avoid|require|must|should|do not|don'?t)\b|(?:^|[.!?]\s+)(?:going forward|from now on|henceforth)\b|^\s*i\s+(?:always|never|prefer|avoid|require|must|should|like)\b",
        )
        .expect("static explicit-preference expression")
    })
}

fn health_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:medical|diagnosis|diagnostic|therapeutic|therapy|medication|prescription|dosage|clinical|patient|disease|symptom)\b",
        )
        .expect("static health-domain expression")
    })
}

/// Explicitly bounded instructions belong to the current operation, not to
/// durable Taste. Keep this deliberately narrow: an unqualified preference
/// remains eligible, while clear task/turn/response markers fail closed out
/// of the durable extraction lane.
fn task_local_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*(?:for (?:this|the current) (?:task|request|response|turn)\b|only (?:for|in) (?:this|the current) (?:task|request|response|turn)\b|today\s*[,;:])",
        )
        .expect("static task-local expression")
    })
}

fn normalized_rule(text: &str) -> String {
    let text = text.trim();
    let stripped = Regex::new(
        r"(?i)^\s*(?:no,?\s+that(?:'s| is)\s+(?:wrong|not right|not what|not how)\s*[:,.!-]*\s*|wrong\s*[:,.!-]*\s*|incorrect\s*[:,.!-]*\s*|correction\s*:\s*|(?:from now on|going forward|henceforth)\s*[:,.!-]*\s*|(?:decision|locked|constraint|invariant|rule)\s*:\s*|please\s+)",
    )
    .expect("static correction-prefix expression")
    .replace(text, "");
    let value = stripped.trim();
    if value.is_empty() {
        text.to_string()
    } else {
        value.chars().take(1_200).collect()
    }
}

/// Extract rule-shaped directives independently from surrounding user prose.
/// The candidate remains bound to the complete native event below; this only
/// chooses its normalized rule body. Correction messages retain their existing
/// whole-message projection unless they contain explicit directive lines.
fn candidate_rules(text: &str) -> Vec<String> {
    fn sentence_re() -> &'static Regex {
        static VALUE: OnceLock<Regex> = OnceLock::new();
        VALUE.get_or_init(|| {
            Regex::new(r"(?s)[^.!?]+[.!?]?(?:\s+|$)").expect("static sentence expression")
        })
    }

    let mut rules = Vec::new();
    let mut in_code_fence = false;
    let mut excluded_evidence = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            excluded_evidence = true;
            continue;
        }
        let quoted_line = trimmed.starts_with('"')
            || trimmed.ends_with('"')
            || trimmed.starts_with('\'')
            || trimmed.ends_with('\'')
            || trimmed.starts_with('“')
            || trimmed.ends_with('”')
            || trimmed.starts_with('‘')
            || trimmed.ends_with('’');
        if in_code_fence || trimmed.contains('`') || trimmed.starts_with('>') || quoted_line {
            excluded_evidence = true;
            continue;
        }
        if trimmed.chars().any(|ch| matches!(ch, '"' | '“' | '”')) {
            excluded_evidence = true;
        }
        // Split prose into verbatim sentences. Restricting each candidate to
        // one bounded sentence avoids turning explanatory paragraphs into a
        // durable rule while preserving the complete event as its evidence.
        for matched in sentence_re().find_iter(line) {
            let sentence = matched.as_str().trim();
            let begins_quoted = sentence.starts_with('"') || sentence.starts_with('\'');
            let contains_quoted_text = sentence.chars().any(|ch| matches!(ch, '"' | '“' | '”'));
            if sentence.is_empty()
                || sentence.chars().count() > 1_200
                || sentence.ends_with('?')
                || begins_quoted
                || contains_quoted_text
                || !explicit_re().is_match(sentence)
            {
                continue;
            }
            let rule = normalized_rule(sentence);
            if crate::admission::rule_shape_valid(&rule) {
                rules.push(rule);
            }
            if rules.len() == MAX_TASTE_CANDIDATES_PER_EVENT {
                break;
            }
        }
        if rules.len() == MAX_TASTE_CANDIDATES_PER_EVENT {
            break;
        }
    }
    if !rules.is_empty() {
        return rules;
    }
    if !excluded_evidence && correction_re().is_match(text) && !text.trim_end().ends_with('?') {
        let rule = normalized_rule(text);
        if text.chars().count() <= 1_200 && crate::admission::rule_shape_valid(&rule) {
            return vec![rule];
        }
    }
    Vec::new()
}

fn category(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    // Prefer explicit tool names over the activity performed with them. For
    // example, a requested Cargo/lint command is a tooling preference, while
    // an unqualified request to run lint is a verification preference.
    if ["tool", "cli", "command", "pipeline", "script", "cargo"]
        .iter()
        .any(|token| lower.contains(token))
    {
        "tooling"
    } else if ["test", "verify", "lint", "type-check", "spec"]
        .iter()
        .any(|token| lower.contains(token))
    {
        "verification"
    } else if ["safe", "permission", "credential", "fail closed"]
        .iter()
        .any(|token| lower.contains(token))
        || Regex::new(r"\bauth(?:entication|orization)?\b")
            .expect("static auth category expression")
            .is_match(&lower)
    {
        "safety"
    } else if lower.contains("docstring") {
        "documentation"
    } else if [
        "architecture",
        "module",
        "layer",
        "interface",
        "abstraction",
    ]
    .iter()
    .any(|token| lower.contains(token))
    {
        "architecture"
    } else if ["style", "format", "naming", "indent"]
        .iter()
        .any(|token| lower.contains(token))
    {
        "code-style"
    } else if ["doc", "readme", "comment", "docstring"]
        .iter()
        .any(|token| lower.contains(token))
    {
        "documentation"
    } else if ["model", "codex", "claude", "opus", "sonnet", "haiku"]
        .iter()
        .any(|token| lower.contains(token))
    {
        "model-routing"
    } else {
        "workflow"
    }
}

fn eligible(event: &rightkit_agent_transcript::TranscriptEventV1) -> bool {
    event.kind == "user_message"
        && event.role.as_deref() == Some("user")
        && !event.synthetic
        && !event.meta
        && !event.private_reasoning_omitted
        && !event.redacted
        && !event.flags.synthetic
        && !event.flags.meta
        && !event.flags.private_reasoning_omitted
        && !event.flags.redacted
        && !matches!(
            event.classification.as_str(),
            "unresolved_failure" | "failed_verification"
        )
        && !event.text.trim().is_empty()
        && !health_re().is_match(&event.text)
        && !task_local_re().is_match(&event.text)
}

fn context(
    events: &[rightkit_agent_transcript::TranscriptEventV1],
    source_index: usize,
) -> Vec<TasteContextEventV1> {
    let start = source_index.saturating_sub(MAX_CONTEXT_EVENTS);
    let end = events.len().min(source_index + MAX_CONTEXT_EVENTS + 1);
    let source_event_id = events
        .get(source_index)
        .map(|event| event.event_id.as_str());
    let mut remaining = MAX_CONTEXT_CHARS;
    events[start..end]
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            !event.synthetic
                && !event.flags.synthetic
                && !event.meta
                && !event.flags.meta
                && !event.private_reasoning_omitted
                && !event.flags.private_reasoning_omitted
                && !event.redacted
                && !event.flags.redacted
        })
        .filter_map(|(_offset, event)| {
            let is_source = source_event_id == Some(event.event_id.as_str());
            if remaining == 0 && !is_source {
                return None;
            }
            let count = event.text.chars().count();
            let take = if is_source {
                count
            } else {
                remaining.min(count)
            };
            let mut text: String = event.text.chars().take(take).collect();
            let truncated = take < count;
            if truncated {
                text.push('…');
            }
            if !is_source {
                remaining = remaining.saturating_sub(take);
            }
            Some(TasteContextEventV1 {
                event_id: event.event_id.clone(),
                kind: event.kind.clone(),
                role: event.role.clone(),
                session_id: event.session_id.clone(),
                transcript_id: event.transcript_id.clone(),
                parser_digest: event.parser_digest.clone(),
                byte_start: event.byte_start,
                byte_end: event.byte_end,
                text,
                classification: event.classification.clone(),
                synthetic: event.synthetic || event.flags.synthetic,
                meta: event.meta || event.flags.meta,
                redacted: event.redacted || event.flags.redacted,
                is_source,
                truncated,
            })
        })
        .collect()
}

pub fn extract_candidates(
    events: &[rightkit_agent_transcript::TranscriptEventV1],
    scope: &str,
) -> Vec<TasteCandidateV1> {
    extract_candidates_with_source(events, scope, "")
}

/// Extract candidates while binding each one to the frozen transcript digest
/// supplied by the transcript parser receipt.
pub fn extract_candidates_with_source(
    events: &[rightkit_agent_transcript::TranscriptEventV1],
    scope: &str,
    source_transcript_sha256: &str,
) -> Vec<TasteCandidateV1> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| eligible(event))
        .flat_map(|(index, event)| {
            candidate_rules(&event.text).into_iter().map(move |rule| {
                let correction = correction_re().is_match(&event.text);
                let avoided_alternative = correction
                    .then(|| {
                        events[..index]
                            .iter()
                            .rev()
                            .find(|candidate| {
                                candidate.kind == "assistant_message"
                                    && !candidate.synthetic
                                    && !candidate.flags.synthetic
                                    && !candidate.meta
                                    && !candidate.flags.meta
                                    && !candidate.private_reasoning_omitted
                                    && !candidate.flags.private_reasoning_omitted
                                    && !candidate.redacted
                                    && !candidate.flags.redacted
                                    && !candidate.text.trim().is_empty()
                                    && candidate.session_id == event.session_id
                                    && candidate.transcript_id == event.transcript_id
                                    && candidate.parser_digest == event.parser_digest
                            })
                            .map(|candidate| candidate.text.chars().take(800).collect::<String>())
                    })
                    .flatten();
                let seed = format!(
                    "{}\0{}\0{}\0{}\0{}",
                    scope, event.event_id, event.byte_start, event.byte_end, rule
                );
                TasteCandidateV1 {
                    schema_version: TASTE_CANDIDATE_SCHEMA.into(),
                    candidate_id: format!("taste_{}", &sha256_hex(seed.as_bytes())[..24]),
                    rule: rule.clone(),
                    category: category(&rule).into(),
                    record_type: if explicit_re().is_match(&event.text)
                        || explicit_re().is_match(&rule)
                    {
                        "standing_preference".into()
                    } else {
                        "operational_playbook".into()
                    },
                    scope: scope.to_string(),
                    scope_dimensions: BTreeMap::new(),
                    source_event_id: event.event_id.clone(),
                    source_session_id: event.session_id.clone(),
                    source_transcript_id: event.transcript_id.clone(),
                    source_transcript_sha256: source_transcript_sha256
                        .trim_start_matches("sha256:")
                        .to_lowercase(),
                    source_parser_digest: event.parser_digest.clone(),
                    source_host: event.host.clone(),
                    source_byte_start: event.byte_start,
                    source_byte_end: event.byte_end,
                    evidence_text_sha256: sha256_hex(event.text.as_bytes()),
                    evidence_text: event.text.clone(),
                    context_events: context(events, index),
                    authority_effect: classify_authority_effect(&rule),
                    confidence: if correction { 0.65 } else { 0.85 },
                    needs_review: true,
                    act_kind: if correction {
                        rightkit_agent_transcript::evidence::ActKind::Correction
                    } else {
                        rightkit_agent_transcript::evidence::ActKind::ExplicitPreference
                    },
                    // The native transcript parser has already established this
                    // signal as an external-user authoritative act. Review remains
                    // mandatory before admission, but review must not downgrade
                    // the evidence class to behavioral evidence.
                    evidence_class:
                        rightkit_agent_transcript::evidence::EvidenceClass::UserAuthoritative,
                    avoided_alternative,
                    integrity_sha256: String::new(),
                }
                .seal_integrity()
            })
        })
        .collect()
}
