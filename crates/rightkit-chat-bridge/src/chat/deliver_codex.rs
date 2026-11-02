//! Push a Chat message into a Codex thread on this computer, as far as the
//! bundled Codex CLI allows. Three routes, chosen by what the host app's Codex hook last showed
//! for the thread (`codex_hook::hook_activity`) and by the thread's rollout file:
//! * Hook route, when the chat is busy: the hook ran for it within six hours, and either its
//!   rollout says a turn is running or its last hook run was less than ten minutes ago and
//!   was not the end of a run. The text is left in the host app's per-thread inbox and the hook
//!   hands it to the chat after its next tool call, prompt or run end. No command-line
//!   length limit applies. A message the hook claimed within ~3 s is `delivered`; otherwise
//!   `queued`. `codex queue` is not also called. A chat its owner stopped, or one that has
//!   not run a tool for ten minutes, gets the same inbox and a `held` receipt: it reads the
//!   message with its owner's next prompt or tool call.
//! * Wake route, when the chat is idle (its last Stop hook run ended the run, or its rollout
//!   shows the last turn finished): `codex exec resume --skip-git-repo-check <thread> -` is
//!   started detached with the message on stdin, so the idle chat starts a turn on it. The
//!   setting `codexWakeIdle` (default on, the owner 2026-10-22: "Wake by default") allows it, and only a chat positively idle whose
//!   owner is positively absent is woken: a Codex app that may hold the chat (owner unknown, or a
//!   window holds it) never gets a second, headless writer, the message goes to the hook inbox or
//!   `codex queue` instead (Astra F4); a chat its owner stopped is never woken;
//!   one wake per thread runs at a time. The receipt is `delivered` once the turn is seen
//!   starting. See `wake_idle`.
//! * Queue route: `codex queue`, described next. It takes the hook-less threads (the hook
//!   never ran for the thread, or not within six hours, or the inbox can't be written) and,
//!   by default, an idle chat that is not woken: wake off, no usable Codex, or the wake hit
//!   the writer-lock Conflict (the Codex app has the thread loaded). Codex's queue watcher
//!   starts the row once the thread is loaded and idle (about every 10 s), or when the thread
//!   is next opened or resumed. The receipt is `queued`; the row is not also put in the hook
//!   inbox, and a stopped chat is never queued.
//!
//! `codex queue --thread <id> --message <text>` ("Queue a message for an
//! existing session", in the Codex CLI bundled with the ChatGPT app) delivers
//! into a thread the Codex desktop app owns; the thread processes it and can
//! reply. It runs with a 5 s limit and no terminal or console window; stdout and
//! stderr are drained concurrently into 256 KiB buffers. Exit 0 with stdout
//! `"Queued message <id> for thread <id>."` is a `queued` receipt carrying that
//! message id (the thread may not have consumed it yet); exit 0 without that
//! line, or a timeout, is `unknown`; a nonzero exit is `refused` with Codex's
//! stderr. A Codex without `queue` (probed once per binary path and mtime with
//! `queue --help`, see `codex_capability`) or without a CLI is `unsupported`.
//!
//! When a queued message is read (proven against Codex 0.161.0 with a throwaway
//! thread, see docs/chat.md): Codex keeps queued messages as rows of `queued_items` in
//! `queue_1.sqlite`; whichever app-server has the thread loaded sends the next one when a
//! turn finishes normally, or when a thread whose last turn finished is loaded. A turn the
//! user stopped (`turn_aborted` in the rollout) leaves the queue untouched, however many
//! messages are waiting, until the next turn finishes normally. So after queuing, the
//! thread's last lifecycle event (`task_started`, `task_complete`, `turn_aborted`, read
//! from the tail of its rollout file) is checked: a stopped thread gives a `held` receipt
//! saying it will not read the message until its owner continues it (the caller also keeps
//! the message in the chat inbox); a running thread is `queued`; otherwise the message's
//! queue row is watched for a second, and a row that vanished is `delivered`. A row still
//! there on an idle thread is `queued` (Codex starts it when the chat is idle and loaded, or
//! next opened), as is one whose rollout could not be read ("could not tell whether the chat
//! is running"). No
//! supported way to start a turn inside the desktop app's own app-server was found (it is a
//! stdio child with no control socket); the wake route starts the turn with `codex exec
//! resume` instead.
//! `codex queue` takes the text only as `--message <TEXT>` (no stdin form), so a message
//! longer than the command line allows (8000 bytes on Windows, 100 000 elsewhere) goes by the
//! wake route instead (message on stdin, no length limit; an idle chat is woken when
//! `codexWakeIdle` allows it, otherwise the message waits in the hook inbox as `held`), and is
//! `unsupported` ("message too long for Codex's command line") only when that cannot take it.
//!
//! Where the CLI is looked for (`codex_candidates`); the first candidate whose
//! `queue --help` mentions threads is used, at most six are probed, and when none
//! supports it the newest candidate's version is named in the `unsupported` receipt:
//! * Test builds only (`test-overrides` feature): `$RIGHTKIT_CODEX_BIN`, when it names a file,
//!   wins everywhere and is the only candidate. A release build never reads it, and never
//!   looks for `codex` on the search path: only the install locations below.
//! * macOS: `/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex`, then
//!   `/opt/homebrew/bin/codex`, `/usr/local/bin/codex` (test builds add `codex` on PATH).
//! * Windows: `%LOCALAPPDATA%\Programs\ChatGPT\resources\codex-cli\bin\codex.exe`,
//!   `%LOCALAPPDATA%\Programs\Codex\resources\codex-cli\bin\codex.exe`, the desktop app's
//!   `%LOCALAPPDATA%\OpenAI\Codex\bin\<hash>\codex.exe` folders newest-modified first,
//!   `%LOCALAPPDATA%\OpenAI\Codex\bin\codex.exe` (can be a stale version); test builds
//!   add `codex.exe` / `codex.cmd` on PATH. A packaged (WindowsApps) install is not
//!   readable and is not searched.
//!
//! Thread discovery: the highest `~/.codex/state_<N>.sqlite` (table `threads`,
//! opened read-only; the Codex app holds it open in WAL mode), not archived,
//! newest first: app chats (`source = 'vscode'` with originator `Codex Desktop`, or none on
//! older threads) and interactive terminal threads (`source = 'cli'` with originator
//! `codex-tui`, or none) that were written within `roster::TERMINAL_CHAT_WINDOW` (6 hours);
//! the roster drops the terminal ones when the setting `terminalChats` is off. `exec` runs,
//! the Chrome side panel, sub-agents and reviews (their `source` is `exec` or a sub-agent
//! object) are left out. Only when no state database exists does it fall back to
//! `~/.codex/session_index.jsonl` (one `{id, thread_name, updated_at}` per
//! line), which lags and has no archived flag. Failures are explicit
//! (`DiscoveryError`, `codex_discovery_status`); the last good list is kept with its
//! age. `find_thread_by_id` resolves one id exactly, archived included. The roster lists
//! the most recent threads as reachable peers (`roster::local_sessions`).

use super::codex_hook;
use super::deliver_claude::shim;
use super::store::Store;
use super::{ChatError, Envelope, LocalSession, Receipt, ReceiptState};
use rightkit_child_process::{OwnedCommand, RunOptions};
use rightkit_db::{DbError, DbOptions, rusqlite};
use serde::Serialize;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const QUEUE_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const INDEX_TAIL_BYTES: u64 = 512 * 1024;
/// Most bytes read from the end of a thread's rollout file to find its last lifecycle event.
const ROLLOUT_TAIL_BYTES: u64 = 512 * 1024;
/// How often, and how many times, the queue row of a just-queued message is checked.
const CONSUME_POLLS: u32 = 4;
const CONSUME_POLL_GAP: Duration = Duration::from_millis(250);
/// How long a hook-route message is watched for its claim (12 polls of 250 ms), how recent a
/// hook run must be for the hook route to apply, and how recent a run that did not end must
/// be for the chat to count as busy.
const HOOK_POLLS: u32 = 12;
const HOOK_SEEN_WITHIN: Duration = Duration::from_secs(6 * 3600);
const HOOK_ACTIVE_WITHIN: Duration = Duration::from_secs(10 * 60);
/// A second wake of the same thread is not started within this long of the first.
const WAKE_ONE_AT_A_TIME: Duration = Duration::from_secs(45);
/// How long a started turn is watched for evidence that it began, and how long a child that
/// is still running with no error in its log counts as evidence when no hook ever ran.
const WAKE_CONFIRM_WITHIN: Duration = Duration::from_secs(10);
const WAKE_QUIET_AFTER: Duration = Duration::from_secs(3);
/// Wake files (message, log, marker) are removed after this long; at most this many entries
/// of the folder are examined per send.
const WAKE_PRUNE_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);
const WAKE_PRUNE_EXAMINE: usize = 200;
/// Most bytes of a wake log read back.
const WAKE_LOG_READ: u64 = 64 * 1024;
/// Most bytes kept from each of Codex's stdout and stderr; the rest is drained and dropped.
const CAPTURE_CAP: usize = 256 * 1024;
/// The longest message `codex queue --message` is given: Windows command lines are
/// short, other systems allow far more.
#[cfg(windows)]
const ARGV_LIMIT: usize = 8_000;
#[cfg(not(windows))]
const ARGV_LIMIT: usize = 100_000;

/// `~/.codex`, or `$CODEX_HOME`.
pub fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => rightkit_app_host::account_home().join(".codex"),
    }
}

/// Most Codex binaries probed when looking for one that can queue.
const MAX_PROBED: usize = 6;

/// Every Codex CLI this computer has, best guess first. In a build with `test-overrides`,
/// `$RIGHTKIT_CODEX_BIN`, when it names a file, is the only entry and the search path is read
/// too; a release build reads neither.
fn codex_candidates() -> Vec<PathBuf> {
    #[cfg(feature = "test-overrides")]
    if let Some(chosen) = std::env::var_os("RIGHTKIT_CODEX_BIN").filter(|v| !v.is_empty()) {
        let chosen = PathBuf::from(chosen);
        if chosen.is_file() {
            return vec![chosen];
        }
    }
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(unused_mut))]
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from(
            "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex",
        ));
        candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
    }
    #[cfg(windows)]
    {
        if let Some(local) = rightkit_app_host::env_var("LOCALAPPDATA").map(PathBuf::from) {
            candidates.push(local.join(r"Programs\ChatGPT\resources\codex-cli\bin\codex.exe"));
            candidates.push(local.join(r"Programs\Codex\resources\codex-cli\bin\codex.exe"));
            // The desktop app keeps each version in a hash-named folder; the newest is current.
            let bin = local.join(r"OpenAI\Codex\bin");
            let mut versions: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(&bin)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let exe = e.path().join("codex.exe");
                    let modified = e.metadata().ok()?.modified().ok()?;
                    exe.is_file().then_some((modified, exe))
                })
                .collect();
            versions.sort_by_key(|a| std::cmp::Reverse(a.0));
            candidates.extend(versions.into_iter().map(|(_, exe)| exe));
            candidates.push(bin.join("codex.exe"));
        }
    }
    #[cfg(feature = "test-overrides")]
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let names: &[&str] = if cfg!(windows) {
                &["codex.exe", "codex.cmd"]
            } else {
                &["codex"]
            };
            for name in names {
                candidates.push(dir.join(name));
            }
        }
    }
    let mut found: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        if candidate.is_file() && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    found
}

