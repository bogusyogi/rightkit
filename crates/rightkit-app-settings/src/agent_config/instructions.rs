//! Layered project-instruction system.
//!
//! Loads and merges instructions from multiple sources with precedence:
//! `Local > Project > User > Managed`.
//!
//! Supported file patterns:
//! - `CLAUDE.md` (project/user-level instructions)
//! - `AGENTS.md` (agent-wide project instructions)
//! - `right.md` (flat project instructions)
//! - `.claude/rules/*.md` (granular instruction rules)
//! - `.claude/instructions/*.md` (scoped instructions with glob/path matching)
//!
//! Frontmatter syntax (YAML-ish, between `---` markers):
//! ```markdown
//! ---
//! globs:
//!   - src/**/*.rs
//!   - tests/**/*.rs
//! paths:
//!   - /src/lib.rs
//! includes:
//!   - @base-rules
//!   - @security
//! hooks:
//!   - name: pre-commit
//!     script: ./scripts/check.sh
//! ---
//! # Instruction body...
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Precedence order for instruction layers.
///
/// Higher-precedence layers override lower ones. Use [`Ord`] to compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstructionScope {
    Managed = 0,
    User = 1,
    Project = 2,
    Local = 3,
}

impl std::fmt::Display for InstructionScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstructionScope::Managed => write!(f, "managed"),
            InstructionScope::User => write!(f, "user"),
            InstructionScope::Project => write!(f, "project"),
            InstructionScope::Local => write!(f, "local"),
        }
    }
}

/// Parsed frontmatter from an instruction file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Frontmatter {
    /// Glob patterns (e.g., `src/**/*.rs`) for applicability matching.
    #[serde(default)]
    pub globs: Vec<String>,
    /// Exact file paths for applicability matching.
    #[serde(default)]
    pub paths: Vec<String>,
    /// References to other instruction files (e.g., `@base-rules`, `@security`).
    /// Resolved via `expand_includes`.
    #[serde(default)]
    pub includes: Vec<String>,
    /// Hook references for instruction-loaded hooks.
    #[serde(default)]
    pub hook_refs: Vec<String>,
    /// OKF type identifier (e.g., "instruction", "guide", "reference").
    /// Present only for OKF-formatted files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Link targets from OKF-formatted instruction files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

/// A loaded instruction file with its scope and parsed content.
#[derive(Debug, Clone)]
pub struct InstructionFile {
    pub scope: InstructionScope,
    pub path: String,
    pub frontmatter: Frontmatter,
    pub body: String,
}

/// Loads and merges instructions from multiple scopes.
///
/// Supports discovery of instruction files under configured root directories,
/// parsing frontmatter (YAML-ish), glob-based applicability filtering, and
/// include expansion.
#[derive(Debug, Clone)]
pub struct InstructionLoader {
    roots: HashMap<InstructionScope, PathBuf>,
}

impl InstructionLoader {
    /// Create a new instruction loader with no roots.
    pub fn new() -> Self {
        InstructionLoader {
            roots: HashMap::new(),
        }
    }

    /// Register a root directory for a given scope.
    /// Files will be discovered under this directory for the scope.
    pub fn add_root(&mut self, scope: InstructionScope, dir: PathBuf) {
        self.roots.insert(scope, dir);
    }

    /// Discover all instruction files under registered roots.
    ///
    /// Searches for:
    /// - `CLAUDE.md` at root level
    /// - `AGENTS.md` at root level
    /// - `right.md` at root level
    /// - `.claude/rules/*.md` (all files)
    /// - `.claude/instructions/*.md` (all files)
    ///
    /// Returns files sorted by scope (managed -> local).
    pub fn discover(&self) -> Vec<InstructionFile> {
        self.discover_gated(|_| true)
    }

