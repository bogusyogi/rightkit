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
