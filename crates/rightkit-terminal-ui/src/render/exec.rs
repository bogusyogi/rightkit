//! Command-execution history cells: one bordered command block, or a
//! grouped "Explored" block for read/list/search batches.
//!
//! Donor lineage: `codex-rs/tui/src/exec_cell/{model,render,live_output}.rs`
//! and `codex-rs/tui/src/history_cell/exec.rs` at
//! `444da310e108da16aaeb18fd790b0ac464f08aca`, adapted to this crate's
//! seams:
//!
//! - **Sanitizer-in, never ANSI-reinterpreted.** Codex re-parses ANSI in
//!   command output (`ansi_escape_line`); the host app treats tool output as
//!   data and strips controls at the boundary, so [`CommandOutput`] and
//!   [`LiveCommandOutput`] take sanitized text and never emit terminal
//!   control bytes.
//! - **Roles, not colours.** Success/failure resolve through
//!   [`Theme`]/[`Role`] with a `•`/`•`-glyph signal pair; no literal
//!   `green()`/`red()`.
//! - **No animation.** The donor's `activity_indicator` is replaced by a
//!   static `•` marker while a call is in flight — the plan forbids
//!   inventing motion to hide latency.
//! - **Host supplies classification.** `ExploreEntry` rows (`Read`,
//!   `List`, `Search`, `Run`) are classified by whoever owns the tool
//!   registry; this module only draws them.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::component::{Role, Theme};
use crate::transcript::wrap::wrap;

use super::cell::{Cell, prefix_lines};
use super::sanitize::sanitize;

mod ledger;

/// Compact preview cap for a single command's output — donor
/// `TOOL_CALL_MAX_LINES` (`exec_cell/render.rs`).
pub const TOOL_CALL_MAX_LINES: usize = 5;
/// A user-invoked shell keeps a wider window; donor
/// `USER_SHELL_TOOL_CALL_MAX_LINES`.
pub const USER_SHELL_TOOL_CALL_MAX_LINES: usize = 50;
const MAX_INTERACTION_PREVIEW_CHARS: usize = 80;

const LIVE_COMMAND_OUTPUT_MAX_BYTES: usize = 1024 * 1024;
const LIVE_COMMAND_OUTPUT_MAX_LINES: usize = 50;
const LIVE_COMMAND_OUTPUT_MAX_LINE_BYTES: usize =
    LIVE_COMMAND_OUTPUT_MAX_BYTES / (2 * LIVE_COMMAND_OUTPUT_MAX_LINES + 2);
const LIVE_COMMAND_OUTPUT_LINE_HEAD_BYTES: usize = LIVE_COMMAND_OUTPUT_MAX_LINE_BYTES / 2;
const LIVE_COMMAND_OUTPUT_LINE_TAIL_BYTES: usize =
    LIVE_COMMAND_OUTPUT_MAX_LINE_BYTES - LIVE_COMMAND_OUTPUT_LINE_HEAD_BYTES;