    /// Like [`discover`](Self::discover) but `.claude/rules/*.md` under Project
    /// and Local roots is read only when `workspace_trusted(root)` is true.
    /// Root-level files (CLAUDE.md, AGENTS.md, right.md) and
    /// `.claude/instructions` stay ungated to match the workspace-trust lane
    /// (#131); Managed and User roots always load rules.
    pub fn discover_gated(
        &self,
        workspace_trusted: impl Fn(&Path) -> bool,
    ) -> Vec<InstructionFile> {
        let mut files = Vec::new();

        for (scope, root) in &self.roots {
            let rules_allowed =
                !matches!(scope, InstructionScope::Project | InstructionScope::Local)
                    || workspace_trusted(root);
            // Check for CLAUDE.md
            let claude_path = root.join("CLAUDE.md");
            if claude_path.exists() {
                if let Some(file) = Self::load_context_file(*scope, &claude_path) {
                    files.push(file);
                }
            }

            // Check for AGENTS.md
            let agents_path = root.join("AGENTS.md");
            if agents_path.exists() {
                if let Some(file) = Self::load_context_file(*scope, &agents_path) {
                    files.push(file);
                }
            }

            // Check for right.md
            let right_path = root.join("right.md");
            if right_path.exists() {
                if let Some(file) = Self::load_context_file(*scope, &right_path) {
                    files.push(file);
                }
            }

            // Check for .claude/rules/*.md
            let rules_dir = root.join(".claude").join("rules");
            if rules_allowed && rules_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&rules_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().is_some_and(|ext| ext == "md") {
                            if let Some(file) = Self::load_context_file(*scope, &path) {
                                files.push(file);
                            }
                        }
                    }
                }
            }

            // Check for .claude/instructions/*.md
            let instructions_dir = root.join(".claude").join("instructions");
            if instructions_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&instructions_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().is_some_and(|ext| ext == "md") {
                            if let Some(file) = Self::load_context_file(*scope, &path) {
                                files.push(file);
                            }
                        }
                    }
                }
            }
        }

        // Sort by scope: managed < user < project < local
        files.sort_by_key(|f| f.scope);

        files
    }

    fn load_context_file(scope: InstructionScope, path: &Path) -> Option<InstructionFile> {
        let content = std::fs::read_to_string(path).ok()?;
        let content = Self::sanitize_context_file_content(path, content);
        let (frontmatter, body) = Self::parse_frontmatter(&content);
        Some(InstructionFile {
            scope,
            path: path.to_string_lossy().to_string(),
            frontmatter,
            body,
        })
    }

    fn sanitize_context_file_content(path: &Path, content: String) -> String {
        let result = rightkit_agent_policy::scan_prompt_injection(
            &content,
            rightkit_agent_policy::ThreatScanScope::Context,
        );
        if !result.blocked {
            return content;
        }

        let finding_ids = result
            .findings
            .iter()
            .map(|finding| finding.pattern_id)
            .collect::<Vec<_>>()
            .join(",");
        eprintln!(
            "[instructions] replaced {} with prompt-injection placeholder; findings={}",
            path.display(),
            finding_ids
        );

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("context file");
        rightkit_agent_policy::prompt_injection_placeholder(file_name)
    }

    /// Parse frontmatter from markdown content.
    ///
    /// Looks for YAML-ish frontmatter between `---` markers at the start.
    /// Frontmatter is optional; if absent, body is the entire content.
    ///
    /// Returns `(frontmatter, body)` where frontmatter is parsed (or empty
    /// if not found) and body is the remaining content.
    pub fn parse_frontmatter(text: &str) -> (Frontmatter, String) {
        let mut lines = text.lines().peekable();

        // Check for opening ---
        if lines.peek().is_none_or(|l| !l.starts_with("---")) {
            // No frontmatter
            return (Frontmatter::default(), text.to_string());
        }

        lines.next(); // consume opening ---

        let mut fm_lines = Vec::new();
        for line in lines.by_ref() {
            if line.starts_with("---") {
                break;
            }
            fm_lines.push(line);
        }

        let fm_text = fm_lines.join("\n");
        let body = lines.collect::<Vec<_>>().join("\n");

        let frontmatter = Self::parse_yaml_frontmatter(&fm_text).unwrap_or_default();

        (frontmatter, body)
    }

    /// Parse YAML-ish frontmatter into a [`Frontmatter`] struct.
    ///
    /// Hand-rolled YAML parser supporting:
    /// - `globs: [item1, item2, ...]` or `globs:\n  - item1\n  - item2`
    /// - `paths: [...]` (similar)
    /// - `includes: [...]` (similar)
    /// - `hooks:\n  - hook_name1\n  - hook_name2` (list of hook references)
    ///
    /// Returns `None` if parsing fails; caller should treat as empty frontmatter.
    fn parse_yaml_frontmatter(fm_text: &str) -> Option<Frontmatter> {
        let mut fm = Frontmatter::default();

        let lines: Vec<&str> = fm_text.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i].trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                i += 1;
                continue;
            }

            // Try to parse key: value
            if let Some(colon_pos) = line.find(':') {
                let key = line[..colon_pos].trim();
                let value_part = line[colon_pos + 1..].trim();

                match key {
                    "globs" => {
                        fm.globs = Self::parse_yaml_list(value_part, &lines, &mut i)?;
                    }
                    "paths" => {
                        fm.paths = Self::parse_yaml_list(value_part, &lines, &mut i)?;
                    }
                    "includes" => {
                        fm.includes = Self::parse_yaml_list(value_part, &lines, &mut i)?;
                    }
                    "hooks" => {
                        fm.hook_refs = Self::parse_yaml_list(value_part, &lines, &mut i)?;
                    }
                    "type" => {
                        let trimmed = value_part.trim_matches('"').trim_matches('\'');
                        if !trimmed.is_empty() {
                            fm.r#type = Some(trimmed.to_string());
                        }
                    }
                    _ => {
                        // Unknown key, skip
                    }
                }
            }

            i += 1;
        }

        Some(fm)
    }

    /// Parse a YAML list, handling both inline `[a, b, c]` and block style.
    fn parse_yaml_list(inline_part: &str, lines: &[&str], i: &mut usize) -> Option<Vec<String>> {
        let mut items = Vec::new();

        if inline_part.starts_with('[') && inline_part.ends_with(']') {
            // Inline list: [a, b, c]
            let inner = &inline_part[1..inline_part.len() - 1];
            for item in inner.split(',') {
                let trimmed = item.trim().trim_matches('"').trim_matches('\'');
                if !trimmed.is_empty() {
                    items.push(trimmed.to_string());
                }
            }
        } else if inline_part.is_empty() {
            // Block list: lines starting with -
            *i += 1;
            while *i < lines.len() {
                let line = lines[*i];
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    *i += 1;
                    continue;
                }

                if !trimmed.starts_with('-') {
                    break;
                }

                let item = trimmed[1..].trim().trim_matches('"').trim_matches('\'');
                if !item.is_empty() {
                    items.push(item.to_string());
                }
                *i += 1;
            }
            *i -= 1; // Back up one since loop will increment
        } else {
            // Single item on same line
            let trimmed = inline_part.trim_matches('"').trim_matches('\'');
            if !trimmed.is_empty() {
                items.push(trimmed.to_string());
            }
        }

        Some(items)
    }

    /// Expand `@include` references within an instruction file's body.
    ///
    /// Looks for `@include path` or `@include:path` patterns and replaces them
    /// with the content of the referenced file. The `fs_read` closure is called
    /// to fetch file content (allowing tests to inject mock data).
    ///
    /// Returns the expanded body, or the original if expansion fails.
    pub fn expand_includes(
        &self,
        file: &InstructionFile,
        fs_read: &dyn Fn(&str) -> Option<String>,
    ) -> String {
        let mut result = file.body.clone();

        for include_ref in &file.frontmatter.includes {
            // Normalize include references: @name or name -> look up
            let normalized = include_ref.strip_prefix('@').unwrap_or(include_ref);

            if let Some(content) = fs_read(normalized) {
                // Replace @include directives in body
                let patterns = vec![
                    format!("@include {}", normalized),
                    format!("@include:{}", normalized),
                ];

                for pattern in patterns {
                    result = result.replace(&pattern, &content);
                }
            }
        }

        result
    }

    /// Filter instruction files by applicability to a given file path.
    ///
    /// Returns only files whose frontmatter `globs` and `paths` match the
    /// `current_path`. If a file has no globs/paths, it applies to everything.
    pub fn applicable<'a>(
        &self,
        files: &'a [InstructionFile],
        current_path: &str,
    ) -> Vec<&'a InstructionFile> {
        files
            .iter()
            .filter(|f| {
                // No globs/paths = applies to everything
                if f.frontmatter.globs.is_empty() && f.frontmatter.paths.is_empty() {
                    return true;
                }

                // Check exact path matches
                if !f.frontmatter.paths.is_empty()
                    && f.frontmatter.paths.iter().any(|p| p == current_path)
                {
                    return true;
                }

                // Check glob matches
                if !f.frontmatter.globs.is_empty()
                    && f.frontmatter
                        .globs
                        .iter()
                        .any(|g| Self::matches_glob(g, current_path))
                {
                    return true;
                }

                false
            })
            .collect()
    }

    /// Match a glob pattern against a file path.
    ///
    /// Supports:
    /// - `*` matches any sequence of characters except `/`
    /// - `**` matches any sequence including `/`
    /// - Literal characters match exactly
    fn matches_glob(glob: &str, path: &str) -> bool {
        Self::glob_match_impl(glob.as_bytes(), path.as_bytes())
    }

    fn glob_match_impl(glob: &[u8], path: &[u8]) -> bool {
        let mut gi = 0;
        let mut pi = 0;

        while gi < glob.len() && pi < path.len() {
            if gi + 1 < glob.len() && glob[gi] == b'*' && glob[gi + 1] == b'*' {
                // ** pattern: match zero or more path segments
                if gi + 2 >= glob.len() {
                    // ** at end matches everything
                    return true;
                }

                // Skip the **
                gi += 2;

                // Skip optional /
                if gi < glob.len() && glob[gi] == b'/' {
                    gi += 1;
                }

                // Try to match rest of pattern at any position
                while pi <= path.len() {
                    if Self::glob_match_impl(&glob[gi..], &path[pi..]) {
                        return true;
                    }
                    if pi < path.len() {
                        pi += 1;
                    } else {
                        break;
                    }
                }
                return false;
            } else if glob[gi] == b'*' {
                // * pattern: match until /
                gi += 1;
                while pi < path.len() && path[pi] != b'/' {
                    if Self::glob_match_impl(&glob[gi..], &path[pi..]) {
                        return true;
                    }
                    pi += 1;
                }
                // Also try matching nothing
                if Self::glob_match_impl(&glob[gi..], &path[pi..]) {
                    return true;
                }
                return false;
            } else if glob[gi] == path[pi] {
                // Literal match
                gi += 1;
                pi += 1;
            } else {
                return false;
            }
        }

        // Both should be consumed
        gi == glob.len() && pi == path.len()
    }

    /// Compose multiple instruction files into a single merged document.
    ///
    /// Respects precedence: higher-scope files are appended last and are
    /// conceptually "outermost" in the merged result. Each section is prefixed
    /// with a comment indicating its source.
    pub fn compose(&self, files: &[InstructionFile]) -> String {
        let mut result = String::new();

        for file in files {
            if !result.is_empty() {
                result.push('\n');
            }

            result.push_str(&format!(
                "<!-- Instructions from {} ({} scope) -->\n",
                file.path, file.scope
            ));

            result.push_str(&file.body);

            result.push('\n');
        }

        result
    }

    /// Resolve markdown-link targets across instruction files.
    ///
    /// Builds a lookup from filename stem to absolute path from all loaded files,
    /// then rewrites relative link targets to absolute paths when the target
    /// exists in the loaded set.
    pub fn resolve_links(files: &mut [InstructionFile]) {
        use std::collections::HashMap;

        let mut by_stem: HashMap<String, String> = HashMap::new();
        for file in files.iter() {
            let stem = std::path::Path::new(&file.path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if !stem.is_empty() {
                by_stem.insert(stem, file.path.clone());
            }
        }

        for file in files.iter_mut() {
            let parent = std::path::Path::new(&file.path)
                .parent()
                .map(|p| p.to_path_buf());
            file.body = Self::resolve_links_in_text(&file.body, &by_stem, parent.as_deref());
        }
    }

    fn resolve_links_in_text(
        text: &str,
        by_stem: &HashMap<String, String>,
        parent: Option<&Path>,
    ) -> String {
        let mut out = String::with_capacity(text.len());
        let bytes = text.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == b'[' {
                if let Some((end, title, target)) = Self::try_parse_md_link(text, i) {
                    let resolved = Self::resolve_target(&target, by_stem, parent);
                    out.push_str(&format!("[{}]({})", title, resolved));
                    i = end;
                    continue;
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    fn try_parse_md_link(text: &str, start: usize) -> Option<(usize, String, String)> {
        let bytes = text.as_bytes();
        if start > 0 && bytes[start - 1] == b'!' {
            return None;
        }
        let close_bracket = text[start + 1..].find(']')? + start + 1;
        if close_bracket + 1 >= text.len() || text.as_bytes()[close_bracket + 1] != b'(' {
            return None;
        }
        let open_paren = close_bracket + 1;
        let close_paren = text[open_paren + 1..].find(')')? + open_paren + 1;
        let title = text[start + 1..close_bracket].to_string();
        let target = text[open_paren + 1..close_paren].to_string();
        Some((close_paren + 1, title, target))
    }

    fn resolve_target(
        target: &str,
        by_stem: &HashMap<String, String>,
        parent: Option<&Path>,
    ) -> String {
        if target.starts_with("http://") || target.starts_with("https://") {
            return target.to_string();
        }
        let target_stem = Path::new(target)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if let Some(absolute) = by_stem.get(target_stem) {
            return absolute.clone();
        }
        if let Some(parent) = parent {
            let resolved = parent.join(target);
            if resolved.exists() {
                return resolved.to_string_lossy().to_string();
            }
        }
        target.to_string()
    }
}

impl Default for InstructionLoader {
    fn default() -> Self {
        Self::new()
    }
}
