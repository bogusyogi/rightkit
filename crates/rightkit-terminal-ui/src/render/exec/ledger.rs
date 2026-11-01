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

#[cfg(test)]
mod tests {
    use super::super::ExecCell;
    use super::*;
    use crate::render::cell::Cell;
    use crate::terminal::{Background, ColorDepth};

    fn theme() -> Theme {
        Theme::new(Background::Dark, ColorDepth::TrueColor)
    }

    fn text_of(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn done(name: &str, args: &str, code: i32, out: &str, ms: u64) -> ExecCall {
        let mut call = ExecCall::tool("c1", name, args);
        call.active = false;
        call.output = Some(CommandOutput::new(code, out));
        call.duration = Some(Duration::from_millis(ms));
        call
    }

    #[test]
    fn a_multi_edit_shows_its_changed_rows_in_the_ledger() {
        let args = r#"{"path":"a.py","edits":[{"old_string":"x = 1","new_string":"x = 2"},{"old_string":"y","new_string":"z"}]}"#;
        let preview = edit_preview(
            args,
            80,
            &Theme::new(
                crate::terminal::Background::Dark,
                crate::terminal::ColorDepth::TrueColor,
            ),
        )
        .unwrap();
        assert_eq!((preview.adds, preview.dels), (2, 2));
        assert_eq!(preview.rows.len(), 4);
    }

    #[test]
    fn the_task_list_is_a_plan_row_not_a_file_edit() {
        let entry = classify(
            "todo_write",
            r#"{"todos":[{"content":"a","status":"completed"},{"content":"b","status":"pending"}]}"#,
        );
        assert_eq!(
            (entry.verb.as_str(), entry.target.as_str()),
            ("plan", "1/2 done")
        );
        assert_eq!(entry.kind, Kind::Other);
    }

    #[test]
    fn the_verb_and_target_come_from_the_call_not_its_raw_json() {
        let shell = classify("shell", r#"{"command":"echo hi"}"#);
        assert_eq!(
            (shell.verb.as_str(), shell.target.as_str()),
            ("shell", "echo hi")
        );
        let read = classify("read_file", r#"{"path":"src/lib.rs"}"#);
        assert_eq!(
            (read.verb.as_str(), read.target.as_str()),
            ("read", "src/lib.rs")
        );
        let edit = classify(
            "edit_file",
            r#"{"path":"a.rs","old_string":"x","new_string":"y"}"#,
        );
        assert_eq!((edit.verb.as_str(), edit.target.as_str()), ("edit", "a.rs"));
        let search = classify("grep", r#"{"pattern":"Instant::now","path":"engine"}"#);
        assert_eq!(search.target, "\"Instant::now\" in engine");
        let unknown = classify("weather", r#"{"city":"Oslo"}"#);
        assert_eq!(
            (unknown.verb.as_str(), unknown.target.as_str()),
            ("weather", "Oslo")
        );
        let plain = classify("shell", "echo hi");
        assert_eq!(plain.target, "echo hi");
        let empty = classify("shell", "");
        assert_eq!(empty.target, "");
    }

    #[test]
    fn a_finished_call_is_one_aligned_row_with_result_and_time_on_the_right() {
        let args = r#"{"path":"a/b.rs"}"#;
        let call = done("read_file", args, 0, "l1\nl2\nl3\n", 4);
        let lines = ledger_lines(&call, "read_file", args, 60, &theme());
        assert_eq!(lines.len(), 1);
        let row = text_of(&lines[0]);
        assert!(row.starts_with("  ✓ read    a/b.rs"), "{row:?}");
        assert!(row.ends_with("3 lines"), "{row:?}");
        assert_eq!(display_width(&row), 59, "{row:?}");
    }

    #[test]
    fn verbs_align_across_calls() {
        let read_args = r#"{"path":"x"}"#;
        let grep_args = r#"{"pattern":"y"}"#;
        let a = ledger_lines(
            &done("read_file", read_args, 0, "", 1),
            "read_file",
            read_args,
            50,
            &theme(),
        );
        let b = ledger_lines(
            &done("grep", grep_args, 0, "", 1),
            "grep",
            grep_args,
            50,
            &theme(),
        );
        let col = |l: &Line<'static>, needle: &str| text_of(l).find(needle).unwrap();
        assert_eq!(col(&a[0], "x"), col(&b[0], "\"y\""));
    }

    #[test]
    fn a_successful_shell_collapses_to_its_row() {
        let out = (0..23)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let args = r#"{"command":"cargo test"}"#;
        let call = done("shell", args, 0, &out, 3100);
        let lines = ledger_lines(&call, "shell", args, 80, &theme());
        assert_eq!(lines.len(), 2);
        let row = text_of(&lines[0]);
        assert!(row.contains("✓ shell   cargo test"), "{row:?}");
        assert!(row.ends_with("3.1s"), "{row:?}");
        assert_eq!(text_of(&lines[1]).trim_end(), "     ╰ 23 lines · ctrl+o");
    }

    #[test]
    fn a_single_short_output_line_is_the_result() {
        let args = r#"{"command":"echo hi"}"#;
        let call = done("shell", args, 0, "hi\n", 9);
        let row = text_of(&ledger_lines(&call, "shell", args, 60, &theme())[0]);
        assert!(row.ends_with("hi"), "{row:?}");
    }

    #[test]
    fn a_running_shell_shows_its_tail_under_a_gutter() {
        let args = r#"{"command":"cargo test"}"#;
        let mut cell = ExecCell::new(ExecCall::tool("c1", "shell", args));
        let many = (0..10).map(|i| format!("out {i}\n")).collect::<String>();
        assert!(cell.append_output("c1", &many));
        let texts: Vec<String> = cell
            .display_lines(60, &theme())
            .iter()
            .map(text_of)
            .collect();
        assert!(texts[0].starts_with("  ● shell   cargo test"), "{texts:?}");
        assert_eq!(texts.len(), 1 + TAIL_ROWS + 1, "{texts:?}");
        assert!(texts[1].starts_with("     │ out 6"), "{texts:?}");
        assert!(texts[4].starts_with("     │ out 9"), "{texts:?}");
        assert_eq!(
            texts[5].trim_end(),
            "     ╰ last 4 of 10 lines · ctrl+o full output"
        );
    }

    #[test]
    fn a_failure_opens_to_the_first_error() {
        let args = r#"{"command":"cargo test"}"#;
        let out = "Compiling x\nrunning 3 tests\ntest a ... ok\ntest b ... FAILED\n  left: 3\n  right: 2\nmore\n";
        let call = done("shell", args, 101, out, 4800);
        let texts: Vec<String> = ledger_lines(&call, "shell", args, 80, &theme())
            .iter()
            .map(text_of)
            .collect();
        assert!(texts[0].contains("✗ shell   cargo test"), "{texts:?}");
        assert!(texts[0].ends_with("failed  4.8s"), "{texts:?}");
        assert_eq!(texts[1].trim_end(), "     │ test b ... FAILED");
        assert_eq!(texts[2].trim_end(), "     │   left: 3");
        assert_eq!(texts[3].trim_end(), "     │   right: 2");
        assert_eq!(
            texts[4].trim_end(),
            "     ╰ first error · exit 101 · ctrl+o full output"
        );
    }

    #[test]
    fn an_edit_shows_tinted_changed_rows_and_a_signed_count() {
        let args = r#"{"path":"src/a.rs","old_string":"let a = 1;","new_string":"let a = 2;"}"#;
        let call = done("edit_file", args, 0, "ok", 12);
        let lines = ledger_lines(&call, "edit_file", args, 70, &theme());
        assert_eq!(lines.len(), 3);
        assert!(
            text_of(&lines[0]).ends_with("+1 −1"),
            "{:?}",
            text_of(&lines[0])
        );
        assert!(text_of(&lines[1]).contains("- let a = 1;"));
        assert!(text_of(&lines[2]).contains("+ let a = 2;"));
        // Tinted across the row, in the diff palette.
        let del_bg = lines[1].spans[0].style.bg;
        let add_bg = lines[2].spans[0].style.bg;
        assert!(del_bg.is_some() && add_bg.is_some() && del_bg != add_bg);
        assert_eq!(display_width(&text_of(&lines[1])), 70);
    }

    #[test]
    fn a_unified_diff_edit_numbers_its_rows() {
        let diff = "@@ -41,2 +41,2 @@\n-    let d = Instant::now();\n+    let d = clock.now();\n     keep\n";
        let args = serde_json::json!({"path":"retry.rs","patch":diff}).to_string();
        let call = done("apply_patch", &args, 0, "ok", 5);
        let texts: Vec<String> = ledger_lines(&call, "apply_patch", &args, 80, &theme())
            .iter()
            .map(text_of)
            .collect();
        assert!(texts[1].contains("41 -"), "{texts:?}");
        assert!(texts[2].contains("41 +"), "{texts:?}");
    }

    #[test]
    fn narrow_widths_shed_the_result_before_the_target() {
        let args = r#"{"command":"cargo test -p router retry"}"#;
        let call = done("shell", args, 0, "a\nb\nc\n", 3100);
        for width in [24u16, 30, 44] {
            let row = text_of(&ledger_lines(&call, "shell", args, width, &theme())[0]);
            assert!(
                display_width(&row) <= usize::from(width),
                "{width}: {row:?}"
            );
            assert!(row.contains("shell"), "{width}: {row:?}");
        }
    }

    #[test]
    fn hostile_arguments_cannot_inject_control_bytes() {
        let args = serde_json::json!({"command":"echo \u{1b}]0;pwned\u{7}"}).to_string();
        let call = done("shell", &args, 0, "\u{1b}[31mred\u{1b}[0m", 1);
        for line in ledger_lines(&call, "shell", &args, 60, &theme()) {
            let text = text_of(&line);
            assert!(
                !text.contains('\u{1b}') && !text.contains('\u{7}'),
                "{text:?}"
            );
        }
    }

    #[test]
    fn durations_use_the_ledger_format() {
        assert_eq!(ledger_time(Duration::from_millis(4)), "4ms");
        assert_eq!(ledger_time(Duration::from_millis(3100)), "3.1s");
        assert_eq!(ledger_time(Duration::from_secs(42)), "42s");
        assert_eq!(ledger_time(Duration::from_secs(63)), "1m 03s");
    }

    #[test]
    fn a_failure_footer_names_the_exit_code() {
        let args = r#"{"command":"cargo test"}"#;
        let call = done(
            "shell",
            args,
            101,
            "ok
error: boom
more
",
            4800,
        );
        let texts: Vec<String> = ledger_lines(&call, "shell", args, 80, &theme())
            .iter()
            .map(text_of)
            .collect();
        let footer = texts.last().unwrap();
        assert!(
            footer.contains("exit 101 · ctrl+o full output"),
            "{texts:?}"
        );
    }

    #[test]
    fn parallel_calls_are_bracketed_with_a_count() {
        let mk = |n: &str, a: &str| {
            let call = done(n, a, 0, "", 4);
            ledger_lines(&call, n, a, 60, &theme())
        };
        let rows = bracket_parallel(
            vec![
                mk("read", r#"{"path":"a.rs"}"#),
                mk("read", r#"{"path":"b.rs"}"#),
                mk("search", r#"{"pattern":"x"}"#),
            ],
            &theme(),
        );
        let t: Vec<String> = rows.iter().map(text_of).collect();
        assert_eq!(t.len(), 4, "{t:?}");
        assert!(t[0].starts_with("┌ ✓ read"), "{t:?}");
        assert!(t[1].starts_with("│ ✓ read"), "{t:?}");
        assert!(t[2].starts_with("└ ✓ search"), "{t:?}");
        assert_eq!(t[3].trim_end(), "  3 ran in parallel");
    }
}
