//! Ignore-aware directory walk for repository and project trees.
//!
//! The walk reads `.gitignore` files as it descends, so a rule in a directory governs only the
//! entries below it, as in git. `.git` is always skipped. Symbolic links are reported but never
//! followed, so a link cannot pull a walk outside the tree it was started in or loop forever.
//! Entries within one directory are visited in name order, so the output is deterministic.
//!
//! Supported pattern syntax: `*`, `?`, `[...]` (with `!` or `^` negation and ranges), `**` as a
//! whole path segment, a leading or interior `/` for anchoring, a trailing `/` for directories
//! only, `#` comments, `!` negation (the last matching rule wins), and `\#`, `\!` escapes.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Options for [`Walk`].
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Honour `.gitignore` files and [`WalkOptions::extra_ignores`]. Default `true`.
    pub respect_gitignore: bool,
    /// Yield and descend into names starting with `.`. Default `false`.
    pub include_hidden: bool,
    /// Levels below the root to yield: `1` yields only the root's children. `None` is unlimited.
    pub max_depth: Option<usize>,
    /// Extra patterns applied at the root, as if they were in a root `.gitignore`.
    pub extra_ignores: Vec<String>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            respect_gitignore: true,
            include_hidden: false,
            max_depth: None,
            extra_ignores: Vec::new(),
        }
    }
}

/// One entry yielded by [`Walk`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkEntry {
    /// Full path of the entry.
    pub path: PathBuf,
    /// Path relative to the walk root, with `/` separators.
    pub rel: String,
    /// `true` for a real directory. A symbolic link to a directory is `false` and is not descended.
    pub is_dir: bool,
    /// Levels below the root: root children are `1`.
    pub depth: usize,
}

#[derive(Debug, Clone)]
struct Rule {
    /// Directory of the ignore file, relative to the root, with `/` separators; empty at the root.
    base: String,
    /// Pattern split on `/`.
    segments: Vec<String>,
    negate: bool,
    dir_only: bool,
    anchored: bool,
}

impl Rule {
    fn matches(&self, rel: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        let local = if self.base.is_empty() {
            rel
        } else {
            match rel
                .strip_prefix(self.base.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
            {
                Some(local) => local,
                None => return false,
            }
        };
        if self.anchored {
            let pattern: Vec<&str> = self.segments.iter().map(String::as_str).collect();
            let parts: Vec<&str> = local.split('/').collect();
            match_segments(&pattern, &parts)
        } else {
            let name = local.rsplit('/').next().unwrap_or("");
            self.segments.len() == 1 && fnmatch(self.segments[0].as_bytes(), name.as_bytes())
        }
    }
}

fn parse_line(line: &str, base: &str) -> Option<Rule> {
    let line = line.trim_end();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (negate, body) = match line.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let body = if body.starts_with("\\#") || body.starts_with("\\!") {
        &body[1..]
    } else {
        body
    };
    let (dir_only, body) = match body.strip_suffix('/') {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    let anchored = body.contains('/');
    let body = body.strip_prefix('/').unwrap_or(body);
    let segments: Vec<String> = body
        .split('/')
        .filter(|part| !part.is_empty())
        .map(String::from)
        .collect();
    if segments.is_empty() {
        return None;
    }
    Some(Rule {
        base: base.to_string(),
        segments,
        negate,
        dir_only,
        anchored,
    })
}

fn match_segments(pattern: &[&str], parts: &[&str]) -> bool {
    match pattern.split_first() {
        None => parts.is_empty(),
        Some((head, rest)) if *head == "**" => {
            (0..=parts.len()).any(|skip| match_segments(rest, &parts[skip..]))
        }
        Some((head, rest)) => match parts.split_first() {
            Some((part, parts_rest)) => {
                fnmatch(head.as_bytes(), part.as_bytes()) && match_segments(rest, parts_rest)
            }
            None => false,
        },
    }
}

fn fnmatch(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first().copied() {
        None => text.is_empty(),
        Some(b'*') => (0..=text.len()).any(|skip| fnmatch(&pattern[1..], &text[skip..])),
        Some(b'?') => !text.is_empty() && fnmatch(&pattern[1..], &text[1..]),
        Some(b'[') => match text.first().copied() {
            None => false,
            Some(byte) => match class_match(&pattern[1..], byte) {
                Some((matched, consumed)) => {
                    matched && fnmatch(&pattern[1 + consumed..], &text[1..])
                }
                None => byte == b'[' && fnmatch(&pattern[1..], &text[1..]),
            },
        },
        Some(b'\\') if pattern.len() > 1 => {
            !text.is_empty() && text[0] == pattern[1] && fnmatch(&pattern[2..], &text[1..])
        }
        Some(literal) => {
            !text.is_empty() && text[0] == literal && fnmatch(&pattern[1..], &text[1..])
        }
    }
}

/// Matches one byte against a bracket class that starts after `[`. Returns the match result and
/// the number of pattern bytes consumed, including the closing `]`, or `None` if it is unclosed.
fn class_match(pattern: &[u8], byte: u8) -> Option<(bool, usize)> {
    let negate = matches!(pattern.first(), Some(b'!') | Some(b'^'));
    let mut index = usize::from(negate);
    let mut matched = false;
    let mut first = true;
    while index < pattern.len() {
        if pattern[index] == b']' && !first {
            return Some((matched != negate, index + 1));
        }
        first = false;
        let low = pattern[index];
        if index + 2 < pattern.len() && pattern[index + 1] == b'-' && pattern[index + 2] != b']' {
            if low <= byte && byte <= pattern[index + 2] {
                matched = true;
            }
            index += 3;
        } else {
            if low == byte {
                matched = true;
            }
            index += 1;
        }
    }
    None
}

fn is_ignored(rules: &[Rule], rel: &str, is_dir: bool) -> bool {
    let mut ignored = false;
    for rule in rules {
        if rule.matches(rel, is_dir) {
            ignored = !rule.negate;
        }
    }
    ignored
}

fn load_rules(dir: &Path, base: &str, parent: &Arc<Vec<Rule>>) -> io::Result<Arc<Vec<Rule>>> {
    let text = match fs::read_to_string(dir.join(".gitignore")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Arc::clone(parent)),
        Err(error) => return Err(error),
    };
    let mut rules: Vec<Rule> = parent.as_ref().clone();
    rules.extend(text.lines().filter_map(|line| parse_line(line, base)));
    Ok(Arc::new(rules))
}

struct Frame {
    dir: PathBuf,
    rel: String,
    depth: usize,
    rules: Arc<Vec<Rule>>,
}

/// Iterator over the entries under a root that are not ignored.
pub struct Walk {
    options: WalkOptions,
    stack: Vec<Frame>,
    ready: VecDeque<io::Result<WalkEntry>>,
}

impl Walk {
    /// Starts a walk at `root`. Fails if the root's own `.gitignore` exists but cannot be read.
    pub fn new(root: impl AsRef<Path>, options: WalkOptions) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let mut base_rules = Vec::new();
        for pattern in &options.extra_ignores {
            if let Some(rule) = parse_line(pattern, "") {
                base_rules.push(rule);
            }
        }
        let mut rules = Arc::new(base_rules);
        if options.respect_gitignore {
            rules = load_rules(&root, "", &rules)?;
        }
        Ok(Self {
            options,
            stack: vec![Frame {
                dir: root,
                rel: String::new(),
                depth: 0,
                rules,
            }],
            ready: VecDeque::new(),
        })
    }

