//! Bounded, resumable regex search over a workspace (feature `workspace-grep`).
//!
//! The walk honours `.gitignore`, `.ignore` and hidden-file rules, also outside a git
//! repository, and visits files in a fixed order: sorted by name at each directory level. A page
//! runs until one of its budgets is spent, its deadline passes or its cancel flag is set, and then
//! returns a cursor for the next page. The cursor names the position reached and is bound by
//! SHA-256 to the root, pattern and options, so it cannot resume a different search. A cursor
//! whose file has been removed since is refused with [`GrepError::CursorStale`].
//!
//! Files over `max_file_bytes` and binary files (a NUL in the first 8 KiB) are skipped and counted
//! on the page; they are never searched partially.

use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use regex::bytes::RegexBuilder;
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Longest accepted pattern, in bytes.
pub const MAX_PATTERN_BYTES: usize = 4096;
/// Most context lines before or after a match.
pub const MAX_CONTEXT_LINES: usize = 10;
const CURSOR_PREFIX: &str = "rkg1";
const BINARY_SNIFF_BYTES: usize = 8192;

/// Why a search or its cursor was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrepError {
    /// The root is not an existing directory.
    NotADirectory(PathBuf),
    /// The pattern is empty, too long, or not a valid regular expression.
    BadPattern(String),
    /// The glob is not a valid override glob.
    BadGlob(String),
    /// A budget field is zero.
    BadBudget(&'static str),
    /// More context lines than [`MAX_CONTEXT_LINES`].
    BadContext(usize),
    /// The cursor is not one this crate issued.
    BadCursor,
    /// The cursor belongs to a different root, pattern or option set.
    CursorMismatch,
    /// The file the cursor names is no longer in the walk, so the position cannot be found.
    CursorStale,
}

impl fmt::Display for GrepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADirectory(p) => write!(f, "search root is not a directory: {}", p.display()),
            Self::BadPattern(why) => write!(f, "invalid search pattern: {why}"),
            Self::BadGlob(why) => write!(f, "invalid glob: {why}"),
            Self::BadBudget(field) => write!(f, "budget field {field} must be positive"),
            Self::BadContext(n) => write!(f, "context {n} is above {MAX_CONTEXT_LINES} lines"),
            Self::BadCursor => write!(f, "cursor was not issued by this search"),
            Self::CursorMismatch => write!(f, "cursor belongs to a different search"),
            Self::CursorStale => write!(
                f,
                "cursor file is no longer in the search; restart the search"
            ),
        }
    }
}

impl std::error::Error for GrepError {}

/// What each match reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepMode {
    /// One item per matching line, with its context.
    Content,
    /// One item per file that has a match.
    FilesWithMatches,
    /// One item per file with a match, carrying its number of matching lines.
    Count,
}

/// How a page ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepStatus {
    /// The walk reached its end: there is no next page.
    Complete,
    /// A page budget (results, files, bytes) stopped the page; the cursor continues it.
    Truncated,
    /// The cancel flag was set; the cursor continues from the same position.
    Cancelled,
    /// The page deadline passed; the cursor continues from the same position.
    Deadline,
}

/// Per-page limits. Every field must be positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrepBudget {
    /// Items (lines, files or counts) per page.
    pub max_results: usize,
    /// Files read per page.
    pub max_files: usize,
    /// Largest file read; bigger files are skipped and counted.
    pub max_file_bytes: u64,
    /// Bytes read per page, checked before each file.
    pub max_page_bytes: u64,
    /// Wall-clock limit for one page.
    pub deadline: Duration,
}

impl Default for GrepBudget {
    fn default() -> Self {
        Self {
            max_results: 200,
            max_files: 2_000,
            max_file_bytes: 4 * 1024 * 1024,
            max_page_bytes: 64 * 1024 * 1024,
            deadline: Duration::from_secs(5),
        }
    }
}

/// One search request.
#[derive(Debug, Clone)]
pub struct GrepRequest {
    /// Existing directory to search under.
    pub root: PathBuf,
    /// Regular expression, matched per line over bytes.
    pub pattern: String,
    /// Case-sensitive matching; `false` folds ASCII and Unicode case.
    pub case_sensitive: bool,
    /// Optional override glob that keeps only matching files, for example `*.rs`.
    pub glob: Option<String>,
    pub mode: GrepMode,
    /// Lines of context before and after each match (content mode).
    pub context: usize,
    pub budget: GrepBudget,
    /// Cursor from the previous page, or `None` for the first page.
    pub cursor: Option<String>,
}

/// One result item. Paths are relative to the root, with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrepItem {
    Line {
        path: String,
        /// 1-based line number.
        line: u64,
        text: String,
        before: Vec<String>,
        after: Vec<String>,
    },
    File {
        path: String,
    },
    Count {
        path: String,
        count: u64,
    },
}

