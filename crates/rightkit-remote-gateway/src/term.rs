//! `term.exec`, `term.write`, `term.read` and `term.stop`: a program started on this computer by
//! a remote caller, tier 2. They need `remoteTools` and `remoteTerminals` (both off by default)
//! and a local grant (`approvals`): one approval per new session and door, or the standing policy
//! `remoteTermStanding` the user turned on. Every call re-checks both, so turning the switch off
//! or letting the grant lapse ends access at once.
//!
//! **What an approval means.** A terminal is real execution authority. The command runs as the
//! user, with the user's files and network, and the starting folder is no sandbox: nothing here
//! can see or stop what a program does once it runs. the host app cannot intercept its consequences, and
//! the approval says so in plain words. The guards are around the session, not inside it:
//!
//! * a session is a program started by `term.exec` through the system shell, owned by the host app as a
//!   process tree (`OwnedCommand::spawn`, RightKit's process owner): `term.stop`, the idle expiry
//!   and the host app quitting end the tree;
//! * the environment is scrubbed: only an allowlist of names (`PATH`, `HOME`, locale, temp and
//!   Windows system variables) is passed, so tokens and keys in the host app's own environment are not
//!   handed to the program; `TERM` is `dumb`;
//! * a session that has not been called for 30 minutes (`IDLE`) is stopped, and every session ends
//!   when `remoteTerminals` or `remoteTools` goes off (a reaper thread checks every few seconds);
//! * at most `MAX_SESSIONS` sessions exist, each keeps at most `BUFFER_CAP` bytes of output (older
//!   output is dropped and `dropped` says so), and a session answers only the door that started
//!   it;
//! * output that goes to the ChatGPT gateway passes the secret redactor (`redactions`).
//!
//! By default there is no terminal emulator: standard output and error are one stream, standard
//! input is a pipe, a program that wants a terminal (an editor, a pager) will not work. With
//! `tty: true` (Zenon's `exec_command` `tty`, tools-core.ts) `term.exec` starts the program on a
//! pseudo-terminal instead (Unix pty, Windows ConPTY, through `portable-pty`, the transport
//! RightKit's `rightkit-sandbox` PTY uses): one stream of the terminal's bytes, `TERM` is
//! `xterm-256color`, a size of `rows` (5 to 200) and `cols` (20 to 500) that `term.resize` changes
//! (Zenon's `terminal` resize, tools-laptop.ts:83), and `term.list` shows the sessions of this
//! door (Zenon's `terminal` list). Escape sequences are removed from the text returned. A PTY
//! session has the same switch, grant, scrubbed environment, session limit, idle expiry and output
//! cap as a pipe session; stopping it ends its process tree (`sweep_descendants`, the session's
//! groups on Unix, then the leader). On Windows the command is typed into an interactive
//! `cmd.exe` (so a PTY session there stays open until stopped or idle). Sessions live in the
//! process that started them (the engine, or `the gateway`); a restart ends them.

use super::redact;
use super::settings::Settings;
use super::{Origin, Refusal, Result, approvals};
use rightkit_child_process::{AdoptedIdentity, JobAssignmentMode, OwnedChild, OwnedCommand};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::process::{ChildStdin, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// A session idle this long is stopped.
pub const IDLE: Duration = approvals::SESSION_IDLE;
const MAX_SESSIONS: usize = 8;
const BUFFER_CAP: usize = 1024 * 1024;
const COMMAND_MAX: usize = 8 * 1024;
const WRITE_MAX: usize = 16 * 1024;
const DEFAULT_WAIT_MS: u64 = 10_000;
const MAX_WAIT_MS: u64 = 30_000;
const DEFAULT_OUTPUT: u64 = 64 * 1024;
const MAX_OUTPUT: u64 = 512 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);
const REAP_EVERY: Duration = Duration::from_secs(10);
pub const FAMILY: &str = "terminal";

/// Environment names a program started here receives beyond RightKit's defaults
/// (`OwnedCommand::env_allowlist_with_defaults`). The `LC_*` locale variables are added by prefix.
const ENV_EXTRA: &[&str] = &[
    "USERNAME",
    "LANGUAGE",
    "SHELL",
    "HOMEDRIVE",
    "HOMEPATH",
    "SYSTEMDRIVE",
    "WINDIR",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "OS",
];

/// Gives `command` only the allowlisted environment: the inherited one is dropped, then the
/// allowlist, the `LC_*` variables and `TERM=dumb` are set.
pub(super) fn scrubbed(command: &mut OwnedCommand) {
    command.env_allowlist_with_defaults(ENV_EXTRA);
    for (key, value) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("LC_") {
            command.command_mut().env(&key, &value);
        }
    }
    command.command_mut().env("TERM", "dumb");
}

