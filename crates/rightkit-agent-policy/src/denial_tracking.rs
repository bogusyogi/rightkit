use crate::types::PermissionDecision;
use serde::{Deserialize, Serialize};

/// Source of a permission rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleSource {
    /// Built-in/hardcoded rules.
    Builtin,
    /// Project-level rules (e.g., from CLAUDE.md or .claude/settings.json).
    Project,
    /// User-supplied rules (e.g., ~/.claude/settings.json).
    User,
    /// Session-specific rules (ephemeral, cleared on session end).
    Session,
}

/// A permission rule with its source.
#[derive(Debug, Clone)]
pub struct Rule {
    pub pattern: String,
    pub effect: PermissionDecision,
    pub source: RuleSource,
    /// The argument inside `Tool(arg)` is matched as the exact text, with no `*` wildcard and no
    /// per-stage shell matching. Session grants use this so a command that contains `*` (or any
    /// other tool's raw text) never widens into a pattern (D2, rules 4, 5 and 53).
    pub literal: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolRuleTarget {
    pub tool: String,
    pub command: Option<String>,
    pub path: Option<String>,
}

impl ToolRuleTarget {
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            command: None,
            path: None,
        }
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// A permission rule that cannot be installed because it is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid permission rule '{pattern}': {reason}")]
pub struct RuleError {
    pub pattern: String,
    pub reason: &'static str,
}

impl Rule {
    /// Reject patterns that would silently never match (a malformed deny is a
    /// dropped deny). Qualified patterns must be `Tool(arg)` with both parts
    /// non-empty and balanced parentheses.
    pub fn validate(&self) -> Result<(), RuleError> {
        let pattern = self.pattern.trim();
        let err = |reason| RuleError {
            pattern: self.pattern.clone(),
            reason,
        };
        if pattern.is_empty() {
            return Err(err("empty pattern"));
        }
        if pattern.contains('(') || pattern.contains(')') {
            if split_qualified_pattern(pattern).is_none() {
                return Err(err("malformed qualified pattern; expected Tool(arg)"));
            }
            let open = pattern.find('(').unwrap_or(0);
            let inner = &pattern[open + 1..pattern.len() - 1];
            if inner.contains('(') || inner.contains(')') {
                // Shell arguments legitimately contain parentheses; require balance only.
                let mut depth = 0i32;
                for ch in inner.chars() {
                    match ch {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    if depth < 0 {
                        return Err(err("unbalanced parentheses"));
                    }
                }
                if depth != 0 {
                    return Err(err("unbalanced parentheses"));
                }
            }
        }
        Ok(())
    }

    pub fn new(pattern: impl Into<String>, effect: PermissionDecision, source: RuleSource) -> Self {
        Self {
            pattern: pattern.into(),
            effect,
            source,
            literal: false,
        }
    }

    /// A session allow for exactly one command of one tool. `*` in `command` is an ordinary
    /// character, and a chained command never matches a grant for one of its parts.
    pub fn session_command_grant(tool: &str, command: &str) -> Self {
        Self {
            pattern: format!("{tool}({command})"),
            effect: PermissionDecision::Allow,
            source: RuleSource::Session,
            literal: true,
        }
    }

    /// Pattern argument comparison honouring [`Rule::literal`].
    fn arg_matches(&self, arg_pattern: &str, text: &str) -> bool {
        if self.literal {
            arg_pattern == text.trim()
        } else {
            wildcard_match(arg_pattern, text)
        }
    }

    /// Compound-aware shell matching (a rule for one command must never
    /// authorize a chained one).
    ///
    /// - Deny and ask fire when the raw string OR any stage matches, which can
    ///   only make them stricter.
    /// - Allow matches here only a single, confidently parsed command; a
    ///   compound command is covered stage by stage in
    ///   [`RuleStore::effective_match_for`], and an unparseable one is never
    ///   auto-allowed.
    fn shell_arg_matches(&self, arg_pattern: &str, command: &str) -> bool {
        if self.literal {
            return arg_pattern == command.trim();
        }
        let analysis = crate::shell_stages::analyze(command);
        if self.effect == PermissionDecision::Allow {
            return analysis.is_single() && wildcard_match(arg_pattern, command);
        }
        wildcard_match(arg_pattern, command)
            || analysis
                .stages
                .iter()
                .chain(analysis.exposed_stages.iter())
                .any(|stage| wildcard_match(arg_pattern, stage))
    }

    /// True when this is an allow rule for the shell tool whose argument
    /// pattern covers one stage of a compound command.
    fn allows_shell_stage(&self, stage: &str) -> bool {
        if self.effect != PermissionDecision::Allow || self.literal {
            return false;
        }
        let Some((tool_pattern, arg_pattern)) = split_qualified_pattern(self.pattern.trim()) else {
            return false;
        };
        shell_alias_matches(tool_pattern, "shell") && wildcard_match(arg_pattern, stage)
    }

    pub fn matches_target(&self, target: &ToolRuleTarget) -> bool {
        let pattern = self.pattern.trim();
        if let Some((tool_pattern, arg_pattern)) = split_qualified_pattern(pattern) {
            if !tool_pattern_matches(tool_pattern, &target.tool) {
                return false;
            }
            let Some(value) = tool_arg_for_pattern(tool_pattern, target) else {
                return false;
            };
            if shell_alias_matches(tool_pattern, &target.tool) {
                return self.shell_arg_matches(arg_pattern, value);
            }
            if target.path.as_deref() == Some(value)
                && !shell_alias_matches(tool_pattern, &target.tool)
            {
                // Canonical file target: `.`/`..`/`//`/`\` variants must not
                // dodge a deny or sneak past an allow. A deny fires on the raw
                // OR canonical form; allow/ask only on the canonical form.
                let canon = crate::deny_globs::collapse_dots(value);
                let pat = arg_pattern.replace('\\', "/");
                let canon_hit = wildcard_match(&pat, &canon);
                return if self.effect == PermissionDecision::Deny {
                    canon_hit
                        || wildcard_match(&pat, value)
                        || wildcard_match(&pat, &value.replace('\\', "/"))
                } else {
                    canon_hit
                };
            }
            return self.arg_matches(arg_pattern, value);
        }
        tool_pattern_matches(pattern, &target.tool)
    }
}

/// The stages of a confidently parsed multi-stage shell command, else `None`.
fn shell_compound_stages(target: &ToolRuleTarget) -> Option<Vec<String>> {
    if !matches!(canonical_tool(&target.tool).as_str(), "shell") {
        return None;
    }
    let analysis = crate::shell_stages::analyze(target.command.as_deref()?);
    (analysis.confident && analysis.stages.len() > 1).then_some(analysis.stages)
}

fn split_qualified_pattern(pattern: &str) -> Option<(&str, &str)> {
    let open = pattern.find('(')?;
    if !pattern.ends_with(')') || open == 0 {
        return None;
    }
    let tool = pattern[..open].trim();
    let arg = pattern[open + 1..pattern.len() - 1].trim();
    if tool.is_empty() || arg.is_empty() {
        None
    } else {
        Some((tool, arg))
    }
}

fn tool_arg_for_pattern<'a>(tool_pattern: &str, target: &'a ToolRuleTarget) -> Option<&'a str> {
    if shell_alias_matches(tool_pattern, &target.tool) {
        return target.command.as_deref();
    }
    if file_alias_matches(tool_pattern, &target.tool) || is_file_like_tool(&target.tool) {
        return target.path.as_deref();
    }
    target.path.as_deref().or(target.command.as_deref())
}

