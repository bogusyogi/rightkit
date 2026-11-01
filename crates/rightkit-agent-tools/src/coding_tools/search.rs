// RIGHTKIT-SERIOUS: workspace regex content search; rightkit-search 0.4.0 only compiles SQL/FTS queries, no file-tree grep exists (report T-1)
//! `search` — bounded streaming content search respecting .gitignore.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use ignore::WalkBuilder;
use regex::bytes::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

pub struct SearchTool;

const SEARCH_DESCRIPTION: &str = concat!(
    "Search files for regex pattern with bounded streaming byte I/O; respects .gitignore. ",
    "Modes: 'content' (lines, default), 'files_with_matches' (paths), 'count' (per-file). ",
    "Returns execution/evidence output only: Agent performs no semantic ranking or context admission. ",
    "Returns explicit Complete/Truncated/Cancelled/Deadline status and a continuation cursor when resumable."
);

const MAX_RESULTS: usize = 200;
const MAX_CONTEXT_LINES: usize = 100;
const MAX_LINE_BYTES: usize = 256 * 1024;
const MAX_LOGICAL_LINE_SCAN_BYTES: u64 = 1024 * 1024;
const MAX_FILE_BYTES_PER_PAGE: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES_PER_PAGE: u64 = 64 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_CURSOR_BYTES: usize = 64 * 1024;
const MAX_RESULT_BYTES: usize = MAX_OUTPUT_BYTES - MAX_CURSOR_BYTES - 512;
const MAX_FILES_PER_PAGE: usize = 20_000;
const BINARY_PROBE_BYTES: usize = 8 * 1024;
const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);
const MAX_DEADLINE: Duration = Duration::from_secs(60);
const CURSOR_VERSION: u32 = 3;
const LIVE_CONSISTENCY: &str = "best_effort_live_workspace";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchStatus {
    Complete,
    Truncated,
    Cancelled,
    Deadline,
}

impl SearchStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Complete => "Complete",
            Self::Truncated => "Truncated",
            Self::Cancelled => "Cancelled",
            Self::Deadline => "Deadline",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SearchCursor {
    version: u32,
    binding: String,
    consistency: String,
    path: String,
    offset: u64,
    line: usize,
    /// Count mode must carry partial work across a mid-file page boundary.
    count: usize,
    /// Content mode may stop immediately after a matching line while trailing
    /// context is still owed. Carry that finite state instead of silently
    /// dropping the requested after-context on resume.
    after_remaining: usize,
    before_lines: Vec<ContextPosition>,
    last_printed_line: Option<usize>,
    separator_for_line: Option<usize>,
}

/// Resume metadata contains only bounded positions, never unmasked file text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ContextPosition {
    line: usize,
    offset: u64,
    len: u64,
}

struct ContextLine {
    position: ContextPosition,
    text: Option<String>,
}

impl SearchCursor {
    fn at(path: String, offset: u64, line: usize, count: usize, after_remaining: usize) -> Self {
        Self {
            version: CURSOR_VERSION,
            binding: String::new(),
            consistency: LIVE_CONSISTENCY.to_string(),
            path,
            offset,
            line,
            count,
            after_remaining,
            before_lines: Vec::new(),
            last_printed_line: None,
            separator_for_line: None,
        }
    }

    fn file_start(path: String) -> Self {
        Self::at(path, 0, 0, 0, 0)
    }

    fn with_context(
        mut self,
        before: &VecDeque<ContextLine>,
        last_printed_line: Option<usize>,
        separator_for_line: Option<usize>,
    ) -> Self {
        self.before_lines = before.iter().map(|line| line.position.clone()).collect();
        self.last_printed_line = last_printed_line;
        self.separator_for_line = separator_for_line;
        self
    }
}

#[derive(Debug)]
struct SearchState {
    binding: String,
    status: SearchStatus,
    reason: Option<&'static str>,
    cursor: Option<SearchCursor>,
    files_scanned: usize,
    bytes_scanned: u64,
    output_bytes: usize,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            binding: String::new(),
            status: SearchStatus::Complete,
            reason: None,
            cursor: None,
            files_scanned: 0,
            bytes_scanned: 0,
            output_bytes: 0,
        }
    }
}

impl SearchState {
    fn stop(&mut self, status: SearchStatus, reason: &'static str, cursor: Option<SearchCursor>) {
        self.status = status;
        self.reason = Some(reason);
        self.cursor = cursor.map(|mut cursor| {
            cursor.binding.clone_from(&self.binding);
            cursor
        });
        if self.cursor.as_ref().is_some_and(|cursor| {
            serde_json::to_string(cursor).map_or(true, |encoded| encoded.len() > MAX_CURSOR_BYTES)
        }) {
            self.cursor = None;
        }
    }

    fn note_truncated(&mut self, reason: &'static str) {
        if self.status == SearchStatus::Complete {
            self.status = SearchStatus::Truncated;
            self.reason = Some(reason);
        }
    }
}

struct SearchBudget {
    deadline: Instant,
    total_bytes: u64,
}

struct LineRead {
    bytes: Vec<u8>,
    consumed: u64,
    oversized: bool,
    terminated: bool,
}

