//! "Always allow" rule derivation for the approval card.
//!
//! The card offers a persistent allow rule only when the request is a single
//! plain shell command or a single-file edit, and it always states the exact
//! rule patterns that will be installed. This module is pure: it derives and
//! describes; the host owns installation through the existing permission-rule
//! REST API (which validates before install) and owns the deny check.
//!
//! Safety shape:
//! - a command containing shell control syntax (`; & | < > ` $ ( )`, newlines)
//!   is never offered a prefix rule;
//! - interpreters, privilege launchers and destructive utilities never get a
//!   prefix rule (an allow on `bash` or `sudo` would allow anything);
//! - only requests whose policy source is a plain mode/auto-review ask are
//!   eligible: a request held by a rule or ABAC policy is not widened here.

use std::path::PathBuf;

use super::approval::ApprovalKind;

/// A persistent allow rule the card will install, exactly as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowRule {
    /// Wire patterns, installed in order, each with effect `allow`.
    pub patterns: Vec<String>,
    /// Short human label for the card option ("git status …").
    pub label: String,
    /// What the rule covers, in the card's own words.
    pub scope_note: String,
}

impl AllowRule {
    /// One-line statement of exactly what is installed.
    pub fn installs(&self) -> String {
        format!("Installs allow rule: {}", self.patterns.join("  +  "))
    }
}

/// Words that never receive a prefix allow rule: shells, interpreters,
/// privilege/launcher wrappers, and destructive or exfiltrating utilities.
const NEVER_PREFIX: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "csh",
    "tcsh",
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "pwsh",
    "python",
    "python3",
    "py",
    "node",
    "deno",
    "bun",
    "ruby",
    "perl",
    "php",
    "lua",
    "sudo",
    "su",
    "doas",
    "env",
    "exec",
    "eval",
    "xargs",
    "nohup",
    "time",
    "watch",
    "timeout",
    "nice",
    "command",
    "find",
    "npx",
    "pnpx",
    "bunx",
    "uvx",
    "rm",
    "rmdir",
    "del",
    "dd",
    "mkfs",
    "shred",
    "chmod",
    "chown",
    "chgrp",
    "kill",
    "pkill",
    "killall",
    "curl",
    "wget",
    "nc",
    "ncat",
    "ssh",
    "scp",
    "rsync",
    "ftp",
    "telnet",
];

/// Tools whose first argument is a subcommand: the prefix is `tool sub`.
const SUBCOMMAND_TOOLS: &[&str] = &[
    "git", "cargo", "npm", "pnpm", "yarn", "docker", "gh", "kubectl", "go", "dotnet", "pip", "uv",
    "rightkit", "legion", "rustup", "make",
];

const CONTROL_CHARS: &[char] = &[
    ';', '&', '|', '<', '>', '`', '$', '(', ')', '\n', '\r', '\\',
];

fn plain_word(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+' | ':' | '@')
        })
}

/// The command-prefix words an allow rule would cover, or `None` when the
/// command is not eligible.
pub fn shell_prefix_words(command: &str) -> Option<Vec<String>> {
    let command = command.trim();
    if command.is_empty() || command.contains(CONTROL_CHARS) {
        return None;
    }
    let mut words = command.split_whitespace();
    let first = words.next()?;
    if !plain_word(first) || first.starts_with('-') {
        return None;
    }
    let base = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_ascii_lowercase();
    if NEVER_PREFIX.contains(&base.as_str()) {
        return None;
    }
    if SUBCOMMAND_TOOLS.contains(&base.as_str()) {
        // Global flags before the subcommand (`git -c k=v status`) change what
        // the command does; no prefix is offered for them.
        let sub = words.next()?;
        if sub.starts_with('-') || !plain_word(sub) {
            return None;
        }
        return Some(vec![first.to_string(), sub.to_string()]);
    }
    Some(vec![first.to_string()])
}

/// Policy sources that are a plain "ask" and may be offered an allow rule.
fn source_is_plain_ask(policy_source: Option<&str>) -> bool {
    match policy_source.map(str::trim) {
        None | Some("") => true,
        Some(source) => source.starts_with("mode:") || source.starts_with("auto_approval:"),
    }
}

fn path_pattern_ok(path: &str) -> bool {
    !path.trim().is_empty()
        && path.trim() == path
        && !path.contains(['*', '?', '[', ']', '(', ')', '\n', '\r'])
        && !path
            .replace('\\', "/")
            .split('/')
            .any(|seg| seg == ".." || seg == ".")
}