/// Escape argv back into a single displayable command line. Adapted from
/// codex `exec_command::escape_command`/`split_command_string` — single-quote
/// arguments containing whitespace or shell punctuation; join plainly
/// otherwise.
pub fn escape_command(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| escape_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn escape_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .chars()
        .any(|c| c.is_whitespace() || "\"'\\$`!#&|<>*?()[]{}~^;".contains(c))
    {
        format!("'{}'", arg.replace('\'', r#"'\''"#))
    } else {
        arg.to_string()
    }
}

/// Strip a shell wrapper (`bash -lc`, `sh -c`, `pwsh -Command`,
/// `cmd /c`, ...) so the displayed command is the script the user would
/// recognize. Adapted from codex `exec_command::strip_bash_lc_and_escape`
/// + `shell-command::extract_shell_command`.
pub fn strip_shell_and_escape(command: &[String]) -> String {
    if let Some(script) = extract_shell_script(command) {
        return script.to_string();
    }
    escape_command(command)
}

/// Recognize a `shell <flag> <script>` triple: the binary basename in
/// {bash, zsh, sh, pwsh, powershell, powershell.exe, cmd, cmd.exe} plus a
/// script flag (-lc, -c, /c, /C, -Command). Returns the script.
fn extract_shell_script(command: &[String]) -> Option<&str> {
    let [shell, flag, script] = command else {
        return None;
    };
    let basename = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shell)
        .to_ascii_lowercase();
    let basename = basename.strip_suffix(".exe").unwrap_or(&basename);
    let is_shell = matches!(
        basename,
        "bash" | "zsh" | "sh" | "pwsh" | "powershell" | "cmd"
    );
    let is_script_flag = matches!(
        flag.as_str(),
        "-lc" | "-c" | "-Command" | "-command" | "/c" | "/C"
    );
    (is_shell && is_script_flag).then_some(script.as_str())
}

/// A bounded, incremental preview of streamed command output.
///
/// Lifted from `exec_cell/live_output.rs` with the ANSI-terminator logic
/// removed: input is already sanitized upstream, so no `\x1b` can be present
/// and the omission marker needs no escape-sequence closing.
#[derive(Debug, Default)]
pub struct LiveCommandOutput {
    full_output: String,
    truncated: bool,
    head: Vec<String>,
    tail: VecDeque<String>,
    current: LiveCommandOutputLine,
    completed_lines: usize,
    has_partial_line: bool,
}

impl LiveCommandOutput {
    /// Appends a delta, preserving `str::lines` semantics across arbitrary
    /// chunk boundaries. Input is sanitized at this boundary too, so a
    /// hostile stream cannot smuggle terminal controls into the preview.
    pub fn push_str(&mut self, chunk: &str) {
        let chunk = sanitize(chunk);
        self.push_sanitized(&chunk);
    }

    fn push_sanitized(&mut self, chunk: &str) {
        if !self.truncated {
            if self.full_output.len().saturating_add(chunk.len()) <= LIVE_COMMAND_OUTPUT_MAX_BYTES {
                self.full_output.push_str(chunk);
                self.completed_lines = self
                    .completed_lines
                    .saturating_add(chunk.bytes().filter(|b| *b == b'\n').count());
                if !chunk.is_empty() {
                    self.has_partial_line = !chunk.ends_with('\n');
                }
                return;
            }
            self.truncated = true;
            let full_output = std::mem::take(&mut self.full_output);
            self.completed_lines = 0;
            self.has_partial_line = false;
            self.push_truncated_str(&full_output);
        }
        self.push_truncated_str(chunk);
    }

    /// Bounded append after the byte budget is exceeded: keep head + tail.
    fn push_truncated_str(&mut self, chunk: &str) {
        for part in chunk.split_inclusive('\n') {
            let Some(part) = part.strip_suffix('\n') else {
                if part.is_empty() {
                    continue;
                }
                self.current.push_str(part);
                self.has_partial_line |= !part.is_empty();
                continue;
            };
            let part = part.strip_suffix('\r').unwrap_or(part);
            self.current.push_str(part);
            self.completed_lines = self.completed_lines.saturating_add(1);
            self.has_partial_line = false;

            let line = std::mem::take(&mut self.current).render();
            if self.head.len() < LIVE_COMMAND_OUTPUT_MAX_LINES {
                self.head.push(line);
            } else {
                if self.tail.len() == LIVE_COMMAND_OUTPUT_MAX_LINES {
                    self.tail.pop_front();
                }
                self.tail.push_back(line);
            }
        }
    }

    pub fn total_lines(&self) -> usize {
        self.completed_lines
            .saturating_add(usize::from(self.has_partial_line))
    }

    pub fn retained_lines(&self) -> usize {
        if self.truncated {
            self.head
                .len()
                .saturating_add(self.tail.len())
                .saturating_add(usize::from(self.has_partial_line))
        } else {
            self.total_lines()
        }
    }

    /// Preview lines, reverse-capable for cheap tail rendering.
    pub fn lines(&self) -> Vec<Cow<'_, str>> {
        if self.truncated {
            self.head
                .iter()
                .map(|l| Cow::Borrowed(l.as_str()))
                .chain(self.tail.iter().map(|l| Cow::Borrowed(l.as_str())))
                .chain(
                    self.has_partial_line
                        .then(|| Cow::Owned(self.current.render())),
                )
                .collect()
        } else {
            self.full_output
                .lines()
                .map(|line| {
                    if line.len() <= LIVE_COMMAND_OUTPUT_MAX_LINE_BYTES {
                        Cow::Borrowed(line)
                    } else {
                        let mut truncated = LiveCommandOutputLine::default();
                        truncated.push_str(line);
                        Cow::Owned(truncated.render())
                    }
                })
                .collect()
        }
    }

    /// Transcript lines: lossless until truncated, then head + omission
    /// marker + tail.
    pub fn transcript_lines(&self) -> Vec<Cow<'_, str>> {
        let omitted = self.total_lines().saturating_sub(self.retained_lines());
        if self.truncated {
            self.head
                .iter()
                .map(|l| Cow::Borrowed(l.as_str()))
                .chain((omitted > 0).then(|| {
                    let noun = if omitted == 1 { "line" } else { "lines" };
                    Cow::Owned(format!("… +{omitted} {noun}"))
                }))
                .chain(self.tail.iter().map(|l| Cow::Borrowed(l.as_str())))
                .chain(
                    self.has_partial_line
                        .then(|| Cow::Owned(self.current.render())),
                )
                .collect()
        } else {
            self.full_output.lines().map(Cow::Borrowed).collect()
        }
    }
}

