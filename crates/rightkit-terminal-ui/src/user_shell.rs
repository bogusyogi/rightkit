//! `!cmd` / `!!cmd`: user-initiated shell commands typed into the composer.
//!
//! `!cmd` runs the command in the session directory, shows its output in the
//! transcript, and hands the result to the model as context on the next
//! prompt. `!!cmd` runs and shows the same way but is kept out of the model's
//! context (pi semantics). The host owns when to call [`run`] and where the
//! output is displayed; this module owns parsing, execution, output bounds
//! and the pending-context queue.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Hard stop for a user command; the host awaits it, so it must be bounded.
pub const TIMEOUT: Duration = Duration::from_secs(120);
/// Output kept per stream (tail wins: errors are usually last).
const MAX_STREAM_BYTES: usize = 48 * 1024;
/// Lines shown in the transcript; the model gets the same bounded text.
const MAX_DISPLAY_LINES: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserShell {
    pub command: String,
    /// `!!cmd`: shown, but never given to the model.
    pub exclude_from_context: bool,
}

/// Parse `!cmd` / `!!cmd`. `None` for plain prompts and empty commands.
pub fn parse(text: &str) -> Option<UserShell> {
    let rest = text.trim_start().strip_prefix('!')?;
    let (rest, exclude) = match rest.strip_prefix('!') {
        Some(rest) => (rest, true),
        None => (rest, false),
    };
    let command = rest.trim();
    if command.is_empty() {
        return None;
    }
    Some(UserShell {
        command: command.to_string(),
        exclude_from_context: exclude,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellRun {
    /// `None` when the command could not start or timed out.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Killed by the user (Esc / Ctrl+C).
    pub cancelled: bool,
    /// Combined stdout then stderr, bounded.
    pub output: String,
    pub truncated: bool,
}

impl ShellRun {
    /// Transcript text: `$ cmd`, bounded output, and an exit footer.
    pub fn display(&self, command: &str) -> String {
        let mut shown = format!("$ {command}\n");
        let lines: Vec<&str> = self.output.lines().collect();
        let skipped = lines.len().saturating_sub(MAX_DISPLAY_LINES);
        if skipped > 0 {
            shown.push_str(&format!("[{skipped} earlier lines hidden]\n"));
        }
        for line in &lines[skipped..] {
            shown.push_str(line);
            shown.push('\n');
        }
        if self.truncated {
            shown.push_str("[output truncated]\n");
        }
        shown.push_str(&self.status_line());
        shown
    }

    fn status_line(&self) -> String {
        if self.cancelled {
            "[cancelled]".to_string()
        } else if self.timed_out {
            format!("[timed out after {}s]", TIMEOUT.as_secs())
        } else {
            match self.exit_code {
                Some(0) => "[exit 0]".to_string(),
                Some(code) => format!("[exit {code}]"),
                None => "[did not run]".to_string(),
            }
        }
    }

    /// Model-facing block, framed like Codex's `<user_shell_command>`.
    pub fn context_block(&self, command: &str, cwd: &Path) -> String {
        let mut text = self.display(command);
        // `display` already carries the command and exit footer.
        text.insert_str(0, &format!("cwd: {}\n", cwd.display()));
        format!("<user_shell_command>\n{text}\n</user_shell_command>")
    }
}

/// Run `command` through the platform shell in `cwd`, blocking up to
/// [`TIMEOUT`]. Never panics; a start failure is reported in `output`.
pub fn run(command: &str, cwd: &Path) -> ShellRun {
    let job = spawn(command, cwd);
    loop {
        if let Some(run) = job.try_finish() {
            return run;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[derive(Default)]
struct Shared {
    /// Combined stdout+stderr in arrival order, bounded to the tail.
    output: Vec<u8>,
    truncated: bool,
    /// Bytes of `output` already handed out by `take_new_output`.
    delivered: usize,
}

/// A user command running off the UI thread. Output streams into a shared
/// buffer (poll with [`ShellJob::take_new_output`]); [`ShellJob::cancel`]
/// kills the process; [`ShellJob::try_finish`] yields the final run.
pub struct ShellJob {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    done: Mutex<std::sync::mpsc::Receiver<(Option<i32>, bool, bool)>>,
    finished: Mutex<Option<ShellRun>>,
}

/// Start `command` in the background. Never blocks; never panics.
pub fn spawn(command: &str, cwd: &Path) -> ShellJob {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, done) = std::sync::mpsc::channel();
    let mut process = shell_process(command);
    process
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // `OwnedCommand` hides the console on Windows and owns the whole child tree.
    match rightkit_child_process::OwnedCommand::from_command(process).spawn() {
        Err(error) => {
            push_output(
                &shared,
                format!("failed to start shell: {error}").as_bytes(),
            );
            let _ = tx.send((None, false, false));
        }
        Ok(mut child) => {
            let readers: Vec<Reader> = [
                child.take_stdout().map(|s| spawn_reader(s, shared.clone())),
                child.take_stderr().map(|s| spawn_reader(s, shared.clone())),
            ]
            .into_iter()
            .flatten()
            .collect();
            let cancel_flag = cancel.clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                let (mut timed_out, mut cancelled) = (false, false);
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if cancel_flag.load(Ordering::SeqCst) => {
                            cancelled = true;
                            let _ = child.terminate_tree();
                            break None;
                        }
                        Ok(None) if started.elapsed() >= TIMEOUT => {
                            timed_out = true;
                            let _ = child.terminate_tree();
                            break None;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(15)),
                        Err(_) => break None,
                    }
                };
                // A killed shell may leave grandchildren holding the pipes;
                // do not wait on readers after a kill.
                if !timed_out && !cancelled {
                    for reader in readers {
                        let _ = reader.join();
                    }
                }
                let _ = tx.send((status.and_then(|s| s.code()), timed_out, cancelled));
            });
        }
    }
    ShellJob {
        shared,
        cancel,
        done: Mutex::new(done),
        finished: Mutex::new(None),
    }
}

impl ShellJob {
    /// Request the process be killed (Esc / Ctrl+C). Idempotent.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Output that arrived since the last call.
    pub fn take_new_output(&self) -> String {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let start = shared.delivered.min(shared.output.len());
        let text = String::from_utf8_lossy(&shared.output[start..]).replace('\r', "");
        shared.delivered = shared.output.len();
        text
    }

    /// The final run once the process has ended, else `None`.
    pub fn try_finish(&self) -> Option<ShellRun> {
        let mut finished = self.finished.lock().unwrap_or_else(|e| e.into_inner());
        if finished.is_none() {
            let (exit_code, timed_out, cancelled) = self
                .done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .try_recv()
                .ok()?;
            let shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            *finished = Some(ShellRun {
                exit_code,
                timed_out,
                cancelled,
                output: String::from_utf8_lossy(&shared.output)
                    .replace('\r', "")
                    .trim_end()
                    .to_string(),
                truncated: shared.truncated,
            });
        }
        finished.clone()
    }
}

fn push_output(shared: &Mutex<Shared>, bytes: &[u8]) {
    let mut shared = shared.lock().unwrap_or_else(|e| e.into_inner());
    shared.output.extend_from_slice(bytes);
    if shared.output.len() > MAX_STREAM_BYTES {
        let excess = shared.output.len() - MAX_STREAM_BYTES;
        shared.output.drain(..excess);
        shared.delivered = shared.delivered.saturating_sub(excess);
        shared.truncated = true;
    }
}

type Active = (ShellJob, UserShell, std::path::PathBuf);

/// The one user command the composer is running, if any (`!cmd` is
/// asynchronous: the UI keeps polling this instead of blocking).
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

/// Register `job` as the active user command. Returns false if one is
/// already running.
pub fn set_active(job: ShellJob, shell: UserShell, cwd: std::path::PathBuf) -> bool {
    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    if active.is_some() {
        return false;
    }
    *active = Some((job, shell, cwd));
    true
}

pub fn is_active() -> bool {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Kill the active user command, if any. Returns whether one was running.
pub fn cancel_active() -> bool {
    match ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some((job, _, _)) => {
            job.cancel();
            true
        }
        None => false,
    }
}

/// A command that ended: (command, cwd, exclude_from_context, run).
pub type Finished = (String, std::path::PathBuf, bool, ShellRun);

/// Result of polling the active command from the UI tick.
pub struct ActivePoll {
    pub new_output: String,
    pub finished: Option<Finished>,
}

pub fn poll_active() -> Option<ActivePoll> {
    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    let (job, shell, cwd) = active.as_ref()?;
    let new_output = job.take_new_output();
    let finished = job.try_finish().map(|run| {
        (
            shell.command.clone(),
            cwd.clone(),
            shell.exclude_from_context,
            run,
        )
    });
    if finished.is_some() {
        *active = None;
    }
    Some(ActivePoll {
        new_output,
        finished,
    })
}

fn shell_process(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // windowless: output is piped and the caller hands this to `OwnedCommand`, which hides any new console window at spawn.
        #[allow(clippy::disallowed_methods)]
        let mut process = Command::new("cmd");
        process.raw_arg(format!("/C \"{command}\""));
        process
    }
    #[cfg(not(windows))]
    {
        #[allow(clippy::disallowed_methods)]
        let mut process = Command::new("sh");
        process.arg("-c").arg(command);
        process
    }
}