fn tool_pattern_matches(pattern: &str, tool: &str) -> bool {
    let pattern = pattern.trim();
    if pattern == "*" {
        return true;
    }
    if shell_alias_matches(pattern, tool) || file_alias_matches(pattern, tool) {
        return true;
    }
    wildcard_match(pattern, tool)
}

fn shell_alias_matches(pattern: &str, tool: &str) -> bool {
    matches!(canonical_tool(pattern).as_str(), "shell")
        && matches!(canonical_tool(tool).as_str(), "shell")
}

fn file_alias_matches(pattern: &str, tool: &str) -> bool {
    let pattern = canonical_tool(pattern);
    let tool = canonical_tool(tool);
    match pattern.as_str() {
        "file_edit" => matches!(
            tool.as_str(),
            "write_file" | "edit_file" | "multi_edit" | "apply_patch"
        ),
        "file_read" => matches!(tool.as_str(), "read_file"),
        _ => false,
    }
}

fn canonical_tool(name: &str) -> String {
    match name.trim().to_ascii_lowercase().as_str() {
        "bash" | "shell" => "shell".to_string(),
        "fileedit" | "file_edit" | "edit" => "file_edit".to_string(),
        "fileread" | "file_read" | "read" => "file_read".to_string(),
        other => other.to_string(),
    }
}

