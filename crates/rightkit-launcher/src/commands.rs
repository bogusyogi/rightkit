//! Custom launcher commands (the Windows counterpart of the Mac's `LauncherCommand` and
//! `LauncherShell`). A command is a name, a keyword and a command line, read from the
//! `commands` array of the `launcherConfig` JSON. A run goes through `cmd.exe /D /S /C`
//! with no console window, its output is captured and capped, and it is ended after
//! `TIME_LIMIT`.

#[cfg(windows)]
mod shell;
#[cfg(windows)]
pub use shell::{run, spawn};

use crate::json::Value;

/// A named command. `line` is read from the JSON member `command`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Command {
    pub name: String,
    pub keyword: String,
    pub line: String,
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The text to show; "Done. No output." when nothing was printed.
    Output(String),
    /// The output followed by "Exited with status N.", or the reason it could not run.
    Failed(String),
    /// The time limit passed and the child was ended. Output is not kept.
    TimedOut,
}

fn text_of(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The commands in `value` (a parsed `launcherConfig`); empty when there are none.
pub fn parse(value: &Value) -> Vec<Command> {
    value
        .get("commands")
        .and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .map(|item| Command {
            name: text_of(item, "name"),
            keyword: text_of(item, "keyword"),
            line: text_of(item, "command"),
        })
        .collect()
}
