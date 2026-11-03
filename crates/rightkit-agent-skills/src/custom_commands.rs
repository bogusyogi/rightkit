//! Claude Code-style custom slash commands (Fixes #132).
//!
//! Spec: https://code.claude.com/docs/en/slash-commands ("Custom commands").
//!
//! A custom command is a Markdown file. Its file stem is the command name,
//! each subdirectory below the commands root becomes a `namespace:` prefix
//! (`frontend/button.md` -> `/frontend:button`), optional `---` frontmatter
//! carries `description`, `argument-hint`, `allowed-tools` and `model`, and
//! the body is the prompt template. This module is pure parsing, discovery,
//! argument substitution and `@file` splicing; `!{cmd}` / backtick-bang shell
//! expansion stays with the policy-backed runner in the binary
//! (`inline_shell_runner::expand_custom_command`), and trust gating of
//! project roots is the caller's job (it passes only roots it may read).

use std::path::{Path, PathBuf};

const MAX_FILE_REF_BYTES: usize = 64 * 1024;
const MAX_COMMAND_FILE_BYTES: u64 = 256 * 1024;
const MAX_DEPTH: usize = 6;

/// Where a command came from. Later roots override earlier ones by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandScope {
    User,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCommand {
    /// Slash name without the leading `/`, namespaced with `:`.
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    /// Prompt template (frontmatter removed).
    pub body: String,
    pub scope: CommandScope,
    pub path: PathBuf,
}

impl CustomCommand {
    /// File stem: the bare name Claude Code shows (`button` for `frontend:button`).
    pub fn stem(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    /// Directory namespace (`frontend`, or `a:b` for nested), if any.
    pub fn namespace(&self) -> Option<&str> {
        self.name.rsplit_once(':').map(|(ns, _)| ns)
    }

    /// One-line popup / `/help` description, e.g. `Review code [file] (project)`.
    pub fn summary(&self) -> String {
        let scope = match self.scope {
            CommandScope::Project => "project",
            CommandScope::User => "user",
        };
        let mut out = if self.description.is_empty() {
            first_body_line(&self.body)
        } else {
            self.description.clone()
        };
        if let Some(hint) = &self.argument_hint {
            out.push_str(&format!(" {hint}"));
        }
        match self.namespace() {
            Some(ns) => out.push_str(&format!(" ({scope}:{ns})")),
            None => out.push_str(&format!(" ({scope})")),
        }
        out
    }
}

fn first_body_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(80).collect())
        .unwrap_or_default()
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

/// Split `---` frontmatter from the body. Returns (key/value pairs, body).
/// Keys are lowercased; YAML list items (`- a` lines) are joined by commas.
fn split_frontmatter(text: &str) -> (Vec<(String, String)>, String) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Walk line by line while tracking the unconsumed remainder, so the body
    // keeps the file's text exactly (including its trailing newline), like the
    // no-frontmatter path does.
    fn next_line(s: &str) -> (&str, &str) {
        match s.find('\n') {
            Some(i) => (&s[..i], &s[i + 1..]),
            None => (s, ""),
        }
    }
    let (first, mut rest) = next_line(text);
    if first.trim_end() != "---" {
        return (Vec::new(), text.to_string());
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut closed = false;
    let mut header = Vec::new();
    while !rest.is_empty() {
        let (line, after) = next_line(rest);
        rest = after;
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        header.push(line);
    }
    if !closed {
        return (Vec::new(), text.to_string());
    }
    for line in header {
        let trimmed = line.trim();
        if let Some(item) = trimmed.strip_prefix("- ") {
            if let Some((_, v)) = pairs.last_mut() {
                if !v.is_empty() {
                    v.push(',');
                }
                v.push_str(&unquote(item));
            }
            continue;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            pairs.push((k.trim().to_ascii_lowercase(), unquote(v)));
        }
    }
    (pairs, rest.to_string())
}