/// The first Codex CLI candidate, if this computer has one (`codex_capability` picks the
/// first that can queue).
pub fn codex_binary() -> Option<PathBuf> {
    codex_candidates().into_iter().next()
}

// ---- running Codex ---------------------------------------------------------------

/// What a finished Codex process left.
struct Captured {
    success: bool,
    status: String,
    stdout: String,
    stderr: String,
}

enum RunFailure {
    /// Codex never ran.
    NotStarted(String),
    /// Codex ran but its outcome is not known (timeout, lost watch).
    Ambiguous(String),
}

/// Run `binary args` with no console window and no stdin, stdout and stderr
/// drained concurrently, and a kill at `timeout`.
fn run_capture(binary: &Path, args: &[String], timeout: Duration) -> Result<Captured, RunFailure> {
    let options = RunOptions::new(timeout)
        .stdout_cap(CAPTURE_CAP)
        .stderr_cap(CAPTURE_CAP);
    match rightkit_child_process::run(binary, args, &options) {
        Ok(done) if done.timed_out => {
            let why = format!("Codex didn't answer within {} seconds.", timeout.as_secs());
            Err(RunFailure::Ambiguous(why))
        }
        Ok(done) => Ok(Captured {
            success: done.status.success(),
            status: done.status.to_string(),
            stdout: String::from_utf8_lossy(&done.stdout.bytes).into_owned(),
            stderr: String::from_utf8_lossy(&done.stderr.bytes).into_owned(),
        }),
        // `run` reports a failed start and a failed wait alike; only a missing or unrunnable
        // program surely never ran, so anything else may have delivered and is not resent.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            let why = format!("Couldn't start Codex: {}", e.kind());
            Err(RunFailure::NotStarted(why))
        }
        Err(e) => {
            let why = format!("Couldn't watch Codex: {}", e.kind());
            Err(RunFailure::Ambiguous(why))
        }
    }
}

// ---- capability -------------------------------------------------------------------

/// What the chosen Codex binary can do, probed once per path and mtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodexCapability {
    /// The binary chosen, when there is one.
    pub path: Option<String>,
    /// The first line of `codex --version`, when it answered.
    pub version: Option<String>,
    /// `codex queue --help` succeeded and mentions threads.
    pub queue_supported: bool,
    /// `codex exec resume --help` succeeded, so an idle thread can be started with it.
    pub exec_resume_supported: bool,
    /// Why probing failed, when it did.
    pub error: Option<String>,
}

type ProbeCache = Mutex<Vec<(PathBuf, Option<SystemTime>, CodexCapability)>>;
static CAPABILITY: ProbeCache = Mutex::new(Vec::new());

fn probe(binary: &Path) -> CodexCapability {
    let mut found = CodexCapability {
        path: Some(binary.to_string_lossy().into_owned()),
        version: None,
        queue_supported: false,
        exec_resume_supported: false,
        error: None,
    };
    let run = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        run_capture(binary, &args, PROBE_TIMEOUT)
    };
    if let Ok(done) = run(&["--version"])
        && done.success
    {
        found.version = done.stdout.lines().next().map(|l| l.trim().to_string());
    }
    match run(&["queue", "--help"]) {
        Ok(done) if done.success => {
            let help = format!("{}\n{}", done.stdout, done.stderr).to_ascii_lowercase();
            found.queue_supported = help.contains("thread");
            if !found.queue_supported {
                found.error = Some("`codex queue --help` doesn't mention threads.".to_string());
            }
        }
        Ok(done) => {
            found.error = Some(format!("`codex queue --help` failed ({}).", done.status));
        }
        Err(RunFailure::NotStarted(e) | RunFailure::Ambiguous(e)) => found.error = Some(e),
    }
    if let Ok(done) = run(&["exec", "resume", "--help"])
        && done.success
    {
        let help = format!("{}\n{}", done.stdout, done.stderr).to_ascii_lowercase();
        found.exec_resume_supported = help.contains("resume");
    }
    found
}

/// The probe of one binary, cached per path and modification time.
fn cached_probe(binary: &Path) -> CodexCapability {
    let mtime = std::fs::metadata(binary).and_then(|m| m.modified()).ok();
    let mut cache = CAPABILITY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, _, known)) = cache
        .iter()
        .find(|(path, stamp, _)| path == binary && *stamp == mtime)
    {
        return known.clone();
    }
    let fresh = probe(binary);
    cache.retain(|(path, _, _)| path != binary);
    cache.push((binary.to_path_buf(), mtime, fresh.clone()));
    fresh
}

/// The Codex CLI's abilities: the first candidate (at most `MAX_PROBED` are probed) that
/// supports `queue --thread`; when none does, the newest candidate's probe.
pub fn codex_capability() -> CodexCapability {
    let candidates = codex_candidates();
    let mut first: Option<CodexCapability> = None;
    for binary in candidates.iter().take(MAX_PROBED) {
        let found = cached_probe(binary);
        if found.queue_supported {
            return found;
        }
        first.get_or_insert(found);
    }
    first.unwrap_or_else(|| CodexCapability {
        path: None,
        version: None,
        queue_supported: false,
        exec_resume_supported: false,
        error: Some("No Codex CLI found; install Codex.".to_string()),
    })
}

// ---- thread discovery ------------------------------------------------------------

/// A Codex thread listed in the session index.
#[derive(Clone, Debug)]
pub struct CodexThread {
    pub id: String,
    pub name: String,
    pub updated_at: String,
    pub cwd: String,
    /// Milliseconds since the epoch of the last write (0 when unknown).
    pub updated_ms: u64,
    /// The thread is archived (only the state database knows).
    pub archived: bool,
    /// An interactive terminal thread (`source` `cli`, originator `codex-tui` or none); only
    /// the state database knows. The roster lists it only while the setting is on and it was
    /// active within `roster::TERMINAL_CHAT_WINDOW`.
    pub terminal: bool,
}

/// Why Codex threads could not be listed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// Neither a state database nor a session index exists.
    #[error("no Codex state database or session index was found")]
    NoSource,
    /// The state database or index could not be read (locked, corrupt, changed schema).
    #[error("couldn't read Codex's threads: {0}")]
    Unreadable(String),
}

const THREAD_COLUMNS: &str = "id, name, title, first_user_message, preview, cwd, updated_at_ms, archived, source, originator";

/// SQL for an interactive terminal thread: Codex's `source` is `cli` (exec runs are `exec`,
/// sub-agents are an object), and the originator is `codex-tui` or absent.
const TERMINAL_THREAD: &str = "source = 'cli' AND (originator IS NULL OR originator = 'codex-tui')";

/// The highest-numbered `<prefix>_<N>.sqlite` in the Codex home.
///
/// The Codex home holds thousands of entries and the roster asks once for every chat every two
/// seconds, so a database that was found is kept for 30 seconds, for as long as it still exists.
/// A miss is never kept: the database can be created at any moment.
fn highest_db(prefix: &str) -> Option<PathBuf> {
    struct Known {
        home: PathBuf,
        prefix: String,
        at: Instant,
        found: PathBuf,
    }
    static KNOWN: Mutex<Vec<Known>> = Mutex::new(Vec::new());
    let home = codex_home();
    let mut known = KNOWN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let held = known.iter().find(|k| {
        k.home == home
            && k.prefix == prefix
            && k.at.elapsed() < Duration::from_secs(30)
            && k.found.is_file()
    });
    if let Some(k) = held {
        return Some(k.found.clone());
    }
    let found = scan_highest_db(&home, prefix)?;
    known.retain(|k| !(k.home == home && k.prefix == prefix));
    known.push(Known {
        home,
        prefix: prefix.to_string(),
        at: Instant::now(),
        found: found.clone(),
    });
    Some(found)
}

fn scan_highest_db(home: &Path, prefix: &str) -> Option<PathBuf> {
    let lead = format!("{prefix}_");
    std::fs::read_dir(home)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name
                .strip_prefix(lead.as_str())?
                .strip_suffix(".sqlite")?
                .parse::<u64>()
                .ok()?;
            Some((n, e.path()))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, p)| p)
}

/// The highest-numbered `state_<N>.sqlite` in the Codex home.
fn state_db() -> Option<PathBuf> {
    highest_db("state")
}

fn display_name(name: &str, title: &str, first: &str, preview: &str) -> String {
    for candidate in [name, title, first, preview] {
        let line = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() {
            return line.chars().take(60).collect();
        }
    }
    String::new()
}

fn thread_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CodexThread> {
    let text = |i: usize| -> rusqlite::Result<String> {
        Ok(row.get::<_, Option<String>>(i)?.unwrap_or_default())
    };
    let ms = row.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u64;
    let archived = row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0;
    let source = text(8)?;
    let originator = text(9)?;
    let terminal = source == "cli" && (originator.is_empty() || originator == "codex-tui");
    Ok(CodexThread {
        id: text(0)?,
        name: display_name(&text(1)?, &text(2)?, &text(3)?, &text(4)?),
        updated_at: String::new(),
        cwd: text(5)?,
        updated_ms: ms,
        archived,
        terminal,
    })
}

/// The Codex state database, opened read-only: never created, no journal change, no bootstrap
/// statement. A busy database is waited for 250 ms.
fn open_state(path: &Path) -> Result<rusqlite::Connection, rusqlite::Error> {
    let mut options = DbOptions::new(path);
    options.busy_timeout = Duration::from_millis(250);
    options.open_read_only().map_err(|error| match error {
        DbError::Sqlite(error) => error,
        other => rusqlite::Error::ToSqlConversionFailure(Box::new(other)),
    })
}

