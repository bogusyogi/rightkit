//! Path-affinity routing: maps file glob patterns to preferred skill slugs.
//!
//! Skills can declare affinity for particular file types. When the runtime
//! knows which file a trigger fired on, it calls [`PathAffinityRouter::route`]
//! to retrieve the ordered list of skills that prefer that file type.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// A rule stored inside [`PathAffinityRouter`].
struct AffinityRule {
    /// The compiled glob set (always a single-pattern set for clean error sites).
    glob_set: GlobSet,
    /// Human-readable pattern string kept for diagnostics.
    pattern: String,
    /// The skill slug this rule routes to.
    skill_slug: String,
}

/// Routes a file path to the set of skills that have declared affinity for it.
///
/// Rules are evaluated in insertion order; all matching slugs are returned.
///
/// ```rust
/// use crate::routing::PathAffinityRouter;
///
/// let mut router = PathAffinityRouter::new();
/// router.add_rule("*.rs", "rust-expert").unwrap();
/// router.add_rule("*.tsx", "react-expert").unwrap();
///
/// assert_eq!(router.route("src/main.rs"), vec!["rust-expert"]);
/// assert_eq!(router.route("App.tsx"), vec!["react-expert"]);
/// assert!(router.route("README.md").is_empty());
/// ```
pub struct PathAffinityRouter {
    rules: Vec<AffinityRule>,
}

impl PathAffinityRouter {
    /// Create an empty router.
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Register a glob pattern → skill slug mapping.
    ///
    /// `pattern` follows gitignore-style glob syntax (via `globset`):
    /// `*` matches within a path component, `**` matches across components.
    ///
    /// Returns an error if the pattern fails to compile.
    pub fn add_rule(&mut self, pattern: &str, skill_slug: &str) -> Result<(), String> {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
            .map_err(|e| format!("invalid glob pattern '{pattern}': {e}"))?;
        let glob_set = GlobSetBuilder::new()
            .add(glob)
            .build()
            .map_err(|e| format!("failed to build glob set for '{pattern}': {e}"))?;
        self.rules.push(AffinityRule {
            glob_set,
            pattern: pattern.to_string(),
            skill_slug: skill_slug.to_string(),
        });
        Ok(())
    }

    /// Return all skill slugs whose glob pattern matches `file_path`, in
    /// insertion order.
    pub fn route(&self, file_path: &str) -> Vec<String> {
        self.rules
            .iter()
            .filter(|rule| rule.glob_set.is_match(file_path))
            .map(|rule| rule.skill_slug.clone())
            .collect()
    }

    /// Return all registered (pattern, skill_slug) pairs for inspection.
    pub fn rules(&self) -> impl Iterator<Item = (&str, &str)> {
        self.rules
            .iter()
            .map(|r| (r.pattern.as_str(), r.skill_slug.as_str()))
    }
}

impl Default for PathAffinityRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rs_matches_rust_expert() {
        let mut router = PathAffinityRouter::new();
        router.add_rule("*.rs", "rust-expert").unwrap();
        assert_eq!(router.route("src/main.rs"), vec!["rust-expert"]);
    }

    #[test]
    fn tsx_matches_react_expert() {
        let mut router = PathAffinityRouter::new();
        router.add_rule("*.tsx", "react-expert").unwrap();
        assert_eq!(router.route("App.tsx"), vec!["react-expert"]);
    }

    #[test]
    fn unmatched_returns_empty() {
        let mut router = PathAffinityRouter::new();
        router.add_rule("*.rs", "rust-expert").unwrap();
        assert!(router.route("README.md").is_empty());
    }

    #[test]
    fn multiple_rules_all_match() {
        let mut router = PathAffinityRouter::new();
        router.add_rule("*.rs", "rust-expert").unwrap();
        router.add_rule("src/**", "src-scope").unwrap();
        let matches = router.route("src/lib.rs");
        assert!(matches.contains(&"rust-expert".to_string()));
        assert!(matches.contains(&"src-scope".to_string()));
    }

    #[test]
    fn insertion_order_preserved() {
        let mut router = PathAffinityRouter::new();
        router.add_rule("*.ts", "ts-first").unwrap();
        router.add_rule("*.ts", "ts-second").unwrap();
        assert_eq!(router.route("index.ts"), vec!["ts-first", "ts-second"]);
    }

    #[test]
    fn invalid_pattern_errors() {
        let mut router = PathAffinityRouter::new();
        // globset does not error on most patterns; use something truly invalid
        // to verify error propagation.
        let result = router.add_rule("src/[invalid", "skill");
        assert!(result.is_err(), "expected pattern error");
    }
}
