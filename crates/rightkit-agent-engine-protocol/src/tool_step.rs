//! Plain-words facts about one tool step, derived from a stored tool call and its result.
//!
//! Pure functions only: the transcript steps route feeds them the stored call (name and
//! arguments) and the stored result summary, and shows what comes back as is.

use serde_json::Value;

/// How many characters of a shell command a step reports.
pub const STEP_COMMAND_MAX_CHARS: usize = 200;
const LABEL_TEXT_MAX_CHARS: usize = 60;

fn args_of(arguments_json: &str) -> Value {
    serde_json::from_str(arguments_json).unwrap_or(Value::Null)
}

fn text_arg(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

/// First line of `text`, cut to `max` characters with an ellipsis when it was cut.
fn shorten(text: &str, max: usize) -> String {
    let first = text.trim().lines().next().unwrap_or("").trim();
    let multi_line = text.trim().lines().count() > 1;
    if first.chars().count() > max {
        let cut: String = first.chars().take(max).collect();
        format!("{}…", cut.trim_end())
    } else if multi_line {
        format!("{first}…")
    } else {
        first.to_string()
    }
}

/// True for the tools that run a command line.
pub fn is_shell_tool(name: &str) -> bool {
    matches!(name, "shell" | "powershell")
}

/// The kind a step is counted under in the per-turn roll-up:
/// `read`, `search`, `edit`, `run`, `agent` or `other`.
pub fn tool_step_kind(name: &str) -> &'static str {
    match name {
        "read_file" | "view_image" | "list_files" | "git_read" | "git_diff"
        | "get_shell_output" | "read_mcp_resource" | "task_get" | "task_list" | "task_output" => {
            "read"
        }
        "search" | "web_search" | "tool_search" | "lsp" => "search",
        "write_file" | "edit_file" | "multi_edit" | "undo_edit" | "apply_patch"
        | "file_service" | "notebook" | "git_write" => "edit",
        "shell" | "powershell" | "process" | "kill_shell" | "repl" => "run",
        "spawn_subagent" => "agent",
        _ => "other",
    }
}

/// The command a shell-like tool ran, cut to [`STEP_COMMAND_MAX_CHARS`] characters.
pub fn step_command(name: &str, arguments_json: &str) -> Option<String> {
    if !is_shell_tool(name) {
        return None;
    }
    let command = text_arg(&args_of(arguments_json), &["command"])?;
    Some(command.chars().take(STEP_COMMAND_MAX_CHARS).collect())
}

/// The exit code a shell result reports. A failed run is stored with a leading `[exit N]` line;
/// a run that did not fail exited with 0. `None` when the result says neither (for example a
/// denied call or a timeout).
pub fn step_exit_code(name: &str, summary: &str, is_error: bool) -> Option<i32> {
    if !is_shell_tool(name) {
        return None;
    }
    let marked = summary
        .trim_start()
        .strip_prefix("[exit ")
        .and_then(|rest| rest.split(']').next())
        .and_then(|digits| digits.trim().parse::<i32>().ok());
    match marked {
        Some(code) => Some(code),
        None if !is_error => Some(0),
        None => None,
    }
}

/// A short label in plain words for one tool step.
pub fn tool_step_label(name: &str, arguments_json: &str) -> String {
    let args = args_of(arguments_json);
    let path = || text_arg(&args, &["path", "file_path", "filePath"]);
    let with = |verb: &str, subject: Option<String>| match subject {
        Some(subject) => format!("{verb} {}", shorten(&subject, LABEL_TEXT_MAX_CHARS)),
        None => verb.to_string(),
    };
    match name {
        "read_file" => with("Read", path()),
        "view_image" => with("Looked at", path()),
        "list_files" => with("Listed", path()),
        "search" => with("Searched", text_arg(&args, &["pattern", "query"])),
        "web_search" | "tool_search" => with("Searched", text_arg(&args, &["query", "pattern"])),
        "write_file" => with("Wrote", path()),
        "edit_file" | "multi_edit" | "undo_edit" | "notebook" => with("Edited", path()),
        "apply_patch" => match path() {
            Some(path) => with("Edited", Some(path)),
            None => "Edited files".to_string(),
        },
        "shell" | "powershell" => with("Ran", text_arg(&args, &["command"])),
        "spawn_subagent" => match text_arg(&args, &["role"]) {
            Some(role) => format!("Started a sub-agent ({role})"),
            None => "Started a sub-agent".to_string(),
        },
        other => {
            let words = other.replace('_', " ");
            let mut chars = words.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => "Used a tool".to_string(),
            }
        }
    }
}

/// Milliseconds since the Unix epoch for an RFC 3339 time, or `None` when it does not parse.
pub fn rfc3339_to_unix_ms(text: &str) -> Option<i64> {
    let parsed = rightkit_time::DateTime::parse_rfc3339(text)?;
    Some(parsed.unix_seconds() * 1000 + i64::from(parsed.nanos / 1_000_000))
}

/// Non-negative milliseconds from `started` to `ended`; `None` when either does not parse.
pub fn step_duration_ms(started: &str, ended: &str) -> Option<i64> {
    Some((rfc3339_to_unix_ms(ended)? - rfc3339_to_unix_ms(started)?).max(0))
}