/// Output of both streams, newest `BUFFER_CAP` bytes, addressed by absolute byte position.
#[derive(Default)]
struct Buffer {
    data: Vec<u8>,
    /// Absolute position of `data[0]`.
    base: u64,
    /// When output last arrived (ready-for-input detection).
    last: Option<Instant>,
}

impl Buffer {
    fn push(&mut self, bytes: &[u8]) {
        self.last = Some(Instant::now());
        self.data.extend_from_slice(bytes);
        if self.data.len() > BUFFER_CAP {
            let drop = self.data.len() - BUFFER_CAP;
            self.data.drain(..drop);
            self.base += drop as u64;
        }
    }

    fn end(&self) -> u64 {
        self.base + self.data.len() as u64
    }
}

struct State {
    child: Option<OwnedChild>,
    /// `Some(code)` once the program ended; the inner value is its exit code when it has one.
    exit: Option<Option<i32>>,
    signal: Option<i32>,
    last_used: Instant,
    /// Where `term.read` continues when no cursor is given.
    read_pos: u64,
}

/// The pseudo-terminal side of a `tty` session.
struct Pty {
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    /// The leader's identity, to stop its descendants only while they are still the same processes.
    identity: Option<AdoptedIdentity>,
    /// Rows and columns now.
    size: Mutex<(u16, u16)>,
}

struct Session {
    id: String,
    origin: Origin,
    pid: u32,
    buffer: Arc<Mutex<Buffer>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// `Some` for a session started with `tty: true`; `stdin` and `State::child` are unused then.
    pty: Option<Pty>,
    state: Mutex<State>,
}

/// Quiet this long, with a prompt at the end of the output, means the program waits for input.
const QUIET: Duration = Duration::from_millis(400);

/// True when the program is running, has printed something, has been quiet for `QUIET` and its
/// last line has no line break and ends like a prompt (`$ `, `# `, `> `, `% `, `: `, `? `).
/// A heuristic: a program that prints such a line and then works is reported as waiting.
fn waiting(session: &Session) -> bool {
    if lock(&session.state).exit.is_some() {
        return false;
    }
    let tail = {
        let buffer = lock(&session.buffer);
        match buffer.last {
            Some(at) if at.elapsed() >= QUIET => {}
            _ => return false,
        }
        let from = buffer.data.len().saturating_sub(256);
        String::from_utf8_lossy(&buffer.data[from..]).into_owned()
    };
    let tail = strip_ansi(&tail);
    if tail.is_empty() || tail.ends_with('\n') || tail.ends_with('\r') {
        return false;
    }
    ["$ ", "# ", "> ", "% ", ": ", "? ", ">>> ", "... "]
        .iter()
        .any(|p| tail.ends_with(p))
}

/// The text without terminal escape sequences (CSI, OSC and two-character escapes).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                let mut escaped = false;
                for n in chars.by_ref() {
                    if escaped || n == '\u{7}' {
                        break;
                    }
                    escaped = n == '\u{1b}';
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn registry() -> &'static Mutex<Vec<Arc<Session>>> {
    static SESSIONS: OnceLock<Mutex<Vec<Arc<Session>>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(Vec::new()))
}

/// The switch and the local grant, checked on every call. A refusal that asks for approval has
/// the code `approval_required`.
pub(super) fn gate(settings: &Settings, origin: Origin) -> Result<()> {
    settings.require_terminals()?;
    if settings.term_standing {
        return Ok(());
    }
    if approvals::session_active_held(origin, FAMILY)? {
        return Ok(());
    }
    Err(approvals::request_session_held(
        origin,
        FAMILY,
        &format!(
            "allow {} terminal access: run any command as you, with your files and network, no sandbox, until idle for 30 minutes",
            origin.as_str()
        ),
    ))
}

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

fn whole(args: &Value, key: &str, min: u64, max: u64, default: u64) -> Result<u64> {
    match &args[key] {
        Value::Null => Ok(default),
        value => match value.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(n),
            _ => Err(invalid(format!(
                "{key} is a whole number from {min} to {max}"
            ))),
        },
    }
}

fn start_reaper() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("rightkit-term-reaper".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(REAP_EVERY);
                    reap();
                }
            });
    });
}

