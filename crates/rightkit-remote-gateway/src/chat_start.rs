//! `chat_start`: ChatGPT starts a new Codex or Claude chat in an approved folder with a prompt,
//! through the ChatGPT gateway only. See `gptchat` for the sender rules.
//!
//! * **One named tool, never implicit.** Starting a chat is its own tool with its own approval;
//!   no other tool starts one, and the host app's rule that a chat is never created without the user's
//!   express say-so is met by a local approval for each start.
//! * **Approval.** Every call without an approval answers `approval_required` with a request id.
//!   The request summary names the kind, the folder's full path, the permissions note, the
//!   character count and then the whole prompt (up to `PROMPT_SHOWN` characters; a longer prompt
//!   or one with control characters is refused, never cut); the
//!   approval is bound to the kind, the resolved folder and the hash of the exact prompt, so a
//!   different prompt or folder needs a new one. It is used once. Only a local interface answers
//!   it (`approvals`).
//! * **The folder** must be inside an approved folder (`remoteRoots`, the same boundary every
//!   file tool uses) and a folder.
//! * **What runs.** `codex exec --skip-git-repo-check -` (a new thread) or `claude -p` (print
//!   mode, a new session), the prompt on a private stdin file. the host app passes no permission,
//!   sandbox or approval flag, so the chat runs under that tool's own defaults and the user's own
//!   configuration; it does not widen them. The environment is scrubbed to the terminal tools'
//!   allowlist, so the gateway's own environment (the tunnel's key included) is not handed on.
//!   The process is detached: it outlives the gateway and nothing here stops it.
//! * **The prompt** begins with the unverified-sender stamp and says how to report back:
//!   `pulse chat send ChatGPT <text>`, which `chat_inbox` reads.
//! * **Not recorded.** The log of the run is a private file in the host app's state folder (`chat-start/`)
//!   that no remote tool can read (`confine::protected`). Activity keeps only the tool and the
//!   outcome.
//!
//! Unproven: that Codex and Claude's non-interactive modes behave this way on every installed
//! version, that a CLI which authenticates only through an environment variable works with the
//! scrubbed environment, and anything on Windows.

mod imp {
    use super::super::gptchat::{PROMPT_MAX, STAMP, invalid, need_enabled, need_gateway, text_of};
    use super::super::settings::Settings;
    use super::super::{Origin, Refusal, Result, approvals, confine, files_write, term};
    use rightkit_child_process::OwnedCommand;
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    /// How long the start waits to see whether the program died at once.
    const WATCH: Duration = Duration::from_millis(1500);
    /// Most characters of prompt `chat_start` takes (`PROMPT_MAX` bytes at most): what the approval can show in full (`approvals::SUMMARY_MAX`).
    const PROMPT_SHOWN: usize = PROMPT_MAX;

    /// Chats this process started and can still name; the oldest is forgotten past this many.
    const STARTED_MAX: usize = 32;

    struct Started {
        id: String,
        identity: rightkit_child_process::AdoptedIdentity,
    }

