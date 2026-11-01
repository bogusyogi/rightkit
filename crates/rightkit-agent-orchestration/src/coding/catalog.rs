//! The agent catalog — the trigger surface handed to the conductor.
//!
//! Renders roster agent descriptions (always present) followed by any installed
//! skill descriptions, capped to a character budget (Codex renders its tool
//! catalog at ~2% of context). The first sentence of every entry — the trigger
//! intent (see [`crate::coding::trigger`]) — is never truncated; only the prose suffix
//! is, and skills are shortened/dropped before any agent is. Agents are never
//! dropped.

use crate::coding::registry::{AgentDescriptor, AgentRegistry};

/// Coarse path label for an agent. Derived from tier + capabilities (no struct
/// field — Task 9 already infers the same for promoted agents, so one code path
/// serves both). Distinct from [`crate::coding::trigger::Path`]: `Audit` is a sub-stage
/// of an `ArchitectPlan` build, not a top-level route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathHint {
    DirectEdit,
    ArchitectPlan,
    Audit,
}

/// One rendered catalog row.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    pub name: String,
    pub description: String,
    pub path_hint: PathHint,
}

const CODING_CAPS: &[&str] = &[
    "coding",
    "implementation",
    "refactoring",
    "complex-implementation",
    "ui",
    "frontend",
    "styling",
    "components",
];
const AUDIT_CAPS: &[&str] = &[
    "audit",
    "evidence-audit",
    "claim-audit",
    "claim-verification",
    "code-review",
    "security-smell",
];

/// Derive the path hint from an agent's tier + capabilities.
///
// ponytail: heuristic label off tier+caps, not a stored field. Add a struct
// field only if a builtin ever needs to override the derivation (none does).
pub fn path_hint_for(a: &AgentDescriptor) -> PathHint {
    let has_cap = |set: &[&str]| a.capabilities.iter().any(|c| set.contains(&c.as_str()));
    if a.name.contains("audit") || has_cap(AUDIT_CAPS) {
        PathHint::Audit
    } else if a.escalation_tier <= 2
        && a.allowed_tools.iter().any(|t| t == "apply_patch")
        && has_cap(CODING_CAPS)
    {
        // A leaf implementer (coder/engineer tier 1, ui-coder tier 2). A tier-3
        // lead like the architect plans even when it can also patch code.
        PathHint::DirectEdit
    } else {
        PathHint::ArchitectPlan
    }
}

/// The agent roster as catalog entries, in registry order.
pub fn roster_entries(registry: &AgentRegistry) -> Vec<CatalogEntry> {
    registry
        .all()
        .iter()
        .map(|a| CatalogEntry {
            name: a.name.clone(),
            description: a.tool_description.clone(),
            path_hint: path_hint_for(a),
        })
        .collect()
}

/// First sentence (up to and including the first `.`), else the whole string.
/// This is the trigger intent that must survive any truncation.
fn first_sentence(s: &str) -> &str {
    match s.find(". ") {
        Some(i) => &s[..=i],
        None => s,
    }
}

fn render_row(name: &str, desc: &str) -> String {
    format!("- {name}: {desc}\n")
}