/// Split a comma list, keeping commas inside parentheses (`Bash(git add:*, x)`).
fn split_list(value: &str) -> Vec<String> {
    let inner = value.trim().trim_start_matches('[').trim_end_matches(']');
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    items.push(cur);
    items
        .into_iter()
        .map(|i| unquote(&i))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse one command file. `name` is the already-namespaced name.
pub fn parse_command(name: &str, text: &str, scope: CommandScope, path: &Path) -> CustomCommand {
    let (pairs, body) = split_frontmatter(text);
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    CustomCommand {
        name: name.to_string(),
        description: get("description").unwrap_or_default(),
        argument_hint: get("argument-hint").or_else(|| get("argument_hint")),
        allowed_tools: get("allowed-tools")
            .or_else(|| get("allowed_tools"))
            .map(|v| split_list(&v))
            .unwrap_or_default(),
        model: get("model"),
        body,
        scope,
        path: path.to_path_buf(),
    }
}

fn walk(root: &Path, dir: &Path, scope: CommandScope, depth: usize, out: &mut Vec<CustomCommand>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk(root, &path, scope, depth + 1, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if entry
            .metadata()
            .map(|m| m.len() > MAX_COMMAND_FILE_BYTES)
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let parts: Vec<String> = rel
            .with_extension("")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        if parts
            .iter()
            .any(|p| p.is_empty() || p.contains(char::is_whitespace))
        {
            continue;
        }
        let name = parts.join(":").to_lowercase();
        out.push(parse_command(&name, &text, scope, &path));
    }
}

/// Discover commands under each root, in order; a later root overrides an
/// earlier command of the same name. Result is sorted by name.
pub fn discover(roots: &[(PathBuf, CommandScope)]) -> Vec<CustomCommand> {
    let mut by_name: std::collections::BTreeMap<String, CustomCommand> = Default::default();
    for (root, scope) in roots {
        let mut found = Vec::new();
        walk(root, root, *scope, 0, &mut found);
        for cmd in found {
            by_name.insert(cmd.name.clone(), cmd);
        }
    }
    by_name.into_values().collect()
}

/// Resolve typed `/name` (no slash). Bare names match the file stem; a typed
/// `ns:name` matches only that full name. If several commands share the stem,
/// project beats user; if more than one still remains, the one without a
/// namespace owns the bare name, otherwise it is ambiguous (`None`) and the
/// caller must type `ns:name`.
pub fn resolve<'a>(commands: &'a [CustomCommand], typed: &str) -> Option<&'a CustomCommand> {
    let typed = typed.trim_start_matches('/').to_lowercase();
    let mut found: Vec<&CustomCommand> = commands
        .iter()
        .filter(|c| c.name == typed || (!typed.contains(':') && c.stem() == typed))
        .collect();
    if found.iter().any(|c| c.scope == CommandScope::Project) {
        found.retain(|c| c.scope == CommandScope::Project);
    }
    match found.len() {
        0 => None,
        1 => Some(found[0]),
        _ => {
            let mut plain = found.into_iter().filter(|c| c.namespace().is_none());
            let first = plain.next()?;
            plain.next().is_none().then_some(first)
        }
    }
}

/// Names to list in the popup and `/help`: the bare stem when it resolves to
/// the command, else the full `ns:name`. A namespace-less command hidden by a
/// project command of the same name is not listed.
pub fn listing(commands: &[CustomCommand]) -> Vec<(String, &CustomCommand)> {
    commands
        .iter()
        .filter_map(|c| {
            let bare_wins = resolve(commands, c.stem()).is_some_and(|r| std::ptr::eq(r, c));
            if bare_wins {
                Some((c.stem().to_string(), c))
            } else if c.namespace().is_some() {
                Some((c.name.clone(), c))
            } else {
                None
            }
        })
        .collect()
}

/// Replace `$ARGUMENTS` with the whole argument string and `$1..$n` with the
/// n-th argument (whitespace-split, quotes group). Missing positionals become
/// empty; `$0` and a bare `$` are left alone.
pub fn substitute_arguments(body: &str, args: &str) -> String {
    let positional = split_args(args);
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        if let Some(tail) = after.strip_prefix("ARGUMENTS") {
            out.push_str(args.trim());
            rest = tail;
        } else {
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            match digits.parse::<usize>() {
                Ok(n) if n >= 1 => {
                    out.push_str(positional.get(n - 1).map(String::as_str).unwrap_or(""));
                    rest = &after[digits.len()..];
                }
                _ => {
                    out.push('$');
                    rest = after;
                }
            }
        }
    }
    out.push_str(rest);
    out
}

/// Split an argument string on whitespace, honouring simple quotes.
pub fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut quoted = false;
    for c in args.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                quoted = true;
            }
            None if c.is_whitespace() => {
                if quoted || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    quoted = false;
                }
            }
            None => cur.push(c),
        }
    }
    if quoted || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Splice `@path` references that resolve to a regular file inside
/// `workspace` as a fenced block after the reference. Anything else (email
/// addresses, missing files, paths escaping the workspace, symlink escapes,
/// directories) is left untouched. Files are capped at 64 KiB.
pub fn expand_file_references(body: &str, workspace: &Path) -> String {
    let Ok(root) = workspace.canonicalize() else {
        return body.to_string();
    };
    let mut out = String::with_capacity(body.len());
    let mut prev: Option<char> = None;
    let mut chars = body.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let at_boundary = prev
            .map(|p| p.is_whitespace() || "([\"'".contains(p))
            .unwrap_or(true);
        if c != '@' || !at_boundary {
            out.push(c);
            prev = Some(c);
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while let Some(&(j, n)) = chars.peek() {
            if n.is_whitespace() || ")]\"'`,;".contains(n) {
                break;
            }
            end = j + n.len_utf8();
            chars.next();
        }
        let token = body[start..end].trim_end_matches(|ch| ".:!?".contains(ch));
        out.push('@');
        out.push_str(token);
        if let Some(content) = read_inside(&root, workspace, token) {
            out.push_str(&format!("\n```\n{content}\n```\n"));
        }
        out.push_str(&body[start + token.len()..end]);
        prev = body[..end].chars().last();
    }
    out
}

fn read_inside(root: &Path, workspace: &Path, token: &str) -> Option<String> {
    if token.is_empty() {
        return None;
    }
    let candidate = workspace.join(token).canonicalize().ok()?;
    if !candidate.starts_with(root) || !candidate.is_file() {
        return None;
    }
    let bytes = std::fs::read(&candidate).ok()?;
    let slice = &bytes[..bytes.len().min(MAX_FILE_REF_BYTES)];
    Some(
        String::from_utf8_lossy(slice)
            .trim_end_matches('\n')
            .to_string(),
    )
}