/// One page of results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepPage {
    pub items: Vec<GrepItem>,
    pub status: GrepStatus,
    /// Cursor for the next page; `None` when the search is complete.
    pub next: Option<String>,
    /// Files read on this page.
    pub files_searched: usize,
    /// Bytes read on this page.
    pub bytes_read: u64,
    /// Files skipped for exceeding `max_file_bytes`.
    pub skipped_large: usize,
    /// Files skipped as binary.
    pub skipped_binary: usize,
    /// Entries or files that could not be read.
    pub skipped_unreadable: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Position {
    /// The start of the walk.
    Start,
    /// Continue after this file, which is done.
    After(String),
    /// Continue inside this file, from this 1-based line.
    At { path: String, line: u64 },
}

fn validate(req: &GrepRequest) -> Result<(), GrepError> {
    if !req.root.is_dir() {
        return Err(GrepError::NotADirectory(req.root.clone()));
    }
    if req.pattern.is_empty() || req.pattern.len() > MAX_PATTERN_BYTES {
        return Err(GrepError::BadPattern(format!(
            "1 to {MAX_PATTERN_BYTES} bytes are needed"
        )));
    }
    if req.context > MAX_CONTEXT_LINES {
        return Err(GrepError::BadContext(req.context));
    }
    let b = &req.budget;
    if b.max_results == 0 {
        return Err(GrepError::BadBudget("max_results"));
    }
    if b.max_files == 0 {
        return Err(GrepError::BadBudget("max_files"));
    }
    if b.max_file_bytes == 0 {
        return Err(GrepError::BadBudget("max_file_bytes"));
    }
    if b.max_page_bytes == 0 {
        return Err(GrepError::BadBudget("max_page_bytes"));
    }
    if b.deadline.is_zero() {
        return Err(GrepError::BadBudget("deadline"));
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// SHA-256 over the root, pattern, glob, case mode, mode and context: the inputs a cursor is
/// bound to. Budgets are not included, so a later page may use different limits.
fn binding_digest(req: &GrepRequest) -> String {
    let root = req.root.to_string_lossy().into_owned();
    let glob = req.glob.clone().unwrap_or_default();
    let mode = match req.mode {
        GrepMode::Content => "content",
        GrepMode::FilesWithMatches => "files",
        GrepMode::Count => "count",
    };
    let case = if req.case_sensitive { "cs" } else { "ci" };
    let mut hasher = Sha256::new();
    for field in [
        root.as_str(),
        req.pattern.as_str(),
        glob.as_str(),
        mode,
        case,
    ] {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hasher.update((req.context as u64).to_le_bytes());
    hex_encode(&hasher.finalize())
}

fn encode_cursor(digest: &str, position: &Position) -> String {
    let (kind, path, line) = match position {
        Position::Start => ("s", "", 0),
        Position::After(p) => ("a", p.as_str(), 0),
        Position::At { path, line } => ("l", path.as_str(), *line),
    };
    format!(
        "{CURSOR_PREFIX}.{digest}.{kind}.{}.{line}",
        hex_encode(path.as_bytes())
    )
}

fn decode_cursor(text: &str, digest: &str) -> Result<Position, GrepError> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 5 || parts[0] != CURSOR_PREFIX {
        return Err(GrepError::BadCursor);
    }
    if parts[1] != digest {
        return Err(GrepError::CursorMismatch);
    }
    let path = || {
        hex_decode(parts[3])
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or(GrepError::BadCursor)
    };
    let line: u64 = parts[4].parse().map_err(|_| GrepError::BadCursor)?;
    match parts[2] {
        "s" => Ok(Position::Start),
        "a" => Ok(Position::After(path()?)),
        "l" if line >= 1 => Ok(Position::At {
            path: path()?,
            line,
        }),
        _ => Err(GrepError::BadCursor),
    }
}

/// Path relative to `root` with `/` separators, lossy for non-UTF-8 names.
fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Lines of `data`, without the newline that ends the last line.
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&[u8]> = data.split(|b| *b == b'\n').collect();
    if data.last() == Some(&b'\n') {
        lines.pop();
    }
    lines
}

fn line_text(line: &[u8]) -> String {
    let trimmed = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(trimmed).into_owned()
}

/// Run one page of the search. `cancel` is polled before each file; set it to stop early.
pub fn grep_page(req: &GrepRequest, cancel: &AtomicBool) -> Result<GrepPage, GrepError> {
    validate(req)?;
    let regex = RegexBuilder::new(&req.pattern)
        .case_insensitive(!req.case_sensitive)
        .build()
        .map_err(|e| GrepError::BadPattern(e.to_string()))?;
    let digest = binding_digest(req);
    let resume = match &req.cursor {
        None => Position::Start,
        Some(text) => decode_cursor(text, &digest)?,
    };

    let mut walk = WalkBuilder::new(&req.root);
    walk.require_git(false).sort_by_file_name(|a, b| a.cmp(b));
    if let Some(glob) = &req.glob {
        let mut overrides = OverrideBuilder::new(&req.root);
        overrides
            .add(glob)
            .map_err(|e| GrepError::BadGlob(e.to_string()))?;
        let built = overrides
            .build()
            .map_err(|e| GrepError::BadGlob(e.to_string()))?;
        walk.overrides(built);
    }

    let deadline = Instant::now() + req.budget.deadline;
    let max_results = req.budget.max_results;
    let mut page = GrepPage {
        items: Vec::new(),
        status: GrepStatus::Complete,
        next: None,
        files_searched: 0,
        bytes_read: 0,
        skipped_large: 0,
        skipped_binary: 0,
        skipped_unreadable: 0,
    };
    let mut started = resume == Position::Start;
    // Path of the last file this page finished. `None` means the page has finished no file.
    let mut last_done: Option<String> = None;

    'walk: for entry in walk.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                page.skipped_unreadable += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Some(rel) = rel_path(&req.root, entry.path()) else {
            continue;
        };
        let mut start_line = 1u64;
        if !started {
            match &resume {
                Position::After(p) if *p == rel => {
                    started = true;
                    continue;
                }
                Position::At { path, line } if *path == rel => {
                    started = true;
                    start_line = *line;
                }
                _ => continue,
            }
        }
        // Where the next page starts if this page stops before reading this file.
        let here = match &last_done {
            Some(p) => Position::After(p.clone()),
            None => resume.clone(),
        };
        if cancel.load(Ordering::Relaxed) {
            page.status = GrepStatus::Cancelled;
            page.next = Some(encode_cursor(&digest, &here));
            break 'walk;
        }
        if Instant::now() >= deadline {
            page.status = GrepStatus::Deadline;
            page.next = Some(encode_cursor(&digest, &here));
            break 'walk;
        }
        if page.files_searched >= req.budget.max_files
            || page.bytes_read >= req.budget.max_page_bytes
        {
            page.status = GrepStatus::Truncated;
            page.next = Some(encode_cursor(&digest, &here));
            break 'walk;
        }
        let size = match entry.metadata() {
            Ok(meta) => meta.len(),
            Err(_) => {
                page.skipped_unreadable += 1;
                last_done = Some(rel);
                continue;
            }
        };
        if size > req.budget.max_file_bytes {
            page.skipped_large += 1;
            last_done = Some(rel);
            continue;
        }
        let data = match std::fs::read(entry.path()) {
            Ok(data) => data,
            Err(_) => {
                page.skipped_unreadable += 1;
                last_done = Some(rel);
                continue;
            }
        };
        if data.len() as u64 > req.budget.max_file_bytes {
            page.skipped_large += 1;
            last_done = Some(rel);
            continue;
        }
        page.files_searched += 1;
        page.bytes_read += data.len() as u64;
        if data[..data.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
            page.skipped_binary += 1;
            last_done = Some(rel);
            continue;
        }

        let lines = split_lines(&data);
        match req.mode {
            GrepMode::Count => {
                let count = lines.iter().filter(|l| regex.is_match(l)).count() as u64;
                if count > 0 {
                    if page.items.len() >= max_results {
                        page.status = GrepStatus::Truncated;
                        page.next = Some(encode_cursor(&digest, &here));
                        break 'walk;
                    }
                    page.items.push(GrepItem::Count {
                        path: rel.clone(),
                        count,
                    });
                }
            }
            GrepMode::FilesWithMatches => {
                if lines.iter().any(|l| regex.is_match(l)) {
                    if page.items.len() >= max_results {
                        page.status = GrepStatus::Truncated;
                        page.next = Some(encode_cursor(&digest, &here));
                        break 'walk;
                    }
                    page.items.push(GrepItem::File { path: rel.clone() });
                }
            }
            GrepMode::Content => {
                let first = start_line as usize - 1;
                for idx in first..lines.len() {
                    if !regex.is_match(lines[idx]) {
                        continue;
                    }
                    if page.items.len() >= max_results {
                        page.status = GrepStatus::Truncated;
                        let at = Position::At {
                            path: rel.clone(),
                            line: idx as u64 + 1,
                        };
                        page.next = Some(encode_cursor(&digest, &at));
                        break 'walk;
                    }
                    let before_from = idx.saturating_sub(req.context);
                    let after_to = (idx + 1 + req.context).min(lines.len());
                    page.items.push(GrepItem::Line {
                        path: rel.clone(),
                        line: idx as u64 + 1,
                        text: line_text(lines[idx]),
                        before: lines[before_from..idx]
                            .iter()
                            .map(|l| line_text(l))
                            .collect(),
                        after: lines[idx + 1..after_to]
                            .iter()
                            .map(|l| line_text(l))
                            .collect(),
                    });
                }
            }
        }
        last_done = Some(rel);
    }

    if !started {
        return Err(GrepError::CursorStale);
    }
    Ok(page)
}