/// Build the catalog string: agents first (never dropped, first sentence always
/// intact), then skills (shortened to first sentence, then dropped, before any
/// agent is touched), within `budget_chars`. Appends a shortened-note when it
/// truncates anything.
pub fn build_catalog(
    agents: &[CatalogEntry],
    skills: &[CatalogEntry],
    budget_chars: usize,
) -> String {
    let mut out = String::new();
    let mut truncated = false;

    // Agents: always rendered. Full description if it fits the running budget,
    // else the first sentence — which is rendered even if it overflows (an
    // agent is never dropped).
    for a in agents {
        let full = render_row(&a.name, &a.description);
        if out.len() + full.len() <= budget_chars {
            out.push_str(&full);
        } else {
            out.push_str(&render_row(&a.name, first_sentence(&a.description)));
            truncated = true;
        }
    }

    // Skills: appended while budget remains. Full, then first-sentence, then
    // dropped (and all subsequent skills with it).
    for s in skills {
        let full = render_row(&s.name, &s.description);
        if out.len() + full.len() <= budget_chars {
            out.push_str(&full);
            continue;
        }
        let short = render_row(&s.name, first_sentence(&s.description));
        if out.len() + short.len() <= budget_chars {
            out.push_str(&short);
            truncated = true;
        } else {
            truncated = true;
            break;
        }
    }

    if truncated {
        out.push_str("(descriptions shortened to fit budget)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, desc: &str, hint: PathHint) -> CatalogEntry {
        CatalogEntry {
            name: name.into(),
            description: desc.into(),
            path_hint: hint,
        }
    }

    #[test]
    fn path_hint_matches_the_real_roster() {
        let reg = AgentRegistry::with_builtins();
        let hint = |n: &str| path_hint_for(reg.get(n).unwrap());
        assert_eq!(hint("coder"), PathHint::DirectEdit);
        assert_eq!(hint("engineer"), PathHint::DirectEdit);
        assert_eq!(hint("ui-coder"), PathHint::DirectEdit);
        assert_eq!(hint("auditor"), PathHint::Audit);
        assert_eq!(hint("architect"), PathHint::ArchitectPlan);
        // A content writer with write_file but no coding cap is NOT a DirectEdit target.
        assert_eq!(hint("writer"), PathHint::ArchitectPlan);
    }

    #[test]
    fn every_roster_entry_has_a_trigger_description() {
        let reg = AgentRegistry::with_builtins();
        for e in roster_entries(&reg) {
            assert!(
                !e.description.trim().is_empty(),
                "{} has an empty trigger description",
                e.name
            );
        }
        // The four routing-target descriptions lead with the trigger intent.
        let desc = |n: &str| reg.get(n).unwrap().tool_description.clone();
        assert!(desc("coder").starts_with("Use for a small"));
        assert!(desc("architect").starts_with("Use to design"));
        assert!(desc("auditor").starts_with("Use at the end"));
    }

    #[test]
    fn roster_entries_present_and_ordered() {
        let reg = AgentRegistry::with_builtins();
        let entries = roster_entries(&reg);
        assert_eq!(entries.len(), reg.len());
        assert_eq!(entries[0].name, reg.all()[0].name);
        let cat = build_catalog(&entries, &[], 100_000);
        assert!(cat.contains("- coder:"));
        assert!(cat.contains("- architect:"));
        assert!(!cat.contains("shortened")); // generous budget → no note
    }

    #[test]
    fn skills_are_appended_after_agents() {
        let agents = vec![entry("coder", "Small change.", PathHint::DirectEdit)];
        let skills = vec![entry(
            "seo",
            "Audit and improve SEO.",
            PathHint::ArchitectPlan,
        )];
        let cat = build_catalog(&agents, &skills, 100_000);
        let coder_at = cat.find("- coder:").unwrap();
        let seo_at = cat.find("- seo:").unwrap();
        assert!(coder_at < seo_at, "agents must render before skills");
    }

    #[test]
    fn over_budget_shortens_skills_first_keeps_agent_first_sentence() {
        let agents = vec![entry(
            "coder",
            "Small localized change. Lots of extra prose that should be cut under budget pressure.",
            PathHint::DirectEdit,
        )];
        let skills = vec![entry(
            "seo",
            "SEO audit. A long tail of description text that will be dropped entirely.",
            PathHint::ArchitectPlan,
        )];
        // Budget big enough for the agent's first sentence but not the skill.
        let cat = build_catalog(&agents, &skills, 40);
        assert!(cat.contains("- coder: Small localized change."));
        assert!(!cat.contains("extra prose")); // agent suffix truncated
        assert!(cat.contains("shortened")); // note emitted
    }

    #[test]
    fn agents_never_dropped_even_when_first_sentence_exceeds_budget() {
        let agents = vec![
            entry("coder", "A.", PathHint::DirectEdit),
            entry("architect", "B.", PathHint::ArchitectPlan),
        ];
        // Budget of 1 char — smaller than either row. Both agents still appear.
        let cat = build_catalog(&agents, &[], 1);
        assert!(cat.contains("- coder:"));
        assert!(cat.contains("- architect:"));
        assert!(cat.contains("shortened"));
    }
}
