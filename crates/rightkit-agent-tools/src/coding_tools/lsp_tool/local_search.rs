/// Hard bound on how many code files the local fallback scanner will walk.
/// Definition/hover fall back to a plain-text symbol scan only when no live
/// language server answered; that scan must stay bounded so a huge repository
/// (or a symlink cycle) cannot turn a single tool call into an unbounded walk.
const MAX_LOCAL_SCAN_FILES: usize = 20_000;

/// Files larger than this are skipped by the local fallback scan. A single
/// generated or vendored file above this size cannot usefully contain a
/// hand-written definition worth surfacing, and reading it would dominate the
/// scan's latency and memory for no benefit.
const MAX_LOCAL_SCAN_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
struct SymbolMatch {
    path: PathBuf,
    line: usize,
    character: usize,
    snippet: String,
}

fn find_definitions(root: &Path, symbol: &str) -> Vec<SymbolMatch> {
    let mut matches = Vec::new();
    for file in code_files(root) {
        if std::fs::metadata(&file)
            .map(|meta| meta.len() > MAX_LOCAL_SCAN_FILE_BYTES)
            .unwrap_or(false)
        {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            if is_definition_line(line, symbol) {
                matches.push(SymbolMatch {
                    path: file.clone(),
                    line: idx + 1,
                    character: line.find(symbol).unwrap_or(0),
                    snippet: line.trim().to_string(),
                });
            }
        }
    }
    matches
}

fn render_matches(matches: &[SymbolMatch]) -> String {
    matches
        .iter()
        .take(50)
        .map(|m| {
            format!(
                "{}:{}:{} {}",
                m.path.display(),
                m.line,
                m.character,
                m.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn code_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    visit_code_files(root, &mut files);
    files.sort();
    files
}

fn visit_code_files(dir: &Path, files: &mut Vec<PathBuf>) {
    if files.len() >= MAX_LOCAL_SCAN_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() >= MAX_LOCAL_SCAN_FILES {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), ".git" | "target" | "node_modules" | "dist") {
            continue;
        }
        // Symlinks are skipped rather than followed: `file_type()` reports the
        // link itself, so a symlinked directory would otherwise recurse into
        // `visit_code_files` and can cycle back to an ancestor, growing the
        // walk without bound.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            visit_code_files(&path, files);
        } else if is_code_file(&path) {
            files.push(path);
        }
    }
}

fn is_code_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py"
    )
}

fn is_definition_line(line: &str, symbol: &str) -> bool {
    let trimmed = line.trim_start();
    let prefixes = [
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "impl ",
        "function ",
        "export function ",
        "class ",
        "export class ",
        "const ",
        "let ",
        "export const ",
        "def ",
    ];
    prefixes.iter().any(|prefix| {
        trimmed
            .strip_prefix(prefix)
            .map(|rest| rest.starts_with(symbol) && follows_symbol(rest, symbol.len()))
            .unwrap_or(false)
    })
}

fn follows_symbol(s: &str, len: usize) -> bool {
    s.chars()
        .nth(len)
        .map(|c| !is_symbol_char(c))
        .unwrap_or(true)
}

fn symbol_at(content: &str, line_index: usize, character: usize) -> Option<String> {
    let line = content.lines().nth(line_index)?;
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut idx = character.min(chars.len().saturating_sub(1));
    if !is_symbol_char(chars[idx]) && idx > 0 {
        idx -= 1;
    }
    if !is_symbol_char(chars[idx]) {
        return None;
    }
    let mut start = idx;
    while start > 0 && is_symbol_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = idx + 1;
    while end < chars.len() && is_symbol_char(chars[end]) {
        end += 1;
    }
    Some(chars[start..end].iter().collect())
}

fn is_symbol_char(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

pub(super) fn file_uri(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('\\', "/");
    if !s.starts_with('/') {
        s = format!("/{s}");
    }
    format!("file://{s}")
}