/// Stops sessions that are idle, or all of them when the switches went off.
fn reap() {
    let settings = Settings::load();
    let on = settings.enabled && settings.terminals;
    let doomed: Vec<Arc<Session>> = {
        let mut all = lock(registry());
        let mut gone = Vec::new();
        all.retain(|session| {
            let idle = lock(&session.state).last_used.elapsed() >= IDLE;
            let off = !on || (session.origin == Origin::Gateway && !settings.gateway);
            if idle || off {
                gone.push(session.clone());
                false
            } else {
                true
            }
        });
        gone
    };
    for session in doomed {
        let _ = finish(&session);
    }
    if !on {
        let _ = approvals::revoke_sessions_held(FAMILY);
    }
}

/// Ends a session: its process tree is stopped (bounded) and its pipes closed.
fn finish(session: &Session) -> bool {
    lock(&session.stdin).take();
    if let Some(pty) = &session.pty {
        return stop_pty(session.pid, pty);
    }
    let child = lock(&session.state).child.take();
    match child {
        // The owner's bounded tree stop; a child not seen to exit is leaked, never dropped (dropping an
        // owned child waits for it without a bound).
        Some(mut child) => match child.terminate_tree_bounded(Duration::from_secs(5)) {
            Ok(rightkit_child_process::ReapOutcome::Exited(_)) => true,
            Ok(rightkit_child_process::ReapOutcome::TimedOut) | Err(_) => {
                std::mem::forget(child);
                false
            }
        },
        None => true,
    }
}

fn shell(command_text: &str) -> OwnedCommand {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let comspec = std::env::var_os("COMSPEC")
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "cmd.exe".into());
        let mut command = OwnedCommand::new(comspec);
        command
            .command_mut()
            .arg("/D")
            .arg("/S")
            .arg("/C")
            .raw_arg(format!("\"{command_text}\""));
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = OwnedCommand::new("/bin/sh");
        command.command_mut().arg("-c").arg(command_text);
        command
    }
}

/// The scrubbed environment as name and value pairs, for a program started on a pseudo-terminal.
fn scrubbed_pairs() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut probe = OwnedCommand::new("rightkit-term-env");
    scrubbed(&mut probe);
    probe
        .command_mut()
        .get_envs()
        .filter_map(|(k, v)| v.map(|v| (k.to_os_string(), v.to_os_string())))
        .collect()
}

/// Starts `text` on a new pseudo-terminal of `rows` by `cols` in `dir`; its output goes to `buffer`.
fn open_pty(
    text: &str,
    dir: &std::path::Path,
    rows: u16,
    cols: u16,
    buffer: &Arc<Mutex<Buffer>>,
) -> Result<(u32, Pty)> {
    let failed = |why: String| Refusal::new("spawn_failed", format!("could not start it: {why}"));
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| failed(e.to_string()))?;
    #[cfg(windows)]
    let (program, args, typed) = {
        let comspec = std::env::var("COMSPEC")
            .ok()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "cmd.exe".to_string());
        (comspec, vec!["/D".to_string()], Some(format!("{text}\r\n")))
    };
    #[cfg(not(windows))]
    let (program, args, typed) = (
        "/bin/sh".to_string(),
        vec!["-c".to_string(), text.to_string()],
        None::<String>,
    );
    let mut builder = portable_pty::CommandBuilder::new(program);
    builder.args(args);
    builder.cwd(dir);
    builder.env_clear();
    for (key, value) in scrubbed_pairs() {
        builder.env(key, value);
    }
    builder.env("TERM", "xterm-256color");
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| failed(e.to_string()))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| failed(e.to_string()))?;
    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|e| failed(e.to_string()))?;
    // The slave end stays closed here, so the reader sees the end when the program exits.
    drop(pair.slave);
    let pid = child.process_id().unwrap_or(0);
    if let Some(line) = typed {
        let _ = writer.write_all(line.as_bytes());
        let _ = writer.flush();
    }
    pump(reader, buffer.clone());
    let identity = NonZeroU32::new(pid)
        .and_then(|pid| AdoptedIdentity::capture(pid).ok())
        .flatten();
    Ok((
        pid,
        Pty {
            master: Mutex::new(pair.master),
            writer: Arc::new(Mutex::new(Some(writer))),
            child: Mutex::new(child),
            identity,
            size: Mutex::new((rows, cols)),
        },
    ))
}