fn is_file_like_tool(tool: &str) -> bool {
    matches!(
        canonical_tool(tool).as_str(),
        "read_file" | "write_file" | "edit_file" | "multi_edit" | "apply_patch"
    )
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_idx = None;
    let mut star_match = 0usize;

    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star_idx = Some(pi);
            star_match = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(star) = star_idx {
            pi = star + 1;
            star_match += 1;
            ti = star_match;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Stable token correlating one permission decision with the exact tool call
/// that prompted it (S:B5).
///
/// A denial record alone says only "tool T was denied in session S at time A".
/// That is not enough to answer "which prompt did the operator actually see,
/// and what did they decide" when a session issues the same tool repeatedly.
/// The token is derived from the session and the tool-call id, so the audit
/// record and the decision prompt can be matched after the fact without
/// storing the prompt text alongside every denial.
///
/// Derivation is deterministic and collision-resistant across the pair, and
/// carries no secret material — it is safe to surface in logs and API
/// responses.
pub fn correlation_token(session: &str, tool_call_id: &str) -> String {
    // A missing identity cannot safely stand in for an exact call. Returning
    // the legacy/untracked marker prevents every malformed call in a session
    // from sharing one fabricated correlation token.
    if session.trim().is_empty() || tool_call_id.trim().is_empty() {
        return String::new();
    }
    // FNV-1a over a length-prefixed encoding of both fields. Length prefixes
    // keep ("ab","c") and ("a","bc") distinct, which plain concatenation would
    // not. Kept dependency-free on purpose: this is an audit correlator, not a
    // security primitive, and must not imply cryptographic strength.
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x1000_0000_01b3;
    let mut hash = OFFSET;
    for part in [session, tool_call_id] {
        for byte in (part.len() as u64).to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(PRIME);
        }
        for byte in part.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(PRIME);
        }
    }
    format!("corr-{hash:016x}")
}

/// A record of a denied permission request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DenialRecord {
    pub tool: String,
    pub reason: String,
    pub session: String,
    pub at: String,
    /// Correlation token tying this decision to its originating tool call.
    ///
    /// Defaulted rather than required: records persisted before S:B5 have no
    /// token, and an audit trail must stay readable across the upgrade rather
    /// than failing to deserialize. An empty token means "pre-correlation
    /// record", never "correlated to nothing".
    #[serde(default)]
    pub correlation: String,
}

impl DenialRecord {
    pub fn new(
        tool: impl Into<String>,
        reason: impl Into<String>,
        session: impl Into<String>,
        at: impl Into<String>,
    ) -> Self {
        Self {
            tool: tool.into(),
            reason: reason.into(),
            session: session.into(),
            at: at.into(),
            correlation: String::new(),
        }
    }

    /// Attach a correlation token produced by [`correlation_token`].
    pub fn with_correlation(mut self, correlation: impl Into<String>) -> Self {
        self.correlation = correlation.into();
        self
    }