    fn expand(&mut self, frame: Frame) {
        let read = match fs::read_dir(&frame.dir) {
            Ok(read) => read,
            Err(error) => {
                self.ready.push_back(Err(error));
                return;
            }
        };
        let mut children = Vec::new();
        for item in read {
            match item {
                Ok(child) => children.push(child),
                Err(error) => self.ready.push_back(Err(error)),
            }
        }
        children.sort_by_key(|child| child.file_name());

        let mut subdirectories = Vec::new();
        for child in children {
            let name = child.file_name().to_string_lossy().into_owned();
            if name == ".git" || (!self.options.include_hidden && name.starts_with('.')) {
                continue;
            }
            let file_type = match child.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    self.ready.push_back(Err(error));
                    continue;
                }
            };
            let is_dir = file_type.is_dir();
            let rel = if frame.rel.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", frame.rel, name)
            };
            if self.options.respect_gitignore && is_ignored(&frame.rules, &rel, is_dir) {
                continue;
            }
            let depth = frame.depth + 1;
            let path = child.path();
            self.ready.push_back(Ok(WalkEntry {
                path: path.clone(),
                rel: rel.clone(),
                is_dir,
                depth,
            }));
            let descend = is_dir && self.options.max_depth.is_none_or(|limit| depth < limit);
            if descend {
                let rules = if self.options.respect_gitignore {
                    match load_rules(&path, &rel, &frame.rules) {
                        Ok(rules) => rules,
                        Err(error) => {
                            self.ready.push_back(Err(error));
                            continue;
                        }
                    }
                } else {
                    Arc::clone(&frame.rules)
                };
                subdirectories.push(Frame {
                    dir: path,
                    rel,
                    depth,
                    rules,
                });
            }
        }
        // Reverse so the first subdirectory in name order is expanded first.
        self.stack.extend(subdirectories.into_iter().rev());
    }
}

impl Iterator for Walk {
    type Item = io::Result<WalkEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.ready.pop_front() {
                return Some(item);
            }
            let frame = self.stack.pop()?;
            self.expand(frame);
        }
    }
}