/// Stops a pseudo-terminal session's whole process tree (bounded). True when the leader was seen
/// to end.
fn stop_pty(pid: u32, pty: &Pty) -> bool {
    lock(&pty.writer).take();
    if let Some(identity) = &pty.identity {
        let _ = rightkit_child_process::sweep_descendants(identity, Duration::from_millis(500));
    }
    #[cfg(unix)]
    if let Some(leader) = NonZeroU32::new(pid) {
        // The session leader's session holds every group the program started.
        let _ = rightkit_child_process::signal_session(
            leader,
            rightkit_child_process::AdoptedSignal::Force,
        );
    }
    #[cfg(not(unix))]
    let _ = pid;
    let mut child = lock(&pty.child);
    let _ = child.kill();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < until => std::thread::sleep(POLL),
            _ => return false,
        }
    }
}

/// The user's command policy (`remoteTermAllow`, `remoteTermDeny`; both empty by default, which
/// changes nothing). Applied to every command `term.exec` starts, before anything runs. It reads
/// the program word of each part of the line (split at `; | & ( ) { }`, line breaks, `` ` `` and
/// `$(`): a deny entry refuses a part whose program (path stripped, case ignored) or leading words
/// equal it; with an allow list, every part must match an entry and a line with redirections or
/// substitutions is refused. It is a user control on what ChatGPT may start, not an operating
/// system sandbox: a permitted program can still do what that program does.
fn check_policy(settings: &Settings, command: &str) -> Result<()> {
    if settings.term_allow.is_empty() && settings.term_deny.is_empty() {
        return Ok(());
    }
    let entry_matches = |entry: &str, part: &str| -> bool {
        let entry = entry.trim().to_lowercase();
        if entry.is_empty() {
            return false;
        }
        let words: Vec<&str> = part.split_whitespace().collect();
        let Some(first) = words.first() else {
            return false;
        };
        let program = first
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(first)
            .to_lowercase();
        let program = program.strip_suffix(".exe").unwrap_or(&program).to_string();
        if !entry.contains(' ') {
            return program == entry.strip_suffix(".exe").unwrap_or(&entry);
        }
        let lowered = format!("{program} {}", words[1..].join(" ")).to_lowercase();
        lowered == entry || lowered.starts_with(&format!("{entry} "))
    };
    let parts: Vec<&str> = command
        .split([';', '|', '&', '(', ')', '{', '}', '\n', '\r', '`'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    for part in &parts {
        if settings
            .term_deny
            .iter()
            .any(|entry| entry_matches(entry, part))
        {
            return Err(Refusal::new(
                "command_denied",
                "that command is on the user's deny list in the app's settings (remoteTermDeny); nothing was run",
            ));
        }
    }
    if !settings.term_allow.is_empty() {
        let plain = !command.contains(['>', '<', '`', '$']);
        let all_allowed = parts.iter().all(|part| {
            settings
                .term_allow
                .iter()
                .any(|entry| entry_matches(entry, part))
        });
        if !plain || !all_allowed || parts.is_empty() {
            return Err(Refusal::new(
                "command_not_allowed",
                "that command is not on the user's allow list in the app's settings (remoteTermAllow; every program in the line must be listed, and redirections, `$` and backticks are not accepted); nothing was run",
            ));
        }
    }
    Ok(())
}

fn pump(mut stream: impl Read + Send + 'static, buffer: Arc<Mutex<Buffer>>) {
    let _ = std::thread::Builder::new()
        .name("rightkit-term-pump".into())
        .spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => lock(&buffer).push(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
}

/// Records the program's end when it has ended. Returns whether it has.
fn poll_exit(session: &Session) -> bool {
    let mut state = lock(&session.state);
    if state.exit.is_some() {
        return true;
    }
    if let Some(pty) = &session.pty {
        return match lock(&pty.child).try_wait() {
            Ok(Some(status)) => {
                state.exit = Some(i32::try_from(status.exit_code()).ok());
                true
            }
            Ok(None) | Err(_) => false,
        };
    }
    let Some(child) = state.child.as_mut() else {
        state.exit = Some(None);
        return true;
    };
    match child.try_wait() {
        Ok(Some(status)) => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                state.signal = status.signal();
            }
            state.exit = Some(status.code());
            true
        }
        Ok(None) => false,
        // A wait that fails is not an exit: the session stays until stopped or idle.
        Err(_) => false,
    }
}

/// Most commands one `cmds` batch holds.
const BATCH_MAX: usize = 20;

/// `term.exec`. `args`: `command` (run by the system shell) or `cmds` (1 to 20 commands run one
/// after the other, each in its own session; it stops at the first that is still running or exits
/// with an error, and every command shares the one `waitMs` and `maxBytes` budget), `cwd` (an
/// existing absolute folder; default the user's home), `waitMs` (how long to wait for it to end,
/// default 10000, at most 30000; it also ends when the program waits for input), `maxBytes`
/// (output returned, default 65536, at most 524288), `tty` (a pseudo-terminal, with `rows` 5 to 200
/// and `cols` 20 to 500; not with `cmds`). A program still running after `waitMs` stays as a
/// session: the answer carries its `session`. The user's command policy (`remoteTermAllow`,
/// `remoteTermDeny`) is checked on every command before any of them runs.
pub fn exec(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    gate(settings, origin)?;
    let commands: Vec<&str> = match (&args["command"], &args["cmds"]) {
        (Value::String(text), Value::Null) => vec![text.as_str()],
        (Value::Null, Value::Array(list)) if (1..=BATCH_MAX).contains(&list.len()) => list
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| invalid("cmds holds command strings"))
            })
            .collect::<Result<Vec<_>>>()?,
        _ => {
            return Err(invalid(format!(
                "give exactly one of command, or cmds (1 to {BATCH_MAX} commands)"
            )));
        }
    };
    for text in &commands {
        if text.trim().is_empty() || text.len() > COMMAND_MAX || text.contains('\0') {
            return Err(invalid(format!(
                "a command is 1 to {COMMAND_MAX} bytes and has no NUL"
            )));
        }
        check_policy(settings, text)?;
    }
    let tty = match &args["tty"] {
        Value::Null => false,
        Value::Bool(b) => *b,
        _ => return Err(invalid("tty is true or false")),
    };
    if !tty && (!args["rows"].is_null() || !args["cols"].is_null()) {
        return Err(invalid("rows and cols need tty: true"));
    }
    if tty && commands.len() > 1 {
        return Err(invalid("cmds does not run with tty"));
    }
    let size = if tty {
        Some((
            whole(args, "rows", 5, 200, 24)? as u16,
            whole(args, "cols", 20, 500, 80)? as u16,
        ))
    } else {
        None
    };
    let wait_ms = whole(args, "waitMs", 0, MAX_WAIT_MS, DEFAULT_WAIT_MS)?;
    let max_bytes = whole(args, "maxBytes", 1, MAX_OUTPUT, DEFAULT_OUTPUT)? as usize;
    let cwd = match &args["cwd"] {
        Value::Null => None,
        Value::String(dir) => {
            let path = std::path::Path::new(dir);
            if !path.is_absolute() || !path.is_dir() {
                return Err(invalid(
                    "cwd must be an existing folder, as an absolute path",
                ));
            }
            Some(path.to_path_buf())
        }
        _ => return Err(invalid("cwd must be a string")),
    };
    start_reaper();
    let wait = Duration::from_millis(wait_ms);
    if commands.len() == 1 {
        return run_one(commands[0], cwd.as_deref(), size, wait, max_bytes, origin);
    }
    // A batch: one wait budget and one output budget shared by all of it. Each command runs only
    // after the one before ended with exit code 0 (like `&&`); the first that is still running,
    // or ended with an error, stops the batch and is the last result.
    let started = Instant::now();
    let per_command = (max_bytes / commands.len()).max(4096).min(max_bytes);
    let mut results: Vec<Value> = Vec::new();
    let mut returned = 0u64;
    let mut completed = 0usize;
    for text in &commands {
        let left = wait.saturating_sub(started.elapsed());
        let answer = match run_one(text, cwd.as_deref(), None, left, per_command, origin) {
            Ok(answer) => answer,
            Err(refusal) if results.is_empty() => return Err(refusal),
            Err(refusal) => {
                results.push(json!({"ok": false, "code": refusal.code, "error": refusal.message}));
                break;
            }
        };
        returned += answer["returned"].as_u64().unwrap_or(0);
        let clean = answer["running"] == json!(false) && answer["exitCode"] == json!(0);
        results.push(answer);
        if !clean {
            break;
        }
        completed += 1;
    }
    Ok(json!({
        "ok": true,
        "batch": true,
        "results": results,
        "completed": completed,
        "requested": commands.len(),
        "returned": returned,
        "bytes": returned,
    }))
}

