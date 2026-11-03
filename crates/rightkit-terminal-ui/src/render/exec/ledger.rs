//! The tool ledger: one aligned row per tool call.
//!
//! ```text
//!   ✓ read    engine/crates/router/src/retry.rs              212 lines   4ms
//!   ✓ edit    engine/crates/router/src/retry.rs                 +2 −2   12ms
//!         41 -     let deadline = Instant::now() + backoff;
//!         41 +     let deadline = clock.now() + backoff;
//!   ● shell   rustright cargo test -p router retry                        0:12
//!      │    Compiling router v0.4.2
//!      ╰ last 4 of 23 lines
//!   ✗ shell   rustright cargo test -p router --target x           failed  4.8s
//!      │ test retry::flaky_deadline ... FAILED
//!      ╰ first error
//! ```
//!
//! Verb column aligned, result and time right-aligned. A finished call is one
//! line; a running shell shows its tail under a `│`/`╰` gutter; a failure
//! opens to the first error by itself. Nothing here is guessed: the verb and
//! target come from the call's own name and arguments, the result from its
//! real output, and anything unknown is omitted rather than invented.

use std::time::Duration;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::component::{Role, Theme};
use crate::render::diff::{ledger_diff_row, ledger_diff_rows};
use crate::transcript::wrap::display_width;

use super::sanitize;
use super::{CommandOutput, ExecCall};

const VERB_WIDTH: usize = 7;
const INDENT: &str = "  ";
const GUTTER: &str = "     ";
/// Rows of a running shell's tail.
const TAIL_ROWS: usize = 4;
/// Rows of a failure opened to its first error.
const ERROR_ROWS: usize = 3;
/// Changed rows shown under an edit.
const DIFF_ROWS: usize = 6;

/// What a call is, as the ledger names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LedgerEntry {
    pub verb: String,
    pub target: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Shell,
    Path,
    Edit,
    Other,
}

fn arg_str<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(serde_json::Value::as_str))
        .filter(|v| !v.trim().is_empty())
}

/// Classify a tool call by name and JSON arguments.
pub(super) fn classify(name: &str, args_json: &str) -> LedgerEntry {
    let parsed = serde_json::from_str::<serde_json::Value>(args_json).ok();
    // Arguments that are not JSON (a plain command or path from a producer
    // that does not wrap them) are taken as the target text itself.
    let plain = match &parsed {
        None => args_json.lines().next().unwrap_or("").trim().to_string(),
        Some(_) => String::new(),
    };
    let args = parsed.unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let first_line = |text: &str| text.lines().next().unwrap_or("").trim().to_string();
    let (verb, target, kind) = if lower.contains("shell")
        || lower.contains("bash")
        || lower == "exec"
        || lower == "run"
        || lower.contains("run_command")
    {
        (
            "shell",
            arg_str(&args, &["command", "cmd", "script"])
                .map(first_line)
                .unwrap_or_default(),
            Kind::Shell,
        )
    } else if lower.contains("todo") {
        // The task list is bookkeeping, not a file edit: `plan  2/3 done`.
        let items = args.get("todos").and_then(serde_json::Value::as_array);
        let target = items
            .map(|items| {
                let done = items
                    .iter()
                    .filter(|item| {
                        item.get("status").and_then(serde_json::Value::as_str) == Some("completed")
                    })
                    .count();
                format!("{done}/{} done", items.len())
            })
            .unwrap_or_default();
        ("plan", target, Kind::Other)
    } else if lower.contains("edit") || lower.contains("write") || lower.contains("patch") {
        (
            "edit",
            arg_str(&args, &["path", "file_path", "file", "filePath"])
                .map(str::to_string)
                .unwrap_or_default(),
            Kind::Edit,
        )
    } else if lower.contains("read") || lower == "cat" || lower.contains("view") {
        (
            "read",
            arg_str(&args, &["path", "file_path", "file", "filePath"])
                .map(str::to_string)
                .unwrap_or_default(),
            Kind::Path,
        )
    } else if lower.contains("search") || lower.contains("grep") || lower.contains("glob") {
        let pattern = arg_str(&args, &["pattern", "query", "regex", "glob"]);
        let path = arg_str(&args, &["path", "dir", "directory"]);
        let target = match (pattern, path) {
            (Some(p), Some(d)) => format!("\"{p}\" in {d}"),
            (Some(p), None) => format!("\"{p}\""),
            (None, Some(d)) => d.to_string(),
            (None, None) => String::new(),
        };
        ("search", target, Kind::Other)
    } else if lower.starts_with("list") || lower == "ls" || lower.contains("list_dir") {
        (
            "list",
            arg_str(&args, &["path", "dir", "directory"])
                .map(str::to_string)
                .unwrap_or_default(),
            Kind::Path,
        )
    } else {
        // Unknown tool: its own name, and its first plain string argument.
        let target = args
            .as_object()
            .and_then(|o| o.values().find_map(serde_json::Value::as_str))
            .map(first_line)
            .unwrap_or_default();
        (name, target, Kind::Other)
    };
    let target = if target.is_empty() { plain } else { target };
    LedgerEntry {
        verb: sanitize(verb),
        // Escapes inside the JSON decode to real control bytes, so the
        // sanitizer must run on the decoded value, not the raw argument text.
        target: sanitize(&target),
        kind,
    }
}