fn read_state(path: &Path, limit: usize) -> Result<Vec<CodexThread>, rusqlite::Error> {
    let conn = open_state(path)?;
    // Terminal threads only within the activity window, so a days-old one cannot use up
    // the limit; the roster applies the window again to the cached list.
    let window_ms =
        i64::try_from(super::roster::TERMINAL_CHAT_WINDOW.as_millis()).unwrap_or(i64::MAX);
    let since = i64::try_from(super::envelope::now_ms())
        .unwrap_or(i64::MAX)
        .saturating_sub(window_ms);
    let sql = format!(
        "SELECT {THREAD_COLUMNS} FROM threads WHERE archived = 0 AND \
         ((source = 'vscode' AND (originator IS NULL OR originator = 'Codex Desktop')) \
          OR ({TERMINAL_THREAD} AND updated_at_ms >= ?2)) \
         ORDER BY updated_at_ms DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        [i64::try_from(limit).unwrap_or(i64::MAX), since],
        thread_from_row,
    )?;
    Ok(rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|t| !t.id.is_empty())
        .collect())
}

/// One thread by exact id, archived or not, whatever the list limit.
fn read_state_by_id(path: &Path, id: &str) -> Result<Option<CodexThread>, rusqlite::Error> {
    use rusqlite::OptionalExtension;
    let conn = open_state(path)?;
    let sql = format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id = ?1");
    conn.query_row(&sql, [id], thread_from_row).optional()
}

/// What the last discovery served and how it went.
struct Discovery {
    /// "state_db", "index" or "cache".
    source: &'static str,
    /// The state database the cached list came from.
    db: Option<PathBuf>,
    good: Vec<CodexThread>,
    good_at: Option<Instant>,
    error: Option<String>,
    /// `(mtime, len)` of the database and its WAL, and the limit, of the last good read.
    stamp: Option<(DbStamp, usize)>,
}

type DbStamp = [Option<(u64, Option<SystemTime>)>; 2];

fn db_stamp(db: &Path) -> DbStamp {
    let of = |p: &Path| {
        std::fs::metadata(p)
            .ok()
            .map(|m| (m.len(), m.modified().ok()))
    };
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    [of(db), of(Path::new(&wal))]
}

static DISCOVERY: Mutex<Discovery> = Mutex::new(Discovery {
    source: "none",
    db: None,
    good: Vec::new(),
    good_at: None,
    error: None,
    stamp: None,
});

/// `(source, age_ms, error)` of the last thread listing: where the threads came from
/// ("state_db", "index", "cache" for the last good list after a failure, or "none"), how
/// old that data is, and the last error, if any.
pub fn codex_discovery_status() -> (String, u64, Option<String>) {
    let state = DISCOVERY.lock().unwrap_or_else(|e| e.into_inner());
    let age = state.good_at.map_or(0, |at| {
        u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)
    });
    (state.source.to_string(), age, state.error.clone())
}

/// The most recently updated threads (newest first, at most `limit`), or why they
/// can't be listed. After a read failure the last good list for the same database is
/// returned (see `codex_discovery_status` for its age and the error).
pub fn try_list_threads(limit: usize) -> Result<Vec<CodexThread>, DiscoveryError> {
    let mut state = DISCOVERY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(db) = state_db() else {
        return match list_threads_from_index(limit) {
            Ok(threads) => {
                state.source = "index";
                state.error = None;
                state.good = threads.clone();
                state.good_at = Some(Instant::now());
                state.db = None;
                state.stamp = None;
                Ok(threads)
            }
            Err(e) => {
                state.source = "none";
                state.error = Some(e.to_string());
                Err(e)
            }
        };
    };
    let stamp = db_stamp(&db);
    // Database and WAL both unchanged (and present with a known time): the listing is too.
    // The roster applies the activity window to the list again.
    if state.db.as_ref() == Some(&db)
        && state.source == "state_db"
        && state.error.is_none()
        && stamp.iter().all(|s| s.is_none_or(|s| s.1.is_some()))
        && stamp[0].is_some()
        && state
            .stamp
            .is_some_and(|(known, wide)| known == stamp && wide >= limit)
    {
        return Ok(state.good.iter().take(limit).cloned().collect());
    }
    match read_state(&db, limit) {
        Ok(threads) => {
            state.source = "state_db";
            state.error = None;
            state.good = threads.clone();
            state.good_at = Some(Instant::now());
            state.db = Some(db);
            state.stamp = Some((stamp, limit));
            let ids: Vec<String> = threads.iter().map(|t| t.id.clone()).collect();
            drop(state);
            evict_rollouts_except(&ids);
            Ok(threads)
        }
        Err(e) => {
            let error = DiscoveryError::Unreadable(e.to_string());
            state.error = Some(error.to_string());
            if state.db.as_ref() == Some(&db) && state.good_at.is_some() {
                state.source = "cache";
                return Ok(state.good.iter().take(limit).cloned().collect());
            }
            state.source = "none";
            Err(error)
        }
    }
}

/// `try_list_threads`, empty on failure; the failure is in `codex_discovery_status`.
pub fn list_threads(limit: usize) -> Vec<CodexThread> {
    try_list_threads(limit).unwrap_or_default()
}

/// One thread by its exact id, archived threads included, independent of how many
/// threads are listed. `Ok(None)` means the sources were read and the id is not there.
pub fn find_thread_by_id(id: &str) -> Result<Option<CodexThread>, DiscoveryError> {
    if id.is_empty() {
        return Ok(None);
    }
    match state_db() {
        Some(db) => {
            read_state_by_id(&db, id).map_err(|e| DiscoveryError::Unreadable(e.to_string()))
        }
        None => Ok(list_threads_from_index(usize::MAX)?
            .into_iter()
            .find(|t| t.id == id)),
    }
}

fn list_threads_from_index(limit: usize) -> Result<Vec<CodexThread>, DiscoveryError> {
    let path = codex_home().join("session_index.jsonl");
    let mut file = std::fs::File::open(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => DiscoveryError::NoSource,
        kind => DiscoveryError::Unreadable(format!("session index: {kind}")),
    })?;
    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
    if length > INDEX_TAIL_BYTES {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(length - INDEX_TAIL_BYTES))
            .map_err(|e| DiscoveryError::Unreadable(format!("session index: {}", e.kind())))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| DiscoveryError::Unreadable(format!("session index: {}", e.kind())))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut latest: std::collections::HashMap<String, CodexThread> =
        std::collections::HashMap::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = entry["id"].as_str().filter(|i| !i.is_empty()) else {
            continue;
        };
        let thread = CodexThread {
            id: id.to_string(),
            name: entry["thread_name"].as_str().unwrap_or("").to_string(),
            updated_at: entry["updated_at"].as_str().unwrap_or("").to_string(),
            cwd: String::new(),
            updated_ms: 0,
            archived: false,
            terminal: false,
        };
        let newer = latest
            .get(id)
            .is_none_or(|old| thread.updated_at >= old.updated_at);
        if newer {
            latest.insert(id.to_string(), thread);
        }
    }
    let mut threads: Vec<CodexThread> = latest.into_values().collect();
    threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    threads.truncate(limit);
    for t in &mut threads {
        t.updated_ms = super::roster::iso_epoch(&t.updated_at).map_or(0, |s| s * 1000);
    }
    Ok(threads)
}

// ---- delivery ---------------------------------------------------------------------

/// The message id in `"Queued message <id> for thread <id>."`.
fn queued_id(stdout: &str) -> Option<&str> {
    stdout
        .trim()
        .strip_prefix("Queued message ")?
        .split_whitespace()
        .next()
}

/// What Codex's thread sees: the message header (who sent it, that it is another agent chat
/// and not the user, how to reply), with the full the host app Chat introduction on the thread's
/// first message, then the body. The sender's labels are its own claims; the machine is
/// stamped by this computer, and the reply route is the opaque message id.
/// How recently any Codex chat on this computer must have run the host app's hook for messages to go
/// hidden: the hooks are installed and the running Codex loaded them.
const HOOKS_RUNNING_WITHIN: Duration = Duration::from_secs(7 * 24 * 3600);

/// Whether Codex on this computer runs the host app's hook (any chat, within `HOOKS_RUNNING_WITHIN`).
fn hooks_running() -> bool {
    codex_hook::newest_heartbeat().is_some_and(|secs| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(secs) <= HOOKS_RUNNING_WITHIN.as_secs()
    })
}

pub(super) fn thread_text(env: &Envelope) -> String {
    thread_text_as(env, false)
}

/// `thread_text` for a message that shows as a visible turn: the user's stamped words are left
/// out (they belong in hidden context only; `said` still reads them).
fn visible_thread_text(env: &Envelope) -> String {
    thread_text_as(env, true)
}

fn thread_text_as(env: &Envelope, visible: bool) -> String {
    let id: String = shim::env_id(env)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    if super::envelope::is_chatgpt(env) {
        return format!(
            "{}\n{}",
            super::envelope::chatgpt_header(&id),
            shim::env_text(env)
        );
    }
    let origin = super::origin_of(env);
    let first = super::first_contact_pending(&env.to.session);
    let header = if visible {
        super::envelope::message_header_visible(&origin, &id, first)
    } else {
        super::envelope::message_header(&origin, &id, first)
    };
    format!("{header}\n{}", shim::env_text(env))
}

/// Where a thread's last turn stands, from its rollout file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunState {
    /// The last lifecycle event is a turn starting.
    Running,
    /// The last turn finished normally.
    Finished,
    /// The user stopped the last turn.
    Stopped,
    /// The rollout was read to its start and has no lifecycle event: nothing is running.
    Idle,
    /// No rollout, or it could not be read, or its tail has no lifecycle event but is cut.
    Unknown,
}

/// The last `task_started` / `task_complete` / `turn_aborted` event in the tail of a
/// rollout file (at most `ROLLOUT_TAIL_BYTES` are read at once).
///
/// A file with the same size and modified time as at the last ask has the same answer. A file
/// that only grew is read from where the last read stopped (a rollout is append-only), not
/// from its tail again: a busy chat's rollout changes on every ask, and a whole tail read and
/// parse per chat per pass was the weight of the chat list.
fn rollout_run_state(path: &Path) -> RunState {
    let stamp: Option<RolloutStamp> = std::fs::metadata(path)
        .ok()
        .map(|m| (m.len(), m.modified().ok()));
    let mut earlier = None;
    if let Some(stamp) = stamp {
        let known = ROLLOUT_STATES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = known.iter().find(|e| e.path == path) {
            // Unchanged size and (known) modified time: the answer cannot have changed.
            if entry.stamp == stamp && stamp.1.is_some() {
                return entry.state;
            }
            earlier = Some(Earlier {
                length: entry.stamp.0,
                state: entry.state,
                resume: entry.resume,
            });
        }
    }
    // Stamped before the read: a file that grows meanwhile is read again next time.
    let (state, resume) = read_rollout_run_state(path, earlier);
    if let Some(stamp) = stamp {
        let mut known = ROLLOUT_STATES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        known.retain(|e| e.path != path);
        while known.len() >= ROLLOUT_CACHE_MAX {
            known.remove(0);
        }
        known.push(RolloutEntry {
            path: path.to_path_buf(),
            stamp,
            state,
            resume,
        });
    }
    state
}

