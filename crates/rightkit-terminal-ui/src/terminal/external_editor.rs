//! Ctrl+G: edit text in the user's `$VISUAL` / `$EDITOR`.
//!
//! The caller owns pausing the input reader. This module owns the terminal
//! hand-off (leave raw mode, run the editor on the real tty, re-enter the TUI
//! modes) and the temp file.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Editor command line: `$VISUAL`, then `$EDITOR`, then a platform fallback.
pub fn editor_command() -> String {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| (if cfg!(windows) { "notepad" } else { "vi" }).to_string())
}

/// Open `initial` in the editor and return the edited text. Trailing newlines
/// the editor appends are trimmed. The TUI terminal modes are released while
/// the editor runs and restored before returning, on success and failure.
pub fn edit_text(initial: &str) -> io::Result<String> {
    let path = temp_path();
    std::fs::write(&path, initial)?;
    let result = run_editor(&path).and_then(|()| std::fs::read_to_string(&path));
    let _ = std::fs::remove_file(&path);
    result.map(|text| text.trim_end_matches(['\n', '\r']).to_string())
}

fn temp_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("rightkit-prompt-{}-{nanos}.md", std::process::id()))
}

fn run_editor(path: &Path) -> io::Result<()> {
    super::session::restore()?;
    let status = rightkit_child_process::status_inherited_terminal(&mut editor_process(
        &editor_command(),
        path,
    ));
    // Re-enter the TUI modes even when the editor failed to start.
    let reenter = super::session::set_modes();
    let status = status?;
    reenter?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("editor exited with {status}")))
    }
}

/// Run through the platform shell so `code --wait`, `.cmd` shims and quoted
/// paths in `$EDITOR` behave as they do in the user's shell.
fn editor_process(editor: &str, path: &Path) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // windowless: deliberately NOT CREATE_NO_WINDOW: an interactive editor must inherit the
        // TUI's console, so no new window is created.
        #[allow(clippy::disallowed_methods)]
        let mut command = Command::new("cmd");
        command.raw_arg(format!("/C \"{editor} \"{}\"\"", path.display()));
        command
    }
    #[cfg(not(windows))]
    {
        #[allow(clippy::disallowed_methods)]
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("rightkit-editor")
            .arg(path);
        command
    }
}