/// Makes room for one more session: sessions that ended and were read in full are dropped when
/// `MAX_SESSIONS` are open; `busy` when none can be.
fn make_room() -> Result<()> {
    {
        let mut all = lock(registry());
        if all.len() >= MAX_SESSIONS {
            // Sessions that ended and were read in full are dropped to make room.
            let mut finished_sessions = Vec::new();
            all.retain(|session| {
                // One lock at a time: `output` takes the buffer, then the state.
                let (has_ended, read) = {
                    let state = lock(&session.state);
                    (state.exit.is_some(), state.read_pos)
                };
                let over = has_ended && read >= lock(&session.buffer).end();
                if over {
                    finished_sessions.push(session.clone());
                }
                !over
            });
            drop(all);
            for session in finished_sessions {
                finish(&session);
            }
            if lock(registry()).len() >= MAX_SESSIONS {
                return Err(Refusal::new(
                    "busy",
                    format!("{MAX_SESSIONS} terminal sessions are open; stop one with term.stop"),
                ));
            }
        }
    }
    Ok(())
}

/// Starts one command (a pipe session, or a pseudo-terminal when `size` is given) and waits up to
/// `wait` for it to end or to ask for input.
fn run_one(
    text: &str,
    cwd: Option<&std::path::Path>,
    size: Option<(u16, u16)>,
    wait: Duration,
    max_bytes: usize,
    origin: Origin,
) -> Result<Value> {
    make_room()?;
    let dir = cwd
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(rightkit_app_host::account_home);
    let session_id = || {
        approvals::bind_of(&[
            &std::process::id().to_be_bytes()[..],
            text.as_bytes(),
            &now_nanos()[..],
        ])[..16]
            .to_string()
    };
    if let Some((rows, cols)) = size {
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        let (pid, pty) = open_pty(text, &dir, rows, cols, &buffer)?;
        let session = Arc::new(Session {
            id: session_id(),
            origin,
            pid,
            buffer,
            stdin: Arc::new(Mutex::new(None)),
            pty: Some(pty),
            state: Mutex::new(State {
                child: None,
                exit: None,
                signal: None,
                last_used: Instant::now(),
                read_pos: 0,
            }),
        });
        return Ok(settle(session, text, origin, wait, max_bytes));
    }
    let mut command = shell(text);
    scrubbed(&mut command);
    command.command_mut().current_dir(&dir);
    command
        .command_mut()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.job_assignment_mode(JobAssignmentMode::BestEffort);
    let mut child = command
        .spawn()
        .map_err(|e| Refusal::new("spawn_failed", format!("could not start it: {}", e.kind())))?;
    let buffer = Arc::new(Mutex::new(Buffer::default()));
    if let Some(out) = child.take_stdout() {
        pump(out, buffer.clone());
    }
    if let Some(err) = child.take_stderr() {
        pump(err, buffer.clone());
    }
    let session = Arc::new(Session {
        id: session_id(),
        origin,
        pid: child.id(),
        buffer,
        stdin: Arc::new(Mutex::new(child.take_stdin())),
        pty: None,
        state: Mutex::new(State {
            child: Some(child),
            exit: None,
            signal: None,
            last_used: Instant::now(),
            read_pos: 0,
        }),
    });
    Ok(settle(session, text, origin, wait, max_bytes))
}

