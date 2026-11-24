//! Bounded, resumable, byte-streaming regex search over a workspace with a per-file
//! authorization hook, multiline mode, asymmetric context and a mid-file byte cursor.
//!
//! It complements [`crate::grep::grep_page`], which reads whole files and resumes only at line
//! starts, and leaves it unchanged.
//!
//! # Authority
//!
//! [`StreamHooks::admit_file`] and [`StreamHooks::authorize_read`] are consulted before a file is
//! opened. The hook is the caller's authority: a hook that returns `true` for a path the caller
//! must not read leaks that file.
//!
//! # Resume
//!
//! The walk and the files stay live between pages, so continuation is best-effort. The cursor is
//! bound by SHA-256 to the roots, pattern and options (budgets are not bound).

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use regex::bytes::{Regex, RegexBuilder};
use sha2::{Digest, Sha256};

/// Largest number of context lines kept before or after a match.
pub const MAX_CONTEXT_LINES: usize = 100;
/// Longest retained line, in bytes.
pub const MAX_LINE_BYTES: usize = 256 * 1024;
/// Longest line scanned before giving up, in bytes.
pub const MAX_LOGICAL_LINE_SCAN_BYTES: u64 = 1024 * 1024;
/// Most bytes read from one file per page (also the multiline file cap).
pub const MAX_FILE_BYTES_PER_PAGE: u64 = 8 * 1024 * 1024;
/// Most bytes read per page across all files.
pub const MAX_TOTAL_BYTES_PER_PAGE: u64 = 64 * 1024 * 1024;
/// Output budget of one page, in bytes.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Longest cursor string; a longer cursor is dropped.
pub const MAX_CURSOR_BYTES: usize = 64 * 1024;
/// Most files opened per page.
pub const MAX_FILES_PER_PAGE: usize = 20_000;
/// Leading bytes probed for NUL to detect a binary file.
pub const BINARY_PROBE_BYTES: usize = 8 * 1024;
/// Default result limit.
pub const DEFAULT_MAX_RESULTS: usize = 200;
/// Default deadline.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);
/// Longest accepted deadline.
pub const MAX_DEADLINE: Duration = Duration::from_secs(60);
/// Cursor schema version.
pub const CURSOR_VERSION: u32 = 1;

const MAX_RESULT_BYTES: usize = MAX_OUTPUT_BYTES - MAX_CURSOR_BYTES - 512;
const READER_BYTES: usize = 64 * 1024;
const BAD_CURSOR: &str = "cursor does not match query/options, roots or schema; restart search";
const BAD_POSITIONS: &str = "invalid cursor context positions; restart search";

/// What a search reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamMode {
    /// Matching lines, with optional context.
    Content,
    /// One row per file that has a match.
    FilesWithMatches,
    /// One row per file with the number of matching lines (multiline: matches).
    Count,
}

/// How a page ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamStatus {
    /// The walk finished with no ceiling hit.
    Complete,
    /// A budget, ceiling or skipped input cut the page short; see the reason.
    Truncated,
    /// `StreamHooks::cancelled` returned true.
    Cancelled,
    /// The deadline passed.
    Deadline,
}

/// One row of a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamItem {
    /// A matching line.
    Match {
        /// Absolute path as walked.
        path: PathBuf,
        /// 1-based line number.
        line: u64,
        /// Line text (lossy UTF-8, line ending removed).
        text: String,
    },
    /// A context line next to a match.
    Context {
        /// Absolute path as walked.
        path: PathBuf,
        /// 1-based line number.
        line: u64,
        /// Line text (lossy UTF-8, line ending removed).
        text: String,
    },
    /// A gap between two context groups.
    Separator,
    /// A file with at least one match.
    FileHit {
        /// Absolute path as walked.
        path: PathBuf,
    },
    /// The match count of one file.
    Count {
        /// Absolute path as walked.
        path: PathBuf,
        /// Number of matches.
        count: u64,
    },
}

