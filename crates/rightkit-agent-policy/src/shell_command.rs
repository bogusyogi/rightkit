//! Command-aware shell approval for `AutoSafe` mode.
//!
//! The POSIX/Bash parser and wrapper decomposition in this file are adapted from
//! xAI Grok Build's Apache-2.0 `bash_command_splitting.rs` at commit
//! `8adf9013a0929e5c7f1d4e849492d2387837a28d`. Local modifications make the
//! parser escalation-only, use this crate's permission types, and deliberately keep
//! PowerShell fail-closed. See `../THIRD-PARTY-NOTICES`.

use crate::owning_process::OwningProcess;
use crate::{
    CapabilityClass, PermissionDecision, PermissionEngine, PermissionMode, PermissionVerdict,
    WorkspaceTrust,
};
use tree_sitter::{Node, Parser, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellDialect {
    Posix,
    PowerShell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellCommandClassification {
    Routine,
    RequiresApproval { reason: String, destructive: bool },
}

#[derive(Debug, Clone)]
struct PlainCommand {
    words: Vec<String>,
}

impl PlainCommand {
    fn words(&self) -> &[String] {
        &self.words
    }
}

impl PermissionEngine {
    /// Evaluate a shell call and apply command-aware escalation before AutoSafe
    /// can silently allow it. Bypass remains explicit; catastrophic string guards
    /// are independent and remain enforced by the shell tool before this method.
    pub fn evaluate_shell_command(
        &self,
        mode: PermissionMode,
        trust: WorkspaceTrust,
        command: &str,
        dialect: ShellDialect,
    ) -> PermissionVerdict {
        self.evaluate_shell_command_in(mode, trust, command, dialect, false)
    }

    /// [`Self::evaluate_shell_command`] for a command that will run under an enforcing OS
    /// sandbox (writes confined to the workspace, network denied; the shell refuses to run
    /// when the sandbox cannot start). There AutoSafe also treats interpreters and test
    /// runners as routine, as Codex's workspace sandbox does (decision 2026-11-01). Destructive patterns, redirects outside the
    /// workspace and everything else still escalate.
    pub fn evaluate_shell_command_in(
        &self,
        mode: PermissionMode,
        trust: WorkspaceTrust,
        command: &str,
        dialect: ShellDialect,
        os_sandbox_enforced: bool,
    ) -> PermissionVerdict {
        // Owning-process protection is unconditional: it applies in every mode,
        // including bypass, and cannot be approved.
        if let Some(reason) =
            crate::owning_process::owning_process_kill_reason(command, &OwningProcess::current())
        {
            return PermissionVerdict::new(
                PermissionDecision::Deny,
                reason,
                "shell-command:owning-process-protection",
            );
        }
        if mode != PermissionMode::AutoSafe {
            return self.evaluate(CapabilityClass::Shell, mode, trust, None, false);
        }

        match classify_shell_command_in(command, dialect, os_sandbox_enforced) {
            ShellCommandClassification::Routine => {
                self.evaluate(CapabilityClass::Shell, mode, trust, None, false)
            }
            ShellCommandClassification::RequiresApproval {
                destructive: true, ..
            } => self.evaluate(CapabilityClass::Shell, mode, trust, None, true),
            ShellCommandClassification::RequiresApproval {
                reason,
                destructive: false,
            } => {
                let base = self.evaluate(CapabilityClass::Shell, mode, trust, None, false);
                if base.decision != PermissionDecision::Allow {
                    base
                } else {
                    PermissionVerdict::new(
                        PermissionDecision::Ask,
                        reason,
                        match dialect {
                            ShellDialect::Posix => "shell-command:posix",
                            ShellDialect::PowerShell => "shell-command:powershell-fail-closed",
                        },
                    )
                }
            }
        }
    }
}

pub fn classify_shell_command(command: &str, dialect: ShellDialect) -> ShellCommandClassification {
    classify_shell_command_in(command, dialect, false)
}

/// [`classify_shell_command`] with the sandbox-only routine tier enabled when `sandboxed`.
pub fn classify_shell_command_in(
    command: &str,
    dialect: ShellDialect,
    sandboxed: bool,
) -> ShellCommandClassification {
    if command.trim().is_empty() {
        return approval("empty shell command");
    }
    match dialect {
        ShellDialect::PowerShell => approval(
            "PowerShell AutoSafe classification is not available; explicit approval is required",
        ),
        ShellDialect::Posix => classify_posix(command, 0, sandboxed),
    }
}

fn classify_posix(command: &str, depth: usize, sandboxed: bool) -> ShellCommandClassification {
    if depth >= 8 {
        return approval("shell command nesting exceeds the approval classifier limit");
    }
    let Some(tree) = try_parse_shell(command) else {
        return approval("shell command could not be parsed safely");
    };
    let Some(commands) = try_parse_word_only_commands_sequence(&tree, command, sandboxed) else {
        return approval("shell command uses ambiguous or unsupported syntax");
    };
    if commands.is_empty() {
        return approval("shell command contains no classifiable command segment");
    }
    if sets_unsafe_env(tree.root_node(), command, &commands, sandboxed) {
        return approval("shell command changes an execution-affecting environment variable");
    }
    // Under the sandbox a redirect into a file is an ordinary write: it is confined like any
    // other, and an interpreter can already write the same file.
    if !sandboxed && has_unsafe_file_redirect(tree.root_node(), command) {
        return approval_with_destructive(
            "shell command redirects output to a non-sink path",
            true,
        );
    }

    for parsed in &commands {
        let raw = parsed.words();
        let inner = unwrap_wrappers(raw);
        if let Some(script) = shell_dash_c_script(inner) {
            if let ShellCommandClassification::RequiresApproval {
                reason,
                destructive,
            } = classify_posix(script, depth + 1, sandboxed)
            {
                return approval_with_destructive(
                    format!("nested shell requires approval: {reason}"),
                    destructive,
                );
            }
            continue;
        }
        if !(command_is_routine(inner) || (sandboxed && command_is_routine_when_sandboxed(inner))) {
            return approval_with_destructive(
                format!(
                    "shell segment is not on the AutoSafe command allowlist: {}",
                    display_segment(inner)
                ),
                segment_is_destructive(inner),
            );
        }
    }

    ShellCommandClassification::Routine
}

fn approval(reason: impl Into<String>) -> ShellCommandClassification {
    approval_with_destructive(reason, false)
}

fn approval_with_destructive(
    reason: impl Into<String>,
    destructive: bool,
) -> ShellCommandClassification {
    ShellCommandClassification::RequiresApproval {
        reason: reason.into(),
        destructive,
    }
}

fn display_segment(words: &[String]) -> String {
    let rendered = words.join(" ");
    const MAX: usize = 160;
    if rendered.chars().count() <= MAX {
        rendered
    } else {
        format!("{}…", rendered.chars().take(MAX).collect::<String>())
    }
}

fn try_parse_shell(src: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    // tree-sitter-bash 0.25 replaced the `language()` function with a
    // `LANGUAGE: LanguageFn` constant. `Language` still implements
    // `From<LanguageFn>`, so the conversion is the whole change.
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    parser.parse(src, None)
}

/// Parse scripts composed only of plain commands joined by `&&`, `||`, `;`, or
/// `|`. Substitution, control flow, subshells, backgrounding, expansions, and
/// parser recovery all fail closed.
fn try_parse_word_only_commands_sequence(
    tree: &Tree,
    src: &str,
    sandboxed: bool,
) -> Option<Vec<PlainCommand>> {
    if tree.root_node().has_error() {
        return None;
    }

    const ALLOWED_KINDS: &[&str] = &[
        "program",
        "list",
        "pipeline",
        "command",
        "command_name",
        "word",
        "string",
        "string_content",
        "raw_string",
        "number",
        "concatenation",
        "variable_assignment",
        "variable_name",
        "redirected_statement",
        "file_redirect",
        "file_descriptor",
        "comment",
        "heredoc_redirect",
        "heredoc_start",
        "heredoc_body",
        "heredoc_content",
        "heredoc_end",
    ];
    const ALLOWED_PUNCTUATION: &[&str] = &[
        "&&", "||", ";", "|", "\"", "'", "=", ">", ">>", "<", "<<", ">&", "&>", "&>>",
    ];
    // Under the enforcing OS sandbox (writes confined to the workspace, network denied) the
    // sandbox is the boundary, and interpreters are already routine there. So the shell's own
    // composition is accepted too: variables, command substitution, subshells, groups, loops and
    // conditionals. Every command inside them is still collected and must be routine by itself,
    // the program of a command must still be a literal word, and a redirect into a file and
    // backgrounding (`&`) still ask. Decision 2026-11-11 (Adrian: decide for the best product
    // shape): 143 of 248 unattended refusals in 100 task-set runs on build 97 were this syntax.
    const SANDBOXED_KINDS: &[&str] = &[
        "subshell",
        "compound_statement",
        "for_statement",
        "while_statement",
        "if_statement",
        "elif_clause",
        "else_clause",
        "do_group",
        "negated_command",
        "test_command",
        "binary_expression",
        "unary_expression",
        "test_operator",
        "simple_expansion",
        "expansion",
        "command_substitution",
        "special_variable_name",
        "ansi_c_string",
        "declaration_command",
        "regex",
        "extglob_pattern",
    ];
    const SANDBOXED_PUNCTUATION: &[&str] = &[
        "(", ")", "{", "}", "$(", "`", "${", "$", "[", "]", "[[", "]]", "!", "for", "in", "do",
        "done", "while", "until", "if", "then", "elif", "else", "fi", "export", "local", "==",
        "!=", "=~", "-", "+", ":-", ":=", ":+", ":?", "#", "%", "/", "*", "@", "?", ":", ",", "<=",
        ">=",
    ];

    let root = tree.root_node();
    let mut stack = vec![root];
    let mut command_nodes = Vec::new();
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if node.is_named() {
            if !(ALLOWED_KINDS.contains(&kind) || (sandboxed && SANDBOXED_KINDS.contains(&kind))) {
                return None;
            }
            if kind == "command" {
                command_nodes.push(node);
            }
        } else {
            if kind.chars().any(|c| "&;|".contains(c)) && !ALLOWED_PUNCTUATION.contains(&kind) {
                return None;
            }
            if !(ALLOWED_PUNCTUATION.contains(&kind)
                || kind.trim().is_empty()
                || (sandboxed && SANDBOXED_PUNCTUATION.contains(&kind)))
            {
                return None;
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    command_nodes.sort_by_key(Node::start_byte);
    command_nodes
        .into_iter()
        .map(|node| parse_plain_command_from_node(node, src, sandboxed))
        .collect()
}

fn parse_plain_command_from_node(
    command: Node<'_>,
    src: &str,
    sandboxed: bool,
) -> Option<PlainCommand> {
    if command.kind() != "command" {
        return None;
    }
    let mut words = Vec::new();
    let mut cursor = command.walk();
    for child in command.named_children(&mut cursor) {
        match child.kind() {
            "variable_assignment" => {}
            "command_name" => {
                let word = child.named_child(0)?;
                if word.kind() != "word" {
                    return None;
                }
                words.push(word.utf8_text(src.as_bytes()).ok()?.to_owned());
            }
            "word" | "number" => {
                words.push(child.utf8_text(src.as_bytes()).ok()?.to_owned());
            }
            "string" => {
                // A double-quoted string of literal text only. The parser splits the text
                // into several `string_content` pieces around newlines and backslash
                // escapes (a multi-line `python3 -c "..."`), so any number of pieces is
                // accepted; any other child (`$name`, `${...}`, `$(...)`, backticks, a bare
                // `$`) is an expansion and still fails closed. The word is the raw text
                // between the quotes, backslashes kept.
                let count = child.child_count();
                if count < 2
                    || child.child(0)?.kind() != "\""
                    || child.child(count - 1)?.kind() != "\""
                {
                    return None;
                }
                for index in 1..count - 1 {
                    // Outside the sandbox only literal pieces; inside it an expansion in the
                    // string is accepted like any other expansion argument.
                    if child.child(index)?.kind() != "string_content" && !sandboxed {
                        return None;
                    }
                }
                let text = child.utf8_text(src.as_bytes()).ok()?;
                words.push(
                    text.strip_prefix('"')
                        .and_then(|text| text.strip_suffix('"'))?
                        .to_owned(),
                );
            }
            "raw_string" => {
                let text = child.utf8_text(src.as_bytes()).ok()?;
                words.push(
                    text.strip_prefix('\'')
                        .and_then(|text| text.strip_suffix('\''))?
                        .to_owned(),
                );
            }
            "concatenation" => {
                words.push(child.utf8_text(src.as_bytes()).ok()?.to_owned());
            }
            // Under the sandbox an argument may be an expansion; it stays an opaque word (its
            // raw text). The program itself must be a literal word (`command_name` above).
            "simple_expansion" | "expansion" | "command_substitution" | "ansi_c_string"
                if sandboxed =>
            {
                words.push(child.utf8_text(src.as_bytes()).ok()?.to_owned());
            }
            _ => return None,
        }
    }
    Some(PlainCommand { words })
}

fn is_env_assignment(token: &str) -> bool {
    let Some(eq) = token.find('=') else {
        return false;
    };
    let name = &token[..eq];
    !name.is_empty()
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn env_scan(command: &[String]) -> usize {
    let mut index = 1;
    while let Some(token) = command.get(index).map(String::as_str) {
        if token == "--" {
            index += 1;
            break;
        }
        if token != "-" && token.starts_with('-') {
            if matches!(
                token,
                "-C" | "--chdir" | "-u" | "--unset" | "-S" | "--split-string"
            ) {
                index += 2;
            } else {
                index += 1;
            }
        } else if is_env_assignment(token) {
            index += 1;
        } else {
            break;
        }
    }
    index
}

fn strip_wrapper_command(command: &[String]) -> Option<&[String]> {
    let head = command.first()?.rsplit(['/', '\\']).next()?;
    let mut index = 1;
    match head {
        "timeout" => {
            while index < command.len() && command[index].starts_with('-') {
                index += if matches!(
                    command[index].as_str(),
                    "-k" | "-s" | "--kill-after" | "--signal"
                ) {
                    2
                } else {
                    1
                };
            }
            if index >= command.len() {
                return None;
            }
            index += 1;
        }
        "nice" | "stdbuf" => {
            while index < command.len() && command[index].starts_with('-') {
                index += if matches!(
                    command[index].as_str(),
                    "-n" | "--adjustment" | "-i" | "-o" | "-e"
                ) {
                    2
                } else {
                    1
                };
            }
        }
        "ionice" => {
            while index < command.len() && command[index].starts_with('-') {
                index += if matches!(
                    command[index].as_str(),
                    "-c" | "-n"
                        | "-p"
                        | "-P"
                        | "-u"
                        | "--class"
                        | "--classdata"
                        | "--pid"
                        | "--pgid"
                        | "--uid"
                ) {
                    2
                } else {
                    1
                };
            }
        }
        "chrt" => {
            while index < command.len() && command[index].starts_with('-') {
                index += 1;
            }
            if index >= command.len() {
                return None;
            }
            index += 1;
        }
        "env" => index = env_scan(command),
        _ => return None,
    }
    let inner = command.get(index..)?;
    (!inner.is_empty()).then_some(inner)
}

fn unwrap_wrappers(words: &[String]) -> &[String] {
    let mut current = words;
    for _ in 0..8 {
        match strip_wrapper_command(current) {
            Some(inner) => current = inner,
            None => break,
        }
    }
    current
}

fn shell_dash_c_script(words: &[String]) -> Option<&str> {
    let program = words.first()?.rsplit(['/', '\\']).next()?;
    if !matches!(program, "bash" | "sh" | "dash" | "zsh" | "ksh") {
        return None;
    }
    // Only options that precede the first operand belong to the shell. In
    // `bash setup.sh -c ls` the `-c` is an argument of the script, and the
    // script itself is what runs, so that form is not a `-c` invocation.
    for (index, word) in words.iter().enumerate().skip(1) {
        if !word.starts_with('-') || word == "-" || word == "--" {
            return None;
        }
        if !word.starts_with("--") && word.contains('c') {
            return words.get(index + 1).map(String::as_str);
        }
    }
    None
}

const SAFE_ENV_KEYS: &[&str] = &[
    "CARGO_TERM_COLOR",
    "CARGO_TERM_PROGRESS_WHEN",
    "RUST_LOG",
    "RUST_LOG_STYLE",
    "RUST_BACKTRACE",
    "RUST_TEST_THREADS",
    "RUST_MIN_STACK",
    "NO_COLOR",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "FORCE_COLOR",
    "COLORTERM",
];

fn sets_unsafe_env(root: Node<'_>, src: &str, commands: &[PlainCommand], sandboxed: bool) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "variable_assignment" {
            let key = assignment_key(node, src);
            if !(SAFE_ENV_KEYS.contains(&key) || (sandboxed && sandboxed_assignment_is_safe(key))) {
                return true;
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    commands
        .iter()
        .any(|command| command_env_is_unsafe(command.words()))
}

/// Under the sandbox a shell variable (`T=$(mktemp -d)`, `PYTHONPATH=.`) is ordinary. What still
/// asks is a name that changes which program runs or what a program loads before its own code.
fn sandboxed_assignment_is_safe(key: &str) -> bool {
    const UNSAFE: &[&str] = &[
        "PATH",
        "IFS",
        "ENV",
        "SHELL",
        "HOME",
        "CDPATH",
        "PS4",
        "PROMPT_COMMAND",
        "EDITOR",
        "VISUAL",
        "PAGER",
        "PYTHONSTARTUP",
        "PYTHONHOME",
        "PYTHONINSPECT",
        "NODE_OPTIONS",
        "RUSTC",
        "RUSTFLAGS",
        "RUSTC_WRAPPER",
        "CC",
        "CXX",
        "LD",
        "MAKEFLAGS",
    ];
    const UNSAFE_PREFIXES: &[&str] = &[
        "LD_", "DYLD_", "BASH_", "GIT_", "SSH_", "NPM_", "npm_", "PERL5", "RUBY", "CARGO_",
    ];
    !key.is_empty()
        && !UNSAFE.contains(&key)
        && !UNSAFE_PREFIXES.iter().any(|prefix| key.starts_with(prefix))
}

fn command_env_is_unsafe(words: &[String]) -> bool {
    let mut current = words;
    for _ in 0..8 {
        if current
            .first()
            .and_then(|word| word.rsplit(['/', '\\']).next())
            == Some("env")
        {
            for argument in &current[1..] {
                if argument == "--" {
                    break;
                }
                if argument.starts_with('-') {
                    return true;
                }
                match argument.split_once('=') {
                    Some((key, _)) if !SAFE_ENV_KEYS.contains(&key) => return true,
                    Some(_) => {}
                    None => break,
                }
            }
        }
        match strip_wrapper_command(current) {
            Some(inner) => current = inner,
            None => break,
        }
    }
    false
}

fn assignment_key<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_name" {
            if let Ok(text) = child.utf8_text(src.as_bytes()) {
                return text;
            }
        }
    }
    node.utf8_text(src.as_bytes())
        .ok()
        .and_then(|text| text.split('=').next())
        .unwrap_or("")
}

fn has_unsafe_file_redirect(root: Node<'_>, src: &str) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "file_redirect" {
            let text = node.utf8_text(src.as_bytes()).unwrap_or("");
            if !redirect_is_safe(text) {
                return true;
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    false
}

fn redirect_is_safe(text: &str) -> bool {
    let compact: String = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if !compact.contains('>') {
        return true;
    }
    // The whole target must be the sink: `>x/dev/null` names a file in a directory the agent
    // can create, not the device.
    let target = compact
        .trim_start_matches(|character: char| character.is_ascii_digit())
        .trim_start_matches('&')
        .trim_start_matches('>')
        .trim_start_matches('|');
    if ["/dev/null", "/dev/stdout", "/dev/stderr"].contains(&target) {
        return true;
    }
    if let Some((_, target)) = compact.rsplit_once(">&") {
        return !target.is_empty()
            && (target == "-" || target.bytes().all(|byte| byte.is_ascii_digit()));
    }
    false
}

fn command_is_routine(words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let raw_head = words[0].as_str();
    if raw_head.contains('/') || raw_head.contains('\\') {
        return false;
    }
    let head = raw_head.to_ascii_lowercase();
    match head.as_str() {
        "cargo" => safe_subcommand(words, CARGO_SAFE_SUBCOMMANDS),
        "git" => git_command_is_routine(words),
        "npm" | "pnpm" | "yarn" => safe_subcommand(words, PACKAGE_SAFE_SUBCOMMANDS),
        "go" => go_command_is_routine(words),
        "kubectl" => safe_subcommand(words, &["get", "logs", "describe", "version"]),
        "find" => find_is_read_only(words),
        "sed" => sed_is_read_only(words),
        // `--compress-program` runs a program; any `--o...` is `--output`, and a short
        // cluster holding `o` (`-ro file`) is `-o`.
        "sort" => !words.iter().skip(1).any(|word| {
            word.starts_with("--o") || word.starts_with("--comp") || short_flag_contains(word, 'o')
        }),
        "tree" => !words
            .iter()
            .skip(1)
            .any(|word| word.starts_with("--output") || short_flag_contains(word, 'o')),
        "rg" => !words
            .iter()
            .skip(1)
            .any(|word| word == "--pre" || word.starts_with("--pre=")),
        "ls" | "pwd" | "echo" | "printf" | "cat" | "head" | "tail" | "wc" | "grep" | "which"
        | "type" | "true" | "false" | "test" | "[" | "tr" | "cut" | "diff" | "jq" | "date"
        | "whoami" | "hostname" | "uname" | "nproc" | "printenv" | "stat" | "file" | "basename"
        | "dirname" | "realpath" | "readlink" | "strings" | "sleep" | "df" | "du" | "ps"
        | "top" | "htop" | "cd" | "pushd" | "popd" | "set" => true,
        _ => false,
    }
}

/// Interpreters and test runners: arbitrary code, so routine only inside an enforcing OS
/// sandbox (writes confined to the workspace, no network), never outside one.
fn command_is_routine_when_sandboxed(words: &[String]) -> bool {
    let Some(raw_head) = words.first() else {
        return false;
    };
    if raw_head.contains('/') || raw_head.contains('\\') {
        return false;
    }
    let head = raw_head.to_ascii_lowercase();
    let sub = |allowed: &[&str]| {
        words
            .get(1)
            .is_some_and(|word| allowed.contains(&word.to_ascii_lowercase().as_str()))
    };
    match head.as_str() {
        "python" | "python3" | "node" | "deno" | "bun" | "ruby" | "pytest" | "make" => true,
        // Checkers and formatters: the same class as a test runner.
        "pyright" | "mypy" | "ruff" | "flake8" | "pyflakes" | "pylint" | "black" | "tsc"
        | "eslint" | "prettier" | "jest" | "vitest" | "rustfmt" | "gofmt" | "shellcheck" => true,
        // Ordinary file and text tools. They write only where the sandbox lets them. `rm` is
        // not here: deletion still asks.
        "mkdir" | "cp" | "mv" | "touch" | "mktemp" | "sed" | "awk" | "uniq" | "od" | "xxd"
        | "hexdump" | "tr" | "cut" | "sort" | "tee" | "diff" | "cmp" | "comm" | "paste"
        | "column" | "nl" | "seq" | "sqlite3" | "type" | "which" | "test" | "printf" | "export"
        | "set" | "read" | "sleep" | "date" | "basename" | "dirname" | "realpath" | "readlink"
        | "stat" | "file" | "du" | "jq" => true,
        // `command -v NAME` only looks a program up; `command NAME` would run it unclassified.
        "command" => sub(&["-v", "-V"]),
        "cargo" => sub(&[
            "test", "build", "check", "run", "clippy", "fmt", "bench", "doc",
        ]),
        "npm" | "pnpm" | "yarn" => sub(&["test", "run", "build", "exec", "t"]),
        "go" => sub(&["test", "build", "vet", "run"]),
        _ => false,
    }
}

/// Identify parsed segments that are actual destructive/write operations so the
/// central permission engine receives its per-call `destructive` flag. Unknown
/// commands still ask, but are not mislabeled merely because they are unlisted.
fn segment_is_destructive(words: &[String]) -> bool {
    let words = unwrap_wrappers(words);
    let Some(head) = words
        .first()
        .and_then(|word| word.rsplit(['/', '\\']).next())
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };
    if matches!(
        head.as_str(),
        "rm" | "rmdir"
            | "shred"
            | "wipefs"
            | "dd"
            | "tee"
            | "truncate"
            | "cp"
            | "mv"
            | "install"
            | "chmod"
            | "chown"
            | "chgrp"
            | "ln"
    ) || head.starts_with("mkfs")
    {
        return true;
    }
    match head.as_str() {
        "find" => !find_is_read_only(words),
        "sed" => words.iter().skip(1).any(|word| {
            word == "--in-place"
                || word.starts_with("--in-place=")
                || short_flag_contains(word, 'i')
        }),
        "sort" => option_takes_output_path(words, &["-o", "--output"]),
        "uniq" => {
            words
                .iter()
                .skip(1)
                .filter(|word| !word.starts_with('-'))
                .count()
                >= 2
        }
        "tree" => words
            .iter()
            .skip(1)
            .any(|word| word.starts_with("--output") || short_flag_contains(word, 'o')),
        "git" => {
            option_takes_output_path(words, &["--output"])
                || words.iter().skip(1).any(|word| {
                    matches!(
                        word.as_str(),
                        "reset" | "clean" | "restore" | "checkout" | "switch"
                    )
                })
        }
        "go" => option_takes_output_path(words, &["-o", "--output"]),
        _ => false,
    }
}

// AutoSafe is for commands whose arguments cannot turn them into arbitrary
// workspace code execution. Cargo build/test/run/fmt and package-manager
// install/run commands are intentionally absent: build scripts, lifecycle
// hooks, and repository-authored runners can perform any mutation the sandbox
// permits.
const CARGO_SAFE_SUBCOMMANDS: &[&str] = &["metadata", "tree", "version"];
const GIT_SAFE_SUBCOMMANDS: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "blame",
    "grep",
    "ls-files",
    "rev-parse",
    "describe",
    "merge-base",
];
const PACKAGE_SAFE_SUBCOMMANDS: &[&str] = &["list", "ls", "outdated", "why", "view", "info"];

fn go_command_is_routine(words: &[String]) -> bool {
    let Some(subcommand) = words
        .iter()
        .skip(1)
        .find(|word| !word.starts_with('-'))
        .map(String::as_str)
    else {
        return false;
    };
    match subcommand {
        "version" => true,
        // `go env -w/-u` mutates the user's persistent Go environment.
        "env" => !words.iter().skip(2).any(|word| {
            word == "-w" || word.starts_with("-w=") || word == "-u" || word.starts_with("-u=")
        }),
        _ => false,
    }
}

fn safe_subcommand(words: &[String], allowed: &[&str]) -> bool {
    words
        .iter()
        .skip(1)
        .find(|word| !word.starts_with('-'))
        .is_some_and(|word| allowed.contains(&word.as_str()))
}

fn git_command_is_routine(words: &[String]) -> bool {
    if !safe_subcommand(words, GIT_SAFE_SUBCOMMANDS)
        || option_takes_output_path(words, &["--output"])
    {
        return false;
    }
    let subcommand = words
        .iter()
        .skip(1)
        .find(|word| !word.starts_with('-'))
        .map(String::as_str);
    if subcommand == Some("grep") {
        return !words.iter().skip(2).any(|word| {
            word.starts_with("-O")
                || word == "--open-files-in-pager"
                || word.starts_with("--open-files-in-pager=")
        });
    }
    true
}

fn option_takes_output_path(words: &[String], options: &[&str]) -> bool {
    words.iter().skip(1).any(|word| {
        options.iter().any(|option| {
            word == option
                || word
                    .strip_prefix(option)
                    .is_some_and(|suffix| suffix.starts_with('='))
        })
    })
}

/// GNU `sed` can write files (`w`, `W`, `-i`, `s///w`) or run commands (`e`, `s///e`), so only
/// scripts made of known print/select commands stay routine; anything unrecognised asks.
fn sed_is_read_only(words: &[String]) -> bool {
    let mut scripts: Vec<&str> = Vec::new();
    let mut have_expression = false;
    let mut index = 1;
    while index < words.len() {
        let word = words[index].as_str();
        index += 1;
        if word == "--in-place" || word.starts_with("--in-place=") || word == "--file" {
            return false;
        }
        if word == "--expression" {
            have_expression = true;
            match words.get(index) {
                Some(script) => scripts.push(script),
                None => return false,
            }
            index += 1;
        } else if let Some(script) = word.strip_prefix("--expression=") {
            have_expression = true;
            scripts.push(script);
        } else if word.starts_with("--") {
            if word.starts_with("--file") || word.starts_with("--in") {
                return false;
            }
        } else if word.starts_with('-') && word.len() > 1 {
            if short_flag_contains(word, 'i')
                || short_flag_contains(word, 'f')
                || short_flag_contains(word, 'l')
            {
                return false;
            }
            if short_flag_contains(word, 'e') {
                have_expression = true;
                match words.get(index) {
                    Some(script) => scripts.push(script),
                    None => return false,
                }
                index += 1;
            }
        } else if !have_expression && scripts.is_empty() {
            scripts.push(word);
        }
    }
    !scripts.is_empty() && scripts.iter().all(|script| sed_script_is_read_only(script))
}

fn sed_script_is_read_only(script: &str) -> bool {
    sed_script_scan(script).unwrap_or(false)
}

fn sed_script_scan(script: &str) -> Option<bool> {
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        i += 1;
        match ch {
            ' '
            | '\t'
            | '\n'
            | '\r'
            | ';'
            | '{'
            | '}'
            | '!'
            | ','
            | '$'
            | '~'
            | '+'
            | 'p'
            | 'd'
            | 'D'
            | 'n'
            | 'N'
            | 'g'
            | 'G'
            | 'h'
            | 'H'
            | 'x'
            | 'l'
            | '='
            | 'q'
            | 'Q'
            | 'P'
            | 'I'
            | 'M'
            | '0'..='9' => {}
            '/' => i = sed_skip_delimited(&chars, i, '/')?,
            '\\' => {
                let delimiter = *chars.get(i)?;
                i = sed_skip_delimited(&chars, i + 1, delimiter)?;
            }
            'y' => {
                let delimiter = *chars.get(i)?;
                i = sed_skip_delimited(&chars, i + 1, delimiter)?;
                i = sed_skip_delimited(&chars, i, delimiter)?;
            }
            's' => {
                let delimiter = *chars.get(i)?;
                i = sed_skip_delimited(&chars, i + 1, delimiter)?;
                i = sed_skip_delimited(&chars, i, delimiter)?;
                while i < chars.len() && !matches!(chars[i], ';' | '\n' | '}') {
                    if matches!(chars[i], 'w' | 'W' | 'e') {
                        return Some(false);
                    }
                    i += 1;
                }
            }
            'a' | 'i' | 'c' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            'b' | 't' | 'T' | ':' => {
                while i < chars.len() && !matches!(chars[i], ';' | '\n') {
                    i += 1;
                }
            }
            _ => return Some(false),
        }
    }
    Some(true)
}

fn sed_skip_delimited(chars: &[char], mut i: usize, delimiter: char) -> Option<usize> {
    loop {
        let ch = *chars.get(i)?;
        if ch == '\\' {
            i += 2;
        } else if ch == delimiter {
            return Some(i + 1);
        } else {
            i += 1;
        }
    }
}

fn short_flag_contains(word: &str, flag: char) -> bool {
    word.starts_with('-') && !word.starts_with("--") && word[1..].contains(flag)
}

fn find_is_read_only(words: &[String]) -> bool {
    const MUTATING_ACTIONS: &[&str] = &[
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls",
    ];
    !words
        .iter()
        .any(|word| MUTATING_ACTIONS.contains(&word.as_str()))
}