    /// Whether this record predates correlation tracking.
    pub fn is_correlated(&self) -> bool {
        !self.correlation.is_empty()
    }
}

/// Tracks denied permission requests for audit and policy refinement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenialTracker {
    denials: Vec<DenialRecord>,
}

impl DenialTracker {
    pub fn new() -> Self {
        Self {
            denials: Vec::new(),
        }
    }

    /// Record a denial with the tool, reason, session ID, and timestamp.
    pub fn record_denial(&mut self, tool: &str, reason: &str, session: &str, at: &str) {
        self.denials
            .push(DenialRecord::new(tool, reason, session, at));
    }

    /// Record a denial correlated to the tool call that prompted it (S:B5).
    pub fn record_correlated_denial(
        &mut self,
        tool: &str,
        reason: &str,
        session: &str,
        at: &str,
        tool_call_id: &str,
    ) {
        let token = correlation_token(session, tool_call_id);
        self.denials
            .push(DenialRecord::new(tool, reason, session, at).with_correlation(token));
    }

    /// Record at most one audit denial for one proposed tool call.
    ///
    /// Multiple runtime gates may reject one call. They share its correlation
    /// token, so retaining only first rejection prevents one effect from
    /// producing contradictory audit outcomes.
    pub fn record_correlated_denial_once(
        &mut self,
        tool: &str,
        reason: &str,
        session: &str,
        at: &str,
        tool_call_id: &str,
    ) {
        let token = correlation_token(session, tool_call_id);
        if token.is_empty() {
            // Keep each denial visible, but do not pretend it belongs to an
            // identifiable call or deduplicate it against legacy rows.
            self.denials
                .push(DenialRecord::new(tool, reason, session, at));
            return;
        }
        if self
            .denials
            .iter()
            .any(|denial| denial.correlation == token)
        {
            return;
        }
        self.denials
            .push(DenialRecord::new(tool, reason, session, at).with_correlation(token));
    }

    /// All denials carrying the given correlation token.
    pub fn denials_for_correlation(&self, correlation: &str) -> Vec<&DenialRecord> {
        if correlation.is_empty() {
            // An empty token marks a pre-correlation record; treating it as a
            // lookup key would match every legacy row at once.
            return Vec::new();
        }
        self.denials
            .iter()
            .filter(|d| d.correlation == correlation)
            .collect()
    }

    /// Get all denial records for a session.
    pub fn denials_for(&self, session: &str) -> Vec<&DenialRecord> {
        self.denials
            .iter()
            .filter(|d| d.session == session)
            .collect()
    }

    /// Count denials for a specific tool across all sessions.
    pub fn denial_count(&self, tool: &str) -> usize {
        self.denials.iter().filter(|d| d.tool == tool).count()
    }

    /// Get all denial records (immutable view).
    pub fn all_denials(&self) -> &[DenialRecord] {
        &self.denials
    }

    /// Query denials filtered by optional session and tool, with pagination.
    /// Returns (matched_slice, total_matched). `offset` and `limit` are clamped.
    pub fn query(
        &self,
        session: Option<&str>,
        tool: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<&DenialRecord>, usize) {
        let filtered: Vec<&DenialRecord> = self
            .denials
            .iter()
            .filter(|d| session.is_none_or(|s| d.session == s))
            .filter(|d| tool.is_none_or(|t| d.tool == t))
            .collect();
        let total = filtered.len();
        let start = offset.min(total);
        let end = (start + limit).min(total);
        (filtered[start..end].to_vec(), total)
    }

    /// Current number of stored denials.
    pub fn len(&self) -> usize {
        self.denials.len()
    }

    /// Whether the tracker is empty.
    pub fn is_empty(&self) -> bool {
        self.denials.is_empty()
    }
}

impl Default for DenialTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// A store of rules merged across multiple sources with precedence ordering.
///
/// Precedence (highest to lowest):
/// 1. Session rules
/// 2. User rules
/// 3. Project rules
/// 4. Builtin rules
///
/// A matching `Deny` rule from any source overrides all non-deny rules.
/// The first matching rule (by precedence, then insertion order) determines the effect.
#[derive(Debug, Clone)]
pub struct RuleStore {
    rules: Vec<Rule>,
    session_grants: crate::approval_binding::SessionGrants,
}