/// A request error.
#[derive(Debug)]
pub enum StreamError {
    /// The pattern does not compile.
    BadPattern(String),
    /// The root or workspace root does not exist.
    NotFound(PathBuf),
    /// The root is not inside the workspace root.
    RootOutsideWorkspace(PathBuf),
    /// The cursor is refused.
    BadCursor(&'static str),
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamError::BadPattern(e) => write!(f, "invalid pattern: {e}"),
            StreamError::NotFound(p) => write!(f, "path not found: {}", p.display()),
            StreamError::RootOutsideWorkspace(p) => {
                write!(f, "root is outside the workspace: {}", p.display())
            }
            StreamError::BadCursor(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for StreamError {}

/// Caller hooks: cancellation, filtering, per-file authorization and row cost.
pub trait StreamHooks {
    /// True when the caller wants the search to stop.
    fn cancelled(&self) -> bool;

    /// Called for every file and directory entry by name; false prunes it (directories are not
    /// entered).
    fn admit_entry(&self, _name: &str) -> bool {
        true
    }

    /// `rel` is the workspace-relative path with `/` separators; false skips the file silently
    /// (the caller's glob filter and workspace-escape check live here).
    fn admit_file(&self, _rel: &str) -> bool {
        true
    }

    /// False: the file is never opened, never counted, never reported (deny-read).
    fn authorize_read(&self, _abs: &Path) -> bool {
        true
    }

    /// The bytes a rendered row will cost; override it to account masked or rendered text.
    fn row_bytes(&self, item: &StreamItem) -> usize {
        match item {
            StreamItem::Match { path, text, .. } | StreamItem::Context { path, text, .. } => {
                path.to_string_lossy().len() + text.len() + 16
            }
            StreamItem::FileHit { path } | StreamItem::Count { path, .. } => {
                path.to_string_lossy().len() + 16
            }
            StreamItem::Separator => 16,
        }
    }
}

/// One search request.
#[derive(Clone, Debug)]
pub struct StreamRequest {
    /// Workspace root; paths in the cursor and hooks are relative to it.
    pub workspace_root: PathBuf,
    /// Directory or single file under `workspace_root`.
    pub root: PathBuf,
    /// Regular expression, matched on raw bytes.
    pub pattern: String,
    /// Case-sensitive matching.
    pub case_sensitive: bool,
    /// Match across lines (no context, no mid-file cursor).
    pub multiline: bool,
    /// What to report.
    pub mode: StreamMode,
    /// Context lines before a match (clamped to [`MAX_CONTEXT_LINES`]).
    pub before: usize,
    /// Context lines after a match (clamped to [`MAX_CONTEXT_LINES`]).
    pub after: usize,
    /// Row limit (at least 1).
    pub max_results: usize,
    /// Time limit (clamped to [`MAX_DEADLINE`]).
    pub deadline: Duration,
    /// Byte limit across files (clamped to [`MAX_TOTAL_BYTES_PER_PAGE`]).
    pub max_page_bytes: u64,
    /// Walk hidden files.
    pub include_hidden: bool,
    /// Honour `.gitignore`.
    pub git_ignore: bool,
    /// Honour the global gitignore.
    pub git_global: bool,
    /// Require a git repository for gitignore rules to apply.
    pub require_git: bool,
    /// Cursor from a previous page.
    pub cursor: Option<String>,
    /// Caller data bound into the cursor.
    pub caller_binding: String,
}

impl StreamRequest {
    /// A request with the defaults: case-insensitive, single-line, content mode, no context,
    /// [`DEFAULT_MAX_RESULTS`], [`DEFAULT_DEADLINE`], [`MAX_TOTAL_BYTES_PER_PAGE`], hidden files
    /// off, gitignore on, global gitignore on, git not required, no cursor, empty binding.
    pub fn new(workspace_root: PathBuf, root: PathBuf, pattern: String) -> Self {
        StreamRequest {
            workspace_root,
            root,
            pattern,
            case_sensitive: false,
            multiline: false,
            mode: StreamMode::Content,
            before: 0,
            after: 0,
            max_results: DEFAULT_MAX_RESULTS,
            deadline: DEFAULT_DEADLINE,
            max_page_bytes: MAX_TOTAL_BYTES_PER_PAGE,
            include_hidden: false,
            git_ignore: true,
            git_global: true,
            require_git: false,
            cursor: None,
            caller_binding: String::new(),
        }
    }
}

/// One page of results.
#[derive(Debug, Clone)]
pub struct StreamPage {
    /// Rows, in walk order.
    pub items: Vec<StreamItem>,
    /// How the page ended.
    pub status: StreamStatus,
    /// Why the page is not complete.
    pub reason: Option<&'static str>,
    /// Cursor for the next page; present only when the status is not `Complete`.
    pub next: Option<String>,
    /// Files opened.
    pub files_scanned: usize,
    /// Bytes read.
    pub bytes_scanned: u64,
}

// ---------------------------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Pos {
    line: u64,
    offset: u64,
    len: usize,
}

#[derive(Clone, Debug)]
struct CursorState {
    rel: String,
    /// Byte offset of the next line to read.
    offset: u64,
    /// Lines consumed before `offset`.
    line: u64,
    count: u64,
    after_remaining: usize,
    last_printed: Option<u64>,
    separator_for: Option<u64>,
    before: Vec<Pos>,
}

impl CursorState {
    fn file_start(rel: &str) -> Self {
        CursorState {
            rel: rel.to_string(),
            offset: 0,
            line: 0,
            count: 0,
            after_remaining: 0,
            last_printed: None,
            separator_for: None,
            before: Vec::new(),
        }
    }
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

fn binding_digest(
    req: &StreamRequest,
    workspace: &Path,
    root: &Path,
    before: usize,
    after: usize,
) -> String {
    let mode = match req.mode {
        StreamMode::Content => 0u8,
        StreamMode::FilesWithMatches => 1,
        StreamMode::Count => 2,
    };
    let mut hasher = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    field(&CURSOR_VERSION.to_le_bytes());
    field(workspace.to_string_lossy().as_bytes());
    field(root.to_string_lossy().as_bytes());
    field(req.pattern.as_bytes());
    field(&[u8::from(req.case_sensitive)]);
    field(&[mode]);
    field(&[u8::from(req.multiline)]);
    field(&(before as u64).to_le_bytes());
    field(&(after as u64).to_le_bytes());
    field(&[u8::from(req.include_hidden)]);
    field(&[u8::from(req.git_ignore)]);
    field(&[u8::from(req.git_global)]);
    field(&[u8::from(req.require_git)]);
    field(req.caller_binding.as_bytes());
    hex_encode(&hasher.finalize())
}

fn cursor_prefix() -> String {
    format!("rks{CURSOR_VERSION}")
}

fn encode_cursor(binding: &str, c: &CursorState) -> Option<String> {
    let next_line = c.line + 1;
    let last_printed = c
        .last_printed
        .map_or_else(|| "-".to_string(), |v| v.to_string());
    let separator = c
        .separator_for
        .filter(|s| *s == next_line)
        .map_or_else(|| "-".to_string(), |v| v.to_string());
    let before = if c.before.is_empty() {
        "-".to_string()
    } else {
        c.before
            .iter()
            .map(|p| format!("{}:{}:{}", p.line, p.offset, p.len))
            .collect::<Vec<_>>()
            .join(",")
    };
    let text = format!(
        "{}.{}.{}.{}.{}.{}.{}.{}.{}.{}",
        cursor_prefix(),
        binding,
        hex_encode(c.rel.as_bytes()),
        c.offset,
        c.line,
        c.count,
        c.after_remaining,
        last_printed,
        separator,
        before
    );
    if text.len() > MAX_CURSOR_BYTES {
        None
    } else {
        Some(text)
    }
}

/// `Some(None)` for "-", `Some(Some(v))` for a number, `None` for anything else.
fn optional_number(text: &str) -> Option<Option<u64>> {
    if text == "-" {
        Some(None)
    } else {
        text.parse().ok().map(Some)
    }
}

fn decode_cursor(text: &str, binding: &str) -> Result<CursorState, StreamError> {
    let bad = || StreamError::BadCursor(BAD_CURSOR);
    let bad_positions = || StreamError::BadCursor(BAD_POSITIONS);
    if text.len() > MAX_CURSOR_BYTES {
        return Err(bad());
    }
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 10 || parts[0] != cursor_prefix() || parts[1] != binding {
        return Err(bad());
    }
    let rel = hex_decode(parts[2])
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(bad)?;
    let offset: u64 = parts[3].parse().map_err(|_| bad())?;
    let line: u64 = parts[4].parse().map_err(|_| bad())?;
    let count: u64 = parts[5].parse().map_err(|_| bad())?;
    let after_remaining: usize = parts[6].parse().map_err(|_| bad())?;
    let last_printed = optional_number(parts[7]).ok_or_else(bad)?;
    let separator_for = optional_number(parts[8]).ok_or_else(bad)?;
    let mut before = Vec::new();
    if parts[9] != "-" {
        for item in parts[9].split(',') {
            let fields: Vec<&str> = item.split(':').collect();
            if fields.len() != 3 {
                return Err(bad_positions());
            }
            let p_line: u64 = fields[0].parse().map_err(|_| bad_positions())?;
            let p_offset: u64 = fields[1].parse().map_err(|_| bad_positions())?;
            let p_len: usize = fields[2].parse().map_err(|_| bad_positions())?;
            before.push(Pos {
                line: p_line,
                offset: p_offset,
                len: p_len,
            });
        }
    }
    if before.len() > MAX_CONTEXT_LINES || after_remaining > MAX_CONTEXT_LINES {
        return Err(bad_positions());
    }
    if let Some(lp) = last_printed {
        if lp == 0 || lp > line {
            return Err(bad_positions());
        }
    }
    if let Some(s) = separator_for {
        if s != line.saturating_add(1) {
            return Err(bad_positions());
        }
    }
    let mut previous: Option<Pos> = None;
    for p in &before {
        if p.line == 0 || p.line > line || p.len == 0 || p.len > MAX_LINE_BYTES {
            return Err(bad_positions());
        }
        let end = p
            .offset
            .checked_add(p.len as u64)
            .ok_or_else(bad_positions)?;
        if end > offset {
            return Err(bad_positions());
        }
        if let Some(prev) = previous {
            if p.line <= prev.line || p.offset < prev.offset + prev.len as u64 {
                return Err(bad_positions());
            }
        }
        previous = Some(*p);
    }
    Ok(CursorState {
        rel,
        offset,
        line,
        count,
        after_remaining,
        last_printed,
        separator_for,
        before,
    })
}

// ---------------------------------------------------------------------------------------------
// Bounded line reader
// ---------------------------------------------------------------------------------------------

struct BoundedLine {
    /// Bytes taken from the file, including the terminator.
    consumed: u64,
    /// More than `retain` content bytes were seen.
    oversized: bool,
    /// False only when the scan limit was hit before a newline.
    terminated: bool,
}

/// Reads one line into `out` (content only, no LF), retaining at most `retain` bytes and
/// scanning at most `scan` content bytes. `None` at end of file.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    out: &mut Vec<u8>,
    retain: usize,
    scan: u64,
) -> std::io::Result<Option<BoundedLine>> {
    out.clear();
    let mut consumed: u64 = 0;
    let mut seen: u64 = 0;
    let mut terminated = false;
    loop {
        let (used, finished) = {
            let buf = match reader.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if buf.is_empty() {
                terminated = consumed > 0;
                break;
            }
            let newline = buf.iter().position(|b| *b == b'\n');
            let content = newline.unwrap_or(buf.len());
            let allowed = (scan - seen) as usize;
            let chunk = content.min(allowed);
            let room = retain.saturating_sub(out.len());
            out.extend_from_slice(&buf[..chunk.min(room)]);
            seen += chunk as u64;
            if chunk < content {
                (chunk, true)
            } else if newline.is_some() {
                terminated = true;
                (content + 1, true)
            } else {
                (content, false)
            }
        };
        reader.consume(used);
        consumed += used as u64;
        if finished {
            break;
        }
    }
    if consumed == 0 {
        return Ok(None);
    }
    Ok(Some(BoundedLine {
        consumed,
        oversized: seen > retain as u64,
        terminated,
    }))
}

fn trim_eol(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

fn line_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(trim_eol(bytes)).into_owned()
}

fn rel_string(path: &Path) -> String {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------------------------------------
// Page state
// ---------------------------------------------------------------------------------------------

enum Next {
    Nothing,
    Request,
    At(Box<CursorState>),
}

struct Stop {
    status: StreamStatus,
    reason: &'static str,
    next: Next,
}

impl Stop {
    fn none(status: StreamStatus, reason: &'static str) -> Self {
        Stop {
            status,
            reason,
            next: Next::Nothing,
        }
    }

    fn request(status: StreamStatus, reason: &'static str) -> Self {
        Stop {
            status,
            reason,
            next: Next::Request,
        }
    }

    fn at(status: StreamStatus, reason: &'static str, cursor: CursorState) -> Self {
        Stop {
            status,
            reason,
            next: Next::At(Box::new(cursor)),
        }
    }
}

struct Queued {
    line: u64,
    offset: u64,
    len: usize,
    text: Option<Vec<u8>>,
}

impl Queued {
    fn pos(&self) -> Pos {
        Pos {
            line: self.line,
            offset: self.offset,
            len: self.len,
        }
    }
}

struct LineState {
    offset: u64,
    line: u64,
    count: u64,
    after_remaining: usize,
    last_printed: Option<u64>,
    separator_for: Option<u64>,
    before: VecDeque<Queued>,
}

impl LineState {
    fn new() -> Self {
        LineState {
            offset: 0,
            line: 0,
            count: 0,
            after_remaining: 0,
            last_printed: None,
            separator_for: None,
            before: VecDeque::new(),
        }
    }

    fn resume(c: &CursorState, before_cap: usize) -> Self {
        let skip = c.before.len().saturating_sub(before_cap);
        LineState {
            offset: c.offset,
            line: c.line,
            count: c.count,
            after_remaining: c.after_remaining,
            last_printed: c.last_printed,
            separator_for: c.separator_for,
            before: c
                .before
                .iter()
                .skip(skip)
                .map(|p| Queued {
                    line: p.line,
                    offset: p.offset,
                    len: p.len,
                    text: None,
                })
                .collect(),
        }
    }

    fn cursor(&self, rel: &str) -> CursorState {
        CursorState {
            rel: rel.to_string(),
            offset: self.offset,
            line: self.line,
            count: self.count,
            after_remaining: self.after_remaining,
            last_printed: self.last_printed,
            separator_for: self.separator_for,
            before: self.before.iter().map(Queued::pos).collect(),
        }
    }
}

/// Lets the sequential walker's `'static` entry filter call the caller's hooks.
struct HookRef(&'static dyn StreamHooks);

// SAFETY: `ignore::Walk` (the sequential walker) calls its entry filter only on the calling
// thread, and the walker is dropped before `stream_page` returns, so the erased reference is
// never used from another thread or after the real borrow ends.
unsafe impl Send for HookRef {}
unsafe impl Sync for HookRef {}

impl HookRef {
    fn admit(&self, name: &str) -> bool {
        self.0.admit_entry(name)
    }
}

struct Run<'a> {
    hooks: &'a dyn StreamHooks,
    started: Instant,
    deadline: Duration,
    max_results: usize,
    max_page_bytes: u64,
    mode: StreamMode,
    multiline: bool,
    before: usize,
    after: usize,
    regex: Regex,
    resume: Option<CursorState>,
    items: Vec<StreamItem>,
    output_bytes: usize,
    status: StreamStatus,
    reason: Option<&'static str>,
    files_scanned: usize,
    bytes_scanned: u64,
}

impl Run<'_> {
    fn note_truncated(&mut self, reason: &'static str) {
        if self.status == StreamStatus::Complete {
            self.status = StreamStatus::Truncated;
            self.reason = Some(reason);
        }
    }

    fn interrupted(&self) -> Option<(StreamStatus, &'static str)> {
        if self.hooks.cancelled() {
            Some((StreamStatus::Cancelled, "cancelled"))
        } else if self.started.elapsed() >= self.deadline {
            Some((StreamStatus::Deadline, "deadline"))
        } else {
            None
        }
    }

    fn push(&mut self, item: StreamItem) -> Result<(), &'static str> {
        if self.items.len() >= self.max_results {
            return Err("result_ceiling");
        }
        let cost = self.hooks.row_bytes(&item);
        if self.output_bytes + cost + 1 > MAX_RESULT_BYTES {
            return Err("output_ceiling");
        }
        self.output_bytes += cost + 1;
        self.items.push(item);
        Ok(())
    }

    fn walk(
        &mut self,
        builder: WalkBuilder,
        walk_root: &Path,
        rel_root: &Path,
    ) -> Result<(), Stop> {
        for result in builder.build() {
            let entry = match result {
                Ok(e) => e,
                Err(_) => {
                    self.note_truncated("walk_error");
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let abs = entry.path();
            let suffix = match abs.strip_prefix(walk_root) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let rel_path = if suffix.as_os_str().is_empty() {
                rel_root.to_path_buf()
            } else {
                rel_root.join(suffix)
            };
            let rel = rel_string(&rel_path);
            self.visit_file(abs, &rel)?;
        }
        Ok(())
    }

    fn visit_file(&mut self, abs: &Path, rel: &str) -> Result<(), Stop> {
        // (1) Before anything else.
        if let Some((status, reason)) = self.interrupted() {
            return Err(if self.files_scanned == 0 {
                Stop::request(status, reason)
            } else {
                Stop::none(status, reason)
            });
        }
        // (2) The caller's filter.
        if !self.hooks.admit_file(rel) {
            return Ok(());
        }
        // (3) Authorization: never opened, counted or reported when denied.
        if !self.hooks.authorize_read(abs) {
            return Ok(());
        }
        // (4) Skip files before the cursor's path (walk order is by path components).
        if let Some(c) = &self.resume {
            if Path::new(rel).cmp(Path::new(&c.rel)) == Ordering::Less {
                return Ok(());
            }
        }
        let resumed = self.resume.clone().filter(|c| c.rel == rel);
        let start = match &resumed {
            Some(c) if !self.multiline => c.clone(),
            _ => CursorState::file_start(rel),
        };
        // (5)
        if let Some((status, reason)) = self.interrupted() {
            return Err(Stop::at(status, reason, start));
        }
        // (6)
        if self.items.len() >= self.max_results {
            return Err(Stop::at(StreamStatus::Truncated, "result_ceiling", start));
        }
        // (7)
        if self.files_scanned >= MAX_FILES_PER_PAGE {
            return Err(Stop::at(StreamStatus::Truncated, "file_ceiling", start));
        }
        // (8)
        self.files_scanned += 1;
        let mut file = match File::open(abs) {
            Ok(f) => f,
            Err(_) => {
                self.note_truncated("file_unavailable");
                return Ok(());
            }
        };
        let mut probe = Vec::with_capacity(BINARY_PROBE_BYTES);
        if file
            .by_ref()
            .take(BINARY_PROBE_BYTES as u64)
            .read_to_end(&mut probe)
            .is_err()
        {
            self.note_truncated("file_unavailable");
            return Ok(());
        }
        if probe.contains(&0) {
            return Ok(());
        }
        let start_offset = if self.multiline {
            0
        } else {
            resumed.as_ref().map_or(0, |c| c.offset)
        };
        if file.seek(SeekFrom::Start(start_offset)).is_err() {
            self.note_truncated("file_unavailable");
            return Ok(());
        }
        if self.multiline {
            self.scan_multiline(file, abs)
        } else {
            self.scan_stream(file, abs, rel, resumed.as_ref())
        }
    }

    fn scan_multiline(&mut self, mut file: File, abs: &Path) -> Result<(), Stop> {
        let mut text = Vec::new();
        if file
            .by_ref()
            .take(MAX_FILE_BYTES_PER_PAGE + 1)
            .read_to_end(&mut text)
            .is_err()
        {
            self.note_truncated("file_read_error");
            return Ok(());
        }
        if text.len() as u64 > MAX_FILE_BYTES_PER_PAGE {
            self.note_truncated("multiline_file_too_large");
            return Ok(());
        }
        self.bytes_scanned += text.len() as u64;
        let regex = self.regex.clone();
        let mut count: u64 = 0;
        // Line number at `scan_pos`, and the start of that line.
        let mut scan_line: u64 = 1;
        let mut scan_pos: usize = 0;
        let mut line_start: usize = 0;
        let mut emitted_through: u64 = 0;
        for m in regex.find_iter(&text) {
            if let Some((status, reason)) = self.interrupted() {
                return Err(Stop::none(status, reason));
            }
            match self.mode {
                StreamMode::Count => count += 1,
                StreamMode::FilesWithMatches => {
                    self.push(StreamItem::FileHit {
                        path: abs.to_path_buf(),
                    })
                    .map_err(|_| Stop::none(StreamStatus::Truncated, "result_ceiling"))?;
                    break;
                }
                StreamMode::Content => {
                    let start = m.start();
                    let last_byte = if m.end() > start { m.end() - 1 } else { start };
                    let segment = &text[scan_pos..start];
                    let breaks = segment.iter().filter(|b| **b == b'\n').count() as u64;
                    if breaks > 0 {
                        scan_line += breaks;
                        if let Some(i) = segment.iter().rposition(|b| *b == b'\n') {
                            line_start = scan_pos + i + 1;
                        }
                    }
                    scan_pos = start;
                    let mut line_no = scan_line;
                    let mut ls = line_start;
                    loop {
                        let le = text[ls..]
                            .iter()
                            .position(|b| *b == b'\n')
                            .map_or(text.len(), |i| ls + i);
                        if line_no > emitted_through {
                            self.push(StreamItem::Match {
                                path: abs.to_path_buf(),
                                line: line_no,
                                text: line_text(&text[ls..le]),
                            })
                            .map_err(|_| Stop::none(StreamStatus::Truncated, "result_ceiling"))?;
                            emitted_through = line_no;
                        }
                        if last_byte <= le || le >= text.len() {
                            break;
                        }
                        ls = le + 1;
                        line_no += 1;
                    }
                }
            }
        }
        if self.mode == StreamMode::Count && count > 0 {
            self.push(StreamItem::Count {
                path: abs.to_path_buf(),
                count,
            })
            .map_err(|_| Stop::none(StreamStatus::Truncated, "result_ceiling"))?;
        }
        Ok(())
    }

    fn replay(
        &mut self,
        reader: &mut BufReader<File>,
        pos: &Pos,
        file_bytes: &mut u64,
    ) -> Result<Vec<u8>, Stop> {
        let len = pos.len as u64;
        if *file_bytes + len > MAX_FILE_BYTES_PER_PAGE
            || self.bytes_scanned + len > self.max_page_bytes
        {
            return Err(Stop::none(StreamStatus::Truncated, "context_replay_budget"));
        }
        let unavailable = || Stop::none(StreamStatus::Truncated, "context_replay_unavailable");
        let file = reader.get_mut();
        let here = file.stream_position().map_err(|_| unavailable())?;
        file.seek(SeekFrom::Start(pos.offset))
            .map_err(|_| unavailable())?;
        let mut buf = vec![0u8; pos.len];
        file.read_exact(&mut buf).map_err(|_| unavailable())?;
        file.seek(SeekFrom::Start(here))
            .map_err(|_| unavailable())?;
        *file_bytes += len;
        self.bytes_scanned += len;
        Ok(buf)
    }

    fn scan_stream(
        &mut self,
        file: File,
        abs: &Path,
        rel: &str,
        resumed: Option<&CursorState>,
    ) -> Result<(), Stop> {
        let mut reader = BufReader::with_capacity(READER_BYTES, file);
        let mut st = match resumed {
            Some(c) => LineState::resume(c, self.before),
            None => LineState::new(),
        };
        let mut file_bytes: u64 = 0;
        let mut line_buf: Vec<u8> = Vec::new();
        loop {
            if let Some((status, reason)) = self.interrupted() {
                return Err(Stop::at(status, reason, st.cursor(rel)));
            }
            if file_bytes >= MAX_FILE_BYTES_PER_PAGE || self.bytes_scanned >= self.max_page_bytes {
                return Err(Stop::at(
                    StreamStatus::Truncated,
                    "byte_ceiling",
                    st.cursor(rel),
                ));
            }
            let read = match read_bounded_line(
                &mut reader,
                &mut line_buf,
                MAX_LINE_BYTES,
                MAX_LOGICAL_LINE_SCAN_BYTES,
            ) {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(_) => {
                    self.note_truncated("file_read_error");
                    return Ok(());
                }
            };
            file_bytes += read.consumed;
            self.bytes_scanned += read.consumed;
            let line_no = st.line + 1;
            let line_start = st.offset;
            if read.oversized {
                if !read.terminated {
                    return Err(Stop::none(
                        StreamStatus::Truncated,
                        "oversized_line_scan_ceiling",
                    ));
                }
                self.note_truncated("oversized_line_skipped");
                st.offset += read.consumed;
                st.line = line_no;
                st.before.clear();
                st.after_remaining = 0;
                continue;
            }
            let content_len = line_buf
                .strip_suffix(b"\r")
                .map_or(line_buf.len(), <[u8]>::len);
            let is_match = self.regex.is_match(&line_buf[..content_len]);
            match self.mode {
                StreamMode::FilesWithMatches => {
                    if is_match {
                        return match self.push(StreamItem::FileHit {
                            path: abs.to_path_buf(),
                        }) {
                            Ok(()) => Ok(()),
                            Err(_) => Err(Stop::at(
                                StreamStatus::Truncated,
                                "output_ceiling",
                                st.cursor(rel),
                            )),
                        };
                    }
                }
                StreamMode::Count => {
                    if is_match {
                        st.count += 1;
                    }
                }
                StreamMode::Content => {
                    let text_bytes = line_buf[..content_len].to_vec();
                    self.content_line(
                        &mut reader,
                        &mut st,
                        rel,
                        abs,
                        line_no,
                        &text_bytes,
                        is_match,
                        &mut file_bytes,
                    )?;
                    if self.before > 0 {
                        st.before.push_back(Queued {
                            line: line_no,
                            offset: line_start,
                            len: read.consumed.min(MAX_LINE_BYTES as u64) as usize,
                            text: Some(text_bytes),
                        });
                        while st.before.len() > self.before {
                            st.before.pop_front();
                        }
                    }
                }
            }
            st.offset += read.consumed;
            st.line = line_no;
            if self.items.len() >= self.max_results {
                return Err(Stop::at(
                    StreamStatus::Truncated,
                    "result_ceiling",
                    st.cursor(rel),
                ));
            }
        }
        if self.mode == StreamMode::Count && st.count > 0 {
            self.push(StreamItem::Count {
                path: abs.to_path_buf(),
                count: st.count,
            })
            .map_err(|_| {
                Stop::at(
                    StreamStatus::Truncated,
                    "result_ceiling",
                    CursorState::file_start(rel),
                )
            })?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn content_line(
        &mut self,
        reader: &mut BufReader<File>,
        st: &mut LineState,
        rel: &str,
        abs: &Path,
        line_no: u64,
        text_bytes: &[u8],
        is_match: bool,
        file_bytes: &mut u64,
    ) -> Result<(), Stop> {
        if is_match {
            let first_idx = st
                .before
                .iter()
                .position(|q| st.last_printed.is_none_or(|lp| q.line > lp));
            let first_line = first_idx.map_or(line_no, |i| st.before[i].line);
            if let Some(lp) = st.last_printed {
                if first_line > lp + 1
                    && (self.before > 0 || self.after > 0)
                    && st.separator_for != Some(line_no)
                {
                    self.push(StreamItem::Separator)
                        .map_err(|r| Stop::at(StreamStatus::Truncated, r, st.cursor(rel)))?;
                    st.separator_for = Some(line_no);
                }
            }
            if let Some(start) = first_idx {
                for i in start..st.before.len() {
                    let pos = st.before[i].pos();
                    let bytes = match st.before[i].text.take() {
                        Some(t) => t,
                        None => self.replay(reader, &pos, file_bytes)?,
                    };
                    self.push(StreamItem::Context {
                        path: abs.to_path_buf(),
                        line: pos.line,
                        text: line_text(&bytes),
                    })
                    .map_err(|r| Stop::at(StreamStatus::Truncated, r, st.cursor(rel)))?;
                    st.last_printed = Some(pos.line);
                }
            }
            if st.last_printed.is_none_or(|lp| lp < line_no) {
                self.push(StreamItem::Match {
                    path: abs.to_path_buf(),
                    line: line_no,
                    text: line_text(text_bytes),
                })
                .map_err(|r| Stop::at(StreamStatus::Truncated, r, st.cursor(rel)))?;
            }
            st.last_printed = Some(line_no);
            st.after_remaining = self.after;
        } else if st.after_remaining > 0 && st.last_printed.is_none_or(|lp| lp < line_no) {
            self.push(StreamItem::Context {
                path: abs.to_path_buf(),
                line: line_no,
                text: line_text(text_bytes),
            })
            .map_err(|r| Stop::at(StreamStatus::Truncated, r, st.cursor(rel)))?;
            st.last_printed = Some(line_no);
            st.after_remaining -= 1;
        }
        Ok(())
    }
}

/// Runs one page of the search.
///
/// The walk is ordered by full path, so a cursor resumes at the next unscanned position; the
/// workspace stays live between pages, so resume is best-effort. `hooks.admit_file` and
/// `hooks.authorize_read` are consulted before a file is opened; a hook that returns `true` for
/// a path the caller must not read leaks that file. No thread is spawned and only the open file
/// and a bounded buffer are held.
pub fn stream_page(
    req: &StreamRequest,
    hooks: &dyn StreamHooks,
) -> Result<StreamPage, StreamError> {
    let started = Instant::now();
    let walk_root = if req.root.is_absolute() {
        req.root.clone()
    } else {
        req.workspace_root.join(&req.root)
    };
    if !walk_root.exists() {
        return Err(StreamError::NotFound(req.root.clone()));
    }
    let workspace = fs::canonicalize(&req.workspace_root)
        .map_err(|_| StreamError::NotFound(req.workspace_root.clone()))?;
    let root = fs::canonicalize(&walk_root).map_err(|_| StreamError::NotFound(req.root.clone()))?;
    if !root.starts_with(&workspace) {
        return Err(StreamError::RootOutsideWorkspace(req.root.clone()));
    }
    let rel_root = root
        .strip_prefix(&workspace)
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let max_results = req.max_results.max(1);
    let deadline = req.deadline.min(MAX_DEADLINE);
    let max_page_bytes = req.max_page_bytes.min(MAX_TOTAL_BYTES_PER_PAGE);
    let before = req.before.min(MAX_CONTEXT_LINES);
    let after = req.after.min(MAX_CONTEXT_LINES);
    let clamped = req.before > MAX_CONTEXT_LINES || req.after > MAX_CONTEXT_LINES;

    let regex = RegexBuilder::new(&req.pattern)
        .case_insensitive(!req.case_sensitive)
        .multi_line(req.multiline)
        .build()
        .map_err(|e| StreamError::BadPattern(e.to_string()))?;

    let binding = binding_digest(req, &workspace, &root, before, after);
    let resume = match &req.cursor {
        Some(text) => Some(decode_cursor(text, &binding)?),
        None => None,
    };

    let mut run = Run {
        hooks,
        started,
        deadline,
        max_results,
        max_page_bytes,
        mode: req.mode,
        multiline: req.multiline,
        before,
        after,
        regex,
        resume,
        items: Vec::new(),
        output_bytes: 0,
        status: StreamStatus::Complete,
        reason: None,
        files_scanned: 0,
        bytes_scanned: 0,
    };
    if clamped {
        run.note_truncated("context_ceiling");
    }

    let mut builder = WalkBuilder::new(&walk_root);
    builder
        .hidden(!req.include_hidden)
        .git_ignore(req.git_ignore)
        .git_global(req.git_global)
        .require_git(req.require_git)
        .sort_by_file_path(|a, b| a.cmp(b));
    // SAFETY: see `HookRef`; the builder (and the walker it builds) is consumed inside
    // `run.walk` below, within the lifetime of `hooks`.
    let guard = HookRef(unsafe {
        std::mem::transmute::<&dyn StreamHooks, &'static dyn StreamHooks>(hooks)
    });
    builder.filter_entry(move |entry| {
        entry.depth() == 0 || guard.admit(&entry.file_name().to_string_lossy())
    });

    let outcome = run.walk(builder, &walk_root, &rel_root);
    let (status, reason, next) = match outcome {
        Ok(()) => (run.status, run.reason, None),
        Err(stop) => {
            let next = match stop.next {
                Next::Nothing => None,
                Next::Request => req.cursor.clone(),
                Next::At(c) => encode_cursor(&binding, &c),
            };
            (stop.status, Some(stop.reason), next)
        }
    };
    Ok(StreamPage {
        items: run.items,
        status,
        reason,
        next: if status == StreamStatus::Complete {
            None
        } else {
            next
        },
        files_scanned: run.files_scanned,
        bytes_scanned: run.bytes_scanned,
    })
}