/// `4ms`, `12ms`, `3.1s`, `42s`, `1m 03s`.
pub(super) fn ledger_time(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if d.as_secs() < 60 {
        format!("{}s", d.as_secs())
    } else {
        format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

fn clock(d: Duration) -> String {
    format!("{}:{:02}", d.as_secs() / 60, d.as_secs() % 60)
}

fn truncate_end(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = display_width(&ch.to_string());
        if used + w + 1 > max {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

fn truncate_start(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 1;
    for ch in text.chars().rev() {
        let w = display_width(&ch.to_string());
        if used + w > max {
            break;
        }
        used += w;
        tail.push(ch);
    }
    tail.reverse();
    format!("…{}", tail.into_iter().collect::<String>())
}

/// Edit arguments reduced to what the ledger can honestly show.
struct EditPreview {
    rows: Vec<Line<'static>>,
    adds: usize,
    dels: usize,
}

fn edit_preview(args_json: &str, width: usize, theme: &Theme) -> Option<EditPreview> {
    let args: serde_json::Value = serde_json::from_str(args_json).ok()?;
    if let Some(diff) = arg_str(&args, &["patch", "diff", "unified_diff", "unifiedDiff"]) {
        let (rows, adds, dels) = ledger_diff_rows(diff, DIFF_ROWS, width, theme);
        if adds + dels > 0 {
            return Some(EditPreview { rows, adds, dels });
        }
        return None;
    }
    // `edit_file` carries one old/new pair; `multi_edit` carries `edits[]`.
    let pair = |item: &serde_json::Value| -> Option<(String, String)> {
        let old = item.get("old_string").and_then(serde_json::Value::as_str)?;
        let new = item.get("new_string").and_then(serde_json::Value::as_str)?;
        Some((old.to_string(), new.to_string()))
    };
    let pairs: Vec<(String, String)> = match args.get("edits").and_then(serde_json::Value::as_array)
    {
        Some(items) => items.iter().filter_map(pair).collect(),
        None => pair(&args).into_iter().collect(),
    };
    if !pairs.is_empty() {
        let mut rows = Vec::new();
        let (mut adds, mut dels) = (0usize, 0usize);
        for (old, new) in &pairs {
            let old_lines: Vec<&str> = if old.is_empty() {
                Vec::new()
            } else {
                old.lines().collect()
            };
            let new_lines: Vec<&str> = if new.is_empty() {
                Vec::new()
            } else {
                new.lines().collect()
            };
            for text in &old_lines {
                if rows.len() < DIFF_ROWS {
                    rows.push(ledger_diff_row(false, None, text, width, theme));
                }
            }
            for text in &new_lines {
                if rows.len() < DIFF_ROWS {
                    rows.push(ledger_diff_row(true, None, text, width, theme));
                }
            }
            adds += new_lines.len();
            dels += old_lines.len();
        }
        return Some(EditPreview { rows, adds, dels });
    }
    if let Some(content) = arg_str(&args, &["content", "new_content", "newContent"]) {
        let lines: Vec<&str> = content.lines().collect();
        let rows = lines
            .iter()
            .take(DIFF_ROWS)
            .enumerate()
            .map(|(i, text)| ledger_diff_row(true, Some(i + 1), text, width, theme))
            .collect();
        return Some(EditPreview {
            rows,
            adds: lines.len(),
            dels: 0,
        });
    }
    None
}

/// The first line that reads like an error, else the first line, as an index.
/// Stack-trace decoration (`+ CategoryInfo`, `At line:1`, `at ...`) never
/// counts: PowerShell and runtimes put the real message above it.
fn first_error_index(lines: &[std::borrow::Cow<'_, str>]) -> usize {
    const MARKERS: [&str; 14] = [
        "error",
        "failed",
        "panicked",
        "fatal",
        "exception",
        "traceback",
        "denied",
        "not recognized",
        "not found",
        "no such",
        "cannot",
        "unrecognized",
        "invalid",
        "unknown",
    ];
    let decoration = |l: &str| {
        let t = l.trim_start();
        t.starts_with("+ ")
            || t.starts_with("At line:")
            || t.starts_with("at ")
            || t.starts_with("~")
    };
    lines
        .iter()
        .position(|l| {
            if decoration(l) {
                return false;
            }
            let lower = l.to_ascii_lowercase();
            MARKERS.iter().any(|m| lower.contains(m))
        })
        .or_else(|| {
            lines
                .iter()
                .position(|l| !l.trim().is_empty() && !decoration(l))
        })
        .unwrap_or(0)
}

fn gutter_row(glyph: &str, text: &str, width: usize, theme: &Theme) -> Line<'static> {
    let dim = Style::default().fg(theme.color(Role::Dim));
    let budget = width.saturating_sub(display_width(GUTTER) + 2).max(1);
    Line::from(vec![
        Span::styled(format!("{GUTTER}{glyph} "), dim),
        Span::styled(truncate_end(text, budget), dim),
    ])
}

/// Bracket a run of calls that executed in parallel: `┌`/`│`/`└` in the
/// left column of each call's row (and `│` down its body rows), then a dim
/// `N ran in parallel` line. One call is returned unchanged.
pub(super) fn bracket_parallel(
    groups: Vec<Vec<Line<'static>>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let count = groups.len();
    if count < 2 {
        return groups.into_iter().flatten().collect();
    }
    let dim = Style::default().fg(theme.color(Role::Dim));
    let mut out = Vec::new();
    for (index, rows) in groups.into_iter().enumerate() {
        let last = index + 1 == count;
        for (row_index, mut line) in rows.into_iter().enumerate() {
            let mark = if row_index == 0 {
                if index == 0 {
                    "┌"
                } else if last {
                    "└"
                } else {
                    "│"
                }
            } else if last {
                " "
            } else {
                "│"
            };
            // Replace the first column (always a space gutter) in place.
            if let Some(first) = line.spans.first_mut() {
                let rest: String = first.content.chars().skip(1).collect();
                first.content = rest.into();
            }
            line.spans.insert(0, Span::styled(mark.to_string(), dim));
            out.push(line);
        }
    }
    out.push(Line::from(Span::styled(
        format!("{INDENT}{count} ran in parallel"),
        dim,
    )));
    out
}

/// Render one tool call as its ledger row plus any body rows.
pub(super) fn ledger_lines(
    call: &ExecCall,
    name: &str,
    args_json: &str,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let width = usize::from(width).max(8);
    let entry = classify(name, args_json);
    let exit = call.output.as_ref().and_then(|o| o.exit_code);
    let failed = !call.active && exit.is_some_and(|code| code != 0);
    let (glyph, role) = if call.active {
        ("●", Role::Running)
    } else if failed {
        ("✗", Role::Failure)
    } else {
        ("✓", Role::Success)
    };
    let glyph_style = if call.active {
        Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.color(role))
    };
    let dim = Style::default().fg(theme.color(Role::Dim));

    let output: Option<&CommandOutput> = call.output.as_ref();
    let (total, _) = output.map_or((0, 0), CommandOutput::line_counts);
    let preview = if entry.kind == Kind::Edit {
        edit_preview(args_json, width, theme)
    } else {
        None
    };

    // Right column: result, then time.
    let mut result = String::new();
    let mut result_style = dim;
    if failed {
        result = "failed".to_string();
        result_style = Style::default().fg(theme.color(Role::Failure));
    } else if !call.active {
        if let Some(p) = &preview {
            result = format!("+{} −{}", p.adds, p.dels);
        } else if total == 1 {
            let only = output
                .and_then(|o| o.lines().first().map(|l| l.trim().to_string()))
                .unwrap_or_default();
            if !only.is_empty() && display_width(&only) <= 28 {
                result = only;
            } else {
                result = "1 line".to_string();
            }
        } else if total > 1 && entry.kind != Kind::Shell {
            // A shell's line count lives in its `╰` footer instead.
            result = format!("{total} lines");
        }
    }
    let time = if call.active {
        call.started
            .map(|s| s.elapsed())
            .filter(|d| d.as_secs() >= 1)
            .map(clock)
            .unwrap_or_default()
    } else {
        // Sub-second durations are noise (`0ms` on every read); show 1s+.
        call.duration
            .filter(|d| d.as_millis() >= 1_000)
            .map(ledger_time)
            .unwrap_or_default()
    };

    let verb = format!("{:<width$}", entry.verb, width = VERB_WIDTH);
    let fixed_left = display_width(INDENT) + 2 + display_width(&verb) + 1;
    let join = |result: &str, time: &str| match (result.is_empty(), time.is_empty()) {
        (true, true) => String::new(),
        (false, true) => result.to_string(),
        (true, false) => time.to_string(),
        (false, false) => format!("{result}  {time}"),
    };
    // Narrow terminals shed the result first, then the time, never the target.
    let min_target = 12.min(width.saturating_sub(fixed_left));
    let fits = |text: &str| fixed_left + min_target + display_width(text) + 2 <= width;
    let mut shown_result = result.clone();
    let mut shown_time = time.clone();
    if !fits(&join(&shown_result, &shown_time)) {
        shown_result.clear();
        if !fits(&join(&shown_result, &shown_time)) {
            shown_time.clear();
        }
    }
    let right_w = display_width(&join(&shown_result, &shown_time));
    let target_budget = width
        .saturating_sub(fixed_left + right_w + if right_w > 0 { 2 } else { 0 })
        .max(1);
    let target = match entry.kind {
        Kind::Path | Kind::Edit => truncate_start(&entry.target, target_budget),
        _ => truncate_end(&entry.target, target_budget),
    };
    let gap = width.saturating_sub(fixed_left + display_width(&target) + right_w + 1);

    let mut spans = vec![
        Span::raw(INDENT),
        Span::styled(glyph.to_string(), glyph_style),
        Span::raw(" "),
        Span::styled(verb, dim),
        Span::raw(" "),
        Span::styled(target, Style::default().fg(theme.color(Role::Text))),
    ];
    if right_w > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
        if !shown_result.is_empty() {
            let sep = if shown_time.is_empty() { "" } else { "  " };
            spans.push(Span::styled(shown_result.clone(), result_style));
            spans.push(Span::styled(format!("{sep}{shown_time}"), dim));
        } else {
            spans.push(Span::styled(shown_time.clone(), dim));
        }
    }
    let mut lines = vec![Line::from(spans)];

    // Edit: tinted changed rows under the row.
    if let Some(p) = preview {
        lines.extend(p.rows);
    }

    // Shell body.
    if let Some(output) = output {
        let all = output.lines();
        if call.active && entry.kind == Kind::Shell && !all.is_empty() {
            let start = all.len().saturating_sub(TAIL_ROWS);
            for text in &all[start..] {
                lines.push(gutter_row("│", text, width, theme));
            }
            let shown = all.len() - start;
            let footer = if total > shown {
                format!("last {shown} of {total} lines · ctrl+o full output")
            } else {
                format!("{total} line{}", if total == 1 { "" } else { "s" })
            };
            lines.push(gutter_row("╰", &footer, width, theme));
        } else if failed && !all.is_empty() {
            let start = first_error_index(&all);
            let end = (start + ERROR_ROWS).min(all.len());
            for text in &all[start..end] {
                lines.push(gutter_row("│", text, width, theme));
            }
            let mut footer = if start > 0 || end < all.len() {
                "first error".to_string()
            } else {
                "output".to_string()
            };
            if let Some(code) = exit {
                footer.push_str(&format!(" · exit {code}"));
            }
            footer.push_str(" · ctrl+o full output");
            lines.push(gutter_row("╰", &footer, width, theme));
        } else if !call.active && !failed && entry.kind == Kind::Shell && total > 1 {
            // Success collapses: the row above plus one `╰` footer.
            lines.push(gutter_row(
                "╰",
                &format!("{total} lines · ctrl+o"),
                width,
                theme,
            ));
        }
    }
    lines
}