#[async_trait::async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        SEARCH_DESCRIPTION
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "Directory or file to search (relative to workspace root)" },
                "file_glob": { "type": "string", "description": "Glob pattern to filter files, e.g. '*.rs'" },
                "case_sensitive": { "type": "boolean", "description": "Whether to match case-sensitively (default: false)" },
                "output_mode": { "type": "string", "enum": ["content", "files_with_matches", "count"], "description": "Output shape (default: content)" },
                "context": { "type": "integer", "description": "Lines of context before AND after each match (bounded to 100)" },
                "before_context": { "type": "integer", "description": "Lines of context before each match (bounded to 100)" },
                "after_context": { "type": "integer", "description": "Lines of context after each match (bounded to 100)" },
                "max_results": { "type": "integer", "description": "Max result rows (default 200; output also has a 1MiB hard ceiling)" },
                "cursor": { "type": "string", "description": "Continuation from the same query/options and roots; workspace and ignore files remain live, so continuation is explicitly best-effort" },
                "deadline_ms": { "type": "integer", "description": "Optional page deadline in milliseconds, capped at 60s; 0 yields Deadline immediately" },
                "max_bytes": { "type": "integer", "description": "Optional page byte target, capped at 64MiB; a currently scanned logical line remains bounded independently" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let pattern_str = match args["pattern"].as_str() {
            Some(pattern) => pattern,
            None => return ToolResult::error("Missing required argument: pattern"),
        };
        let regex = match build_regex(
            pattern_str,
            args["case_sensitive"].as_bool().unwrap_or(false),
        ) {
            Ok(regex) => regex,
            Err(error) => return ToolResult::error(format!("Invalid regex: {error}")),
        };
        let search_root = if let Some(path) = args["path"].as_str() {
            match crate::coding_tools::read_file::resolve_in_workspace(path, &ctx.workspace_root) {
                Ok(path) => path,
                Err(error) => return ToolResult::error(error),
            }
        } else {
            ctx.workspace_root.clone()
        };
        if let Err(error) = crate::coding_tools::read_file::enforce_path_deny(
            ctx,
            &search_root,
            crate::coding_tools::read_file::PathAccess::Read,
        ) {
            return ToolResult::error(error);
        }
        let file_matcher = match args["file_glob"].as_str() {
            Some(glob) => match crate::coding_tools::list_files::build_matcher(glob) {
                Ok(matcher) => Some(matcher),
                Err(error) => return ToolResult::error(error),
            },
            None => None,
        };
        let mode = match args.get("output_mode") {
            None | Some(Value::Null) => "content",
            Some(Value::String(mode))
                if matches!(mode.as_str(), "content" | "files_with_matches" | "count") =>
            {
                mode.as_str()
            }
            Some(other) => {
                return ToolResult::error(format!(
                "Invalid output_mode {other}: expected one of content, files_with_matches, count"
            ))
            }
        };
        let max_results = args["max_results"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(MAX_RESULTS)
            .max(1);
        let requested_context = args["context"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok());
        let requested_before = requested_context.or_else(|| {
            args["before_context"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
        });
        let requested_after = requested_context.or_else(|| {
            args["after_context"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
        });
        let before = requested_before.unwrap_or(0).min(MAX_CONTEXT_LINES);
        let after = requested_after.unwrap_or(0).min(MAX_CONTEXT_LINES);
        let binding = match cursor_binding(&args, ctx, &search_root, mode, before, after) {
            Ok(binding) => binding,
            Err(error) => return ToolResult::error(error),
        };
        let cursor = match parse_cursor(args["cursor"].as_str(), &binding) {
            Ok(cursor) => cursor,
            Err(error) => return ToolResult::error(error),
        };
        let requested_deadline = args["deadline_ms"]
            .as_u64()
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_DEADLINE)
            .min(MAX_DEADLINE);
        let budget = SearchBudget {
            deadline: Instant::now() + requested_deadline,
            total_bytes: args["max_bytes"]
                .as_u64()
                .unwrap_or(MAX_TOTAL_BYTES_PER_PAGE)
                .min(MAX_TOTAL_BYTES_PER_PAGE),
        };

        let mut state = SearchState {
            binding,
            ..SearchState::default()
        };
        if requested_before.is_some_and(|n| n > MAX_CONTEXT_LINES)
            || requested_after.is_some_and(|n| n > MAX_CONTEXT_LINES)
        {
            state.note_truncated("context_ceiling");
        }
        let mut results = Vec::new();

        // Sort inside the ignore walker rather than collecting every candidate
        // path into a separate Vec. This gives deterministic cursor order while
        // keeping discovery memory independent of repository file count.
        let mut walk_builder = WalkBuilder::new(&search_root);
        walk_builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .require_git(false)
            .filter_entry(not_noise_dir)
            .sort_by_file_path(|a, b| a.cmp(b));
        let walker = walk_builder.build();

        'files: for entry in walker {
            // Directory traversal and rejected candidates consume time too.
            // A filter with no matching files must still honor cancellation.
            if let Some(status) = control_status(ctx, &budget) {
                let resume = if state.files_scanned == 0 {
                    cursor.clone()
                } else {
                    None
                };
                state.stop(status, control_reason(status), resume);
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    state.note_truncated("walk_error");
                    continue;
                }
            };
            let path = entry.path();
            if !path.is_file()
                || !file_glob_matches(path, &ctx.workspace_root, file_matcher.as_ref())
            {
                continue;
            }
            let rel = match path.strip_prefix(&ctx.workspace_root) {
                Ok(rel) => rel,
                Err(_) => continue,
            };
            if crate::coding_tools::read_file::resolve_in_workspace(
                &rel.to_string_lossy(),
                &ctx.workspace_root,
            )
            .is_err()
            {
                continue;
            }
            // deny-read-globs: a denied file is never opened or reported.
            if crate::coding_tools::read_file::enforce_path_deny(
                ctx,
                path,
                crate::coding_tools::read_file::PathAccess::Read,
            )
            .is_err()
            {
                continue;
            }
            let rel_path = cursor_path(path, &ctx.workspace_root);
            if cursor
                .as_ref()
                .is_some_and(|cursor| rel_path.as_str() < cursor.path.as_str())
            {
                continue;
            }
            if let Some(status) = control_status(ctx, &budget) {
                state.stop(
                    status,
                    control_reason(status),
                    Some(
                        cursor
                            .as_ref()
                            .filter(|cursor| cursor.path == rel_path)
                            .cloned()
                            .unwrap_or_else(|| SearchCursor::file_start(rel_path)),
                    ),
                );
                break;
            }
            if results.len() >= max_results {
                state.stop(
                    SearchStatus::Truncated,
                    "result_ceiling",
                    Some(SearchCursor::file_start(rel_path)),
                );
                break;
            }
            if state.files_scanned >= MAX_FILES_PER_PAGE {
                state.stop(
                    SearchStatus::Truncated,
                    "file_ceiling",
                    Some(SearchCursor::file_start(rel_path)),
                );
                break;
            }
            state.files_scanned = state.files_scanned.saturating_add(1);

            let mut file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(_) => {
                    state.note_truncated("file_unavailable");
                    continue;
                }
            };
            match is_binary(&mut file) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(_) => {
                    state.note_truncated("file_unavailable");
                    continue;
                }
            }
            let resumed = cursor.as_ref().filter(|cursor| cursor.path == rel_path);
            let (start_offset, start_line) = resumed
                .map(|cursor| (cursor.offset, cursor.line))
                .unwrap_or((0, 0));
            if file.seek(SeekFrom::Start(start_offset)).is_err() {
                state.note_truncated("file_unavailable");
                continue;
            }
            let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
            let mut line_number = start_line;
            let mut file_bytes = 0_u64;
            let mut absolute_offset = start_offset;
            let mut count = resumed.map(|cursor| cursor.count).unwrap_or(0);
            let mut before_ring: VecDeque<ContextLine> = resumed
                .map(|cursor| {
                    cursor
                        .before_lines
                        .iter()
                        .cloned()
                        .map(|position| ContextLine {
                            position,
                            text: None,
                        })
                        .collect()
                })
                .unwrap_or_else(|| VecDeque::with_capacity(before));
            let mut after_remaining = resumed.map(|cursor| cursor.after_remaining).unwrap_or(0);
            let mut last_printed_line = resumed.and_then(|cursor| cursor.last_printed_line);
            let mut separator_for_line = resumed.and_then(|cursor| cursor.separator_for_line);

            loop {
                if let Some(status) = control_status(ctx, &budget) {
                    state.stop(
                        status,
                        control_reason(status),
                        Some(
                            SearchCursor::at(
                                rel_path.clone(),
                                absolute_offset,
                                line_number,
                                count,
                                after_remaining,
                            )
                            .with_context(
                                &before_ring,
                                last_printed_line,
                                separator_for_line,
                            ),
                        ),
                    );
                    break 'files;
                }
                if file_bytes >= MAX_FILE_BYTES_PER_PAGE
                    || state.bytes_scanned >= budget.total_bytes
                {
                    state.stop(
                        SearchStatus::Truncated,
                        "byte_ceiling",
                        Some(
                            SearchCursor::at(
                                rel_path.clone(),
                                absolute_offset,
                                line_number,
                                count,
                                after_remaining,
                            )
                            .with_context(
                                &before_ring,
                                last_printed_line,
                                separator_for_line,
                            ),
                        ),
                    );
                    break 'files;
                }
                let line_start = absolute_offset;
                let read = match read_bounded_line(
                    &mut reader,
                    MAX_LINE_BYTES,
                    MAX_LOGICAL_LINE_SCAN_BYTES,
                ) {
                    Ok(Some(read)) => read,
                    Ok(None) => break,
                    Err(_) => {
                        state.note_truncated("file_read_error");
                        break;
                    }
                };
                absolute_offset = absolute_offset.saturating_add(read.consumed);
                file_bytes = file_bytes.saturating_add(read.consumed);
                state.bytes_scanned = state.bytes_scanned.saturating_add(read.consumed);
                if read.oversized && !read.terminated {
                    state.stop(SearchStatus::Truncated, "oversized_line_scan_ceiling", None);
                    break 'files;
                }
                line_number = line_number.saturating_add(1);
                if read.oversized {
                    state.note_truncated("oversized_line_skipped");
                    before_ring.clear();
                    after_remaining = 0;
                    continue;
                }

                let line_bytes = trim_line_ending(&read.bytes);
                let matched = regex.is_match(line_bytes);
                // Decode only for human-facing rendering. Matching itself is on
                // raw bytes so malformed UTF-8 cannot be rewritten before regex.
                let line = String::from_utf8_lossy(line_bytes).into_owned();
                match mode {
                    "files_with_matches" => {
                        if matched {
                            if !push_result(
                                &mut results,
                                &path.display().to_string(),
                                max_results,
                                &mut state,
                            ) {
                                state.stop(
                                    SearchStatus::Truncated,
                                    "output_ceiling",
                                    Some(SearchCursor::at(
                                        rel_path.clone(),
                                        line_start,
                                        line_number.saturating_sub(1),
                                        0,
                                        0,
                                    )),
                                );
                                break 'files;
                            }
                            break;
                        }
                    }
                    "count" => {
                        if matched {
                            count = count.saturating_add(1);
                        }
                    }
                    _ => {
                        if matched {
                            let first_context_line = before_ring
                                .front()
                                .map(|line| line.position.line)
                                .unwrap_or(line_number);
                            if last_printed_line
                                .is_some_and(|last| first_context_line > last.saturating_add(1))
                                && (before > 0 || after > 0)
                                && separator_for_line != Some(line_number)
                            {
                                if !push_result(&mut results, "--", max_results, &mut state) {
                                    state.stop(
                                        SearchStatus::Truncated,
                                        "output_ceiling",
                                        Some(
                                            SearchCursor::at(
                                                rel_path.clone(),
                                                line_start,
                                                line_number.saturating_sub(1),
                                                0,
                                                after_remaining,
                                            )
                                            .with_context(
                                                &before_ring,
                                                last_printed_line,
                                                separator_for_line,
                                            ),
                                        ),
                                    );
                                    break 'files;
                                }
                                separator_for_line = Some(line_number);
                            }
                            for index in 0..before_ring.len() {
                                let context_line = before_ring[index].position.line;
                                if last_printed_line.is_some_and(|last| context_line <= last) {
                                    continue;
                                }
                                // Do not materialize a row that cannot fit this page.
                                if results.len() >= max_results {
                                    state.stop(
                                        SearchStatus::Truncated,
                                        "result_ceiling",
                                        Some(
                                            SearchCursor::at(
                                                rel_path.clone(),
                                                line_start,
                                                line_number.saturating_sub(1),
                                                0,
                                                after_remaining,
                                            )
                                            .with_context(
                                                &before_ring,
                                                last_printed_line,
                                                separator_for_line,
                                            ),
                                        ),
                                    );
                                    break 'files;
                                }
                                if before_ring[index].text.is_none() {
                                    match read_context_line(
                                        &mut reader,
                                        &before_ring[index].position,
                                        absolute_offset,
                                        ctx,
                                        &budget,
                                        &mut state,
                                        &mut file_bytes,
                                    ) {
                                        Ok(text) => before_ring[index].text = Some(text),
                                        Err((status, reason)) => {
                                            // Tiny replay budgets must not yield an endlessly
                                            // repeating cursor with no possible progress.
                                            state.stop(status, reason, None);
                                            break 'files;
                                        }
                                    }
                                }
                                let rendered = format!(
                                    "{}:{}- {}",
                                    path.display(),
                                    context_line,
                                    before_ring[index].text.as_deref().unwrap_or_default()
                                );
                                if !push_result(&mut results, &rendered, max_results, &mut state) {
                                    state.stop(
                                        SearchStatus::Truncated,
                                        "result_ceiling",
                                        Some(
                                            SearchCursor::at(
                                                rel_path.clone(),
                                                line_start,
                                                line_number.saturating_sub(1),
                                                0,
                                                after_remaining,
                                            )
                                            .with_context(
                                                &before_ring,
                                                last_printed_line,
                                                separator_for_line,
                                            ),
                                        ),
                                    );
                                    break 'files;
                                }
                                last_printed_line = Some(context_line);
                            }
                            if !last_printed_line.is_some_and(|last| line_number <= last) {
                                let rendered =
                                    format!("{}:{}: {}", path.display(), line_number, line);
                                if !push_result(&mut results, &rendered, max_results, &mut state) {
                                    state.stop(
                                        SearchStatus::Truncated,
                                        "result_ceiling",
                                        Some(
                                            SearchCursor::at(
                                                rel_path.clone(),
                                                line_start,
                                                line_number.saturating_sub(1),
                                                0,
                                                after_remaining,
                                            )
                                            .with_context(
                                                &before_ring,
                                                last_printed_line,
                                                separator_for_line,
                                            ),
                                        ),
                                    );
                                    break 'files;
                                }
                                last_printed_line = Some(line_number);
                            }
                            after_remaining = after;
                        } else if after_remaining > 0 {
                            if !last_printed_line.is_some_and(|last| line_number <= last) {
                                let rendered =
                                    format!("{}:{}- {}", path.display(), line_number, line);
                                if !push_result(&mut results, &rendered, max_results, &mut state) {
                                    state.stop(
                                        SearchStatus::Truncated,
                                        "result_ceiling",
                                        Some(
                                            SearchCursor::at(
                                                rel_path.clone(),
                                                line_start,
                                                line_number.saturating_sub(1),
                                                0,
                                                after_remaining,
                                            )
                                            .with_context(
                                                &before_ring,
                                                last_printed_line,
                                                separator_for_line,
                                            ),
                                        ),
                                    );
                                    break 'files;
                                }
                                last_printed_line = Some(line_number);
                            }
                            after_remaining -= 1;
                        }
                        separator_for_line = None;
                        if before > 0 {
                            before_ring.push_back(ContextLine {
                                position: ContextPosition {
                                    line: line_number,
                                    offset: line_start,
                                    len: read.consumed,
                                },
                                text: Some(line),
                            });
                            while before_ring.len() > before {
                                before_ring.pop_front();
                            }
                        }
                    }
                }
                if results.len() >= max_results {
                    state.stop(
                        SearchStatus::Truncated,
                        "result_ceiling",
                        Some(
                            SearchCursor::at(
                                rel_path.clone(),
                                absolute_offset,
                                line_number,
                                count,
                                after_remaining,
                            )
                            .with_context(
                                &before_ring,
                                last_printed_line,
                                separator_for_line,
                            ),
                        ),
                    );
                    break 'files;
                }
            }

            if mode == "count" && count > 0 {
                let rendered = format!("{}:{}", path.display(), count);
                if !push_result(&mut results, &rendered, max_results, &mut state) {
                    state.stop(
                        SearchStatus::Truncated,
                        "result_ceiling",
                        Some(SearchCursor::at(
                            rel_path,
                            absolute_offset,
                            line_number,
                            count,
                            0,
                        )),
                    );
                    break 'files;
                }
            }
        }

        let mut body = if results.is_empty() {
            "No matches found.".to_string()
        } else {
            results.join("\n")
        };
        body.push('\n');
        body.push_str(&status_line(&state));
        ToolResult::ok(body)
    }
}