/// Registers the session, waits up to `wait` for the program to end or to ask for input, and
/// answers with its output so far.
fn settle(
    session: Arc<Session>,
    text: &str,
    origin: Origin,
    wait: Duration,
    max_bytes: usize,
) -> Value {
    lock(registry()).push(session.clone());
    let deadline = Instant::now() + wait;
    while !poll_exit(&session) && Instant::now() < deadline && !waiting(&session) {
        std::thread::sleep(POLL);
    }
    // A program that just ended may still be flushing its last output through the pipes.
    if poll_exit(&session) {
        std::thread::sleep(Duration::from_millis(60));
    }
    let mut answer = output(&session, origin, None, max_bytes);
    answer["command"] = json!({"chars": text.chars().count()});
    answer["pid"] = json!(session.pid);
    answer
}

fn now_nanos() -> [u8; 16] {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .to_be_bytes()
}

/// The next output of a session from `cursor` (default: where the last read stopped), as text.
fn output(session: &Session, origin: Origin, cursor: Option<u64>, max_bytes: usize) -> Value {
    let finished = poll_exit(session);
    let (bytes, from, next, dropped) = {
        let buffer = lock(&session.buffer);
        let mut state = lock(&session.state);
        state.last_used = Instant::now();
        let want = cursor.unwrap_or(state.read_pos);
        let from = want.max(buffer.base);
        let dropped = want < buffer.base;
        let start = (from - buffer.base) as usize;
        let mut end = (start + max_bytes).min(buffer.data.len());
        // Do not end inside a UTF-8 character; the next read starts there.
        if end < buffer.data.len()
            && let Err(e) = std::str::from_utf8(&buffer.data[start..end])
            && e.error_len().is_none()
            && e.valid_up_to() > 0
        {
            end = start + e.valid_up_to();
        }
        let bytes = buffer.data[start..end].to_vec();
        let next = from + bytes.len() as u64;
        state.read_pos = next;
        (bytes, from, next, dropped)
    };
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if session.pty.is_some() {
        text = strip_ansi(&text);
    }
    let mut redactions = 0;
    if origin == Origin::Gateway {
        (text, redactions) = redact::redact(&text);
    }
    let (exit_code, signal) = {
        let state = lock(&session.state);
        (state.exit.flatten(), state.signal)
    };
    let more = next < lock(&session.buffer).end();
    json!({
        "ok": true,
        "session": session.id,
        "running": !finished,
        "waitingForInput": waiting(session),
        "tty": session.pty.is_some(),
        "exitCode": exit_code,
        "signal": signal,
        "output": text,
        "cursor": from,
        "nextCursor": next,
        "more": more,
        "dropped": dropped,
        "redactions": if origin == Origin::Gateway { json!(redactions) } else { Value::Null },
        "bytes": bytes.len(),
        "returned": bytes.len(),
    })
}