type RolloutStamp = (u64, Option<SystemTime>);

struct RolloutEntry {
    path: PathBuf,
    stamp: RolloutStamp,
    state: RunState,
    /// Where the last read stopped: just after the last complete line it saw.
    resume: u64,
}

/// At most this many rollouts are remembered (oldest asked first out).
const ROLLOUT_CACHE_MAX: usize = 64;

static ROLLOUT_STATES: Mutex<Vec<RolloutEntry>> = Mutex::new(Vec::new());

/// Forget the cached state of every rollout that is not the rollout of one of `ids`: threads
/// that left the newest list.
fn evict_rollouts_except(ids: &[String]) {
    let home = codex_home();
    let keep: Vec<PathBuf> = ids
        .iter()
        .filter_map(|id| kept_rollout_path(&home, id))
        .collect();
    let mut known = ROLLOUT_STATES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    known.retain(|e| keep.contains(&e.path));
}

/// What an earlier read of the same rollout found.
#[derive(Clone, Copy)]
struct Earlier {
    /// The file's length when it was read.
    length: u64,
    state: RunState,
    /// Offset just after the last complete line that read saw.
    resume: u64,
}

/// The last lifecycle event among the lines of `text`.
fn last_run_event(text: &str) -> Option<RunState> {
    let mut state = None;
    // Lines end in LF or CRLF; `str::lines` takes both.
    for line in text.lines() {
        if !line.contains("\"event_msg\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry["type"] != "event_msg" {
            continue;
        }
        state = match entry["payload"]["type"].as_str() {
            Some("task_started") => Some(RunState::Running),
            Some("task_complete") => Some(RunState::Finished),
            Some("turn_aborted") => Some(RunState::Stopped),
            _ => continue,
        };
    }
    state
}

/// Offset just after the last newline in `bytes` (0 when there is none).
fn after_last_newline(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at as u64 + 1)
}

/// The run state of a rollout and where the read stopped (see `Earlier::resume`). With an
/// `earlier` read of a file that has only grown since, only the new lines are read.
fn read_rollout_run_state(path: &Path, earlier: Option<Earlier>) -> (RunState, u64) {
    use std::io::{Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return (RunState::Unknown, 0);
    };
    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
    if let Some(earlier) = earlier
        && earlier.resume > 0
        && length >= earlier.length
        && length >= earlier.resume
        && length - earlier.resume <= ROLLOUT_TAIL_BYTES
    {
        if file.seek(SeekFrom::Start(earlier.resume)).is_err() {
            return (RunState::Unknown, 0);
        }
        let mut bytes = Vec::new();
        if (&mut file)
            .take(ROLLOUT_TAIL_BYTES)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return (RunState::Unknown, 0);
        }
        let state = last_run_event(&String::from_utf8_lossy(&bytes)).unwrap_or(earlier.state);
        return (state, earlier.resume + after_last_newline(&bytes));
    }
    let start = length.saturating_sub(ROLLOUT_TAIL_BYTES);
    if start > 0 && file.seek(SeekFrom::Start(start)).is_err() {
        return (RunState::Unknown, 0);
    }
    let mut bytes = Vec::new();
    let read = (&mut file).take(ROLLOUT_TAIL_BYTES).read_to_end(&mut bytes);
    if read.is_err() {
        return (RunState::Unknown, 0);
    }
    let resume = start + after_last_newline(&bytes);
    let state = match last_run_event(&String::from_utf8_lossy(&bytes)) {
        Some(state) => state,
        // A cut tail may hide an earlier run start; only a whole file proves "no events".
        None if start > 0 => RunState::Unknown,
        None => RunState::Idle,
    };
    (state, resume)
}

/// The run state of a thread, found through its rollout path in the state database.
fn thread_run_state(id: &str) -> RunState {
    match rollout_path_of(id) {
        Some(path) => rollout_run_state(&path),
        None => RunState::Unknown,
    }
}

/// How long a found rollout path is kept.
const ROLLOUT_PATH_KEEP: Duration = Duration::from_secs(300);

/// Rollout paths found in the state database, by Codex home and thread id.
struct KnownPath {
    home: PathBuf,
    id: String,
    at: Instant,
    path: PathBuf,
}

static KNOWN_PATHS: Mutex<Vec<KnownPath>> = Mutex::new(Vec::new());

/// The kept path of a thread, when it was found less than `ROLLOUT_PATH_KEEP` ago.
fn kept_rollout_path(home: &Path, id: &str) -> Option<PathBuf> {
    let known = KNOWN_PATHS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    known
        .iter()
        .find(|k| k.home == home && k.id == id && k.at.elapsed() < ROLLOUT_PATH_KEEP)
        .map(|k| k.path.clone())
}

/// Keep `recorded` (as the database holds it: absolute, or relative to the Codex home) as the
/// rollout path of thread `id`, and return the resolved path.
fn keep_rollout_path(home: &Path, id: &str, recorded: &str) -> PathBuf {
    let recorded = PathBuf::from(recorded);
    let path = if recorded.is_relative() {
        home.join(recorded)
    } else {
        recorded
    };
    let mut known = KNOWN_PATHS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    known.retain(|k| !(k.home == home && k.id == id) && k.at.elapsed() < ROLLOUT_PATH_KEEP);
    known.push(KnownPath {
        home: home.to_path_buf(),
        id: id.to_string(),
        at: Instant::now(),
        path: path.clone(),
    });
    path
}

/// A thread's rollout file, from the state database. A found path is kept for five minutes, so
/// the chat list does not open the database once per chat on every pass. A miss is not kept.
pub(super) fn rollout_path_of(id: &str) -> Option<PathBuf> {
    let home = codex_home();
    if let Some(path) = kept_rollout_path(&home, id) {
        return Some(path);
    }
    let conn = open_state(&state_db()?).ok()?;
    let recorded = conn
        .query_row(
            "SELECT rollout_path FROM threads WHERE id = ?1",
            [id],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .filter(|path| !path.is_empty())?;
    Some(keep_rollout_path(&home, id, &recorded))
}

/// Look up the rollout paths of `ids` in one database open and one query, keeping every one
/// found, so a roster pass over many chats does not open the database once per chat
/// (`rollout_path_of` then answers from the kept paths). Ids already kept are not asked for;
/// a database or column that cannot be read keeps nothing and leaves each lookup to
/// `rollout_path_of`.
pub(super) fn prime_rollout_paths(ids: &[&str]) {
    let home = codex_home();
    let missing: Vec<&str> = ids
        .iter()
        .copied()
        .filter(|id| !id.is_empty() && kept_rollout_path(&home, id).is_none())
        .collect();
    if missing.is_empty() {
        return;
    }
    let Some(db) = state_db() else { return };
    let Ok(conn) = open_state(&db) else { return };
    let marks = vec!["?"; missing.len()].join(",");
    let sql = format!("SELECT id, rollout_path FROM threads WHERE id IN ({marks})");
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return;
    };
    let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(missing.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        return;
    };
    for (id, recorded) in rows.flatten() {
        if !recorded.is_empty() {
            keep_rollout_path(&home, &id, &recorded);
        }
    }
}

/// Whether Codex's queue still holds this message: `None` when that can't be read.
/// Whether Codex's queue still holds the row for `message_id` (None when the queue cannot be read).
pub fn queue_row_pending(message_id: &str) -> Option<bool> {
    let db = highest_db("queue")?;
    let conn = open_state(&db).ok()?;
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM queued_items WHERE id = ?1",
            [message_id],
            |row| row.get(0),
        )
        .ok()?;
    Some(rows > 0)
}

/// The message's queue row disappeared within a second: the thread took it.
fn consumed_soon(message_id: &str) -> bool {
    for _ in 0..CONSUME_POLLS {
        std::thread::sleep(CONSUME_POLL_GAP);
        match queue_row_pending(message_id) {
            Some(false) => return true,
            Some(true) => {}
            None => return false,
        }
    }
    false
}

/// The receipt for a message `codex queue` accepted, judged by the thread's run state.
fn queued_receipt(thread: &str, message_id: &str) -> Receipt {
    let state = thread_run_state(thread);
    // Without `[features] hooks = true` Codex runs no the host app hooks, so hidden delivery is off.
    let place = if super::install::hooks_feature_on() {
        "Queued in the Codex thread"
    } else {
        "Queued in the Codex app; hidden delivery is off"
    };
    if state == RunState::Stopped {
        return Receipt::held(format!(
            "{place} (message {message_id}); this Codex chat is stopped \
             and will not read it until its owner continues it."
        ));
    }
    if state == RunState::Running {
        return shim::queued(&format!(
            "{place} (message {message_id}); it reads the message when \
             its current run finishes normally, not if that run is stopped. Its owner can \
             press Steer on the queued message in Codex to have it read now."
        ));
    }
    if consumed_soon(message_id) {
        let taken = format!("The Codex thread took message {message_id} from its queue.");
        return Receipt::delivered(taken);
    }
    if state == RunState::Unknown {
        return shim::queued(&format!(
            "{place} (message {message_id}); could not tell whether the \
             chat is running, so not confirmed read by the thread."
        ));
    }
    // Idle and not stopped: Codex's queue watcher starts the row once the thread is loaded
    // and idle (about every 10 s), and a row also drains when the thread is next opened or
    // resumed. Not `delivered` until the row is gone (it is deleted only when a turn started).
    shim::queued(&format!(
        "{place} (message {message_id}); Codex starts it when this chat \
         is idle and loaded in Codex, or when the chat is next opened."
    ))
}

/// The first 200 characters of Codex's stderr, on one line.
fn short(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

/// What the hook and the rollout file together say a Codex chat is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatState {
    /// A turn is running: the rollout says so, or the hook ran within `HOOK_ACTIVE_WITHIN`
    /// and that run was not the end of a run.
    Busy,
    /// The owner stopped the last turn (and nothing above says it is running).
    Stopped,
    /// Between turns: the hook saw a run end, or the last turn finished.
    Idle,
    /// The hook ran, but not within `HOOK_ACTIVE_WITHIN`, did not end a run, and the
    /// rollout does not say whether a turn is running.
    Quiet,
    /// Neither a hook run nor a readable rollout.
    Unknown,
}