/// Derive the allow rule the card may offer for a request. `None` means the
/// option must not be shown.
pub fn derive_allow_rule(kind: &ApprovalKind, policy_source: Option<&str>) -> Option<AllowRule> {
    if !source_is_plain_ask(policy_source) {
        return None;
    }
    match kind {
        ApprovalKind::Exec { command, .. } => {
            let words = shell_prefix_words(command)?;
            let prefix = words.join(" ");
            Some(AllowRule {
                patterns: vec![format!("shell({prefix})"), format!("shell({prefix} *)")],
                label: format!("{prefix} …"),
                scope_note: format!(
                    "Any shell command starting with `{prefix}` is allowed in this project from now on, \
                     including longer commands that begin with it. A deny rule still wins."
                ),
            })
        }
        ApprovalKind::Patch { changes, .. } => {
            let mut paths = changes.keys();
            let path: &PathBuf = paths.next()?;
            if paths.next().is_some() {
                return None;
            }
            let path = path.display().to_string();
            if !path_pattern_ok(&path) {
                return None;
            }
            Some(AllowRule {
                patterns: vec![format!("file_edit({path})")],
                label: path.clone(),
                scope_note: "Edits to exactly this file are allowed in this project from now on. A deny rule still wins."
                    .to_string(),
            })
        }
        ApprovalKind::Generic { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::diff::FileChange;
    use std::collections::BTreeMap;

    fn exec(command: &str) -> ApprovalKind {
        ApprovalKind::Exec {
            command: command.into(),
            cwd: None,
            reason: None,
            writes: None,
            network: None,
        }
    }

    fn patch(paths: &[&str]) -> ApprovalKind {
        let mut changes = BTreeMap::new();
        for path in paths {
            changes.insert(PathBuf::from(path), FileChange::add("x".to_string()));
        }
        ApprovalKind::Patch {
            changes,
            cwd: PathBuf::new(),
            reason: None,
        }
    }

    #[test]
    fn plain_command_gets_exact_and_prefix_patterns() {
        let rule = derive_allow_rule(&exec("ls -la src"), Some("mode:ask")).expect("offered");
        assert_eq!(rule.patterns, vec!["shell(ls)", "shell(ls *)"]);
        assert_eq!(rule.label, "ls …");
        assert!(rule.installs().contains("shell(ls *)"));
    }

    #[test]
    fn subcommand_tools_keep_the_subcommand() {
        let rule = derive_allow_rule(&exec("git status --short"), None).expect("offered");
        assert_eq!(
            rule.patterns,
            vec!["shell(git status)", "shell(git status *)"]
        );
        let rule = derive_allow_rule(&exec("cargo test -p tui"), None).expect("offered");
        assert_eq!(rule.patterns[1], "shell(cargo test *)");
    }

    #[test]
    fn global_flags_before_a_subcommand_get_no_rule() {
        assert!(derive_allow_rule(&exec("git -c core.pager=x status"), None).is_none());
        assert!(derive_allow_rule(&exec("git"), None).is_none());
    }

    #[test]
    fn control_syntax_never_gets_a_prefix_rule() {
        for command in [
            "ls; rm -rf x",
            "ls && id",
            "cat a | sh",
            "echo $(id)",
            "echo `id`",
            "ls > out",
            "ls\nid",
        ] {
            assert!(
                derive_allow_rule(&exec(command), None).is_none(),
                "{command}"
            );
        }
    }

    #[test]
    fn interpreters_and_launchers_never_get_a_prefix_rule() {
        for command in [
            "bash script.sh",
            "sudo ls",
            "env FOO=1 ls",
            "python3 x.py",
            "node a.js",
            "rm file",
            "curl http://x",
            "/usr/bin/bash -c ls",
            "find . -delete",
            "npx thing",
            "FOO=1 ls",
        ] {
            assert!(
                derive_allow_rule(&exec(command), None).is_none(),
                "{command}"
            );
        }
    }

    #[test]
    fn rule_held_or_abac_requests_are_not_widened() {
        for source in ["rule:session:shell(ls)", "abac:r1", "durable-write:x"] {
            assert!(
                derive_allow_rule(&exec("ls"), Some(source)).is_none(),
                "{source}"
            );
        }
        assert!(derive_allow_rule(&exec("ls"), Some("auto_approval:gray-zone-review")).is_some());
    }

    #[test]
    fn single_file_edit_gets_an_exact_path_rule() {
        let rule = derive_allow_rule(&patch(&["src/lib.rs"]), Some("mode:ask")).expect("offered");
        assert_eq!(rule.patterns, vec!["file_edit(src/lib.rs)"]);
        assert_eq!(rule.label, "src/lib.rs");
    }

    #[test]
    fn multi_file_wildcard_and_dotdot_paths_get_no_rule() {
        assert!(derive_allow_rule(&patch(&["a.rs", "b.rs"]), None).is_none());
        assert!(derive_allow_rule(&patch(&["src/*.rs"]), None).is_none());
        assert!(derive_allow_rule(&patch(&["../etc/passwd"]), None).is_none());
        assert!(derive_allow_rule(&patch(&["a/./b.rs"]), None).is_none());
    }

    #[test]
    fn generic_requests_get_no_rule() {
        let kind = ApprovalKind::Generic {
            description: "network grant".into(),
        };
        assert!(derive_allow_rule(&kind, None).is_none());
    }
}
