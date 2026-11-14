use crate::decision_log::DecisionLog;
use crate::types::PermissionDecision;
use crate::workspace_trust_gate::WorkspaceTrustGate;

/// A key-value attribute attached to a policy request or subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub key: String,
    pub value: String,
}

impl Attribute {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// A request to evaluate against ABAC policy.
#[derive(Debug, Clone)]
pub struct PolicyRequest {
    pub tool: String,
    pub command: Option<String>,
    pub attributes: Vec<Attribute>,
}

impl PolicyRequest {
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            command: None,
            attributes: Vec::new(),
        }
    }

    pub fn with_command(mut self, cmd: impl Into<String>) -> Self {
        self.command = Some(cmd.into());
        self
    }

    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.push(Attribute::new(key, value));
        self
    }

    pub fn with_attributes(mut self, attrs: Vec<Attribute>) -> Self {
        self.attributes.extend(attrs);
        self
    }
}

/// Operator for matching attributes in conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondOp {
    Eq,
    NotEq,
    Contains,
    StartsWith,
    StartsWithAny,
    In,
}

/// A condition to match against request attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub attribute: String,
    pub op: CondOp,
    pub value: String,
}

impl Condition {
    pub fn new(attribute: impl Into<String>, op: CondOp, value: impl Into<String>) -> Self {
        Self {
            attribute: attribute.into(),
            op,
            value: value.into(),
        }
    }

    fn matches(&self, attributes: &[Attribute]) -> bool {
        let attr_values = attributes
            .iter()
            .filter(|attribute| attribute.key == self.attribute)
            .map(|attribute| attribute.value.as_str())
            .collect::<Vec<_>>();

        if attr_values.is_empty() {
            return false;
        }
        match self.op {
            CondOp::Eq => attr_values.iter().any(|value| *value == self.value),
            CondOp::NotEq => attr_values.iter().all(|value| *value != self.value),
            CondOp::Contains => attr_values.iter().any(|value| value.contains(&self.value)),
            CondOp::StartsWith => attr_values
                .iter()
                .any(|value| value.starts_with(&self.value)),
            CondOp::StartsWithAny => attr_values.iter().any(|value| {
                self.value.split(',').map(str::trim).any(|prefix| {
                    *value == prefix
                        || value
                            .strip_prefix(prefix)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                })
            }),
            CondOp::In => {
                let values: Vec<&str> = self.value.split(',').map(|s| s.trim()).collect();
                attr_values.iter().any(|value| values.contains(value))
            }
        }
    }
}

/// A policy rule matching a tool with conditions and a decision effect.
#[derive(Debug, Clone)]
pub struct PolicyRule {
    pub id: String,
    pub effect: PermissionDecision,
    pub tool_match: String,
    pub conditions: Vec<Condition>,
    /// Optional precedence metadata for adapters that need to preserve a
    /// host policy's exception-capable deny semantics in one PDP pass.
    pub exception_capable: bool,
}

impl PolicyRule {
    pub fn new(
        id: impl Into<String>,
        effect: PermissionDecision,
        tool_match: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            effect,
            tool_match: tool_match.into(),
            conditions: Vec::new(),
            exception_capable: false,
        }
    }

    pub fn with_condition(mut self, cond: Condition) -> Self {
        self.conditions.push(cond);
        self
    }

    pub fn with_exception_capable(mut self, exception_capable: bool) -> Self {
        self.exception_capable = exception_capable;
        self
    }

    fn matches(&self, req: &PolicyRequest) -> bool {
        if !tool_matches(&self.tool_match, &req.tool) {
            return false;
        }

        if self.conditions.is_empty() {
            return true;
        }

        self.conditions.iter().all(|c| c.matches(&req.attributes))
    }
}

/// Complete result of one PDP pass. Matching IDs are returned in stable
/// identifier order while decision still follows deny/allow/ask precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEvaluation {
    pub decision: PermissionDecision,
    pub matched_rule_ids: Vec<String>,
    pub rejected_rule_ids: Vec<String>,
    pub rule_id: Option<String>,
}