impl RuleStore {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            session_grants: crate::approval_binding::SessionGrants::default(),
        }
    }

    /// EFF-002: record a session grant bound to the digest of the final
    /// (post-transform) payload that was approved. It authorizes only a call
    /// whose final payload digest is identical; deny rules still win.
    pub fn grant_session_payload(&mut self, digest: &str) {
        self.session_grants.grant_digest(digest);
    }

    /// True when a session grant covers exactly this final payload digest.
    pub fn has_session_payload_grant(&self, digest: &str) -> bool {
        self.session_grants.matches_digest(digest)
    }

    /// Add a rule to the store.
    pub fn add(&mut self, rule: Rule) {
        self.rules.push(rule);
    }

    /// Validate the complete batch, then install it. If any rule is malformed
    /// nothing is installed, so the previously installed policy stays in force.
    pub fn try_add_all(&mut self, rules: Vec<Rule>) -> Result<(), RuleError> {
        for rule in &rules {
            rule.validate()?;
        }
        self.rules.extend(rules);
        Ok(())
    }

    /// Get the effective permission decision for a tool.
    ///
    /// Returns the effect of the highest-precedence matching rule.
    /// If no rules match, returns `PermissionDecision::Ask`.
    pub fn effective(&self, tool: &str) -> PermissionDecision {
        self.effective_match(tool)
            .map(|rule| rule.effect)
            .unwrap_or(PermissionDecision::Ask)
    }

    /// Get the rule that determines the effective permission decision for a tool.
    ///
    /// Returns `None` when no rule matched, allowing callers to distinguish an
    /// explicit `Ask` rule from the no-rule default.
    pub fn effective_match(&self, tool: &str) -> Option<Rule> {
        self.effective_match_for(&ToolRuleTarget::new(tool))
    }

    pub fn effective_match_for(&self, target: &ToolRuleTarget) -> Option<Rule> {
        // Deny wins over every allow/ask regardless of source: a session
        // "allow" must never override a project/user/builtin deny.
        if let Some(rule) = self
            .rules
            .iter()
            .find(|r| r.effect == PermissionDecision::Deny && r.matches_target(target))
        {
            return Some(rule.clone());
        }

        // A compound shell command is allowed only when every stage is
        // individually covered by an allow rule.
        let compound_stages = shell_compound_stages(target);
        let compound_allowed = compound_stages.as_ref().is_some_and(|stages| {
            stages
                .iter()
                .all(|stage| self.rules.iter().any(|r| r.allows_shell_stage(stage)))
        });

        let precedence_order = [
            RuleSource::Session,
            RuleSource::User,
            RuleSource::Project,
            RuleSource::Builtin,
        ];

        for source in &precedence_order {
            for rule in &self.rules {
                if rule.source != *source {
                    continue;
                }
                let hit = match &compound_stages {
                    Some(stages) if rule.effect == PermissionDecision::Allow && !rule.literal => {
                        compound_allowed && stages.iter().any(|st| rule.allows_shell_stage(st))
                    }
                    _ => rule.matches_target(target),
                };
                if hit {
                    return Some(rule.clone());
                }
            }
        }

        None
    }

    /// Remove a rule by insertion index.
    pub fn remove_at(&mut self, index: usize) -> Option<Rule> {
        if index < self.rules.len() {
            Some(self.rules.remove(index))
        } else {
            None
        }
    }

    /// Get all rules for inspection.
    pub fn all_rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Get rules by source.
    pub fn rules_by_source(&self, source: RuleSource) -> Vec<&Rule> {
        self.rules.iter().filter(|r| r.source == source).collect()
    }
}

impl Default for RuleStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "denial_tracking_tests.rs"]
mod denial_tracking_tests;