fn chat_state(run: RunState, hook: Option<codex_hook::HookActivity>) -> ChatState {
    let recent = hook.is_some_and(|h| !h.ended && h.age_secs <= HOOK_ACTIVE_WITHIN.as_secs());
    if run == RunState::Running || recent {
        return ChatState::Busy;
    }
    if run == RunState::Stopped {
        return ChatState::Stopped;
    }
    if hook.is_some_and(|h| h.ended) || matches!(run, RunState::Finished | RunState::Idle) {
        return ChatState::Idle;
    }
    if hook.is_some() {
        ChatState::Quiet
    } else {
        ChatState::Unknown
    }
}

/// The state of one thread from its hook heartbeat and, when `read_rollout`, its rollout.
pub(super) fn chat_state_of(thread: &str, read_rollout: bool) -> ChatState {
    let run = if read_rollout {
        thread_run_state(thread)
    } else {
        RunState::Unknown
    };
    chat_state(run, codex_hook::hook_activity(thread))
}

/// The hook route (busy chats), the hold routes and the wake route (idle chats); `None`
/// when the hook has not run for the thread within six hours or the inbox could not be
/// written, which sends the message to `codex queue`.
fn deliver_by_hook(session: &LocalSession, env: &Envelope) -> Option<Receipt> {
    let thread = session.id.as_str();
    if thread.is_empty() || thread.starts_with('-') {
        return None;
    }
    let hook =
        codex_hook::hook_activity(thread).filter(|h| h.age_secs <= HOOK_SEEN_WITHIN.as_secs())?;
    let state = chat_state(thread_run_state(thread), Some(hook));
    let id = shim::env_id(env);
    let text = thread_text(env);
    match state {
        ChatState::Busy => {
            codex_hook::enqueue(thread, id, &text).ok()?;
            for _ in 0..HOOK_POLLS {
                std::thread::sleep(CONSUME_POLL_GAP);
                match codex_hook::outcome(thread, id) {
                    Some(codex_hook::HookOutcome::Sent) => {
                        return Some(Receipt::delivered(
                            "The Codex hook handed the message to the chat (seen receipt).",
                        ));
                    }
                    Some(codex_hook::HookOutcome::Refused) => {
                        return Some(shim::refused(
                            "The Codex hook refused the message: too large to hand over whole (it was not cut).",
                        ));
                    }
                    None => {}
                }
            }
            Some(shim::queued(
                "Queued behind this Codex chat's running work: it is in the app's inbox and the chat reads it after its next tool call or when its run ends.",
            ))
        }
        ChatState::Stopped => {
            codex_hook::enqueue(thread, id, &text).ok()?;
            Some(Receipt::held(
                "This Codex chat was stopped by its owner; it reads the message with the owner's next prompt.",
            ))
        }
        ChatState::Idle => wake_idle(session, id, &text, true),
        ChatState::Quiet | ChatState::Unknown => {
            codex_hook::enqueue(thread, id, &text).ok()?;
            // A terminal session, a Codex app window, or an owner the app cannot name is woken
            // through Codex's queue, which starts the line only once the holder is idle (the same
            // rule as `deliver_hidden`); never a headless turn here, whose second writer could
            // hurt the holder.
            #[cfg(unix)]
            if (terminal_holds(thread) || app_ipc::holder(thread) != app_ipc::Holder::Nobody)
                && let Some(binary) = codex_binary().filter(|b| !is_batch_file(b))
            {
                let line = super::envelope::wake_line(&super::origin_of(env), id);
                return Some(queue_wake_line(&binary, thread, id, &line, |why: &str| {
                    Receipt::held(format!(
                        "{why} The message waits for this chat's next prompt or tool call."
                    ))
                }));
            }
            Some(Receipt::held(format!(
                "This Codex chat has not run a tool for {} minutes; it reads the message at its next tool call or prompt.",
                hook.age_secs / 60
            )))
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A Windows batch file cannot safely be handed a message.
fn is_batch_file(binary: &Path) -> bool {
    binary
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// `<state>/chat/codex-wake`: the wake route's message files, logs and markers.
fn wake_dir() -> Option<PathBuf> {
    Some(
        rightkit_app_host::state_directory()
            .ok()?
            .join("chat")
            .join("codex-wake"),
    )
}

/// Remove wake files older than `WAKE_PRUNE_AFTER` (at most `WAKE_PRUNE_EXAMINE` examined).
fn prune_wake(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(WAKE_PRUNE_EXAMINE) {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| SystemTime::now().duration_since(at).ok())
            .is_some_and(|age| age > WAKE_PRUNE_AFTER);
        if old && entry.path().is_file() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The first `WAKE_LOG_READ` bytes of a wake log.
fn read_wake_log(path: &Path) -> String {
    let mut bytes = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(WAKE_LOG_READ).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Whether Codex's output reads as a failure: a line that starts with "error".
fn log_shows_error(log: &str) -> bool {
    log.lines()
        .any(|l| l.trim_start().to_ascii_lowercase().starts_with("error"))
}

/// Whether Codex's output is the per-thread writer-lock refusal ("thread `<id>` already has an
/// active writer"; a `Conflict` from the thread store): another process, the Codex app, has
/// the thread loaded.
fn log_is_writer_conflict(log: &str) -> bool {
    log.to_ascii_lowercase().contains("active writer")
}

/// Start `codex exec resume` for `thread`, detached, with `text` as its prompt on stdin
/// (a private file, not a pipe) and its output appended to `log_path`. The child outlives
/// this process: RightKit starts it in its own session (Windows: breakaway from any job,
/// no console window) and nothing waits for or stops it.
fn start_turn(
    binary: &Path,
    thread: &str,
    cwd: &str,
    stdin_path: &Path,
    log_path: &Path,
    text: &str,
) -> Result<rightkit_child_process::OwnedChild, String> {
    rightkit_fs::atomic_write_with_options(
        stdin_path,
        rightkit_fs::AtomicWriteOptions {
            durable: false,
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| std::io::Write::write_all(file, text.as_bytes()),
    )
    .map_err(|e| {
        format!(
            "Couldn't write the message for Codex: {}.",
            std::io::Error::from(e).kind()
        )
    })?;
    let input = std::fs::File::open(stdin_path)
        .map_err(|e| format!("Couldn't read the message for Codex: {}.", e.kind()))?;
    let log = rightkit_fs::open_private_append(log_path).map_err(|e| {
        format!(
            "Couldn't open the wake log: {}.",
            std::io::Error::from(e).kind()
        )
    })?;
    let errors = log
        .try_clone()
        .map_err(|e| format!("Couldn't open the wake log: {}.", e.kind()))?;
    let folder = if !cwd.is_empty() && Path::new(cwd).is_dir() {
        PathBuf::from(cwd)
    } else {
        rightkit_app_host::account_home()
    };
    let mut command = OwnedCommand::new(binary);
    command
        .command_mut()
        .args(["exec", "resume", "--skip-git-repo-check", thread, "-"])
        .current_dir(folder)
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errors));
    command
        .spawn_uncontained_detached()
        .map_err(|e| format!("Couldn't start Codex: {}.", e.kind()))
}

/// A wake that failed: the message goes to the hook inbox so it is not lost.
fn wake_failed(thread: &str, id: &str, text: &str, why: &str) -> Receipt {
    let kept = if codex_hook::enqueue(thread, id, text).is_ok() {
        "The message was left in the app's inbox for the chat's next hook run."
    } else {
        "The message could not be saved in the app's inbox either."
    };
    Receipt::refused(format!(
        "Couldn't start a turn in this idle Codex chat: {why} {kept}"
    ))
}

/// The wake route for an idle chat: start a turn with the message (see the module doc);
/// when waking is off, unavailable or already under way, the message waits in the hook inbox.
///
/// With `to_queue`, an idle chat that is not woken (setting off, no usable Codex, or the
/// writer-lock Conflict of a thread the Codex app has loaded) returns `None` without writing
/// the hook inbox, so the caller sends it to `codex queue` and the message is never in both.
/// A chat that is no longer positively idle always keeps the inbox, and a stopped one is
/// never queued.
fn wake_idle(session: &LocalSession, id: &str, text: &str, to_queue: bool) -> Option<Receipt> {
    let thread = session.id.as_str();
    if !codex_hook::safe_component(thread) || !codex_hook::safe_component(id) {
        return None;
    }
    let hold = || -> Option<Receipt> {
        codex_hook::enqueue(thread, id, text).ok()?;
        Some(Receipt::held(
            "This Codex chat is idle; it reads the message with its owner's next prompt.",
        ))
    };
    // Never queue or wake on stale evidence: look again first. Anything but idle (busy,
    // quiet, stopped) keeps the inbox; a stopped chat is never queued.
    if chat_state_of(thread, true) != ChatState::Idle {
        return hold();
    }
    if !Store::open_default().is_ok_and(|store| store.codex_wake_idle()) {
        return if to_queue { None } else { hold() };
    }
    // A headless turn is a second writer if the Codex app holds the chat. Only a proven absence
    // of the owner starts one; an owner that is unknown (socket or discovery error) or present
    // takes the hook inbox or `codex queue`, which never write into the rollout (Astra F4).
    #[cfg(unix)]
    if app_ipc::holder(thread) != app_ipc::Holder::Nobody {
        return if to_queue { None } else { hold() };
    }
    // Windows has no ownership check yet (the Codex app's pipe is unverified), so an owner is
    // always unknown, and unknown is not idle: no headless turn, the message waits in the inbox.
    if cfg!(windows) {
        return if to_queue { None } else { hold() };
    }
    let capability = codex_capability();
    let binary = match capability.path.as_deref().map(PathBuf::from) {
        Some(binary) if capability.exec_resume_supported && !is_batch_file(&binary) => binary,
        _ => return if to_queue { None } else { hold() },
    };
    let Some(dir) = wake_dir().filter(|d| rightkit_fs::private_cached::ensure_dir(d).is_ok())
    else {
        return if to_queue { None } else { hold() };
    };
    prune_wake(&dir);
    let marker = dir.join(format!("{thread}.started"));
    let spawned = now_secs();
    let under_way = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .is_some_and(|at| spawned.saturating_sub(at) < WAKE_ONE_AT_A_TIME.as_secs());
    if under_way {
        codex_hook::enqueue(thread, id, text).ok()?;
        return Some(shim::queued(
            "A turn was started in this Codex chat moments ago; the message waits in the app and is read when that turn next runs a tool or ends.",
        ));
    }
    if rightkit_fs::atomic_write_with_options(
        &marker,
        rightkit_fs::AtomicWriteOptions {
            durable: false,
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| std::io::Write::write_all(file, spawned.to_string().as_bytes()),
    )
    .is_err()
    {
        return if to_queue { None } else { hold() };
    }
    let log_path = dir.join(format!("{id}.log"));
    let launched = start_turn(
        &binary,
        thread,
        &session.cwd,
        &dir.join(format!("{id}.in")),
        &log_path,
        text,
    );
    let mut child = match launched {
        Ok(child) => child,
        Err(why) => {
            let _ = std::fs::remove_file(&marker);
            return Some(wake_failed(thread, id, text, &why));
        }
    };
    // The app has the thread loaded: its writer lock refuses a second writer. Nothing was
    // written to the thread, so the message goes to the queue row instead of failing.
    let conflict = |log: &str| to_queue && log_is_writer_conflict(log);
    let begun = Instant::now();
    let started = Receipt::delivered("Started a turn in the idle Codex chat with this message.");
    loop {
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => {
                let _ = std::fs::remove_file(&marker);
                let detail = short(&read_wake_log(&log_path));
                if conflict(&detail) {
                    return None;
                }
                let why = if detail.is_empty() {
                    format!("Codex exited ({status}) without output.")
                } else {
                    detail
                };
                return Some(wake_failed(thread, id, text, &why));
            }
            // A turn that began and ended inside the watch is as good as one seen running.
            Ok(Some(_)) => return Some(started),
            _ => {}
        }
        let heartbeat = codex_hook::hook_activity(thread);
        if let Some(h) = heartbeat
            && h.seen_secs >= spawned
            && (!h.ended || h.seen_secs > spawned)
        {
            return Some(started);
        }
        if heartbeat.is_none() && begun.elapsed() >= WAKE_QUIET_AFTER {
            let log = read_wake_log(&log_path);
            if !log_shows_error(&log) {
                return Some(started);
            }
            let _ = std::fs::remove_file(&marker);
            if conflict(&log) {
                return None;
            }
            return Some(wake_failed(thread, id, text, &short(&log)));
        }
        if begun.elapsed() >= WAKE_CONFIRM_WITHIN {
            return Some(shim::unknown(
                "A turn was started for this message but could not be confirmed.",
            ));
        }
        std::thread::sleep(CONSUME_POLL_GAP);
    }
}

/// Delivery through the Codex desktop app's own IPC socket (`~/.codex/ipc/ipc.sock`), ported
/// from the Codex TUI's IDE-context client (codex-rs `tui/src/ide_context/ipc.rs`, tag
/// rust-v0.163.0) and live-tested with `ipc_client.py`.
///
/// Framing: a `u32` little-endian byte length, then one JSON object. Handshake: a request
/// `{type:"request", requestId, sourceClientId:"initializing-client", version:0,
/// method:"initialize", params:{clientType}}`; the response's `result.clientId` is this
/// client's id for every later request. Then `thread-owner-discovery` (version 1, params
/// `{hostId:"local", conversationId}`): `resultType:"success"` carries `handledByClientId`
/// when an app window owns the thread. Then `thread-follower-start-turn` (version 2,
/// `targetClientId` = that owner, params `{conversationId, turnStart:{request:{threadId,
/// input:[{type:"text",text,text_elements:[]}]}, context:{}}}`), which starts a turn, or
/// steers an active one, inside the app. Anything the router sends this client meanwhile is
/// declined (`client-discovery-request` gets `canHandle:false`, a request gets an error).
/// Nothing is sent when the socket is missing or the handshake fails.
#[cfg(unix)]
mod app_ipc {
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    /// Budget for connect, handshake and owner discovery together, so a send never blocks long.
    pub const DISCOVERY_BUDGET: Duration = Duration::from_millis(1500);
    /// How long `thread-follower-start-turn` is waited for once sent. Writing the request counts
    /// against it too (one deadline per call, see `call`).
    pub const START_TURN_BUDGET: Duration = Duration::from_secs(5);
    /// The longest the hook, wake and queue routes take after the app route declines (a hook
    /// watch of about 3 s, or a probe of 5 s, an ownership look of 1.5 s, a 10 s wake watch and a
    /// 5 s queue run, with room).
    const FALLBACK_RESERVE: Duration = Duration::from_secs(19);
    // A message from another computer arrives over ssh (`links::run`, which gives up at
    // `REMOTE_TIMEOUT`). This route and the slowest route after it must both end inside that, or the
    // sender sees a timeout for a message that was in fact delivered or is still being sent.
    const _: () = assert!(
        DISCOVERY_BUDGET.as_millis() + START_TURN_BUDGET.as_millis() + FALLBACK_RESERVE.as_millis()
            < crate::chat::links::REMOTE_TIMEOUT.as_millis()
    );
    const MAX_FRAME: usize = 16 * 1024 * 1024;

    pub enum Outcome {
        /// The app answered owner discovery and no window owns the thread: nothing was sent. This
        /// is the only proof that no app client holds the thread.
        NoOwner,
        /// The socket, handshake or discovery did not work, so ownership is unknown: nothing was
        /// sent, and nothing may treat the thread as unowned (a second writer could be hurt).
        Unknown,
        /// The chat was no longer provably idle when the turn was about to be sent: nothing was
        /// sent (the app would steer a running turn, #365).
        Busy,
        /// The owner accepted the turn.
        Started,
        /// The owner answered with an error: nothing was started.
        Refused,
        /// The request was sent but no answer came in time: it may have been started.
        Unconfirmed,
    }

    fn socket_path() -> Option<std::path::PathBuf> {
        Some(
            rightkit_app_host::account_home()
                .join(".codex")
                .join("ipc")
                .join("ipc.sock"),
        )
    }

    fn request_id() -> String {
        let mut b = [0u8; 16];
        let ok = std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut b))
            .is_ok();
        if !ok {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            b.copy_from_slice(&n.to_le_bytes());
        }
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
        format!(
            "{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        )
    }

    /// Writes the whole frame before `deadline`: every write is bounded by what is left of it, so
    /// a peer that stops reading cannot hold the sender past the deadline (a plain `write_all`
    /// with one socket timeout restarts that timeout after every partial write).
    fn write_frame(s: &mut UnixStream, v: &Value, deadline: Instant) -> std::io::Result<()> {
        let payload = serde_json::to_vec(v)?;
        let len = u32::try_from(payload.len())
            .map_err(|_| std::io::Error::other("frame exceeds u32 length"))?;
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&payload);
        let mut at = 0;
        while at < frame.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            s.set_write_timeout(Some(left))?;
            match s.write(&frame[at..]) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => at += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        s.flush()
    }

    /// Whether the router's error answer says the request timed out (its own `timeoutMs`, or the
    /// owner not answering): the turn may have started, so this is not a refusal.
    fn router_timed_out(response: &Value) -> bool {
        let text = match &response["error"] {
            Value::String(s) => s.to_lowercase(),
            Value::Null => String::new(),
            other => other.to_string().to_lowercase(),
        };
        text.contains("timeout") || text.contains("timed out") || text.contains("timed-out")
    }

    fn read_exact_by(s: &mut UnixStream, buf: &mut [u8], deadline: Instant) -> std::io::Result<()> {
        let mut at = 0;
        while at < buf.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            s.set_read_timeout(Some(left))?;
            match s.read(&mut buf[at..]) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => at += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn read_frame(s: &mut UnixStream, deadline: Instant) -> std::io::Result<Value> {
        let mut head = [0u8; 4];
        read_exact_by(s, &mut head, deadline)?;
        let len = u32::from_le_bytes(head) as usize;
        if len > MAX_FRAME {
            return Err(std::io::Error::other("frame too large"));
        }
        let mut body = vec![0u8; len];
        read_exact_by(s, &mut body, deadline)?;
        serde_json::from_slice(&body).map_err(std::io::Error::other)
    }

    /// Send one request and wait for its response, declining whatever else is routed here.
    fn call(
        s: &mut UnixStream,
        client: &str,
        method: &str,
        version: u32,
        params: Value,
        target: Option<&str>,
        budget: Duration,
    ) -> std::io::Result<Value> {
        let deadline = Instant::now() + budget;
        let rid = request_id();
        let mut req = json!({"type": "request", "requestId": rid, "sourceClientId": client,
            "version": version, "method": method, "params": params,
            "timeoutMs": budget.as_millis() as u64});
        if let Some(t) = target {
            req["targetClientId"] = json!(t);
        }
        write_frame(s, &req, deadline)?;
        loop {
            let m = read_frame(s, deadline)?;
            match m["type"].as_str() {
                Some("response") if m["requestId"].as_str() == Some(rid.as_str()) => return Ok(m),
                Some("client-discovery-request") => {
                    write_frame(
                        s,
                        &json!({"type": "client-discovery-response",
                            "requestId": m["requestId"], "response": {"canHandle": false}}),
                        deadline,
                    )?;
                }
                Some("request") => {
                    write_frame(
                        s,
                        &json!({"type": "response", "requestId": m["requestId"],
                            "resultType": "error", "error": "no-handler-for-request"}),
                        deadline,
                    )?;
                }
                _ => {}
            }
        }
    }

    fn connect_and_discover(thread: &str) -> Option<(UnixStream, String, Option<String>)> {
        let path = socket_path()?;
        let started = Instant::now();
        let mut s = UnixStream::connect(path).ok()?;
        let left = |used: Instant| DISCOVERY_BUDGET.saturating_sub(used.elapsed());
        let init = call(
            &mut s,
            "initializing-client",
            "initialize",
            0,
            json!({"clientType": crate::chat::brand::brand().client_type}),
            None,
            left(started),
        )
        .ok()?;
        if init["resultType"].as_str() != Some("success") {
            return None;
        }
        let client = init["result"]["clientId"].as_str()?.to_string();
        let found = call(
            &mut s,
            &client,
            "thread-owner-discovery",
            1,
            json!({"hostId": "local", "conversationId": thread}),
            None,
            left(started),
        )
        .ok()?;
        if found["resultType"].as_str() != Some("success") {
            return None;
        }
        let owner = found["handledByClientId"].as_str().map(str::to_string);
        Some((s, client, owner))
    }

    /// Whether a Codex app client may hold a chat, for the headless wake (`wake_idle`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Holder {
        /// Proven: no app is listening (no socket, or nothing accepts on it), or the app answered
        /// owner discovery and no window owns the thread. Only this lets a headless writer start.
        Nobody,
        /// An app window owns the thread.
        Held,
        /// The socket, handshake or discovery failed in some other way: ownership is unknown,
        /// which is never treated as absence (Astra F4).
        Unknown,
    }

    /// Who holds `thread` in the Codex app, looked at once, within `DISCOVERY_BUDGET`.
    pub fn holder(thread: &str) -> Holder {
        let Some(path) = socket_path() else {
            return Holder::Unknown;
        };
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Holder::Nobody,
            _ => {}
        }
        match UnixStream::connect(&path) {
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                return Holder::Nobody;
            }
            Err(_) => return Holder::Unknown,
            Ok(_) => {}
        }
        match connect_and_discover(thread) {
            None => Holder::Unknown,
            Some((_, _, Some(_))) => Holder::Held,
            Some((_, _, None)) => Holder::Nobody,
        }
    }

    /// Whether the desktop app owns `thread`; if so, start a turn with `text` there.
    /// `still_idle` is asked once more after discovery and right before the turn is sent: the app
    /// call starts a turn or steers a running one, and the router offers no idle-only admission,
    /// so a chat that is no longer provably idle is never sent to (`Outcome::Busy`).
    pub fn deliver(thread: &str, text: &str, still_idle: impl FnOnce() -> bool) -> Outcome {
        let Some((mut s, client, owner)) = connect_and_discover(thread) else {
            return Outcome::Unknown;
        };
        let Some(owner) = owner else {
            return Outcome::NoOwner;
        };
        if !still_idle() {
            return Outcome::Busy;
        }
        let params = json!({"conversationId": thread, "turnStart": {
            "request": {"threadId": thread,
                "input": [{"type": "text", "text": text, "text_elements": []}]},
            "context": {}}});
        match call(
            &mut s,
            &client,
            "thread-follower-start-turn",
            2,
            params,
            Some(owner.as_str()),
            START_TURN_BUDGET,
        ) {
            Ok(r) if r["resultType"].as_str() == Some("success") => Outcome::Started,
            // The router (or the owner) timed out waiting: the turn may have started.
            Ok(r) if router_timed_out(&r) => Outcome::Unconfirmed,
            Ok(_) => Outcome::Refused,
            Err(_) => Outcome::Unconfirmed,
        }
    }
}

/// Thread ids that a running interactive Codex terminal session (`codex resume <id>`, a TUI)
/// has open. A TUI holds the thread's writer lock for as long as it runs, so no headless turn
/// can start there, and the Codex app's owner discovery never names it. A headless
/// `codex exec ...` (the host app's own wake) is not a terminal session. Only a `resume <id>` command
/// line is evidence: a TUI started on the session picker names no thread and is not found.
/// `fresh` bypasses the 15 s cache (`TERMINAL_THREADS_CACHE`) the roster's refresh and the hub's
/// five-second scan rely on. Unix only.
pub(super) fn terminal_session_threads(fresh: bool) -> Vec<String> {
    #[cfg(unix)]
    {
        /// How long the `ps` listing is reused for the listing; delivery and wake pass `fresh`.
        const TERMINAL_THREADS_CACHE: Duration = Duration::from_secs(15);
        static CACHE: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);
        if !fresh
            && let Ok(guard) = CACHE.lock()
            && let Some((at, ids)) = guard.as_ref()
            && at.elapsed() < TERMINAL_THREADS_CACHE
        {
            return ids.clone();
        }
        let args = ["-axo".to_string(), "args=".to_string()];
        let ids = run_capture(Path::new("/bin/ps"), &args, Duration::from_secs(3))
            .ok()
            .filter(|out| out.success)
            .map(|out| terminal_threads_in(&out.stdout))
            .unwrap_or_default();
        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some((Instant::now(), ids.clone()));
        }
        ids
    }
    #[cfg(not(unix))]
    {
        let _ = fresh;
        Vec::new()
    }
}