/// Head/tail bounding for a single over-long line: keep the first
/// `HEAD_BYTES` and last `TAIL_BYTES` with an explicit `... N bytes omitted
/// ...` marker between them.
#[derive(Debug, Default)]
struct LiveCommandOutputLine {
    head: String,
    tail: String,
    omitted_bytes: usize,
}

impl LiveCommandOutputLine {
    fn push_str(&mut self, chunk: &str) {
        let head_remaining = if self.tail.is_empty() && self.omitted_bytes == 0 {
            LIVE_COMMAND_OUTPUT_LINE_HEAD_BYTES.saturating_sub(self.head.len())
        } else {
            0
        };
        let mut head_end = head_remaining.min(chunk.len());
        while !chunk.is_char_boundary(head_end) {
            head_end -= 1;
        }
        self.head.push_str(&chunk[..head_end]);
        let chunk = &chunk[head_end..];
        if chunk.is_empty() {
            return;
        }

        if chunk.len() >= LIVE_COMMAND_OUTPUT_LINE_TAIL_BYTES {
            let mut tail_start = chunk.len() - LIVE_COMMAND_OUTPUT_LINE_TAIL_BYTES;
            while !chunk.is_char_boundary(tail_start) {
                tail_start += 1;
            }
            self.omitted_bytes = self
                .omitted_bytes
                .saturating_add(self.tail.len())
                .saturating_add(tail_start);
            self.tail.clear();
            self.tail.push_str(&chunk[tail_start..]);
            return;
        }

        self.tail.push_str(chunk);
        let mut tail_start = self
            .tail
            .len()
            .saturating_sub(LIVE_COMMAND_OUTPUT_LINE_TAIL_BYTES);
        while !self.tail.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        if tail_start > 0 {
            self.tail.drain(..tail_start);
            self.omitted_bytes = self.omitted_bytes.saturating_add(tail_start);
        }
    }

    fn render(&self) -> String {
        let mut line = String::with_capacity(
            self.head
                .len()
                .saturating_add(self.tail.len())
                .saturating_add(48),
        );
        line.push_str(&self.head);
        if self.omitted_bytes > 0 {
            line.push_str(&format!("... {} bytes omitted ...", self.omitted_bytes));
        }
        line.push_str(&self.tail);
        line
    }
}

/// One command's settled (or still streaming) output. `aggregated_output`
/// is the finalized, interleaved stdout/stderr; `live_output` is the
/// bounded preview while deltas are still arriving.
#[derive(Debug, Default)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    aggregated_output: String,
    live_output: Option<LiveCommandOutput>,
}

impl CommandOutput {
    /// Construct from finalized output. `aggregated_output` is sanitized
    /// here so nothing downstream re-parses or re-emits control bytes.
    pub fn new(exit_code: i32, aggregated_output: impl Into<String>) -> Self {
        Self {
            exit_code: Some(exit_code),
            aggregated_output: sanitize(&aggregated_output.into()),
            live_output: None,
        }
    }

    /// Output with no exit code yet — used while a call streams.
    pub fn streaming() -> Self {
        Self::default()
    }

    fn line_counts(&self) -> (usize, usize) {
        match self.live_output.as_ref() {
            Some(output) => (output.total_lines(), output.retained_lines()),
            None => {
                let total = self.aggregated_output.lines().count();
                (total, total)
            }
        }
    }

    fn lines(&self) -> Vec<Cow<'_, str>> {
        match self.live_output.as_ref() {
            Some(output) => output.lines(),
            None => self.aggregated_output.lines().map(Cow::Borrowed).collect(),
        }
    }

    fn transcript_lines(&self) -> Vec<Cow<'_, str>> {
        match self.live_output.as_ref() {
            Some(output) => output.transcript_lines(),
            None => self.aggregated_output.lines().map(Cow::Borrowed).collect(),
        }
    }
}

/// Where a command came from. Mirrors codex's `CommandExecutionSource` —
/// the row header distinguishes "Ran" (agent), "You ran" (interactive
/// shell), and "Interacted with background terminal".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    Agent,
    UserShell,
    Interaction,
}