fn read_context_line<R: Read + Seek>(
    reader: &mut R,
    position: &ContextPosition,
    resume_offset: u64,
    ctx: &ToolContext,
    budget: &SearchBudget,
    state: &mut SearchState,
    file_bytes: &mut u64,
) -> Result<String, (SearchStatus, &'static str)> {
    if let Some(status) = control_status(ctx, budget) {
        return Err((status, control_reason(status)));
    }
    if position.len > budget.total_bytes.saturating_sub(state.bytes_scanned)
        || position.len > MAX_FILE_BYTES_PER_PAGE.saturating_sub(*file_bytes)
    {
        return Err((SearchStatus::Truncated, "context_replay_budget"));
    }
    let mut bytes = vec![0; position.len as usize];
    state.bytes_scanned = state.bytes_scanned.saturating_add(position.len);
    *file_bytes = file_bytes.saturating_add(position.len);
    let read = reader
        .seek(SeekFrom::Start(position.offset))
        .and_then(|_| reader.read_exact(&mut bytes));
    // Seeking through BufReader also discards any obsolete read-ahead buffer.
    reader
        .seek(SeekFrom::Start(resume_offset))
        .map_err(|_| (SearchStatus::Truncated, "context_replay_unavailable"))?;
    read.map_err(|_| (SearchStatus::Truncated, "context_replay_unavailable"))?;
    if let Some(status) = control_status(ctx, budget) {
        return Err((status, control_reason(status)));
    }
    Ok(String::from_utf8_lossy(trim_line_ending(&bytes)).into_owned())
}

