//! Native Rust dispatch for the CLI `rules` verb
//! (`blueprint/scripts/cli/commands.mjs:458` case `"rules"`).
//!
//! Lane V3 (r5 closure). The legacy `rules` verb reads
//! `<repoRoot>/blueprint.rules.yml`, parses it with
//! `blueprint/src/lib/rules/parser.mjs`'s `parseRules` (already ported at
//! [`crate::lib_rules_parser::parse_rules`]), and for every subcommand
//! (`check`, `baseline`, `explain`) reports the parsed rule inventory —
//! legacy never wires graph evaluation into this CLI verb (evaluation is a
//! separate programmatic path via
//! [`crate::lib_rules_evaluate::evaluate_rules`]), so this port matches
//! that exact scope: file discovery, parsing, and the same three
//! deterministic inventory payload shapes, with the same `rules_missing`
//! typed error when `blueprint.rules.yml` is absent.

use crate::lib_rules_parser::parse_rules;
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum RulesCliError {
    /// Mirrors `machineError("rules_missing", ...)` + `EXIT.USAGE`.
    RulesMissing,
    /// Mirrors `machineError("usage", "blueprint rules <sub> is not a known subcommand")`.
    UnknownSubcommand(String),
}

/// Mirrors the `case "rules"` dispatch body for every known subcommand
/// (`check` is the default when none is given, matching
/// `args._[0] ?? args.subcommand ?? "check"`). Returns `Err` for an unknown
/// subcommand, mirroring the legacy `usage` machine error.
pub fn run(repo_root: &Path, subcommand: Option<&str>) -> Result<Value, RulesCliError> {
    let subcommand = subcommand.unwrap_or("check");
    let rules_path = repo_root.join("blueprint.rules.yml");
    if !rules_path.exists() {
        return Err(RulesCliError::RulesMissing);
    }
    let source = fs::read_to_string(&rules_path).map_err(|_| RulesCliError::RulesMissing)?;
    let parsed = parse_rules(&source);

    match subcommand {
        "check" => Ok(serde_json::json!({
            "schemaVersion": 1,
            "ruleCount": parsed.rules.len(),
            "rules": parsed.rules.iter().map(|r| serde_json::json!({"id": r.id, "severity": r.severity})).collect::<Vec<_>>(),
        })),
        "baseline" | "explain" => Ok(serde_json::json!({
            "schemaVersion": 1,
            "command": subcommand,
            "ruleCount": parsed.rules.len(),
        })),
        other => Err(RulesCliError::UnknownSubcommand(other.to_string())),
    }
}

impl RulesCliError {
    pub fn code(&self) -> &'static str {
        match self {
            RulesCliError::RulesMissing => "rules_missing",
            RulesCliError::UnknownSubcommand(_) => "usage",
        }
    }
    pub fn message(&self) -> String {
        match self {
            RulesCliError::RulesMissing => {
                "blueprint.rules.yml not found in repository root".to_string()
            }
            RulesCliError::UnknownSubcommand(sub) => {
                format!("blueprint rules {sub} is not a known subcommand")
            }
        }
    }
}
