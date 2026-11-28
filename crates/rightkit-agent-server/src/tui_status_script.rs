//! Scriptable status line (B-statusscript): `[tui] statusline-command = "<program> <args>"` names a
//! program the terminal runs in the background; it gets one JSON object on stdin (`model`, `cwd`,
//! `session_id`, `context_percent`, `cost_usd`, `permission_mode`) and the first line it prints is
//! shown as the footer item `script`.
//!
//! The command comes from the user's own config or a trusted workspace only (the loader drops it
//! from an untrusted workspace layer, like `[tui.keymap]`). It is split into words, never run
//! through a shell; it runs at most once every [`MIN_INTERVAL`], for at most [`TIMEOUT`], with no
//! credential variable in its environment, and its line is stripped of control characters and cut
//! to [`MAX_CHARS`]. A script that fails or prints nothing shows nothing.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MIN_INTERVAL: Duration = Duration::from_secs(5);
pub const TIMEOUT: Duration = Duration::from_secs(1);
pub const MAX_CHARS: usize = 80;
const MAX_STDOUT: usize = 8 * 1024;

#[derive(Default)]
pub struct StatusScript {
    line: Arc<Mutex<Option<String>>>,
    running: Arc<AtomicBool>,
    last_start: Option<Instant>,
}

impl StatusScript {
    /// The latest line the script printed, if any.
    pub fn line(&self) -> Option<String> {
        self.line.lock().ok().and_then(|line| line.clone())
    }

    /// Start a run when none is running and [`MIN_INTERVAL`] has passed since the last start (the
    /// first run starts at once). `payload` is the JSON written to the script's stdin.
    pub fn refresh(&mut self, command: &str, cwd: &Path, payload: &serde_json::Value) {
        let command = command.trim();
        if command.is_empty() || self.running.load(Ordering::SeqCst) {
            return;
        }
        if self
            .last_start
            .is_some_and(|at| at.elapsed() < MIN_INTERVAL)
        {
            return;
        }
        let words = split_command(command);
        if words.is_empty() {
            return;
        }
        self.last_start = Some(Instant::now());
        self.running.store(true, Ordering::SeqCst);
        let line = self.line.clone();
        let running = self.running.clone();
        let cwd = cwd.to_path_buf();
        let input = payload.to_string();
        std::thread::spawn(move || {
            let result = run_once(&words, &cwd, input.as_bytes());
            if let Ok(mut slot) = line.lock() {
                *slot = result;
            }
            running.store(false, Ordering::SeqCst);
        });
    }
}

fn run_once(words: &[String], cwd: &Path, input: &[u8]) -> Option<String> {
    let mut owned = rightkit_child_process::OwnedCommand::new(&words[0]);
    owned.command_mut().args(&words[1..]).current_dir(cwd);
    // No credential, daemon token or licence locator reaches the script (as for hooks).
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(rightkit_llm::agent_providers::host_credentials::is_credential_env_name)
        {
            owned.command_mut().env_remove(name);
        }
    }
    let options = rightkit_child_process::RunOptions::new(TIMEOUT)
        .stdout_cap(MAX_STDOUT)
        .stderr_cap(0);
    let output = rightkit_child_process::run_command(owned, Some(input), &options).ok()?;
    if output.timed_out || !output.status.success() {
        return None;
    }
    clean_line(&output.stdout.bytes)
}

/// The first non-empty line of `raw`, control characters removed, at most [`MAX_CHARS`] characters.
pub fn clean_line(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let first = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let cleaned: String = first
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CHARS)
        .collect();
    let cleaned = cleaned.trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Words of a command line: whitespace separates, single or double quotes group. No escapes, no
/// expansion, no shell.
pub fn split_command(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in command.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if started || !current.is_empty() {
        words.push(current);
    }
    words
}