type Reader = std::thread::JoinHandle<()>;

fn spawn_reader(mut stream: impl Read + Send + 'static, shared: Arc<Mutex<Shared>>) -> Reader {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(read) = stream.read(&mut chunk) {
            if read == 0 {
                break;
            }
            push_output(&shared, &chunk[..read]);
        }
    })
}

static PENDING_CONTEXT: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Blocks queued ahead of the next prompt; older ones are dropped past this.
const MAX_PENDING_BLOCKS: usize = 8;

/// Queue a context block for the model's next prompt.
pub fn push_context(block: String) {
    let mut pending = PENDING_CONTEXT.lock().unwrap_or_else(|e| e.into_inner());
    pending.push(block);
    if pending.len() > MAX_PENDING_BLOCKS {
        pending.remove(0);
    }
}

/// Prefix `prompt` with every queued block (and clear the queue). Returns the
/// prompt unchanged when nothing is pending.
pub fn frame_with_pending_context(prompt: &str) -> String {
    let blocks = std::mem::take(&mut *PENDING_CONTEXT.lock().unwrap_or_else(|e| e.into_inner()));
    if blocks.is_empty() {
        return prompt.to_string();
    }
    format!("{}\n\n{prompt}", blocks.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(job: &ShellJob) -> ShellRun {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(run) = job.try_finish() {
                return run;
            }
            assert!(Instant::now() < end, "job never finished");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn spawn_does_not_block_and_streams_output() {
        let cwd = std::env::current_dir().unwrap();
        let started = Instant::now();
        let job = spawn("echo hello", &cwd);
        assert!(started.elapsed() < Duration::from_secs(2));
        let run = wait(&job);
        assert_eq!(run.exit_code, Some(0));
        assert!(run.output.contains("hello"));
        assert!(job.take_new_output().contains("hello"));
        assert!(job.take_new_output().is_empty(), "delivered once");
    }

    #[test]
    fn cancel_kills_a_long_command_promptly() {
        let cwd = std::env::current_dir().unwrap();
        let command = if cfg!(windows) {
            "ping -n 60 127.0.0.1"
        } else {
            "sleep 60"
        };
        let job = spawn(command, &cwd);
        std::thread::sleep(Duration::from_millis(200));
        assert!(job.try_finish().is_none(), "still running");
        job.cancel();
        let run = wait(&job);
        assert!(run.cancelled);
        assert_eq!(run.status_line(), "[cancelled]");
    }

    #[test]
    fn active_slot_polls_to_completion_and_clears() {
        let cwd = std::env::current_dir().unwrap();
        let shell = parse("!echo slot").unwrap();
        let job = spawn(&shell.command, &cwd);
        if !set_active(job, shell, cwd) {
            return; // another test owns the process-wide slot
        }
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let Some(poll) = poll_active() else { return };
            if let Some((cmd, _, exclude, run)) = poll.finished {
                assert_eq!(cmd, "echo slot");
                assert!(!exclude);
                assert!(run.output.contains("slot"));
                break;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!is_active());
    }

    #[test]
    fn parse_distinguishes_bang_and_double_bang() {
        assert!(parse("!ls").is_some_and(|s| !s.exclude_from_context));
        assert!(parse("!!ls").is_some_and(|s| s.exclude_from_context));
        assert!(parse("!").is_none());
    }
}
