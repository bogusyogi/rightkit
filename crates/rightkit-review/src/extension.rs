//! Consumer-defined rules. A consumer registers a rule under a kind name and
//! names that kind in a `RuleKind::Extension`. The rule receives the subject and
//! the rule's JSON parameters as data, and returns one verdict. An unregistered
//! kind is inconclusive, so it blocks when the rule is required.

use std::collections::BTreeMap;

use crate::subject::Subject;
use crate::verdict::Verdict;
use crate::ReviewError;

/// A consumer rule. Pure: the same subject and parameters must give the same verdict.
pub trait ExtensionRule {
    fn check(&self, subject: &Subject, params: &serde_json::Value) -> Verdict;
}

impl<F> ExtensionRule for F
where
    F: Fn(&Subject, &serde_json::Value) -> Verdict,
{
    fn check(&self, subject: &Subject, params: &serde_json::Value) -> Verdict {
        self(subject, params)
    }
}

/// The consumer rules a review can resolve, keyed by kind name. Kind names
/// starting with `det.` are reserved for this crate and are refused.
#[derive(Default)]
pub struct RuleRegistry {
    rules: BTreeMap<String, Box<dyn ExtensionRule>>,
}

impl RuleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `rule` under `kind`. Refuses an empty name, a `det.` name, and a name already taken.
    pub fn register<R>(&mut self, kind: &str, rule: R) -> Result<(), ReviewError>
    where
        R: ExtensionRule + 'static,
    {
        if kind.trim().is_empty() {
            return Err(ReviewError::InvalidRuleSet(
                "extension kind is empty".to_string(),
            ));
        }
        if kind.starts_with("det.") {
            return Err(ReviewError::InvalidRuleSet(format!(
                "extension kind `{kind}` uses the reserved det. namespace"
            )));
        }
        if self.rules.contains_key(kind) {
            return Err(ReviewError::InvalidRuleSet(format!(
                "extension kind `{kind}` is already registered"
            )));
        }
        self.rules.insert(kind.to_string(), Box::new(rule));
        Ok(())
    }

    pub fn contains(&self, kind: &str) -> bool {
        self.rules.contains_key(kind)
    }

    pub(crate) fn get(&self, kind: &str) -> Option<&dyn ExtensionRule> {
        self.rules.get(kind).map(|rule| rule.as_ref())
    }
}
