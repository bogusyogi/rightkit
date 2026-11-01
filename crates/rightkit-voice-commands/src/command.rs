//! Typed standalone-command action — the shape carried over IPC from a
//! recognizer to the host's dispatcher. Pure serde; the actual dispatch
//! (Win32 SendInput / ShellExecuteW / power ops) lives in the host application.
//!
//! Boundary rule: the protocol carries TYPED actions, not raw shell commands.

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandAction {
    /// One or more chord strings, sent in order. e.g. `["ctrl+a"]` or
    /// `["home", "shift+end", "delete"]`. Rust parses each chord into Win32
    /// SendInput modifier + key, rejects unknown keys.
    KeySequence {
        chords: Vec<String>,
        #[serde(default)]
        description: Option<String>,
    },
    /// Launch an app by alias-normalized name. Rust uses ShellExecuteW
    /// (NOT shell-out to cmd.exe) for safety; unknown names are tried via
    /// Start-Menu / App-Paths resolution.
    LaunchApp { name: String },
    /// Run a macOS Shortcut by name via the `shortcuts run` CLI. macOS-only;
    /// the engine resolves the spoken name to an installed shortcut (live
    /// `shortcuts list` + fuzzy match) before this fires, and gates it on the
    /// Pro entitlement. Carries only the resolved name — never a shell string.
    RunShortcut { name: String },
    /// Mouse click or scroll at current cursor.
    Mouse {
        action: String,
        #[serde(default)]
        button: Option<String>,
        #[serde(default)]
        clicks: Option<u32>,
        #[serde(default)]
        direction: Option<String>,
        #[serde(default)]
        page: bool,
    },
    /// Destructive power action with explicit variant. Rust enforces delay
    /// + cancel semantics (no arbitrary shutdown command strings over IPC).
    /// `requires_confirm` is always true for user-facing power commands;
    /// the dispatch layer must show a confirmation UI before executing.
    Power {
        op: String,
        #[serde(default = "default_true")]
        requires_confirm: bool,
    },
    /// Apply a casing transform to the last paste delivery.
    LastPasteTransform { transform: String },
    /// Sentinel for the small set of misc actions (always_on_top,
    /// backspace_last, clear_clipboard, etc.). Rust has a small handler
    /// per `op` name; unknown ops are rejected.
    Special { op: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_key_sequence() {
        let a: CommandAction =
            serde_json::from_value(json!({ "kind": "key_sequence", "chords": ["ctrl+a"] }))
                .unwrap();
        assert!(matches!(a, CommandAction::KeySequence { chords, .. } if chords == ["ctrl+a"]));
    }

    #[test]
    fn parses_launch_app_and_power() {
        let a: CommandAction =
            serde_json::from_value(json!({ "kind": "launch_app", "name": "notepad" })).unwrap();
        assert!(matches!(a, CommandAction::LaunchApp { name } if name == "notepad"));
        let p: CommandAction =
            serde_json::from_value(json!({ "kind": "power", "op": "lock" })).unwrap();
        assert!(matches!(p, CommandAction::Power { op, requires_confirm: true } if op == "lock"));
    }

    #[test]
    fn parses_run_shortcut() {
        let a: CommandAction =
            serde_json::from_value(json!({ "kind": "run_shortcut", "name": "Good Night" }))
                .unwrap();
        assert!(matches!(a, CommandAction::RunShortcut { name } if name == "Good Night"));
    }

    #[test]
    fn rejects_unknown_kind() {
        let r: Result<CommandAction, _> =
            serde_json::from_value(json!({ "kind": "format_c_drive" }));
        assert!(r.is_err());
    }
}