/// One classified row in an "Explored" group. The host decides the
/// classification — this enum only carries the label the row draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExploreEntry {
    Read {
        name: String,
    },
    List {
        path: Option<String>,
        command: String,
    },
    Search {
        query: Option<String>,
        path: Option<String>,
        command: String,
    },
    Run {
        command: String,
    },
}

/// One executed command inside a cell.
#[derive(Debug)]
pub struct ExecCall {
    pub call_id: String,
    /// argv as the tool call recorded it — sanitized at construction.
    pub command: Vec<String>,
    /// Classification rows for exploring-group rendering. Empty for a
    /// plain command cell.
    pub entries: Vec<ExploreEntry>,
    pub output: Option<CommandOutput>,
    pub source: CommandSource,
    /// Set while the call is still running; cleared on completion.
    pub active: bool,
    pub duration: Option<Duration>,
    /// Input sent to a background terminal (Interaction source only).
    pub interaction_input: Option<String>,
    /// The tool's own name and JSON arguments, for the aligned ledger row.
    /// `None` for argv-style commands, which keep the codex block shape.
    tool: Option<(String, String)>,
    /// When the call began, so a running row can show a clock.
    pub started: Option<std::time::Instant>,
}

impl ExecCall {
    pub fn agent(call_id: impl Into<String>, command: Vec<String>) -> Self {
        Self {
            call_id: sanitize(&call_id.into()),
            command: command.iter().map(|c| sanitize(c)).collect(),
            entries: Vec::new(),
            output: None,
            source: CommandSource::Agent,
            active: true,
            duration: None,
            interaction_input: None,
            tool: None,
            started: None,
        }
    }

    /// A model tool call, drawn as a ledger row (`✓ shell  cmd … result  time`)
    /// from its name and JSON arguments. Both are sanitized here.
    pub fn tool(
        call_id: impl Into<String>,
        name: impl Into<String>,
        args_json: impl Into<String>,
    ) -> Self {
        let name = sanitize(&name.into());
        let args = sanitize(&args_json.into());
        let mut call = Self::agent(call_id, vec![name.clone(), args.clone()]);
        call.tool = Some((name, args));
        call.started = Some(std::time::Instant::now());
        call
    }

    pub fn is_user_shell(&self) -> bool {
        matches!(self.source, CommandSource::UserShell)
    }

    pub fn is_interaction(&self) -> bool {
        matches!(self.source, CommandSource::Interaction)
    }

    fn command_display(&self) -> String {
        strip_shell_and_escape(&self.command)
    }
}

fn summarize_interaction_input(input: &str) -> String {
    let single_line = input.replace('\n', "\\n");
    let escaped = single_line.replace('`', "\\`");
    if escaped.chars().count() <= MAX_INTERACTION_PREVIEW_CHARS {
        return escaped;
    }
    let mut preview: String = escaped
        .chars()
        .take(MAX_INTERACTION_PREVIEW_CHARS)
        .collect();
    preview.push_str("...");
    preview
}

/// History cell for command execution: a single command or an
/// "Explored"/"Exploring" group of read/list/search calls.
#[derive(Debug)]
pub struct ExecCell {
    calls: Vec<ExecCall>,
}

impl ExecCell {
    /// A single command cell. `call` must be `ExecCall::agent`-style or
    /// `UserShell`/`Interaction` with no `entries`.
    pub fn new(call: ExecCall) -> Self {
        Self { calls: vec![call] }
    }

    /// Tool calls that ran at the same time, drawn as one bracketed block
    /// (`┌ │ └` plus `N ran in parallel`). Calls are looked up by id, so
    /// `append_output`/`complete_call` work on any member.
    pub fn parallel(calls: Vec<ExecCall>) -> Self {
        Self { calls }
    }

    /// An exploring group of read/list/search calls.
    pub fn exploring(calls: Vec<ExecCall>) -> Self {
        Self { calls }
    }

    /// Whether every call qualifies for the compact "Explored" grouping:
    /// agent-sourced and entirely read/list/search entries.
    pub fn is_exploring(&self) -> bool {
        !self.calls.is_empty()
            && self.calls.iter().all(|c| {
                !c.is_user_shell()
                    && !c.entries.is_empty()
                    && c.entries
                        .iter()
                        .all(|e| !matches!(e, ExploreEntry::Run { .. }))
            })
    }

    pub fn is_active(&self) -> bool {
        self.calls.iter().any(|c| c.active)
    }