fn build_regex(pattern: &str, case_sensitive: bool) -> Result<Regex, regex::Error> {
    if case_sensitive {
        Regex::new(pattern)
    } else {
        Regex::new(&format!("(?i){pattern}"))
    }
}

fn cursor_binding(
    args: &Value,
    ctx: &ToolContext,
    search_root: &std::path::Path,
    mode: &str,
    before: usize,
    after: usize,
) -> Result<String, String> {
    let root_identity = |path: &std::path::Path| {
        std::fs::canonicalize(path)
            .map(|path| rightkit_crypto::hex_encode(path.as_os_str().as_encoded_bytes()))
            .map_err(|error| format!("Cannot bind search root: {error}"))
    };
    // Bind effective query semantics, not per-page budgets. Ignore contents
    // and workspace files are live: no unbounded pre-scan or snapshot claim.
    let identity = json!({
        "version": CURSOR_VERSION,
        "workspaceRoot": root_identity(&ctx.workspace_root)?,
        "searchRoot": root_identity(search_root)?,
        "pattern": args["pattern"].as_str(),
        "caseSensitive": args["case_sensitive"].as_bool().unwrap_or(false),
        "mode": mode,
        "fileGlob": args["file_glob"].as_str(),
        "before": before,
        "after": after,
        "ignorePolicy": {
            "hidden": false, "gitIgnore": true, "gitGlobal": true,
            "requireGit": false, "noiseDirectories": [".git", ".rightkit", ".hg", ".svn"],
            "ignoreContents": "live", "workspaceGeneration": "live"
        },
        "consistency": LIVE_CONSISTENCY
    });
    Ok(rightkit_fs::digest::sha256_bytes(
        identity.to_string().as_bytes(),
    ))
}

fn parse_cursor(raw: Option<&str>, binding: &str) -> Result<Option<SearchCursor>, String> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    if raw.len() > MAX_CURSOR_BYTES {
        return Err("Search cursor exceeds metadata limit; restart search".into());
    }
    let cursor: SearchCursor = serde_json::from_str(raw)
        .map_err(|error| format!("Invalid search cursor: {error}; restart search"))?;
    if cursor.version != CURSOR_VERSION
        || cursor.binding != binding
        || cursor.consistency != LIVE_CONSISTENCY
    {
        return Err(
            "Search cursor does not match query/options, roots or schema; restart search".into(),
        );
    }
    if cursor.before_lines.len() > MAX_CONTEXT_LINES
        || cursor.after_remaining > MAX_CONTEXT_LINES
        || cursor
            .last_printed_line
            .is_some_and(|line| line == 0 || line > cursor.line)
        || cursor
            .separator_for_line
            .is_some_and(|line| cursor.line.checked_add(1) != Some(line))
        || cursor.before_lines.iter().any(|position| {
            position.line == 0
                || position.line > cursor.line
                || position.len == 0
                || position.len > MAX_LINE_BYTES as u64
                || position
                    .offset
                    .checked_add(position.len)
                    .is_none_or(|end| end > cursor.offset)
        })
        || cursor.before_lines.windows(2).any(|pair| {
            pair[0].line >= pair[1].line
                || pair[0]
                    .offset
                    .checked_add(pair[0].len)
                    .is_none_or(|end| end > pair[1].offset)
        })
    {
        return Err("Invalid search cursor context positions; restart search".into());
    }
    Ok(Some(cursor))
}

