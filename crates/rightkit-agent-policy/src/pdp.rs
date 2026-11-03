use crate::types::PermissionDecision;

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
    pub fn from_yaml_like(text: &str) -> Result<Self, String> {
        let mut pdp = PolicyDecisionPoint::new();
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| {
                let trimmed = l.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
            .collect();

        let mut idx = 0;
        while idx < lines.len() {
            let line = lines[idx].trim();

            if line.starts_with("- id:") {
                let id = line.strip_prefix("- id:").unwrap_or("").trim().to_string();
                idx += 1;

                let mut effect = PermissionDecision::Ask;
                let mut tool_match = String::new();
                let mut conditions = Vec::new();

                while idx < lines.len() {
                    let l = lines[idx].trim();

                    if l.starts_with("- id:") {
                        break;
                    }

                    if l.starts_with("effect:") {
                        let e = l
                            .strip_prefix("effect:")
                            .unwrap_or("")
                            .trim()
                            .to_lowercase();
                        effect = match e.as_str() {
                            "allow" => PermissionDecision::Allow,
                            "deny" => PermissionDecision::Deny,
                            _ => PermissionDecision::Ask,
                        };
                    } else if l.starts_with("tool:") {
                        tool_match = l.strip_prefix("tool:").unwrap_or("").trim().to_string();
                    } else if l.starts_with("attribute:") {
                        let attr = l
                            .strip_prefix("attribute:")
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        idx += 1;
                        if idx >= lines.len() {
                            break;
                        }

                        let l_op = lines[idx].trim();
                        let op = if l_op.starts_with("op:") {
                            let op_str =
                                l_op.strip_prefix("op:").unwrap_or("").trim().to_lowercase();
                            match op_str.as_str() {
                                "eq" => CondOp::Eq,
                                "noteq" => CondOp::NotEq,
                                "contains" => CondOp::Contains,
                                "startswith" => CondOp::StartsWith,
                                "startswithany" => CondOp::StartsWithAny,
                                "in" => CondOp::In,
                                _ => CondOp::Eq,
                            }
                        } else {
                            CondOp::Eq
                        };
                        idx += 1;
                        if idx >= lines.len() {
                            break;
                        }

                        let l_val = lines[idx].trim();
                        let value = if l_val.starts_with("value:") {
                            l_val
                                .strip_prefix("value:")
                                .unwrap_or("")
                                .trim()
                                .to_string()
                        } else {
                            String::new()
                        };

                        conditions.push(Condition::new(attr, op, value));
                    }

                    idx += 1;
                }

                if !id.is_empty() && !tool_match.is_empty() {
                    let mut rule = PolicyRule::new(id, effect, tool_match);
                    for cond in conditions {
                        rule = rule.with_condition(cond);
                    }
                    pdp.add_rule(rule);
                }
            } else {
                idx += 1;
            }
        }

        Ok(pdp)
    }
}

impl Default for PolicyDecisionPoint {
    fn default() -> Self {
        Self::new()
    }
}