    /// Append streamed output to a call. Sanitized inside
    /// `LiveCommandOutput`. Returns false on an unknown id — the caller
    /// should treat that as a routing mismatch (codex semantics: an orphan
    /// end must not attach to an unrelated active cell).
    pub fn append_output(&mut self, call_id: &str, chunk: &str) -> bool {
        if chunk.is_empty() {
            return false;
        }
        let Some(call) = self.calls.iter_mut().rev().find(|c| c.call_id == call_id) else {
            return false;
        };
        let output = call.output.get_or_insert_with(CommandOutput::streaming);
        output
            .live_output
            .get_or_insert_with(LiveCommandOutput::default)
            .push_str(chunk);
        true
    }

    /// Settle a call: attach final output and duration. Returns false if
    /// `call_id` isn't in this cell.
    pub fn complete_call(
        &mut self,
        call_id: &str,
        output: CommandOutput,
        duration: Duration,
    ) -> bool {
        let Some(call) = self.calls.iter_mut().rev().find(|c| c.call_id == call_id) else {
            return false;
        };
        call.output = Some(output);
        call.duration = Some(duration);
        call.active = false;
        true
    }

    /// Settle all still-running calls as failed (used on cancel).
    pub fn mark_failed(&mut self) {
        for call in self.calls.iter_mut() {
            if call.active {
                call.active = false;
                call.duration = Some(Duration::ZERO);
                call.output
                    .get_or_insert_with(CommandOutput::streaming)
                    .exit_code = Some(1);
            }
        }
    }

