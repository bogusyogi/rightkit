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