/// The session `id` started through this door, or `not_found`.
fn find(origin: Origin, args: &Value) -> Result<Arc<Session>> {
    let id = args["session"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .ok_or_else(|| invalid("session is needed"))?;
    lock(registry())
        .iter()
        .find(|s| s.id == id && s.origin == origin)
        .cloned()
        .ok_or_else(|| {
            Refusal::new(
                "not_found",
                "no such terminal session here (it ended, was stopped, or sat idle for 30 minutes)",
            )
        })
}

/// `term.read`. `args`: `session`, `cursor` (a position from an earlier answer; default: where the
/// last read stopped), `waitMs` (wait up to this long for new output or the end; default 0, at
/// most 30000), `maxBytes`.
pub fn read(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    gate(settings, origin)?;
    let session = find(origin, args)?;
    let wait_ms = whole(args, "waitMs", 0, MAX_WAIT_MS, 0)?;
    let max_bytes = whole(args, "maxBytes", 1, MAX_OUTPUT, DEFAULT_OUTPUT)? as usize;
    let cursor = match &args["cursor"] {
        Value::Null => None,
        value => Some(
            value
                .as_u64()
                .ok_or_else(|| invalid("cursor is a whole number"))?,
        ),
    };
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        let have = {
            let want = cursor.unwrap_or(lock(&session.state).read_pos);
            lock(&session.buffer).end() > want
        };
        if have || poll_exit(&session) || Instant::now() >= deadline || waiting(&session) {
            break;
        }
        std::thread::sleep(POLL);
    }
    Ok(output(&session, origin, cursor, max_bytes))
}

/// `term.write`. `args`: `session`, `input` (text, at most 16 KiB), `closeInput` (true closes the
/// program's standard input after the write).
pub fn write(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    gate(settings, origin)?;
    let session = find(origin, args)?;
    let close = match &args["closeInput"] {
        Value::Null => false,
        Value::Bool(b) => *b,
        _ => return Err(invalid("closeInput is true or false")),
    };
    let input = match &args["input"] {
        Value::Null if close => String::new(),
        Value::String(text) if text.len() <= WRITE_MAX && !text.contains('\0') => text.clone(),
        _ => {
            return Err(invalid(format!(
                "input is text of at most {WRITE_MAX} bytes without NUL"
            )));
        }
    };
    lock(&session.state).last_used = Instant::now();
    if poll_exit(&session) {
        return Err(Refusal::new(
            "session_ended",
            "the program has already ended",
        ));
    }
    // The write runs on its own thread: a program that is not reading input can fill the pipe and
    // block it, and the call must still answer. While one write is stuck another is `busy`.
    let bytes = input.into_bytes();
    let length = bytes.len();
    let receiver = match &session.pty {
        Some(pty) => spawn_write(pty.writer.clone(), bytes, close)?,
        None => spawn_write(session.stdin.clone(), bytes, close)?,
    };
    match receiver.recv_timeout(WRITE_TIMEOUT) {
        Ok(Ok(())) => Ok(json!({
            "ok": true,
            "session": session.id,
            "written": length,
            "inputClosed": close,
            "bytes": length,
        })),
        Ok(Err(why)) if why == "busy" => Err(Refusal::new(
            "busy",
            "an earlier write to this program has not finished",
        )),
        Ok(Err(why)) if why == "closed" => Err(Refusal::new(
            "session_ended",
            "the program's input was closed",
        )),
        Ok(Err(why)) => Err(Refusal::new(
            "write_failed",
            format!("the program did not take the input: {why}"),
        )),
        Err(RecvTimeoutError::Timeout) => Err(Refusal::new(
            "timed_out",
            "the program is not reading its input; the write may still finish",
        )),
        Err(RecvTimeoutError::Disconnected) => Err(Refusal::new(
            "internal_error",
            "the write failed inside the app",
        )),
    }
}

