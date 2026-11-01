use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::rules::{Context, Parameter, Rule, RuleError, Value};

/// A set of rules that resolves effective editing parameters for a context.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleMatrix {
    #[serde(default)]
    rules: Vec<Rule>,
}

/// The winning value for one parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub value: Value,
    pub rule_id: String,
    pub scope_rank: u8,
    pub priority: i32,
}

/// Top-tier rules that set one parameter to different values. The parameter
/// has no effective value and must be decided by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub parameter: Parameter,
    pub scope_rank: u8,
    pub priority: i32,
    /// Conflicting rule ids, sorted.
    pub rule_ids: Vec<String>,
    /// Values in the same order as `rule_ids`.
    pub values: Vec<Value>,
}

/// An applicable rule that lost to a higher-precedence rule for the same parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overridden {
    pub parameter: Parameter,
    pub rule_id: String,
    /// Ids of the winning rules.
    pub by: Vec<String>,
}

/// The result of resolving a matrix for one context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    pub effective: BTreeMap<Parameter, Effective>,
    pub conflicts: Vec<Conflict>,
    pub overridden: Vec<Overridden>,
}

impl Resolution {
    pub fn value(&self, parameter: Parameter) -> Option<&Value> {
        self.effective.get(&parameter).map(|e| &e.value)
    }

    /// True when no parameter has a disagreeing top tier.
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

impl RuleMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a matrix, validating every rule and rejecting duplicate ids.
    pub fn from_rules(rules: Vec<Rule>) -> Result<Self, RuleError> {
        let mut matrix = Self::new();
        for rule in rules {
            matrix.add(rule)?;
        }
        Ok(matrix)
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Adds one validated rule. Rejects a duplicate id.
    pub fn add(&mut self, rule: Rule) -> Result<(), RuleError> {
        rule.validate()?;
        if self.rules.iter().any(|r| r.id == rule.id) {
            return Err(RuleError::DuplicateId(rule.id));
        }
        self.rules.push(rule);
        Ok(())
    }

    /// Validates every rule and checks ids are unique. Use after deserialising,
    /// because serde does not run `add`.
    pub fn validate(&self) -> Result<(), RuleError> {
        let mut seen: Vec<&str> = Vec::with_capacity(self.rules.len());
        for rule in &self.rules {
            rule.validate()?;
            if seen.contains(&rule.id.as_str()) {
                return Err(RuleError::DuplicateId(rule.id.clone()));
            }
            seen.push(rule.id.as_str());
        }
        Ok(())
    }

    /// Resolves the effective value of each parameter for `ctx`.
    ///
    /// Precedence per parameter: higher scope rank, then higher priority. Rules
    /// tied on both must agree or the parameter is reported as a conflict.
    /// Losing rules are listed as overridden. Output is deterministic and does
    /// not depend on insertion order.
    pub fn resolve(&self, ctx: &Context) -> Resolution {
        let mut by_parameter: BTreeMap<Parameter, Vec<&Rule>> = BTreeMap::new();
        for rule in self.rules.iter().filter(|r| r.applies(ctx)) {
            by_parameter
                .entry(rule.effect.parameter)
                .or_default()
                .push(rule);
        }

        let mut out = Resolution::default();
        for (parameter, mut rules) in by_parameter {
            rules.sort_by(|a, b| {
                (b.scope.rank(), b.priority)
                    .cmp(&(a.scope.rank(), a.priority))
                    .then_with(|| a.id.cmp(&b.id))
            });

            let top = (rules[0].scope.rank(), rules[0].priority);
            let winners: Vec<&Rule> = rules
                .iter()
                .filter(|r| (r.scope.rank(), r.priority) == top)
                .copied()
                .collect();
            // Sorted descending, so the top tier is exactly the leading prefix.
            let losers = &rules[winners.len()..];

            let first_value = winners[0].effect.value.clone();
            if winners.iter().all(|r| r.effect.value == first_value) {
                out.effective.insert(
                    parameter,
                    Effective {
                        value: first_value,
                        rule_id: winners[0].id.clone(),
                        scope_rank: top.0,
                        priority: top.1,
                    },
                );
                // Agreeing top-tier rules are not conflicts, but only the lowest id
                // is effective, so the rest are shadowed and reported as overridden.
                let by: Vec<String> = vec![winners[0].id.clone()];
                for shadowed in &winners[1..] {
                    out.overridden.push(Overridden {
                        parameter,
                        rule_id: shadowed.id.clone(),
                        by: by.clone(),
                    });
                }
            } else {
                let mut ids: Vec<(String, Value)> = winners
                    .iter()
                    .map(|r| (r.id.clone(), r.effect.value.clone()))
                    .collect();
                ids.sort_by(|a, b| a.0.cmp(&b.0));
                out.conflicts.push(Conflict {
                    parameter,
                    scope_rank: top.0,
                    priority: top.1,
                    rule_ids: ids.iter().map(|(id, _)| id.clone()).collect(),
                    values: ids.into_iter().map(|(_, v)| v).collect(),
                });
            }

            let by: Vec<String> = winners.iter().map(|r| r.id.clone()).collect();
            for loser in losers {
                out.overridden.push(Overridden {
                    parameter,
                    rule_id: loser.id.clone(),
                    by: by.clone(),
                });
            }
        }
        out
    }
}