    fn started() -> &'static std::sync::Mutex<Vec<Started>> {
        static STARTED: std::sync::OnceLock<std::sync::Mutex<Vec<Started>>> =
            std::sync::OnceLock::new();
        STARTED.get_or_init(|| std::sync::Mutex::new(Vec::new()))
    }

    /// `chat_stop`. `args`: `startId` (the `startId` `chat_start` answered). Ends the chat this
    /// gateway process started and everything it started: its descendants first, then the program,
    /// each only while it is still the process that was recorded at the start. A chat started by
    /// an earlier gateway process, or by anything else, is not found (`not_found`); no approval is
    /// asked, because it only ends work that an approved `chat_start` began.
    pub fn stop(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
        need_gateway(origin, "chat_stop")?;
        need_enabled(settings)?;
        let id = text_of(args, "startId", 64)?.trim().to_string();
        let found = {
            let mut all = started().lock().unwrap_or_else(|e| e.into_inner());
            all.iter()
                .position(|s| s.id == id)
                .map(|at| all.remove(at))
        }
        .ok_or_else(|| {
            Refusal::new(
                "not_found",
                "no chat started by this gateway has that startId (it is gone, or an earlier gateway process started it)",
            )
        })?;
        let grace = Duration::from_millis(1500);
        // A leader that already ended has no descendants to list; that is not a failure.
        let descendants = rightkit_child_process::sweep_descendants(&found.identity, grace)
            .ok()
            .unwrap_or_default();
        let leader = rightkit_child_process::sweep(std::slice::from_ref(&found.identity), grace)
            .map_err(|e| Refusal::new("stop_failed", format!("could not stop it: {}", e.kind())))?;
        let ended = leader.survivors.is_empty() && descendants.survivors.is_empty();
        Ok(json!({
            "ok": true,
            "startId": id,
            "stopped": ended,
            "alreadyEnded": !leader.gone.is_empty() && leader.terminated.is_empty(),
            "survivors": leader.survivors.len() + descendants.survivors.len(),
        }))
    }

    fn home() -> Option<PathBuf> {
        Some(rightkit_app_host::account_home())
    }

    /// The Claude CLI at its install locations. Never a program from `$RIGHTKIT_CLAUDE_BIN` or the `PATH`.
    fn claude_binary() -> Option<PathBuf> {
        let name = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(home) = home() {
            candidates.push(home.join(".claude").join("local").join(name));
            candidates.push(home.join(".local").join("bin").join(name));
        }
        if cfg!(windows)
            && let Some(appdata) = std::env::var_os("APPDATA").filter(|v| !v.is_empty())
        {
            // npm's global folder, where the Claude CLI installs on Windows.
            let npm = PathBuf::from(appdata).join("npm");
            candidates.push(npm.join("claude.cmd"));
            candidates.push(npm.join("claude.exe"));
        }
        if cfg!(not(windows)) {
            candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
            candidates.push(PathBuf::from("/usr/local/bin/claude"));
        }
        candidates.into_iter().find(|c| c.is_file())
    }

    /// The CLI beside this program, else the bare name, for the report-back line.
    fn app_command() -> String {
        let cli = rightkit_chat_bridge::chat::brand::brand().cli.as_str();
        let windows_name = format!("{cli}.exe");
        let name = if cfg!(windows) {
            windows_name.as_str()
        } else {
            cli
        };
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| cli.to_string())
    }

    /// `chat_start`. `args`: `kind` (`codex` or `claude`), `folder` (inside an approved folder),
    /// `prompt`, `approval` (the id of a request the user approved).
    pub fn start(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
        need_gateway(origin, "chat_start")?;
        need_enabled(settings)?;
        let kind = match args["kind"].as_str() {
            Some("codex") => "codex",
            Some("claude") => "claude",
            _ => return Err(invalid("kind is codex or claude")),
        };
        let prompt = text_of(args, "prompt", PROMPT_MAX)?;
        let folder = text_of(args, "folder", 4096)?;
        let approval = match &args["approval"] {
            Value::Null => None,
            Value::String(id) => Some(id.as_str()),
            _ => return Err(invalid("approval must be a string")),
        };
        let roots = confine::approved_roots(settings)?;
        let resolved = confine::resolve(&roots, folder)?;
        // Opened without following a link and checked to be a folder.
        let handle = confine::open_dir(&roots[resolved.root], &resolved.path)?;
        let binary = match kind {
            "codex" => rightkit_chat_bridge::chat::deliver_codex::codex_binary(),
            _ => claude_binary(),
        }
        .ok_or_else(|| {
            Refusal::new(
                "not_installed",
                format!("no {kind} command-line program was found on this computer"),
            )
        })?;

        let shown = confine::display(&resolved.path);
        let prompt_hash = rightkit_fs::digest::sha256_bytes(prompt.as_bytes());
        let bind = approvals::bind_of(&[
            b"chat.start",
            origin.as_str().as_bytes(),
            kind.as_bytes(),
            shown.as_bytes(),
            prompt_hash.as_bytes(),
        ]);
        // The approval shows the whole prompt, last, after the facts a prompt could not forge. A
        // prompt that cannot be shown in full is refused, never cut.
        let chars = prompt.chars().count();
        if chars > PROMPT_SHOWN {
            return Err(invalid(format!(
                "prompt is at most {PROMPT_SHOWN} characters for chat_start, so the user can read all of it in the approval (this one has {chars})"
            )));
        }
        let shown_prompt = prompt
            .replace("\r\n", "\n")
            .replace('\n', "\u{23ce}")
            .replace('\t', " ");
        if shown_prompt.chars().any(char::is_control) {
            return Err(invalid(
                "prompt holds control characters that cannot be shown",
            ));
        }
        let summary = format!(
            "start a new {kind} chat in the folder {shown} for ChatGPT (unverified); it runs under {kind}'s own default permissions; the whole prompt follows ({chars} characters, line breaks shown as \u{23ce}): \"{shown_prompt}\""
        );
        if summary.chars().count() > approvals::SUMMARY_MAX {
            return Err(invalid(
                "the folder path and prompt are too long to show in full in the approval; use a shorter prompt",
            ));
        }
        files_write::act_gate(origin, "chat.start", &bind, &summary, approval)?;
        // The folder is checked again after the approval: a swap for another folder is refused.
        if !confine::still_here(&handle, &resolved.path) {
            return Err(Refusal::new(
                "source_changed",
                "the folder changed after the approval was asked for; nothing was started",
            ));
        }

        let state = rightkit_app_host::state_directory()
            .map_err(|_| Refusal::new("spawn_failed", "the app's state folder is not available"))?;
        let dir = state.join("chat-start");
        let _ = rightkit_fs::ensure_private_dir(&dir);
        let id: String = approvals::bind_of(&[
            kind.as_bytes(),
            prompt_hash.as_bytes(),
            &rightkit_chat_bridge::chat::envelope::now_ms().to_be_bytes()[..],
        ])[..16]
            .to_string();
        let stdin_path = dir.join(format!("{id}.in"));
        let log_path = dir.join(format!("{id}.log"));
        let full = format!(
            "{STAMP}\n\nWhen you have finished, report the result to ChatGPT with: {} chat send ChatGPT \"<short summary>\" (it reads that with chat_inbox).\n\n{prompt}",
            app_command()
        );
        rightkit_fs::atomic_write_with_options(
            &stdin_path,
            rightkit_fs::AtomicWriteOptions {
                durable: false,
                privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                ..Default::default()
            },
            |file| std::io::Write::write_all(file, full.as_bytes()),
        )
        .map_err(|e| {
            Refusal::new(
                "spawn_failed",
                format!(
                    "could not stage the prompt: {}",
                    std::io::Error::from(e).kind()
                ),
            )
        })?;
        let failed = |why: &str| Refusal::new("spawn_failed", why.to_string());
        let input = std::fs::File::open(&stdin_path)
            .map_err(|_| failed("could not read the staged prompt"))?;
        let log = rightkit_fs::open_private_append(&log_path)
            .map_err(|_| failed("could not open the run's log"))?;
        let errors = log
            .try_clone()
            .map_err(|_| failed("could not open the run's log"))?;

        let mut command = OwnedCommand::new(&binary);
        match kind {
            "codex" => {
                command
                    .command_mut()
                    .args(["exec", "--skip-git-repo-check", "-"]);
            }
            _ => {
                command.command_mut().arg("-p");
            }
        }
        term::scrubbed(&mut command);
        command
            .command_mut()
            .current_dir(&resolved.path)
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(errors));
        let mut child = command.spawn_uncontained_detached().map_err(|e| {
            Refusal::new("spawn_failed", format!("could not start it: {}", e.kind()))
        })?;
        let pid = child.id();
        // Remembered with its identity, so `chat_stop` can end exactly this program and what it
        // started, never a process that later got the same pid.
        if let Some(identity) = std::num::NonZeroU32::new(pid)
            .and_then(|nz| rightkit_child_process::AdoptedIdentity::capture(nz).ok())
            .flatten()
        {
            let mut all = started().lock().unwrap_or_else(|e| e.into_inner());
            if all.len() >= STARTED_MAX {
                all.remove(0);
            }
            all.push(Started {
                id: id.clone(),
                identity,
            });
        }
        let begun = Instant::now();
        let mut ended = None;
        while begun.elapsed() < WATCH {
            match child.try_wait() {
                Ok(Some(status)) => {
                    ended = Some(status);
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        // The staged prompt is not kept: removed now when the program is already gone, else a
        // minute from now, long after it has read its standard input.
        if ended.is_some() {
            let _ = std::fs::remove_file(&stdin_path);
        } else {
            let staged = stdin_path.clone();
            let _ = std::thread::Builder::new()
                .name("rightkit-chat-start-cleanup".into())
                .spawn(move || {
                    std::thread::sleep(Duration::from_secs(60));
                    let _ = std::fs::remove_file(staged);
                });
        }
        if let Some(status) = ended
            && !status.success()
        {
            return Err(Refusal::new(
                "spawn_failed",
                format!(
                    "{kind} exited at once ({status}); check that it is installed and signed in"
                ),
            ));
        }
        // The child outlives this call, so a waiter thread reaps it when it ends (no zombie).
        // If the gateway exits first the thread goes with it and the child is reparented.
        if ended.is_none() {
            let _ = std::thread::Builder::new()
                .name("rightkit-chat-start-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
        Ok(json!({
            "ok": true,
            "started": true,
            "kind": kind,
            "startId": id,
            "pid": pid,
            "finished": ended.is_some(),
            "note": rightkit_chat_bridge::chat::brand::branded("The chat runs on its own. It reports back with `@cli@ chat send ChatGPT`; read that with chat_inbox."),
        }))
    }
}

pub use imp::{start, stop};
