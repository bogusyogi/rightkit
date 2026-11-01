//! Authorization decision audit log (ENG-93, M2).
//!
//! Append-only record of every allow/deny/ask (card) decision the PDP makes,
//! with rule attribution. This is explicitly **distinct from the session
//! event journal** (owned elsewhere, e.g. the mobile runtime's
//! `journal.rs`): the journal records session/tool *events* for replay and
//! UX; this log records *authorization decisions* for audit — why a
//! particular call was allowed, denied, or sent to the user, and which rule
//! or gate produced that outcome. It is self-contained within the policy
//! crate and has no dependency on the journal.
//!
//! `rightkit-agent-policy` performs no I/O, so [`DecisionLog`] is an in-memory,
//! append-only store. The host process is responsible for persisting
//! entries (e.g. flushing to the same durable store it uses for other
//! audit trails).

use crate::types::PermissionDecision;

/// One append-only decision record.
///
/// Deliberately excludes free-form request payloads, tool arguments, or
/// secret/credential values — only the decision, its attribution, and
/// minimal identifying context are retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    /// Monotonic sequence number within this log (0-indexed, assigned on append).
    pub sequence: u64,
    /// Unix epoch milliseconds, supplied by the caller (crate performs no
    /// I/O and cannot read the clock itself).
    pub timestamp_ms: u64,
    /// The decision the PDP reached.
    pub decision: PermissionDecision,
    /// Identifier of the rule, gate, or default path that produced the
    /// decision (e.g. `"abac:rule-42"`, `"workspace-trust-gate:unknown"`,
    /// `"default:ask"`).
    pub rule_attribution: String,
    /// Tool name the decision was evaluated for.
    pub tool: String,
    /// Minimal, non-sensitive context (e.g. workspace id, command verb).
    /// Callers must not place secret or credential values here; see
    /// [`DecisionLog::append`] for the enforced denylist of keys.
    pub context: Vec<(String, String)>,
}

/// Attribute key fragments that must never appear in logged context,
/// defense-in-depth against accidental secret capture. Matching is
/// case-insensitive substring match against the context key.
const FORBIDDEN_CONTEXT_KEY_FRAGMENTS: &[&str] = &[
    "secret",
    "password",
    "token",
    "credential",
    "api_key",
    "apikey",
    "authorization",
    "auth_header",
];

/// Append-only, in-memory log of authorization decisions.
#[derive(Debug, Clone, Default)]
pub struct DecisionLog {
    entries: Vec<DecisionRecord>,
}

impl DecisionLog {
    /// Create an empty log.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Append a decision record.
    ///
    /// Any context entry whose key matches a forbidden fragment (see
    /// [`FORBIDDEN_CONTEXT_KEY_FRAGMENTS`]) is dropped rather than stored —
    /// the log never carries secret values even if a caller passes one in
    /// by mistake. Returns the record actually stored (post-scrubbing) so
    /// callers can confirm what was retained.
    pub fn append(
        &mut self,
        timestamp_ms: u64,
        decision: PermissionDecision,
        rule_attribution: impl Into<String>,
        tool: impl Into<String>,
        context: Vec<(String, String)>,
    ) -> &DecisionRecord {
        let scrubbed_context = context
            .into_iter()
            .filter(|(key, _)| {
                let lower = key.to_ascii_lowercase();
                !FORBIDDEN_CONTEXT_KEY_FRAGMENTS
                    .iter()
                    .any(|fragment| lower.contains(fragment))
            })
            .collect();

        let record = DecisionRecord {
            sequence: self.entries.len() as u64,
            timestamp_ms,
            decision,
            rule_attribution: rule_attribution.into(),
            tool: tool.into(),
            context: scrubbed_context,
        };
        self.entries.push(record);
        self.entries.last().expect("just pushed")
    }

    /// All recorded entries, oldest first, in append order.
    pub fn entries(&self) -> &[DecisionRecord] {
        &self.entries
    }

    /// Number of recorded entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_allow_with_rule_attribution() {
        let mut log = DecisionLog::new();
        log.append(
            1_000,
            PermissionDecision::Allow,
            "abac:rule-1",
            "Read",
            vec![("workspace".to_string(), "ws-1".to_string())],
        );
        assert_eq!(log.len(), 1);
        let record = &log.entries()[0];
        assert_eq!(record.decision, PermissionDecision::Allow);
        assert_eq!(record.rule_attribution, "abac:rule-1");
        assert_eq!(record.sequence, 0);
    }

    #[test]
    fn records_deny_with_rule_attribution() {
        let mut log = DecisionLog::new();
        log.append(
            1_001,
            PermissionDecision::Deny,
            "workspace-trust-gate:unknown",
            "Bash",
            vec![],
        );
        let record = &log.entries()[0];
        assert_eq!(record.decision, PermissionDecision::Deny);
        assert_eq!(record.rule_attribution, "workspace-trust-gate:unknown");
    }

    #[test]
    fn is_append_only_and_sequenced() {
        let mut log = DecisionLog::new();
        log.append(1, PermissionDecision::Allow, "r1", "Read", vec![]);
        log.append(2, PermissionDecision::Ask, "r2", "Write", vec![]);
        log.append(3, PermissionDecision::Deny, "r3", "Bash", vec![]);
        let seqs: Vec<u64> = log.entries().iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[test]
    fn never_stores_secret_like_context_values() {
        let mut log = DecisionLog::new();
        log.append(
            1,
            PermissionDecision::Allow,
            "abac:rule-1",
            "Bash",
            vec![
                ("workspace".to_string(), "ws-1".to_string()),
                (
                    "api_key".to_string(),
                    concat!("sk", "-super-secret").to_string(),
                ),
                ("Authorization".to_string(), "Bearer xyz".to_string()),
                ("password".to_string(), "hunter2".to_string()),
            ],
        );
        let record = &log.entries()[0];
        let keys: Vec<&str> = record.context.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["workspace"]);
        for (_, value) in &record.context {
            assert_ne!(value, concat!("sk", "-super-secret"));
            assert_ne!(value, "Bearer xyz");
            assert_ne!(value, "hunter2");
        }
    }
}