/// The thread ids in `ps` output whose command is `codex ... resume <id>` and not `exec`.
#[cfg(unix)]
fn terminal_threads_in(ps: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in ps.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(first) = words.first() else { continue };
        let program = first.rsplit('/').next().unwrap_or(first);
        if program != "codex" || words.contains(&"exec") || words.contains(&"app-server") {
            continue;
        }
        // Option values (`--model gpt-...`, `-c key=value`) also follow `resume`: only a word
        // shaped like a thread id (a UUID) counts.
        if let Some(at) = words.iter().position(|w| *w == "resume")
            && let Some(id) = words[at + 1..].iter().find(|w| is_thread_uuid(w))
            && !ids.iter().any(|known| known == id)
        {
            ids.push((*id).to_string());
        }
    }
    ids
}

/// Whether `word` has a Codex thread id's shape: 36 characters, hex digits and four dashes in
/// the UUID places.
#[cfg(unix)]
fn is_thread_uuid(word: &str) -> bool {
    word.len() == 36
        && word.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Whether a running terminal Codex session holds `thread` (see `terminal_session_threads`).
#[cfg(unix)]
fn terminal_holds(thread: &str) -> bool {
    terminal_session_threads(true).iter().any(|id| id == thread)
}

/// The app route: `Some(receipt)` when the Codex desktop app owns the thread (delivered, or
/// refused/unknown by the owner), `None` to continue with the hook, wake and queue routes
/// (setting off, stopped chat, not Unix, no socket, handshake failure, or no owner).
fn deliver_by_app(session: &LocalSession, env: &Envelope) -> Option<Receipt> {
    #[cfg(unix)]
    {
        let thread = session.id.as_str();
        if thread.is_empty() || thread.starts_with('-') {
            return None;
        }
        if !Store::open_default().is_ok_and(|store| store.codex_app_delivery()) {
            return None;
        }
        // `thread-follower-start-turn` steers a turn that is running, which aborts the owner's
        // turn and leaves the window without a turn id. Start a turn through the app
        // only when the rollout proves the last turn ended (`task_complete`, or no turn ever
        // started) and the hook shows no run in progress. Running, stopped, or any state that
        // cannot be proven idle takes the hook / queue routes, which never interrupt a turn.
        let run = thread_run_state(thread);
        if !matches!(run, RunState::Finished | RunState::Idle)
            || chat_state(run, codex_hook::hook_activity(thread)) != ChatState::Idle
        {
            return None;
        }
        // Hidden delivery: when the host app's hook runs in this chat, the full message waits in
        // its inbox and the turn starts on one short line; the hook hands the text over as added
        // context on that prompt, so the chat shows only the line and the hook's note.
        // Any chat on this computer running the hook is enough: an idle chat has not run it
        // lately by definition, and it runs it on the woken turn's prompt.
        if hooks_running() && Store::open_default().is_ok_and(|store| store.codex_hidden_delivery())
        {
            return deliver_hidden(session, env);
        }
        match app_ipc::deliver(thread, &visible_thread_text(env), || {
            chat_state_of(thread, true) == ChatState::Idle
        }) {
            // No owner, unknown ownership, or no longer idle: nothing was sent, so the hook,
            // wake (switch-gated) and queue routes apply.
            app_ipc::Outcome::NoOwner | app_ipc::Outcome::Unknown | app_ipc::Outcome::Busy => None,
            app_ipc::Outcome::Started => Some(Receipt::delivered(
                "The Codex app took the message into this chat.",
            )),
            // The owner declined: nothing was started, so the other routes still apply.
            app_ipc::Outcome::Refused => None,
            app_ipc::Outcome::Unconfirmed => Some(shim::unknown(
                "The Codex app was sent the message but did not confirm it; it may have started.",
            )),
        }
    }
    #[cfg(not(unix))]
    {
        // Windows: whether the Codex desktop app listens on a named pipe is unverified, so
        // nothing is sent and the queue route stays.
        let _ = (session, env);
        None
    }
}

/// The hidden route once a hook has run on this computer: the full message goes into the
/// hook inbox and only the one-line wake is sent, through Codex's own queue when a window holds
/// the chat (its queue watcher starts the line when idle) or headless when nobody holds it. The
/// app's start-turn (`app_ipc::deliver`) is no longer the wake here; it stays the fallback for
/// machines where no hook has run (`deliver_by_app`). The hook claims the message on the line's
/// prompt. If the wake does not start, the message stays in the inbox (`held`), never sent twice.
#[cfg(unix)]
fn deliver_hidden(session: &LocalSession, env: &Envelope) -> Option<Receipt> {
    let thread = session.id.as_str();
    let id = shim::env_id(env);
    codex_hook::enqueue(thread, id, &thread_text(env)).ok()?;
    // A turn may have started since `deliver_by_app` proved the chat idle: a wake would then
    // queue behind it. Look again; anything but idle leaves the message in the inbox for the hook.
    let state = chat_state_of(thread, true);
    // A terminal session in a state the host app cannot read (stale hook, cut or unreadable rollout)
    // still gets the queue line: Codex starts it only once the session is idle.
    if matches!(state, ChatState::Quiet | ChatState::Unknown)
        && terminal_holds(thread)
        && let Some(binary) = codex_binary().filter(|b| !is_batch_file(b))
    {
        let line = super::envelope::wake_line(&super::origin_of(env), id);
        return Some(queue_wake_line(&binary, thread, id, &line, |why: &str| {
            Receipt::held(format!(
                "{why} The message waits for this chat's next prompt or tool call."
            ))
        }));
    }
    if state != ChatState::Idle {
        return Some(shim::queued(
            "Queued behind this Codex chat's running work: a turn started meanwhile, so the message is in the app's inbox and the chat reads it through the app's hook after its next tool call or when its run ends.",
        ));
    }
    let line = super::envelope::wake_line(&super::origin_of(env), id);
    let held = |why: &str| {
        Receipt::held(format!(
            "{why} The message waits for this chat's next prompt or tool call."
        ))
    };
    // A terminal session (a running `codex resume <thread>` TUI) holds the thread's writer lock
    // and is not a Codex app client, so the app's owner discovery says nothing about it. Codex's
    // queue reaches it: an idle TUI starts a queued line within seconds (probed 2026-10-27 on
    // codex 0.164.0), and the hook hands the waiting message over on that prompt.
    if terminal_holds(thread) {
        return Some(match codex_binary() {
            Some(binary) if !is_batch_file(&binary) => {
                queue_wake_line(&binary, thread, id, &line, held)
            }
            _ => held("A terminal Codex session holds this chat, and the app cannot queue there."),
        });
    }
    match app_ipc::holder(thread) {
        // A window holds the chat: queue the line there, never a second writer.
        app_ipc::Holder::Held => match codex_binary() {
            Some(binary) if !is_batch_file(&binary) => {
                Some(queue_wake_line(&binary, thread, id, &line, held))
            }
            _ => Some(held(
                "The Codex app holds this chat, and the app cannot queue there.",
            )),
        },
        // Nobody holds the chat: a headless turn, on `codexWakeIdle` (on by default).
        app_ipc::Holder::Nobody => {
            if Store::open_default().is_ok_and(|store| store.codex_wake_idle()) {
                Some(wake_hidden_headless(session, id, &line))
            } else {
                Some(Receipt::held(
                    "This Codex chat is idle and no Codex window holds it; waking idle chats is off, so it reads the message with its next prompt.",
                ))
            }
        }
        // Ownership could not be established. No headless turn (the app or a terminal session
        // may hold the writer lock), but Codex's queue is safe for any holder: whoever has the
        // chat loaded starts the line when idle, and the message itself stays in the hook inbox,
        // delivered at most once. Never hold here without trying.
        app_ipc::Holder::Unknown => match codex_binary() {
            Some(binary) if !is_batch_file(&binary) => {
                Some(queue_wake_line(&binary, thread, id, &line, held))
            }
            _ => Some(held(
                "The app could not tell who holds this Codex chat and cannot queue there.",
            )),
        },
    }
}

/// The headless half of hidden delivery: start `codex exec resume` on the wake line for an idle
/// chat the Codex app has not loaded. The message is already in the hook inbox; a start that
/// fails leaves it there (`held`), so it is never lost or sent twice.
#[cfg(unix)]
fn wake_hidden_headless(session: &LocalSession, id: &str, line: &str) -> Receipt {
    let thread = session.id.as_str();
    let held = |why: &str| {
        Receipt::held(format!(
            "{why} The message waits for this chat's next prompt or tool call."
        ))
    };
    let Some(binary) = codex_binary() else {
        return held("Codex isn't installed here, so the chat was not woken.");
    };
    if is_batch_file(&binary) {
        return held("This Codex is a batch file, so the chat was not woken.");
    }
    let Some(dir) = wake_dir().filter(|dir| rightkit_fs::private_cached::ensure_dir(dir).is_ok())
    else {
        return held("the app's wake folder could not be made, so the chat was not woken.");
    };
    let log_path = dir.join(format!("{id}.log"));
    if let Err(why) = start_turn(
        &binary,
        thread,
        &session.cwd,
        &dir.join(format!("{id}.in")),
        &log_path,
        line,
    ) {
        return held(&why);
    }
    for _ in 0..WAKE_CLAIM_POLLS {
        std::thread::sleep(CONSUME_POLL_GAP);
        match codex_hook::outcome(thread, id) {
            Some(codex_hook::HookOutcome::Sent) => {
                return Receipt::delivered(
                    "The app woke this Codex chat and its hook handed it the message (seen receipt); the Codex app shows the turn when the chat is opened.",
                );
            }
            Some(codex_hook::HookOutcome::Refused) => {
                return shim::refused(
                    "The Codex hook refused the message: too large to hand over whole (it was not cut).",
                );
            }
            None => {}
        }
        let log = read_wake_log(&log_path);
        // The Codex app has the chat loaded (its writer lock) though no window follows it: its
        // queue watcher starts a queued line once the chat is idle, so queue the line there.
        if log_is_writer_conflict(&log) {
            return queue_wake_line(&binary, thread, id, line, held);
        }
        if log_shows_error(&log) {
            return held("Codex could not resume this chat headless.");
        }
    }
    held("The app started this Codex chat but its hook did not pick the message up yet.")
}

/// Queue the wake line for a chat the Codex app has loaded but no window follows: the app's
/// queue watcher starts it when the chat is idle (about every 10 s), and the hook hands the
/// waiting message over on that prompt.
#[cfg(unix)]
fn queue_wake_line(
    binary: &Path,
    thread: &str,
    id: &str,
    line: &str,
    held: impl Fn(&str) -> Receipt,
) -> Receipt {
    let args = [
        "queue".to_string(),
        "--thread".to_string(),
        thread.to_string(),
        "--message".to_string(),
        line.to_string(),
    ];
    match run_capture(binary, &args, QUEUE_TIMEOUT) {
        // Queued: the app starts it when the chat is idle. Not watched again, so the whole hidden
        // path stays inside the ssh sender's limit.
        // The chat is idle, so the app starts a `turn_trigger: queue` turn within seconds and
        // the hook claims the inbox file on its prompt. Say a turn started only once the
        // hook's own record says so (`codex_hook::outcome`: the `.sent` mark written after its
        // output, which is when the sender's `seen` receipt is reported); never from the inbox file
        // merely being gone, which a failed claim or a refusal also causes.
        Ok(out) if out.success && queued_id(&out.stdout).is_some() => {
            for _ in 0..QUEUE_TURN_POLLS {
                std::thread::sleep(CONSUME_POLL_GAP);
                match codex_hook::outcome(thread, id) {
                    Some(codex_hook::HookOutcome::Sent) => {
                        return Receipt::delivered(
                            "The Codex hook handed the message to the chat (seen receipt); the turn was started by the app's queue line or was already running.",
                        );
                    }
                    Some(codex_hook::HookOutcome::Refused) => {
                        return shim::refused(
                            "The Codex hook refused the message: too large to hand over whole (it was not cut).",
                        );
                    }
                    None => {}
                }
            }
            shim::queued(&super::brand::branded(
                "Handed to Codex's queue for this chat; no turn seen within 8 s: it starts when the session holding the chat is next idle, or with your next message; check `@cli@ chat receipts`.",
            ))
        }
        _ => held("Codex did not take the app's queue line for this chat."),
    }
}

/// How long a queued wake line is watched for the hook's claim (32 polls of 250 ms = 8 s).
#[cfg(unix)]
const QUEUE_TURN_POLLS: u32 = 32;

/// How long the headless wake watches for the hook's claim (40 polls of 250 ms): `codex exec`
/// needs a few seconds to load the thread before its first prompt hook runs.
#[cfg(unix)]
const WAKE_CLAIM_POLLS: u32 = 40;

/// Queue the message into the Codex thread. Never an `Err` for "can't push": the
/// receipt says why (`unsupported`, `refused`, `unknown`), and the caller keeps the message.
pub fn deliver(
    store: &Store,
    session: &LocalSession,
    env: &Envelope,
) -> Result<Receipt, ChatError> {
    if let Some(receipt) = deliver_by_app(session, env) {
        return Ok(receipt);
    }
    if let Some(receipt) = deliver_by_hook(session, env) {
        return Ok(receipt);
    }
    let capability = codex_capability();
    let Some(binary) = capability.path.as_deref().map(PathBuf::from) else {
        return Ok(shim::unsupported(
            "Codex isn't installed here. Install Codex.",
        ));
    };
    if !capability.queue_supported {
        let version = capability.version.as_deref().unwrap_or("unknown version");
        let why = capability.error.as_deref().unwrap_or("");
        return Ok(shim::unsupported(&format!(
            "This Codex ({version}) can't queue messages into a thread. Install or update \
             Codex to 0.16x or newer. {why}"
        )));
    }
    let thread = session.id.as_str();
    if thread.is_empty() {
        return Ok(shim::unsupported("This Codex chat has no thread id."));
    }
    if thread.starts_with('-') {
        return Ok(shim::refused("This Codex chat's thread id is not valid."));
    }
    // Hidden delivery on this path too (Windows, and any chat the app route did not take):
    // when Codex hooks run on this computer, the full message waits in the host app's hook inbox and
    // only the one-line wake is queued, so the chat shows that line and the hook hands the
    // message over as added context. Before, the whole message was queued as a visible turn.
    let hidden = hooks_running()
        && Store::open_default().is_ok_and(|s| s.codex_hidden_delivery())
        && codex_hook::enqueue(thread, shim::env_id(env), &thread_text(env)).is_ok();
    let text = if hidden {
        super::envelope::wake_line(&super::origin_of(env), shim::env_id(env))
    } else {
        visible_thread_text(env)
    };
    if is_batch_file(&binary) {
        // Neither `codex queue` nor a headless wake can safely take a message through a batch
        // file (Windows `codex.cmd`). A chat whose hook ran keeps it in the hook inbox, so it
        // reads the message with its next prompt or tool call; otherwise nothing can take it.
        let hook_live = codex_hook::hook_activity(thread)
            .is_some_and(|h| h.age_secs <= HOOK_SEEN_WITHIN.as_secs());
        if hook_live && codex_hook::enqueue(thread, shim::env_id(env), &text).is_ok() {
            return Ok(Receipt::held(
                "This Codex is a batch file, so the app cannot start a turn in the chat; the message waits in the app's inbox and the chat reads it at its next prompt or tool call. Install the Codex app, which ships codex.exe, to wake idle chats.",
            ));
        }
        return Ok(shim::unsupported(
            "This Codex is a batch file, which can't safely take a message. Install \
             the Codex app, which ships codex.exe.",
        ));
    }
    if text.len() > ARGV_LIMIT {
        // Too long for `codex queue --message`: the stdin route has no such limit. An idle
        // chat is woken with it (when waking is allowed) or the message waits in the hook
        // inbox; only a message neither route can take is `unsupported`.
        if let Some(receipt) = wake_idle(session, shim::env_id(env), &text, false) {
            return Ok(receipt);
        }
        return Ok(shim::unsupported(&format!(
            "message too long for Codex's command line (limit {ARGV_LIMIT} bytes)"
        )));
    }
    let args = [
        "queue".to_string(),
        "--thread".to_string(),
        thread.to_string(),
        "--message".to_string(),
        text,
    ];
    Ok(match run_capture(&binary, &args, QUEUE_TIMEOUT) {
        Ok(done) if done.success => match queued_id(&done.stdout) {
            Some(id) => {
                let receipt = queued_receipt(thread, id);
                // Still in Codex's queue: watch it, so the sender hears when the thread takes it.
                if matches!(receipt.state, ReceiptState::Queued | ReceiptState::Held) {
                    crate::chat::receipts::watch_codex_queue(store, env, id, &session.name);
                }
                receipt
            }
            None => shim::unknown(
                "Codex exited 0 but did not print the expected \"Queued message\" line, so \
                 the message may not be queued.",
            ),
        },
        Ok(done) => {
            let detail = short(&done.stderr);
            shim::refused(&if detail.is_empty() {
                format!("Codex couldn't queue the message ({}).", done.status)
            } else {
                format!("Codex couldn't queue the message: {detail}")
            })
        }
        Err(RunFailure::NotStarted(reason)) => shim::unsupported(&reason),
        Err(RunFailure::Ambiguous(reason)) => shim::unknown(&format!(
            "{reason} The message may or may not have been queued."
        )),
    })
}
