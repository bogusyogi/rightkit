//! Inline shell expansion for custom commands and prompts (A208).
//!
//! Claude Code semantics: a custom command body may embed shell commands that
//! run before the prompt is sent and whose output replaces the marker. Claude
//! Code spells it !`cmd` (https://code.claude.com/docs/en/slash-commands,
//! "Bash command execution"); the host app also accepts `!{cmd}` (pi inline-bash).
//!
//! This module only parses and splices. Running a command is delegated to a
//! caller-supplied [`ShellRunner`] that MUST route through the normal shell
//! permission policy; nothing here approves a command. A runner that cannot
//! obtain approval returns [`ShellOutcome::Denied`], and the marker is replaced
//! by a visible placeholder (the command is never run and never silently
//! dropped).

/// Result of asking the permission-gated runner to execute one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellOutcome {
    /// Command ran; stdout (already size-bounded by the runner).
    Output(String),
    /// Policy denied or approval was not granted; command did not run.
    Denied(String),
    /// Command was approved but failed to run or exited non-zero.
    Failed(String),
}

/// Executes a command under the normal shell permission policy.
pub trait ShellRunner {
    fn run(&mut self, command: &str) -> ShellOutcome;
}

impl<F: FnMut(&str) -> ShellOutcome> ShellRunner for F {
    fn run(&mut self, command: &str) -> ShellOutcome {
        self(command)
    }
}

/// One marker found in a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineShellMarker {
    pub start: usize,
    pub end: usize,
    pub command: String,
}

/// Find `!{cmd}` (brace-balanced) and backtick-form markers, left to right.
pub fn find_markers(template: &str) -> Vec<InlineShellMarker> {
    let bytes = template.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'!' && (bytes[i + 1] == b'{' || bytes[i + 1] == b'`') {
            let open = bytes[i + 1];
            let body = i + 2;
            let end = if open == b'`' {
                template[body..].find('`').map(|p| body + p)
            } else {
                let mut depth = 1usize;
                let mut found = None;
                for (off, b) in bytes[body..].iter().enumerate() {
                    match b {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                found = Some(body + off);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                found
            };
            if let Some(close) = end {
                let command = template[body..close].trim().to_string();
                if !command.is_empty() {
                    out.push(InlineShellMarker {
                        start: i,
                        end: close + 1,
                        command,
                    });
                    i = close + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// Expand every marker through `runner`. Output loses one trailing newline.
/// Denied/failed commands become bracketed placeholders.
pub fn expand(template: &str, runner: &mut dyn ShellRunner) -> String {
    let markers = find_markers(template);
    let mut out = String::with_capacity(template.len());
    let mut cursor = 0;
    for m in markers {
        out.push_str(&template[cursor..m.start]);
        match runner.run(&m.command) {
            ShellOutcome::Output(text) => {
                out.push_str(text.strip_suffix('\n').unwrap_or(&text));
            }
            ShellOutcome::Denied(reason) => {
                out.push_str(&format!("[command not run: {reason}: {}]", m.command));
            }
            ShellOutcome::Failed(reason) => {
                out.push_str(&format!("[command failed: {reason}: {}]", m.command));
            }
        }
        cursor = m.end;
    }
    out.push_str(&template[cursor..]);
    out
}