fn status_line(state: &SearchState) -> String {
    let cursor = state
        .cursor
        .as_ref()
        .and_then(|cursor| serde_json::to_string(cursor).ok())
        .unwrap_or_else(|| "null".to_string());
    format!(
        "[search status={} reason={} files={} bytes={} consistency=BestEffort workspace=live ignore=live snapshot=none cursor={}]",
        state.status.label(),
        state.reason.unwrap_or("none"),
        state.files_scanned,
        state.bytes_scanned,
        cursor
    )
}

fn push_result(
    results: &mut Vec<String>,
    line: &str,
    max_results: usize,
    state: &mut SearchState,
) -> bool {
    if results.len() >= max_results {
        return false;
    }
    // Account for rendered/masked bytes before advancing the continuation.
    // Reserve the complete status/cursor envelope inside the response ceiling.
    let masked = crate::coding_tools::output_compaction::mask_secrets(line);
    let additional = masked.len().saturating_add(1);
    if state.output_bytes.saturating_add(additional) > MAX_RESULT_BYTES {
        return false;
    }
    state.output_bytes = state.output_bytes.saturating_add(additional);
    results.push(masked);
    true
}

fn control_status(ctx: &ToolContext, budget: &SearchBudget) -> Option<SearchStatus> {
    if ctx
        .cancel_rx
        .as_ref()
        .is_some_and(|receiver| *receiver.borrow())
    {
        Some(SearchStatus::Cancelled)
    } else if Instant::now() >= budget.deadline {
        Some(SearchStatus::Deadline)
    } else {
        None
    }
}

fn control_reason(status: SearchStatus) -> &'static str {
    match status {
        SearchStatus::Cancelled => "cancelled",
        SearchStatus::Deadline => "deadline",
        SearchStatus::Truncated => "truncated",
        SearchStatus::Complete => "complete",
    }
}

fn cursor_path(path: &std::path::Path, workspace_root: &std::path::Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn file_glob_matches(
    path: &std::path::Path,
    workspace_root: &std::path::Path,
    file_matcher: Option<&(globset::GlobMatcher, bool)>,
) -> bool {
    let Some((matcher, path_scoped)) = file_matcher else {
        return true;
    };
    let rel = cursor_path(path, workspace_root);
    let hay = if *path_scoped {
        rel
    } else {
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string()
    };
    matcher.is_match(hay)
}

fn is_binary(file: &mut std::fs::File) -> std::io::Result<bool> {
    let mut probe = [0_u8; BINARY_PROBE_BYTES];
    let read = file.read(&mut probe)?;
    let binary = probe[..read].contains(&0);
    file.seek(SeekFrom::Start(0))?;
    Ok(binary)
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    retain_bytes: usize,
    scan_bytes: u64,
) -> std::io::Result<Option<LineRead>> {
    let mut bytes = Vec::with_capacity(retain_bytes.min(16 * 1024));
    let mut consumed = 0_u64;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if consumed == 0 {
                Ok(None)
            } else {
                Ok(Some(LineRead {
                    bytes,
                    consumed,
                    oversized,
                    terminated: true,
                }))
            };
        }
        let remaining_scan = scan_bytes.saturating_sub(consumed);
        if remaining_scan == 0 {
            return Ok(Some(LineRead {
                bytes,
                consumed,
                oversized: true,
                terminated: false,
            }));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let wanted = newline.map_or(available.len(), |index| index + 1);
        let take = wanted.min(usize::try_from(remaining_scan).unwrap_or(usize::MAX));
        if !oversized {
            let remaining = retain_bytes.saturating_add(1).saturating_sub(bytes.len());
            let copy = take.min(remaining);
            bytes.extend_from_slice(&available[..copy]);
            if copy < take || bytes.len() > retain_bytes {
                oversized = true;
                bytes.truncate(retain_bytes);
            }
        }
        reader.consume(take);
        consumed = consumed.saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
        if newline.is_some_and(|index| index < take) {
            return Ok(Some(LineRead {
                bytes,
                consumed,
                oversized,
                terminated: true,
            }));
        }
        if take < wanted || consumed >= scan_bytes {
            return Ok(Some(LineRead {
                bytes,
                consumed,
                oversized: true,
                terminated: false,
            }));
        }
    }
}

fn trim_line_ending(bytes: &[u8]) -> &[u8] {
    let without_lf = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
}