fn tool_matches(pattern: &str, tool: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    // Tool names are case-insensitive so a rule written `Bash` still matches `bash`.
    // `Bash` is the shell tool's other name (the permission rules and `denial_tracking` already
    // treat them as one), so an ABAC rule written for either matches the `shell` tool (POL-12).
    let alias = |name: String| {
        if name == "bash" {
            "shell".to_string()
        } else {
            name
        }
    };
    let pattern = alias(pattern.to_ascii_lowercase());
    let tool = alias(tool.to_ascii_lowercase());
    if pattern == tool {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("*") {
        return tool.starts_with(prefix);
    }
    false
}

/// Policy Decision Point (PDP): evaluates requests against ABAC rules.
///
/// The PDP uses a first-match-wins strategy: the first rule whose tool and
/// conditions match determines the decision. If no rules match, the default
/// is `PermissionDecision::Ask`.
#[derive(Debug, Clone)]
pub struct PolicyDecisionPoint {
    rules: Vec<PolicyRule>,
}

impl PolicyDecisionPoint {
    /// Create a new empty PDP.
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Add a rule to the PDP.
    pub fn add_rule(&mut self, rule: PolicyRule) {
        self.rules.push(rule);
    }

    /// Evaluate a request against all rules and return the decision.
    ///
    /// Returns the effect of the first matching rule, or `PermissionDecision::Ask`
    /// if no rules match.
    pub fn evaluate(&self, req: &PolicyRequest) -> PermissionDecision {
        for rule in &self.rules {
            if rule.matches(req) {
                return rule.effect;
            }
        }
        PermissionDecision::Ask
    }

    /// Return the first matching rule for a request, if any.
    ///
    /// First-match-wins: the first rule whose tool and conditions match is
    /// returned. Used to surface `policy_source: "abac:<rule-id>"` in the
    /// permission verdict without losing the rule identity.
    pub fn matched_rule(&self, req: &PolicyRequest) -> Option<&PolicyRule> {
        self.rules.iter().find(|r| r.matches(req))
    }

    /// Evaluate and return the decision together with the matched rule id.
    ///
    /// Returns `None` when no rule matches (caller should fall through to
    /// `RuleStore` / mode policy). When a rule matches, returns
    /// `(decision, rule_id)` so the caller can log `policy_source: "abac:<id>"`.
    pub fn evaluate_with_rule(&self, req: &PolicyRequest) -> Option<(PermissionDecision, String)> {
        self.matched_rule(req).map(|r| (r.effect, r.id.clone()))
    }

    /// Evaluate all matching rules once, retaining stable-ID evidence and
    /// Legion-compatible exception precedence for embedded adapters.
    pub fn evaluate_with_evidence(&self, req: &PolicyRequest) -> PolicyEvaluation {
        let mut matched: Vec<&PolicyRule> =
            self.rules.iter().filter(|rule| rule.matches(req)).collect();
        matched.sort_by(|left, right| left.id.cmp(&right.id));
        let matched_rule_ids = matched
            .iter()
            .map(|rule| rule.id.clone())
            .collect::<Vec<_>>();
        let denied = matched
            .iter()
            .copied()
            .filter(|rule| rule.effect == PermissionDecision::Deny)
            .collect::<Vec<_>>();
        let allowed = matched
            .iter()
            .copied()
            .filter(|rule| rule.effect == PermissionDecision::Allow)
            .collect::<Vec<_>>();
        let exception_present = req.attributes.iter().any(|attribute| {
            attribute.key == "exception" && attribute.value.eq_ignore_ascii_case("true")
        });
        let terminal_denied = denied
            .iter()
            .copied()
            .filter(|rule| !rule.exception_capable || !exception_present)
            .collect::<Vec<_>>();
        let eligible = matched
            .iter()
            .copied()
            .filter(|rule| {
                !(rule.effect == PermissionDecision::Deny
                    && rule.exception_capable
                    && exception_present)
            })
            .collect::<Vec<_>>();
        let rejected_rule_ids = denied
            .iter()
            .map(|rule| rule.id.clone())
            .collect::<Vec<_>>();
        let selected = if !terminal_denied.is_empty() {
            terminal_denied.first().copied()
        } else if let Some(rule) = allowed.first().copied() {
            Some(rule)
        } else {
            eligible.first().copied()
        };
        let decision = match selected {
            Some(rule) if rule.effect == PermissionDecision::Deny => PermissionDecision::Deny,
            Some(rule) if rule.effect == PermissionDecision::Allow => PermissionDecision::Allow,
            Some(_) => PermissionDecision::Ask,
            None => PermissionDecision::Ask,
        };
        PolicyEvaluation {
            decision,
            matched_rule_ids,
            rejected_rule_ids,
            rule_id: selected.map(|rule| rule.id.clone()),
        }
    }

    /// Evaluate a request with the workspace trust gate (ENG-89) consulted
    /// first, and every resulting decision appended to `log` with rule
    /// attribution (ENG-93).
    ///
    /// The trust gate check runs *before*, and independently of, the ABAC
    /// rule evaluation and any containment routing performed elsewhere in
    /// the engine: an untrusted/unknown/revoked workspace short-circuits to
    /// `Deny` without consulting `self.rules` at all, because no agent tool
    /// — not even a read-only one covered by an `Allow` rule — may run
    /// until the user makes an explicit trust decision. Once the workspace
    /// is trusted, evaluation proceeds through the normal
    /// [`Self::evaluate_with_evidence`] path.
    ///
    /// `workspace_id` identifies the workspace the request runs in;
    /// `timestamp_ms` is caller-supplied since this crate performs no I/O
    /// and cannot read the clock itself.
    pub fn evaluate_gated(
        &self,
        req: &PolicyRequest,
        workspace_id: &str,
        gate: &WorkspaceTrustGate,
        log: &mut DecisionLog,
        timestamp_ms: u64,
    ) -> PolicyEvaluation {
        let gate_outcome = gate.check(workspace_id);
        if !gate_outcome.allowed {
            log.append(
                timestamp_ms,
                PermissionDecision::Deny,
                gate_outcome.rule_id.clone(),
                req.tool.clone(),
                vec![("workspace".to_string(), workspace_id.to_string())],
            );
            return PolicyEvaluation {
                decision: PermissionDecision::Deny,
                matched_rule_ids: Vec::new(),
                rejected_rule_ids: Vec::new(),
                rule_id: Some(gate_outcome.rule_id),
            };
        }

        let evaluation = self.evaluate_with_evidence(req);
        let attribution = evaluation
            .rule_id
            .clone()
            .unwrap_or_else(|| "default:ask".to_string());
        log.append(
            timestamp_ms,
            evaluation.decision,
            attribution,
            req.tool.clone(),
            vec![("workspace".to_string(), workspace_id.to_string())],
        );
        evaluation
    }

    /// Number of rules in this PDP.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the PDP has no rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Parse a simple YAML-like format and build a PDP.
    ///
    /// Format:
    /// ```text
    /// rules:
    ///   - id: rule1
    ///     effect: allow
    ///     tool: Read
    ///     conditions:
    ///       attribute: env
    ///       op: eq
    ///       value: prod
    /// ```
    ///
    /// Lines starting with `#` are comments. Hand-rolled parser, no external deps.
    ///
    /// Strict (audit 2026-11-05 round 2, M1): every line must be one of the
    /// keys above. An unknown or misspelt key, a malformed `- id:` line, a
    /// rule without `effect:`, a key given twice in one rule, a repeated rule
    /// id and a text with no rule at all are errors, because skipping any of
    /// them loads a rule that grants more than its author wrote.
    pub fn from_yaml_like(text: &str) -> Result<Self, String> {
        let mut pdp = PolicyDecisionPoint::new();
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| {
                let trimmed = l.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
            .collect();

        let mut seen_ids: Vec<String> = Vec::new();
        let mut seen_rules_header = false;
        let mut idx = 0;
        while idx < lines.len() {
            let line = lines[idx].trim();

            if line.starts_with("- id:") {
                let id = line.strip_prefix("- id:").unwrap_or("").trim().to_string();
                idx += 1;

                let mut effect: Option<PermissionDecision> = None;
                let mut tool_match: Option<String> = None;
                let mut conditions = Vec::new();

                while idx < lines.len() {
                    let l = lines[idx].trim();

                    if l.starts_with("- id:") {
                        break;
                    }
                    if l.starts_with('-') {
                        return Err(format!(
                            "rule '{id}': malformed line '{l}' (a rule starts with `- id: <name>`)"
                        ));
                    }

                    if l.starts_with("effect:") {
                        if effect.is_some() {
                            return Err(format!("rule '{id}': `effect:` is given twice"));
                        }
                        let e = l
                            .strip_prefix("effect:")
                            .unwrap_or("")
                            .trim()
                            .to_lowercase();
                        effect = Some(match e.as_str() {
                            "allow" => PermissionDecision::Allow,
                            "deny" => PermissionDecision::Deny,
                            "ask" => PermissionDecision::Ask,
                            // Audit 2026-11-05 policy M1/M2: a typo must not
                            // silently weaken a rule (a misspelt `deny`
                            // became `ask`).
                            other => {
                                return Err(format!(
                                    "rule '{id}': unknown effect '{other}' (expected allow, deny or ask)"
                                ))
                            }
                        });
                    } else if l.starts_with("tool:") {
                        if tool_match.is_some() {
                            return Err(format!("rule '{id}': `tool:` is given twice"));
                        }
                        tool_match = Some(l.strip_prefix("tool:").unwrap_or("").trim().to_string());
                    } else if let Some(rest) = l.strip_prefix("conditions:") {
                        if !rest.trim().is_empty() {
                            return Err(format!(
                                "rule '{id}': `conditions:` takes no inline value; list `attribute:`, `op:` and `value:` lines under it"
                            ));
                        }
                    } else if l.starts_with("attribute:") {
                        let attr = l
                            .strip_prefix("attribute:")
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        idx += 1;

                        // Audit 2026-11-05 policy M1: an unknown or missing
                        // operator used to become `Eq`, so a deny written
                        // with `op: starts_with` matched only the exact
                        // value. Refuse the policy instead.
                        let l_op = lines.get(idx).map(|l| l.trim()).unwrap_or("");
                        let Some(op_str) = l_op.strip_prefix("op:") else {
                            return Err(format!(
                                "rule '{id}': attribute '{attr}' must be followed by an `op:` line"
                            ));
                        };
                        let op = match op_str.trim().to_lowercase().as_str() {
                            "eq" => CondOp::Eq,
                            "noteq" => CondOp::NotEq,
                            "contains" => CondOp::Contains,
                            "startswith" => CondOp::StartsWith,
                            "startswithany" => CondOp::StartsWithAny,
                            "in" => CondOp::In,
                            other => {
                                return Err(format!(
                                    "rule '{id}': unknown condition op '{other}' (expected eq, noteq, contains, startswith, startswithany or in)"
                                ))
                            }
                        };
                        idx += 1;
                        let l_val = lines.get(idx).map(|l| l.trim()).unwrap_or("");
                        let Some(value) = l_val.strip_prefix("value:") else {
                            return Err(format!(
                                "rule '{id}': attribute '{attr}' must be followed by a `value:` line"
                            ));
                        };
                        let value = value.trim().to_string();

                        if attr.is_empty() {
                            return Err(format!("rule '{id}': `attribute:` has no name"));
                        }
                        conditions.push(Condition::new(attr, op, value));
                    } else {
                        // A misspelt key (`atribute:`, `efect:`) used to be
                        // skipped, so the rule lost a condition or its effect.
                        return Err(format!(
                            "rule '{id}': unknown line '{l}' (expected effect:, tool:, conditions:, attribute:, op: or value:)"
                        ));
                    }

                    idx += 1;
                }

                // Audit 2026-11-05 policy M2: a rule missing its id or tool
                // used to vanish while the load still succeeded.
                if id.is_empty() {
                    return Err("a rule has an empty `- id:`".to_string());
                }
                if seen_ids.iter().any(|seen| seen == &id) {
                    return Err(format!("rule id '{id}' is used twice"));
                }
                let Some(effect) = effect else {
                    return Err(format!("rule '{id}' has no `effect:` line"));
                };
                let tool_match = tool_match.unwrap_or_default();
                if tool_match.is_empty() {
                    return Err(format!("rule '{id}' has no `tool:` line"));
                }
                let mut rule = PolicyRule::new(id.clone(), effect, tool_match);
                for cond in conditions {
                    rule = rule.with_condition(cond);
                }
                pdp.add_rule(rule);
                seen_ids.push(id);
            } else if let Some(rest) = line.strip_prefix("rules:") {
                if seen_rules_header || !pdp.is_empty() || !rest.trim().is_empty() {
                    return Err(format!(
                        "malformed line '{line}' (`rules:` appears once, first, with the rules listed under it)"
                    ));
                }
                seen_rules_header = true;
                idx += 1;
            } else {
                // Anything outside a rule (`-id: r2`, `-  id: r2`, a key
                // before the first rule, flow-style YAML) is not understood.
                return Err(format!(
                    "malformed line '{line}' (expected `rules:` or `- id: <name>`)"
                ));
            }
        }

        // A policy with no rules enforces nothing, which is never what a
        // written policy file means (real YAML in flow style, for example).
        if pdp.is_empty() {
            return Err(
                "the policy has no rules (each rule starts with `- id: <name>`)".to_string(),
            );
        }
        Ok(pdp)
    }

    /// The deny-all policy installed when a configured ABAC policy exists but
    /// cannot be read or parsed (audit 2026-11-05 round 2, M2).
    ///
    /// Every tool call is denied, except a call whose `path` is one of
    /// `recovery_paths` (the file to fix): that one is `Ask`, so the user can
    /// approve the agent reading or repairing the broken policy instead of
    /// being locked out. The deny rule's id carries the source, the error and
    /// the fix, so every denial tells the user which file to correct and that
    /// the policy is loaded only when a session starts.
    pub fn fail_closed(
        source: &str,
        error: &str,
        fix_target: &str,
        recovery_paths: &[String],
    ) -> Self {
        let mut pdp = PolicyDecisionPoint::new();
        for (index, path) in recovery_paths
            .iter()
            .map(|path| path.trim())
            .filter(|path| !path.is_empty())
            .enumerate()
        {
            // File tools only: the generic `path` attribute is read from any tool's arguments, so a rule on "*"
            // would let a `shell` call that merely carries a `path` argument ask instead of being denied.
            for tool in RECOVERY_FILE_TOOLS {
                pdp.add_rule(
                    PolicyRule::new(
                        format!("{POLICY_INVALID_RULE_PREFIX}-recovery-{index}: approve each read or edit of {path} to repair the policy"),
                        PermissionDecision::Ask,
                        *tool,
                    )
                    .with_condition(Condition::new("path", CondOp::Eq, path)),
                );
            }
        }
        pdp.add_rule(PolicyRule::new(
            format!(
                "{POLICY_INVALID_RULE_PREFIX}: {source} could not be loaded ({error}); every tool call is denied. Fix {fix_target}, then start a new session (the policy is read only when a session starts)."
            ),
            PermissionDecision::Deny,
            "*",
        ));
        pdp
    }
}

/// The only tools a broken-policy recovery rule may ask for.
const RECOVERY_FILE_TOOLS: &[&str] = &["read_file", "edit_file", "write_file", "multi_edit"];

/// Rule-id prefix of the policy [`PolicyDecisionPoint::fail_closed`] builds.
pub const POLICY_INVALID_RULE_PREFIX: &str = "policy-invalid";

impl Default for PolicyDecisionPoint {
    fn default() -> Self {
        Self::new()
    }
}