    fn output_ellipsis_line(omitted: usize, theme: &Theme) -> Line<'static> {
        let noun = if omitted == 1 { "line" } else { "lines" };
        Line::from(Span::styled(
            format!("… +{omitted} {noun}"),
            Style::default().fg(theme.color(Role::Dim)),
        ))
    }

    fn command_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let [call] = self.calls.as_slice() else {
            // `new` constructs single-call cells; a merged multi-call
            // non-exploring cell would be a programming error, so render
            // each call's header separately rather than panic.
            if self.calls.iter().all(|c| c.tool.is_some()) {
                return ledger::bracket_parallel(
                    self.calls
                        .iter()
                        .map(|c| Self::single_command_lines(c, width, theme))
                        .collect(),
                    theme,
                );
            }
            let mut out = Vec::new();
            for call in &self.calls {
                out.extend(Self::single_command_lines(call, width, theme));
            }
            return out;
        };
        Self::single_command_lines(call, width, theme)
    }

    fn single_command_lines(call: &ExecCall, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        if let Some((name, args)) = call.tool.as_ref() {
            return ledger::ledger_lines(call, name, args, width, theme);
        }
        let success = if call.active {
            None
        } else {
            call.output
                .as_ref()
                .and_then(|o| o.exit_code)
                .map(|code| code == 0)
        };
        let (bullet, bullet_role) = match success {
            Some(true) => ("•", Role::Success),
            Some(false) => ("•", Role::Failure),
            None => ("•", Role::Running),
        };
        let bullet_style = Style::default()
            .fg(theme.color(bullet_role))
            .add_modifier(Modifier::BOLD);

        let title = if call.is_interaction() {
            ""
        } else if call.active {
            "Running"
        } else if call.is_user_shell() {
            "You ran"
        } else {
            "Ran"
        };

        let cmd_display = if call.is_interaction() {
            match call.interaction_input.as_deref() {
                Some(input) if !input.is_empty() => format!(
                    "Interacted with `{}`, sent `{}`",
                    call.command_display(),
                    summarize_interaction_input(input)
                ),
                _ => format!("Waited for `{}`", call.command_display()),
            }
        } else {
            call.command_display()
        };

        let header_prefix_len = if title.is_empty() {
            2 // "• "
        } else {
            2 + title.len() + 1
        };
        let wrap_width = (width as usize).saturating_sub(header_prefix_len).max(1);
        let wrapped_cmd = wrap(&cmd_display, wrap_width as u16);

        let mut lines: Vec<Line<'static>> = Vec::new();
        for (i, row) in wrapped_cmd.into_iter().enumerate() {
            let mut spans = Vec::new();
            if i == 0 {
                spans.push(Span::styled(format!("{bullet} "), bullet_style));
                if !title.is_empty() {
                    spans.push(Span::styled(
                        format!("{title} "),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                }
            } else {
                // Continuation rows indent under the command text, aligned
                // to the first row's prefix.
                spans.push(Span::styled(" ".repeat(header_prefix_len), dim(theme)));
            }
            spans.push(Span::styled(
                row,
                Style::default().add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::from(spans));
        }

        // Output block under a `└` gutter, bounded per call kind.
        if let Some(output) = call.output.as_ref() {
            let line_limit = if call.is_user_shell() {
                USER_SHELL_TOOL_CALL_MAX_LINES
            } else {
                TOOL_CALL_MAX_LINES
            };
            let rendered = output_lines(output, line_limit, theme);
            if rendered.is_empty() && !call.is_interaction() {
                lines.push(Line::from(vec![
                    Span::styled("  └ ".to_string(), dim(theme)),
                    Span::styled("(no output)".to_string(), dim(theme)),
                ]));
            } else {
                lines.extend(rendered);
            }
        }
        lines
    }

    fn exploring_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        let active = self.is_active();
        let failures = self
            .calls
            .iter()
            .filter(|c| {
                !c.active
                    && c.output
                        .as_ref()
                        .and_then(|o| o.exit_code)
                        .is_some_and(|code| code != 0)
            })
            .count();
        let mut header = vec![
            Span::styled(
                "• ".to_string(),
                Style::default()
                    .fg(theme.color(if active { Role::Running } else { Role::Dim }))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if active { "Exploring" } else { "Explored" }.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ];
        if failures > 0 {
            header.push(Span::styled(
                format!(" · {failures} failed"),
                Style::default()
                    .fg(theme.color(Role::Failure))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        out.push(Line::from(header));

        let mut body: Vec<Line<'static>> = Vec::new();
        for call in &self.calls {
            for entry in &call.entries {
                let (verb, detail) = match entry {
                    ExploreEntry::Read { name } => ("Read", name.clone()),
                    ExploreEntry::List { path, command } => {
                        ("List", path.clone().unwrap_or_else(|| command.clone()))
                    }
                    ExploreEntry::Search {
                        query,
                        path,
                        command,
                    } => {
                        let detail = match (query, path) {
                            (Some(q), Some(p)) => format!("{q} in {p}"),
                            (Some(q), None) => q.clone(),
                            _ => command.clone(),
                        };
                        ("Search", detail)
                    }
                    ExploreEntry::Run { command } => ("Run", command.clone()),
                };
                let exit_note = call
                    .output
                    .as_ref()
                    .and_then(|o| o.exit_code)
                    .filter(|code| !call.active && *code != 0)
                    .map(|code| {
                        let label = format!(" (exit {code})");
                        let is_search_no_match =
                            code == 1 && matches!(entry, ExploreEntry::Search { .. });
                        Span::styled(
                            label,
                            Style::default().fg(theme.color(if is_search_no_match {
                                Role::Dim
                            } else {
                                Role::Failure
                            })),
                        )
                    });
                let line_w = (width as usize).saturating_sub(4 + verb.len() + 1).max(1);
                for (i, row) in wrap(&detail, line_w as u16).into_iter().enumerate() {
                    let mut spans = vec![
                        Span::styled(
                            if i == 0 {
                                format!("{verb} ")
                            } else {
                                " ".repeat(verb.len() + 1)
                            },
                            Style::default().fg(theme.color(Role::Dim)),
                        ),
                        Span::raw(row),
                    ];
                    if i == 0 {
                        if let Some(note) = exit_note.clone() {
                            spans.push(note);
                        }
                    }
                    body.push(Line::from(spans));
                }
            }
        }

        out.extend(prefix_lines(
            body,
            Span::styled("  └ ".to_string(), dim(theme)),
            Span::styled("    ".to_string(), dim(theme)),
        ));
        out
    }
}

fn dim(theme: &Theme) -> Style {
    Style::default().fg(theme.color(Role::Dim))
}

/// Bound output rows: head lines under `  └ `/`    ` prefixes, an
/// omission marker, then tail lines. Adapted from `output_lines` in
/// `exec_cell/render.rs` — no ANSI re-parse, plain dim styling.
fn output_lines(output: &CommandOutput, line_limit: usize, theme: &Theme) -> Vec<Line<'static>> {
    let (total, retained) = output.line_counts();
    if total == 0 {
        return Vec::new();
    }
    let lines = output.lines();
    let mut out: Vec<Line<'static>> = Vec::new();

    let head_end = total.min(line_limit).min(retained);
    for (i, raw) in lines.iter().take(head_end).enumerate() {
        let prefix = if i == 0 { "  └ " } else { "    " };
        out.push(Line::from(vec![
            Span::styled(prefix.to_string(), dim(theme)),
            Span::styled(raw.to_string(), dim(theme)),
        ]));
    }

    let tail_len = total
        .saturating_sub(head_end)
        .min(line_limit)
        .min(retained.saturating_sub(head_end));
    let omitted = total.saturating_sub(head_end + tail_len);
    if omitted > 0 {
        let mut marker = ExecCell::output_ellipsis_line(omitted, theme);
        marker
            .spans
            .insert(0, Span::styled("    ".to_string(), dim(theme)));
        out.push(marker);
    }

    for raw in lines.iter().skip(lines.len().saturating_sub(tail_len)) {
        out.push(Line::from(vec![
            Span::styled("    ".to_string(), dim(theme)),
            Span::styled(raw.to_string(), dim(theme)),
        ]));
    }
    out
}

/// A composite `Transcript`-mode listing: head + omission + tail without
/// the `└` gutter — the expanded view keeps the fuller record.
fn transcript_output_lines(output: &CommandOutput) -> Vec<Line<'static>> {
    output
        .transcript_lines()
        .into_iter()
        .map(|l| Line::from(l.into_owned()))
        .collect()
}

impl Cell for ExecCell {
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        if self.is_exploring() {
            self.exploring_lines(width, theme)
        } else {
            self.command_lines(width, theme)
        }
    }

    fn transcript_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        // Expanded transcript keeps the retained output rows losslessly;
        // the bounded `output_lines` head/tail preview is display-only.
        if self.is_exploring() {
            return self.exploring_lines(width, theme);
        }
        let mut out = Vec::new();
        for call in &self.calls {
            let display = Self::single_command_lines(call, width, theme);
            if let Some((name, args)) = call.tool.as_ref() {
                // Expanded view: the ledger row, then the full captured
                // output instead of the collapsed excerpt.
                let full = call
                    .output
                    .as_ref()
                    .map(transcript_output_lines)
                    .unwrap_or_default();
                if full.is_empty() || ledger::classify(name, args).kind == ledger::Kind::Edit {
                    out.extend(display);
                    continue;
                }
                out.extend(display.into_iter().take(1));
                out.extend(full.into_iter().map(|mut l| {
                    l.spans
                        .insert(0, Span::styled("     │ ".to_string(), dim(theme)));
                    l
                }));
                continue;
            }
            // The header ends at the first output-gutter row; everything
            // after it is the bounded preview, replaced below by the
            // lossless retained rows.
            let header_end = display
                .iter()
                .position(|l| {
                    l.spans
                        .first()
                        .map(|s| s.content.as_ref().starts_with("  └"))
                        .unwrap_or(false)
                })
                .unwrap_or(display.len());
            out.extend(display.iter().take(header_end).cloned());
            if let Some(output) = call.output.as_ref() {
                out.extend(transcript_output_lines(output).into_iter().map(|mut l| {
                    l.spans
                        .insert(0, Span::styled("    ".to_string(), dim(theme)));
                    l
                }));
            }
        }
        out
    }
}

/// Format a duration like codex's `format_duration`: `<1s`, `Ns`, `Nm Ss`,
/// `Nh Mm`. Used for elapsed annotations the host may append.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if d.as_millis() < 1_000 {
        "<1s".to_string()
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m {}s", secs / 3_600, (secs % 3_600) / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Background, ColorDepth};

    fn theme() -> Theme {
        Theme::new(Background::Dark, ColorDepth::TrueColor)
    }

    fn text_of(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn escape_command_quotes_args_with_whitespace_or_punctuation() {
        assert_eq!(
            escape_command(&["echo".into(), "a b".into(), "x&y".into()]),
            "echo 'a b' 'x&y'"
        );
    }

    #[test]
    fn shell_wrappers_are_stripped_to_their_script() {
        let cmd = vec!["bash".into(), "-lc".into(), "echo hello".into()];
        assert_eq!(strip_shell_and_escape(&cmd), "echo hello");

        let pwsh = vec![
            "C:\\Windows\\pwsh.exe".into(),
            "-Command".into(),
            "Get-Item .".into(),
        ];
        assert_eq!(strip_shell_and_escape(&pwsh), "Get-Item .");

        // A non-wrapper argv renders as escaped argv.
        let plain = vec!["cargo".into(), "test".into()];
        assert_eq!(strip_shell_and_escape(&plain), "cargo test");
    }

    #[test]
    fn live_output_bounds_unbounded_streams() {
        let mut live = LiveCommandOutput::default();
        // Force the truncated path: ~1.1 MiB of streamed output.
        let chunk = "x".repeat(64 * 1024) + "\n";
        for _ in 0..18 {
            live.push_str(&chunk);
        }
        assert!(live.total_lines() >= 17);
        let retained = live.retained_lines();
        assert!(retained <= 2 * LIVE_COMMAND_OUTPUT_MAX_LINES + 1);
        let transcript: Vec<_> = live.transcript_lines();
        assert!(
            transcript
                .iter()
                .any(|l| l.contains("omitted") || l.contains('…'))
        );
    }

    #[test]
    fn live_output_strips_control_bytes_from_the_stream() {
        let mut live = LiveCommandOutput::default();
        live.push_str("ok\x1b[31m red\x07\n");
        for line in live.lines() {
            assert!(!line.contains('\u{1b}'));
            assert!(!line.contains('\u{7}'));
        }
    }

    #[test]
    fn a_single_overlong_line_keeps_head_and_tail_with_a_named_marker() {
        let mut live = LiveCommandOutput::default();
        let line = "h".repeat(LIVE_COMMAND_OUTPUT_MAX_LINE_BYTES + 4096);
        live.push_str(&line);
        live.push_str("\n");
        let rendered = live.lines();
        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains("bytes omitted"));
    }

    #[test]
    fn a_running_command_shows_running_and_a_settled_one_shows_ran() {
        let mut cell = ExecCell::new(ExecCall::agent("c1", vec!["ls".into()]));
        let running = cell.display_lines(80, &theme());
        assert!(text_of(&running[0]).contains("Running"));
        cell.complete_call(
            "c1",
            CommandOutput::new(0, "file.rs\n"),
            Duration::from_secs(1),
        );
        let settled = cell.display_lines(80, &theme());
        assert!(text_of(&settled[0]).contains("Ran"));
        assert!(settled.iter().any(|l| text_of(l).contains("file.rs")));
    }

    #[test]
    fn output_is_bounded_with_an_explicit_omission_marker() {
        let mut cell = ExecCell::new(ExecCall::agent("c1", vec!["spam".into()]));
        let output = (0..50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        cell.complete_call("c1", CommandOutput::new(0, output), Duration::ZERO);
        let lines = cell.display_lines(80, &theme());
        let texts: Vec<String> = lines.iter().map(text_of).collect();
        assert!(texts.iter().any(|t| t.contains('…')));
        // Head + tail + marker + header, never the whole dump.
        assert!(lines.len() < 30);
    }

    #[test]
    fn append_output_rejects_an_unknown_call_id() {
        let mut cell = ExecCell::new(ExecCall::agent("c1", vec!["ls".into()]));
        assert!(!cell.append_output("nope", "x"));
        assert!(cell.append_output("c1", "ok\n"));
    }

    #[test]
    fn exploring_groups_render_verb_rows() {
        let mut read = ExecCall::agent("r1", vec!["cat".into(), "a.rs".into()]);
        read.entries = vec![ExploreEntry::Read {
            name: "a.rs".into(),
        }];
        let mut search = ExecCall::agent("s1", vec!["rg".into(), "foo".into()]);
        search.entries = vec![ExploreEntry::Search {
            query: Some("foo".into()),
            path: Some("src".into()),
            command: "rg foo src".into(),
        }];
        for c in [&mut read, &mut search] {
            c.active = false;
            c.output = Some(CommandOutput::new(0, ""));
        }
        let cell = ExecCell::exploring(vec![read, search]);
        assert!(cell.is_exploring());
        let lines = cell.display_lines(80, &theme());
        let texts: Vec<String> = lines.iter().map(text_of).collect();
        assert!(texts[0].contains("Explored"));
        assert!(texts.iter().any(|t| t.contains("Read a.rs")));
        assert!(texts.iter().any(|t| t.contains("Search foo in src")));
    }

    #[test]
    fn a_hostile_command_cannot_inject_control_bytes_into_the_header() {
        let mut call = ExecCall::agent("c1", vec!["echo".into(), "\x1b]0;pwned\x07".into()]);
        call.active = false;
        let cell = ExecCell::new(call);
        for line in cell.display_lines(80, &theme()) {
            let text = text_of(&line);
            assert!(!text.contains('\u{1b}') && !text.contains('\u{7}'));
        }
    }

    #[test]
    fn parallel_tool_calls_render_as_one_bracketed_block() {
        let mut cell = ExecCell::parallel(vec![
            ExecCall::tool("a", "read_file", r#"{"path":"src/a.rs"}"#),
            ExecCall::tool("b", "read_file", r#"{"path":"src/b.rs"}"#),
        ]);
        assert!(cell.complete_call(
            "a",
            CommandOutput::new(0, "x\ny\n"),
            Duration::from_millis(4)
        ));
        assert!(cell.complete_call("b", CommandOutput::new(0, "x\n"), Duration::from_millis(3)));
        let t: Vec<String> = cell
            .display_lines(70, &theme())
            .iter()
            .map(text_of)
            .collect();
        assert!(t[0].starts_with("┌ ✓ read"), "{t:?}");
        assert!(t[1].starts_with("└ ✓ read"), "{t:?}");
        assert_eq!(t[2].trim_end(), "  2 ran in parallel");
    }
}