/// Writes `bytes` to a program's input on its own thread and returns where the outcome arrives.
fn spawn_write<W: Write + Send + 'static>(
    target: Arc<Mutex<Option<W>>>,
    bytes: Vec<u8>,
    close: bool,
) -> Result<mpsc::Receiver<std::result::Result<(), String>>> {
    let (sender, receiver) = mpsc::channel();
    let started = std::thread::Builder::new()
        .name("rightkit-term-write".into())
        .spawn(move || {
            let outcome = match target.try_lock() {
                Ok(mut guard) => match guard.as_mut() {
                    Some(pipe) => {
                        let wrote = pipe.write_all(&bytes).and_then(|()| pipe.flush());
                        if close {
                            guard.take();
                        }
                        wrote.map_err(|e| e.kind().to_string())
                    }
                    None => Err("closed".to_string()),
                },
                Err(_) => Err("busy".to_string()),
            };
            let _ = sender.send(outcome);
        });
    if started.is_err() {
        return Err(Refusal::new("internal_error", "the write could not start"));
    }
    Ok(receiver)
}

/// `term.list`: this door's sessions, newest last: id, pid, whether it is a terminal and its size,
/// whether it still runs, `waitingForInput` (running, quiet, and the last line looks like a
/// prompt), seconds since it was last used and how many output bytes it has made. Never the
/// command text or the output.
pub fn list(settings: &Settings, origin: Origin, _args: &Value) -> Result<Value> {
    gate(settings, origin)?;
    let sessions: Vec<Arc<Session>> = lock(registry())
        .iter()
        .filter(|s| s.origin == origin)
        .cloned()
        .collect();
    let rows: Vec<Value> = sessions
        .iter()
        .map(|session| {
            let running = !poll_exit(session);
            let idle = lock(&session.state).last_used.elapsed().as_secs();
            let produced = lock(&session.buffer).end();
            let size = session.pty.as_ref().map(|pty| *lock(&pty.size));
            json!({
                "session": session.id,
                "pid": session.pid,
                "tty": session.pty.is_some(),
                "rows": size.map(|s| s.0),
                "cols": size.map(|s| s.1),
                "running": running,
                "waitingForInput": waiting(session),
                "idleSeconds": idle,
                "outputBytes": produced,
            })
        })
        .collect();
    let returned = rows.len();
    Ok(json!({"ok": true, "sessions": rows, "returned": returned}))
}

/// `term.resize`. `args`: `session` (a pseudo-terminal session), `rows` (5 to 200), `cols` (20 to
/// 500).
pub fn resize(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    gate(settings, origin)?;
    let session = find(origin, args)?;
    let Some(pty) = &session.pty else {
        return Err(invalid(
            "that session has no terminal; start it with tty: true",
        ));
    };
    if args["rows"].is_null() || args["cols"].is_null() {
        return Err(invalid("rows and cols are needed"));
    }
    let rows = whole(args, "rows", 5, 200, 24)? as u16;
    let cols = whole(args, "cols", 20, 500, 80)? as u16;
    lock(&session.state).last_used = Instant::now();
    lock(&pty.master)
        .resize(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| Refusal::new("write_failed", format!("could not resize it: {e}")))?;
    *lock(&pty.size) = (rows, cols);
    Ok(json!({"ok": true, "session": session.id, "rows": rows, "cols": cols}))
}

/// `term.stop`. `args`: `session`. Stops the program and everything it started (bounded),
/// removes the session and answers with what output was left unread, if `maxBytes` asks for it.
pub fn stop(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    gate(settings, origin)?;
    let session = find(origin, args)?;
    lock(registry()).retain(|s| !Arc::ptr_eq(s, &session));
    poll_exit(&session);
    let confirmed = finish(&session);
    let state = lock(&session.state);
    Ok(json!({
        "ok": true,
        "session": session.id,
        "stopped": true,
        "confirmed": confirmed,
        "exitCode": state.exit.flatten(),
    }))
}