pub(crate) fn not_noise_dir(entry: &ignore::DirEntry) -> bool {
    !matches!(
        entry.file_name().to_str(),
        Some(".git") | Some(".rightkit") | Some(".hg") | Some(".svn")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::tempdir;

    fn ctx(root: &std::path::Path) -> ToolContext {
        ToolContext::new("s1", root, PermissionMode::Bypass, WorkspaceTrust::Trusted)
    }

    fn cursor_from_result(result: &ToolResult) -> String {
        let marker = result.content.lines().last().unwrap();
        let cursor_start = marker.find("cursor={").unwrap() + "cursor=".len();
        marker[cursor_start..marker.len() - 1].to_string()
    }

    #[tokio::test]
    async fn continuation_rejects_other_query_options_or_workspace() {
        let dir = tempdir().unwrap();
        let other = tempdir().unwrap();
        for root in [dir.path(), other.path()] {
            std::fs::write(root.join("a.txt"), "needle\nneedle\nother\n").unwrap();
        }
        let first = SearchTool
            .execute(
                json!({"pattern": "needle", "max_results": 1}),
                &ctx(dir.path()),
            )
            .await;
        let cursor = cursor_from_result(&first);
        for changed in [
            json!({"pattern": "other"}),
            json!({"pattern": "needle", "case_sensitive": true}),
            json!({"pattern": "needle", "output_mode": "count"}),
            json!({"pattern": "needle", "before_context": 1}),
            json!({"pattern": "needle", "file_glob": "*.txt"}),
            json!({"pattern": "needle", "path": "a.txt"}),
        ] {
            let mut request = changed.clone();
            request["cursor"] = json!(cursor);
            let result = SearchTool.execute(request, &ctx(dir.path())).await;
            assert!(
                result.is_error,
                "foreign query accepted: {changed}: {}",
                result.content
            );
        }
        let result = SearchTool
            .execute(
                json!({"pattern": "needle", "cursor": cursor}),
                &ctx(other.path()),
            )
            .await;
        assert!(
            result.is_error,
            "foreign workspace accepted: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn continuation_reports_live_workspace_and_ignore_changes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle one\nneedle two\n").unwrap();
        let first = SearchTool
            .execute(
                json!({"pattern": "needle", "max_results": 1}),
                &ctx(dir.path()),
            )
            .await;
        std::fs::write(
            dir.path().join("a.txt"),
            "new prefix\nneedle one\nneedle two\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".gitignore"), "a.txt\n").unwrap();
        let result = SearchTool
            .execute(
                json!({"pattern": "needle", "cursor": cursor_from_result(&first)}),
                &ctx(dir.path()),
            )
            .await;
        assert!(
            result.is_error || result.content.contains("BestEffort"),
            "changed live corpus silently treated as a frozen continuation: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn search_finds_pattern_in_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello world\nfoo bar\nbaz\n").unwrap();
        let result = SearchTool
            .execute(json!({"pattern": "foo"}), &ctx(dir.path()))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("foo bar"));
        assert!(result.content.contains("status=Complete"));
    }

    #[tokio::test]
    async fn search_returns_empty_for_no_match() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello world\n").unwrap();
        let result = SearchTool
            .execute(json!({"pattern": "zzznomatch"}), &ctx(dir.path()))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("No matches found"));
        assert!(result.content.contains("status=Complete"));
    }

    #[test]
    fn explicit_search_is_execution_evidence_not_semantic_admission() {
        let description = SearchTool.description();
        assert!(description.contains("execution/evidence output only"));
        assert!(description.contains("no semantic ranking or context admission"));
        assert!(!description.contains("rerank"));
    }

    #[tokio::test]
    async fn search_files_with_matches_mode() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle here\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing\n").unwrap();
        let r = SearchTool
            .execute(
                json!({"pattern": "needle", "output_mode": "files_with_matches"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("a.txt"));
        assert!(!r.content.contains("b.txt"));
        assert!(!r.content.contains("needle here"));
    }

    #[tokio::test]
    async fn search_count_mode() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x\nx\ny\nx\n").unwrap();
        let r = SearchTool
            .execute(
                json!({"pattern": "x", "output_mode": "count"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("a.txt:3"));
    }

    #[tokio::test]
    async fn count_cursor_carries_partial_count_across_mid_file_resume() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle\nneedle\n").unwrap();
        let first = SearchTool
            .execute(
                json!({"pattern": "needle", "output_mode": "count", "max_bytes": 7}),
                &ctx(dir.path()),
            )
            .await;
        let cursor = cursor_from_result(&first);
        let r = SearchTool
            .execute(
                json!({
                    "pattern": "needle",
                    "output_mode": "count",
                    "cursor": cursor
                }),
                &ctx(dir.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("a.txt:2"), "{}", r.content);
    }

    #[tokio::test]
    async fn search_with_context_includes_neighbors() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nMATCH\nfour\nfive\n").unwrap();
        let r = SearchTool
            .execute(json!({"pattern": "MATCH", "context": 1}), &ctx(dir.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("two"));
        assert!(r.content.contains("MATCH"));
        assert!(r.content.contains("four"));
        assert!(!r.content.contains(":1- one"), "{}", r.content);
        assert!(!r.content.contains(":5- five"), "{}", r.content);
    }

    #[tokio::test]
    async fn after_context_survives_result_page_boundary() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "MATCH\nafter\n").unwrap();
        let first = SearchTool
            .execute(
                json!({"pattern": "MATCH", "after_context": 1, "max_results": 1}),
                &ctx(dir.path()),
            )
            .await;
        assert!(first.content.contains("MATCH"));
        assert!(first.content.contains("status=Truncated"));
        let second = SearchTool
            .execute(
                json!({
                    "pattern": "MATCH",
                    "after_context": 1,
                    "max_results": 10,
                    "cursor": cursor_from_result(&first)
                }),
                &ctx(dir.path()),
            )
            .await;
        assert!(second.content.contains("after"), "{}", second.content);
    }

    fn result_rows(result: &ToolResult) -> Vec<String> {
        result
            .content
            .lines()
            .filter(|line| !line.starts_with("[search status=") && *line != "No matches found.")
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn context_pagination_matches_complete_order_across_every_row_boundary() {
        let dir = tempdir().unwrap();
        let corpus = "PRIVATE_CONTEXT_SENTINEL\nsk-abcdefghijklmnopqrstuvwx\nMATCH one\nafter\nMATCH overlap\nlast\ngap1\ngap2\ngap3\nlead1\nlead2\nMATCH two\nend\n";
        std::fs::write(dir.path().join("a.txt"), corpus).unwrap();
        std::fs::write(dir.path().join("b.txt"), corpus).unwrap();
        for (before, after) in [(2, 1), (0, 1), (2, 0), (3, 3)] {
            let base =
                json!({"pattern": "MATCH", "before_context": before, "after_context": after});
            let complete = SearchTool.execute(base.clone(), &ctx(dir.path())).await;
            assert!(
                complete.content.contains("status=Complete"),
                "{}",
                complete.content
            );
            let expected = result_rows(&complete);
            for page_size in 1..=8 {
                let mut request = base.clone();
                request["max_results"] = json!(page_size);
                let mut actual = Vec::new();
                let mut cursors = std::collections::HashSet::new();
                let mut finished = false;
                for _ in 0..100 {
                    let page = SearchTool.execute(request.clone(), &ctx(dir.path())).await;
                    assert!(!page.is_error, "{}", page.content);
                    assert!(!page.content.contains("sk-abcdefghijklmnopqrstuvwx"));
                    let rows = result_rows(&page);
                    assert!(rows.len() <= page_size);
                    actual.extend(rows);
                    if page.content.contains("status=Complete") {
                        finished = true;
                        break;
                    }
                    let cursor = cursor_from_result(&page);
                    assert!(!cursor.contains("PRIVATE_CONTEXT_SENTINEL"));
                    assert!(!cursor.contains("MATCH"));
                    assert!(
                        cursors.insert(cursor.clone()),
                        "no pagination progress: {cursor}"
                    );
                    request["cursor"] = json!(cursor);
                }
                assert!(finished, "pagination did not terminate");
                assert_eq!(
                    actual, expected,
                    "before={before} after={after} page={page_size}"
                );
            }
        }
    }

    #[tokio::test]
    async fn context_replay_budget_terminates_without_repeating_unusable_cursor() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "A\nB\nMATCH\n").unwrap();
        let mut request = json!({"pattern": "MATCH", "before_context": 2, "max_results": 1});
        let first = SearchTool.execute(request.clone(), &ctx(dir.path())).await;
        request["cursor"] = json!(cursor_from_result(&first));
        request["max_bytes"] = json!(6);
        let page = SearchTool.execute(request, &ctx(dir.path())).await;
        assert!(!page.is_error, "{}", page.content);
        assert!(
            page.content
                .contains("status=Truncated reason=context_replay_budget"),
            "{}",
            page.content
        );
        assert!(page.content.contains("bytes=6 "), "{}", page.content);
        assert!(page.content.ends_with("cursor=null]"));
    }

    #[tokio::test]
    async fn output_ceiling_includes_cursor_without_skipping_rows() {
        use std::io::Write;
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.txt");
        let mut file = std::fs::File::create(&path).unwrap();
        let padding = "x".repeat(100_000);
        for _ in 0..14 {
            writeln!(file, "MATCH {padding}").unwrap();
        }
        drop(file);
        let mut request = json!({"pattern": "MATCH", "before_context": 2});
        let mut next_line = 1;
        let mut finished = false;
        for _ in 0..10 {
            let page = SearchTool.execute(request.clone(), &ctx(dir.path())).await;
            assert!(!page.is_error, "{}", page.content);
            assert!(page.content.len() <= MAX_OUTPUT_BYTES);
            for row in result_rows(&page) {
                assert!(row.starts_with(&format!("{}:{next_line}: MATCH ", path.display())));
                next_line += 1;
            }
            if page.content.contains("status=Complete") {
                finished = true;
                break;
            }
            request["cursor"] = json!(cursor_from_result(&page));
        }
        assert!(finished);
        assert_eq!(next_line, 15);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unreadable_file_is_partial_instead_of_false_complete() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("locked.txt");
        std::fs::write(&path, "MATCH\n").unwrap();
        let _exclusive = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let result = SearchTool
            .execute(json!({"pattern": "MATCH"}), &ctx(dir.path()))
            .await;
        assert!(
            result
                .content
                .contains("status=Truncated reason=file_unavailable"),
            "{}",
            result.content
        );
        assert!(!result.content.contains("status=Complete"));
    }

    #[tokio::test]
    async fn cancelled_continuation_preserves_context_frontier_for_retry() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "A\nB\nMATCH\nafter\n").unwrap();
        let base = json!({"pattern": "MATCH", "before_context": 2, "after_context": 1});
        let complete = SearchTool.execute(base.clone(), &ctx(dir.path())).await;
        let mut request = base.clone();
        request["max_results"] = json!(1);
        let first = SearchTool.execute(request.clone(), &ctx(dir.path())).await;
        let cursor = cursor_from_result(&first);
        request["cursor"] = json!(cursor);
        let (_sender, receiver) = tokio::sync::watch::channel(true);
        let mut cancelled_ctx = ctx(dir.path());
        cancelled_ctx.cancel_rx = Some(receiver);
        let cancelled = SearchTool.execute(request.clone(), &cancelled_ctx).await;
        assert!(cancelled.content.contains("status=Cancelled"));
        assert_eq!(cursor_from_result(&cancelled), cursor);
        request["max_results"] = json!(100);
        request["cursor"] = json!(cursor_from_result(&cancelled));
        let resumed = SearchTool.execute(request, &ctx(dir.path())).await;
        let mut rows = result_rows(&first);
        rows.extend(result_rows(&resumed));
        assert_eq!(rows, result_rows(&complete));
    }

    #[tokio::test]
    async fn continuation_rejects_unbounded_or_invalid_context_metadata() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "A\nB\nMATCH\n").unwrap();
        let base = json!({"pattern": "MATCH", "before_context": 2, "max_results": 1});
        let first = SearchTool.execute(base.clone(), &ctx(dir.path())).await;
        let original: Value = serde_json::from_str(&cursor_from_result(&first)).unwrap();
        let mut variants = Vec::new();
        let mut cursor = original.clone();
        cursor["beforeLines"][0]["len"] = json!(MAX_LINE_BYTES + 1);
        variants.push(cursor);
        let mut cursor = original.clone();
        cursor["beforeLines"] = json!(vec![
            original["beforeLines"][0].clone();
            MAX_CONTEXT_LINES + 1
        ]);
        variants.push(cursor);
        let mut cursor = original.clone();
        cursor["beforeLines"][0]["offset"] = json!(u64::MAX);
        variants.push(cursor);
        let mut cursor = original.clone();
        cursor["separatorForLine"] = json!(99);
        variants.push(cursor);
        for cursor in variants {
            let mut request = base.clone();
            request["cursor"] = json!(cursor.to_string());
            let result = SearchTool.execute(request, &ctx(dir.path())).await;
            assert!(
                result.is_error,
                "invalid cursor accepted: {}",
                result.content
            );
        }
    }

    #[tokio::test]
    async fn invalid_output_mode_and_glob_are_typed_errors() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "needle\n").unwrap();
        let bad_mode = SearchTool
            .execute(
                json!({"pattern": "needle", "output_mode": "bogus"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(bad_mode.is_error);
        assert!(
            bad_mode.content.starts_with("Invalid output_mode"),
            "{}",
            bad_mode.content
        );
        let bad_glob = SearchTool
            .execute(
                json!({"pattern": "needle", "file_glob": "[unclosed"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(bad_glob.is_error);
        assert!(
            bad_glob.content.starts_with("Invalid glob"),
            "{}",
            bad_glob.content
        );
        let bad_regex = SearchTool
            .execute(json!({"pattern": "(unclosed"}), &ctx(dir.path()))
            .await;
        assert!(bad_regex.is_error);
        assert!(bad_regex.content.starts_with("Invalid regex: "));
        let missing = SearchTool.execute(json!({}), &ctx(dir.path())).await;
        assert!(missing.is_error);
    }

    #[tokio::test]
    async fn context_lines_are_bounded_and_glob_and_regex_combine() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "one\nfn alpha_1()\nthree\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "fn alpha_2()\n").unwrap();
        let r = SearchTool
            .execute(
                json!({"pattern": r"fn alpha_\d", "file_glob": "*.rs", "context": 1000}),
                &ctx(dir.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("alpha_1"));
        assert!(r.content.contains("one") && r.content.contains("three"));
        assert!(!r.content.contains("alpha_2"));
        assert!(r.content.contains("context_ceiling"), "{}", r.content);
    }

    #[tokio::test]
    async fn search_file_glob_filters_by_extension() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "needle\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "needle\n").unwrap();
        let r = SearchTool
            .execute(
                json!({"pattern": "needle", "file_glob": "*.rs"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("a.rs"));
        assert!(!r.content.contains("b.txt"));
    }

    #[tokio::test]
    async fn search_respects_gitignore() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("kept.txt"), "needle kept\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "needle ignored\n").unwrap();
        let r = SearchTool
            .execute(json!({"pattern": "needle"}), &ctx(dir.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("kept.txt"));
        assert!(r.content.contains("needle kept"));
        assert!(!r.content.contains("ignored.txt"));
        assert!(!r.content.contains("needle ignored"));
    }

    #[tokio::test]
    async fn search_path_glob_matches_nested() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "needle\n").unwrap();
        std::fs::write(dir.path().join("top.rs"), "needle\n").unwrap();
        let r = SearchTool
            .execute(
                json!({"pattern": "needle", "file_glob": "src/**/*.rs"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("a.rs"));
        assert!(!r.content.contains("top.rs"));
    }

    #[tokio::test]
    async fn search_path_dotdot_traversal_is_rejected() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret needle").unwrap();
        let outside_name = outside.path().file_name().unwrap().to_str().unwrap();
        let r = SearchTool
            .execute(
                json!({"pattern": "needle", "path": format!("../{outside_name}")}),
                &ctx(workspace.path()),
            )
            .await;
        assert!(r.is_error, "got: {}", r.content);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_does_not_follow_file_symlink_outside_workspace() {
        use std::os::unix::fs::symlink;
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "outside needle\n").unwrap();
        symlink(&secret, workspace.path().join("linked.txt")).unwrap();
        let r = SearchTool
            .execute(json!({"pattern": "needle"}), &ctx(workspace.path()))
            .await;
        assert!(!r.is_error);
        assert!(!r.content.contains("outside needle"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn search_path_junction_escape_is_rejected() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret needle").unwrap();
        let link = workspace.path().join("link");
        let made = rightkit_child_process::command_std("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(made, "mklink /J should not require elevation");
        let r = SearchTool
            .execute(
                json!({"pattern": "needle", "path": "link"}),
                &ctx(workspace.path()),
            )
            .await;
        assert!(r.is_error, "got: {}", r.content);
    }

    #[tokio::test]
    async fn search_skips_binary_and_handles_invalid_utf8_without_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("binary.bin"), b"needle\0binary").unwrap();
        std::fs::write(
            dir.path().join("invalid.txt"),
            b"prefix\xffneedle\nsuffix\n",
        )
        .unwrap();
        let r = SearchTool
            .execute(json!({"pattern": "needle"}), &ctx(dir.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("invalid.txt"));
        assert!(!r.content.contains("binary.bin"));
    }

    #[tokio::test]
    async fn invalid_utf8_is_matched_as_bytes_not_lossy_text() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("invalid.txt"), b"prefix\xffneedle\n").unwrap();
        let r = SearchTool
            .execute(
                json!({"pattern": "(?-u:\\xFF)needle", "case_sensitive": true}),
                &ctx(dir.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("invalid.txt"), "{}", r.content);
    }

    #[tokio::test]
    async fn giant_line_is_bounded_and_reports_truncated() {
        let dir = tempdir().unwrap();
        let mut bytes = vec![b'a'; MAX_LINE_BYTES + 1024];
        bytes.extend_from_slice(b"\nsmall needle\n");
        std::fs::write(dir.path().join("giant.txt"), bytes).unwrap();
        let r = SearchTool
            .execute(json!({"pattern": "needle"}), &ctx(dir.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("small needle"));
        assert!(r.content.contains("status=Truncated"));
        assert!(r.content.contains("oversized_line_skipped"));
        assert!(r.content.len() < MAX_OUTPUT_BYTES + 4096);
    }

    #[tokio::test]
    async fn unterminated_giant_line_stops_at_scan_ceiling() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("giant.txt"),
            vec![b'a'; (MAX_LOGICAL_LINE_SCAN_BYTES as usize) + 4096],
        )
        .unwrap();
        let r = SearchTool
            .execute(json!({"pattern": "needle"}), &ctx(dir.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("status=Truncated"));
        assert!(r.content.contains("oversized_line_scan_ceiling"));
        assert!(r.content.len() < 4096);
    }

    #[tokio::test]
    async fn cancellation_is_typed_and_happens_before_file_scan() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
        let (tx, rx) = tokio::sync::watch::channel(false);
        tx.send(true).unwrap();
        let mut context = ctx(dir.path());
        context.cancel_rx = Some(rx);
        let r = SearchTool
            .execute(json!({"pattern": "needle"}), &context)
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("status=Cancelled"));
    }

    #[tokio::test]
    async fn zero_deadline_is_typed_without_unbounded_work() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
        let r = SearchTool
            .execute(
                json!({"pattern": "needle", "deadline_ms": 0}),
                &ctx(dir.path()),
            )
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("status=Deadline"));
    }

    #[tokio::test]
    async fn rejected_candidates_still_honor_deadline_and_cancellation() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();
        let args = json!({"pattern": "needle", "file_glob": "*.rs"});
        let mut deadline_args = args.clone();
        deadline_args["deadline_ms"] = json!(0);
        let deadline = SearchTool.execute(deadline_args, &ctx(dir.path())).await;
        assert!(
            deadline.content.contains("status=Deadline"),
            "{}",
            deadline.content
        );
        let (_sender, receiver) = tokio::sync::watch::channel(true);
        let mut cancelled_ctx = ctx(dir.path());
        cancelled_ctx.cancel_rx = Some(receiver);
        let cancelled = SearchTool.execute(args, &cancelled_ctx).await;
        assert!(
            cancelled.content.contains("status=Cancelled"),
            "{}",
            cancelled.content
        );
    }

    #[tokio::test]
    async fn result_ceiling_returns_resumable_cursor() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "needle one\nneedle two\nneedle three\n",
        )
        .unwrap();
        let first = SearchTool
            .execute(
                json!({"pattern": "needle", "max_results": 1}),
                &ctx(dir.path()),
            )
            .await;
        assert!(first.content.contains("status=Truncated"));
        let second = SearchTool
            .execute(
                json!({
                    "pattern": "needle",
                    "max_results": 10,
                    "cursor": cursor_from_result(&first)
                }),
                &ctx(dir.path()),
            )
            .await;
        assert!(!second.is_error, "{}", second.content);
        assert!(!second.content.contains("needle one"));
        assert!(second.content.contains("needle two"));
        assert!(second.content.contains("needle three"));
    }

    #[tokio::test]
    async fn count_mode_does_not_lose_next_file_at_result_boundary() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "needle\nneedle\n").unwrap();
        let first = SearchTool
            .execute(
                json!({
                    "pattern": "needle",
                    "output_mode": "count",
                    "max_results": 1
                }),
                &ctx(dir.path()),
            )
            .await;
        assert!(first.content.contains("a.txt:1"));
        let second = SearchTool
            .execute(
                json!({
                    "pattern": "needle",
                    "output_mode": "count",
                    "max_results": 10,
                    "cursor": cursor_from_result(&first)
                }),
                &ctx(dir.path()),
            )
            .await;
        assert!(second.content.contains("b.txt:2"), "{}", second.content);
    }

    #[test]
    fn legacy_cursor_without_query_binding_is_rejected() {
        assert!(parse_cursor(Some(r#"{"path":"a.txt","offset":7,"line":1}"#), "binding").is_err());
    }
}
