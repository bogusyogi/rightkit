//! End-to-end TUI journeys: the REAL `coderight` binary runs inside a PTY
//! (ConPTY on Windows), its terminal output is parsed by a real VT100
//! emulator, keystrokes are sent as a user would type them, and an
//! independent scripted OpenAI-compatible provider records what the MODEL
//! actually receives. Every test owns its home, workspace and provider.

use crate::support::scripted_provider;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use scripted_provider::{ScriptedProvider, Step};
use serde::Deserialize;
use serde_json::{json, value::RawValue, Value};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const ROWS: u16 = 36;
const COLS: u16 = 120;
const WAIT: Duration = Duration::from_secs(120);
const MODEL: &str = "deepseek/deepseek-v4-pro";

struct Tui {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    home: TempDir,
    workspace: TempDir,
}

impl Tui {
    fn launch(provider: &ScriptedProvider, extra_args: &[&str]) -> Tui {
        let home = TempDir::new().expect("temp home");
        let workspace = TempDir::new().expect("temp workspace");
        let cfg = crate::common::app_root(home.path());
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("config.toml"),
            format!(
                "[model]\ndefault-model = \"{MODEL}\"\n\n[provider-routes.qual-fixture]\n\
                 display-name = \"Qualification fixture\"\nbase-url = \"{}\"\nmodel = \"{MODEL}\"\n\
                 api-key-required = false\n",
                provider.base_url()
            ),
        )
        .unwrap();
        git(workspace.path(), &["init", "-q", "-b", "e2e-branch"]);
        git(
            workspace.path(),
            &["config", "user.email", "e2e@example.test"],
        );
        git(workspace.path(), &["config", "user.name", "e2e"]);
        std::fs::write(workspace.path().join("README.md"), "e2e\n").unwrap();
        git(workspace.path(), &["add", "."]);
        git(workspace.path(), &["commit", "-q", "-m", "init"]);

        // STATUS D2: an untrusted folder prints a first-launch trust notice.
        // Pre-trust every spelling of the temp workspace so journeys that are
        // not about trust see an unperturbed transcript.
        let trust_store = cfg.join("workspace-trust.json");
        {
            let mut roots = vec![workspace.path().to_string_lossy().to_string()];
            if let Ok(canon) = std::fs::canonicalize(workspace.path()) {
                roots.push(canon.to_string_lossy().to_string());
            }
            let mut projects = serde_json::Map::new();
            for root in roots {
                let plain = root.strip_prefix(r"\\?\").unwrap_or(&root).to_string();
                for r in [root.clone(), plain] {
                    projects.insert(
                        r.trim().replace('\\', "/"),
                        json!({"trust": "trusted", "approvedBy": "user", "updatedAt": "2026-01-01T00:00:00Z"}),
                    );
                }
            }
            std::fs::write(
                &trust_store,
                serde_json::to_string_pretty(&json!({ "projects": projects })).unwrap(),
            )
            .unwrap();
        }

        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_coderight"));
        cmd.env("RIGHTKIT_CONTEXT", "off");
        cmd.args(["--model", &format!("qual-fixture::{MODEL}")]);
        // Keep the built-in context7 npx MCP server out of TUI journeys (see tui_harness.rs).
        cmd.args(extra_args);
        cmd.cwd(workspace.path());
        // Isolation: identical to the other black-box suites.
        cmd.env(
            crate::common::suite_env(home.path()).0,
            home.path().as_os_str(),
        );
        for (k, v) in [
            ("RIGHTKIT_DISABLE_DEFAULT_MCP", std::ffi::OsStr::new("1")),
            ("RIGHTKIT_DISABLE_VENDOR_CLIS", std::ffi::OsStr::new("1")),
            ("HOME", home.path().as_os_str()),
            ("USERPROFILE", home.path().as_os_str()),
            ("XDG_CONFIG_HOME", home.path().join("config").as_os_str()),
            ("XDG_DATA_HOME", home.path().join("data").as_os_str()),
        ] {
            cmd.env(k, v);
        }
        cmd.env("RIGHTKIT_WORKSPACE_TRUST_STORE", &trust_store);
        cmd.env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1");
        cmd.env("RIGHTKIT_QUAL_ISOLATED_MEMBRANE", "1");
        cmd.env("TERM", "xterm-256color");
        for k in [
            "RIGHTKIT_DAEMON_URL",
            "RIGHTKIT_DAEMON_TOKEN",
            "RIGHTKIT_SERVE_FORCE",
        ] {
            cmd.env_remove(k);
        }
        let child = pty
            .slave
            .spawn_command(cmd)
            .expect("spawn coderight in pty");
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().expect("pty reader");
        let writer = pty.master.take_writer().expect("pty writer");
        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 2000)));
        let sink = parser.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().process(&buf[..n]);
            }
        });
        Tui {
            parser,
            writer,
            child,
            _master: pty.master,
            home: home,
            workspace,
        }
    }

    /// Scrollback plus the visible viewport, so streamed history that scrolled
    /// above the viewport is still searchable.
    fn screen_text(&self) -> String {
        let mut p = self.parser.lock().unwrap();
        p.screen_mut().set_scrollback(usize::MAX);
        let max = p.screen().scrollback();
        let mut lines: Vec<String> = Vec::new();
        for offset in (1..=max).rev() {
            p.screen_mut().set_scrollback(offset);
            if let Some(row) = p.screen().rows(0, COLS).next() {
                lines.push(row.trim_end().to_string());
            }
        }
        p.screen_mut().set_scrollback(0);
        lines.extend(p.screen().rows(0, COLS).map(|r| r.trim_end().to_string()));
        lines.join("\n")
    }

    fn viewport(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write keys");
        self.writer.flush().unwrap();
    }

    fn type_text(&mut self, text: &str) {
        self.send(text.as_bytes());
    }

    fn enter(&mut self) {
        self.send(b"\r");
    }

    /// Poll the emulated screen until `pred` holds; panic with the full
    /// screen when it never does.
    fn wait_for(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.screen_text();
            if pred(&text) {
                return text;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}\n--- screen ---\n{text}\n--------------");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_for_text(&self, needle: &str) -> String {
        self.wait_for(&format!("{needle:?}"), |s| s.contains(needle))
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = rightkit_child_process::command_std("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

const ESC: &[u8] = b"\x1b";
const UP: &[u8] = b"\x1b[A";
const CTRL_X: &[u8] = b"\x18";
const TAB: &[u8] = b"\t";

impl Tui {
    /// Type a line the way a person does: the composer treats an Enter that
    /// arrives inside a fast burst of characters as a pasted newline, so the
    /// pause before Enter is part of the simulated typing, not a sync wait.
    fn submit(&mut self, text: &str) {
        self.type_text(text);
        // Match the shared harness: the child must consume typed input before
        // the human pause starts, especially under shared-build-pool load.
        let needle: String = text.chars().filter(|c| *c != '\n').take(16).collect();
        self.wait_viewport("typed prompt", |view| view.contains(&needle));
        std::thread::sleep(Duration::from_millis(350));
        self.enter();
    }

    fn wait_viewport(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let view = self.viewport();
            if pred(&view) {
                return view;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what}\n--- screen ---\n{}\n--------------",
                    self.screen_text()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_requests(&self, provider: &ScriptedProvider, n: usize) {
        let deadline = Instant::now() + WAIT;
        while provider.request_count() < n {
            if Instant::now() > deadline {
                panic!(
                    "provider saw {} request(s), expected {n}\n--- screen ---\n{}\n--------------",
                    provider.request_count(),
                    self.screen_text()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn content_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn messages(request: &Value) -> Vec<Value> {
    request["messages"].as_array().cloned().unwrap_or_default()
}

fn user_texts(request: &Value) -> Vec<String> {
    messages(request)
        .iter()
        .filter(|m| m["role"] == "user")
        .map(content_text)
        .collect()
}

fn last_user_text(request: &Value) -> String {
    user_texts(request).pop().unwrap_or_default()
}

/// The ctx percentage the footer shows (`NN% ctx`).
fn footer_ctx_percent(view: &str) -> Option<u32> {
    let end = view.find("% ctx")?;
    let digits: String = view[..end]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

fn has_card(view: &str) -> bool {
    view.contains("Approve shell command")
}

fn card_ready(view: &str) -> bool {
    has_card(view) && view.contains("esc reject")
}

fn shell_call(id: &str, command: &str) -> Step {
    Step::tool(id, "shell", json!({ "command": command }))
}

// ---------------------------------------------------------------------------
// 1. Startup -> prompt -> streamed answer -> footer, and what the model got.
// ---------------------------------------------------------------------------
#[test]
fn startup_prompt_streams_answer_and_provider_receives_user_text() {
    let provider = ScriptedProvider::start(vec![Step::text("The e2e answer is forty-two.")
        .with_reasoning("weighing the question carefully")]);
    let mut tui = Tui::launch(&provider, &[]);
    let boot = tui.wait_viewport("ready footer with branch, mode and model", |v| {
        v.contains("e2e-branch") && v.contains("Ask") && v.contains(MODEL)
    });
    assert!(
        boot.contains("Ask anything"),
        "composer placeholder missing:\n{boot}"
    );

    tui.submit("hello from the e2e user");
    tui.wait_for_text("forty-two");
    tui.wait_viewport("answer frame complete", |view| {
        view.contains("Ask anything") && footer_ctx_percent(view).is_some()
    });
    let screen = tui.screen_text();
    println!("{screen}");
    // The answer renders above the composer: the answer row precedes the composer rows.
    let answer_row = screen
        .lines()
        .position(|l| l.contains("forty-two"))
        .unwrap();
    let composer_row = screen
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains('▌'))
        .map(|(i, _)| i)
        .last()
        .unwrap();
    assert!(
        answer_row < composer_row,
        "answer must render above the composer:\n{screen}"
    );
    assert!(
        screen.contains("hello from the e2e user"),
        "the prompt is echoed in the transcript:\n{screen}"
    );

    let requests = provider.requests();
    assert_eq!(requests.len(), 1, "exactly one provider call: {requests:?}");
    assert_eq!(requests[0]["stream"], true);
    assert!(
        last_user_text(&requests[0]).contains("hello from the e2e user"),
        "the model must receive the typed prompt: {:?}",
        user_texts(&requests[0])
    );
    let footer = tui.viewport();
    assert!(
        footer.contains("e2e-branch") && footer.contains(MODEL),
        "{footer}"
    );
}

// ---------------------------------------------------------------------------
// 2. Tool call with approval -> run -> result reaches the model -> final answer.
// ---------------------------------------------------------------------------
#[test]
fn shell_tool_call_requires_approval_then_result_reaches_model() {
    let provider = ScriptedProvider::start(vec![
        shell_call("call_e2e_1", "echo e2e-tool-output-7731"),
        Step::text("All done: the command printed its marker."),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.wait_viewport("ready", |v| v.contains("e2e-branch"));
    tui.submit("run the marker command");

    // The card repaints over a viewport that just grew; the first frame that
    // carries its header can still be mid-paint, so wait for its command row.
    let card = tui.wait_viewport("approval card", |v| {
        card_ready(v) && v.contains("e2e-tool-output-7731")
    });
    println!("{card}");
    assert!(
        card.contains("e2e-tool-output-7731"),
        "card shows the command:\n{card}"
    );
    assert!(card.contains("Allow once"), "{card}");
    assert_eq!(
        provider.request_count(),
        1,
        "nothing may run or be re-sent before approval"
    );

    tui.enter(); // default selection: 1 Allow once
    tui.wait_for_text("All done: the command printed its marker.");
    let after = tui.viewport();
    assert!(
        !has_card(&after),
        "card must be gone after the decision:\n{after}"
    );

    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "tool result drives exactly one follow-up call"
    );
    let msgs = messages(&requests[1]);
    let assistant_idx = msgs
        .iter()
        .position(|m| m["role"] == "assistant" && m["tool_calls"][0]["id"] == "call_e2e_1")
        .unwrap_or_else(|| panic!("follow-up must replay the assistant tool call: {msgs:?}"));
    let tool = &msgs[assistant_idx + 1];
    assert_eq!(tool["role"], "tool", "{msgs:?}");
    assert_eq!(tool["tool_call_id"], "call_e2e_1");
    assert!(
        content_text(tool).contains("e2e-tool-output-7731"),
        "the model must receive the real command output: {tool}"
    );
}

// ---------------------------------------------------------------------------
// 3. Esc on the approval card rejects, stops the turn, hands back the composer.
// ---------------------------------------------------------------------------
#[test]
fn esc_on_approval_card_rejects_and_stops_the_turn() {
    let provider = ScriptedProvider::start(vec![
        shell_call("call_e2e_rej", "echo rejected > e2e-reject-marker.txt"),
        Step::text("understood, doing something else"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.wait_viewport("ready", |v| v.contains("e2e-branch"));
    tui.submit("create the marker file");
    tui.wait_viewport("approval card", card_ready);

    tui.send(ESC);
    let view = tui.wait_viewport("card dismissed with redirect hint", |v| {
        !has_card(v) && !v.contains("esc reject") && v.contains("Tell ")
    });
    println!("{view}");
    assert!(
        view.contains("Tell "),
        "composer placeholder asks what to do instead:\n{view}"
    );
    assert!(!view.contains("Working"), "turn must have stopped:\n{view}");

    // Settle: a rejected call must never execute or be re-sent as approved.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        provider.request_count(),
        1,
        "model must not be re-called after Esc"
    );
    assert!(
        !tui.workspace.path().join("e2e-reject-marker.txt").exists(),
        "rejected command must not have run"
    );

    // The composer is focused: the redirect goes to the model as the next turn,
    // and nothing in it claims the rejected command succeeded.
    tui.submit("do something else instead");
    tui.wait_for_text("understood, doing something else");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(last_user_text(&requests[1]).contains("do something else instead"));
    for m in messages(&requests[1]) {
        if m["role"] == "tool" {
            let text = content_text(&m);
            assert!(
                !text.contains("rejected >"),
                "no approved result may exist: {text}"
            );
        }
    }
    assert!(!tui.workspace.path().join("e2e-reject-marker.txt").exists());
}

// ---------------------------------------------------------------------------
// 4a. Queue semantics during an in-flight turn.
// ---------------------------------------------------------------------------
#[test]
fn queue_edit_remove_then_esc_sends_remaining_in_order() {
    let provider = ScriptedProvider::start(vec![
        // Streaming commits whole Markdown blocks only (see
        // tui_e2e_render_qualification), so the first chunk must end a paragraph.
        Step::text("first-half-line\n\nsecond-half-line-and-the-rest").held(),
        Step::text("queued messages were handled"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.wait_viewport("ready", |v| v.contains("e2e-branch"));
    tui.submit("start the long turn");
    tui.wait_requests(&provider, 1);
    tui.wait_for_text("first-half-line"); // streaming is live; the response is held open
    tui.wait_viewport("turn in flight", |v| v.contains("esc cancel"));

    tui.submit("queued alpha");
    tui.submit("queued bravo");
    tui.submit("queued charlie");
    let view = tui.wait_viewport("three queued", |v| v.contains("3 queued"));
    println!("{view}");
    assert_eq!(
        provider.request_count(),
        1,
        "queued messages must not be sent while the turn runs"
    );

    // Up pulls the newest queued message back into the composer for editing.
    tui.send(UP);
    tui.wait_viewport("charlie back in composer, two left queued", |v| {
        v.contains("queued charlie") && v.contains("2 queued")
    });
    tui.type_text(" EDITED");
    std::thread::sleep(Duration::from_millis(350));
    tui.enter();
    tui.wait_viewport("edited message re-queued", |v| v.contains("3 queued"));
    // ctrl+x on the now-empty prompt removes the newest queued message.
    tui.send(CTRL_X);
    tui.wait_viewport("one removed", |v| v.contains("2 queued"));
    assert_eq!(provider.request_count(), 1);

    // Esc on an empty prompt stops the turn and sends what is left, oldest first.
    tui.send(ESC);
    tui.wait_for_text("queued messages were handled");
    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "exactly one follow-up carrying the queue"
    );
    let sent = last_user_text(&requests[1]);
    let a = sent
        .find("queued alpha")
        .unwrap_or_else(|| panic!("alpha missing: {sent}"));
    let b = sent
        .find("queued bravo")
        .unwrap_or_else(|| panic!("bravo missing: {sent}"));
    assert!(a < b, "oldest first: {sent}");
    assert!(
        !sent.contains("charlie"),
        "removed/edited item must never be sent: {sent}"
    );
    let all = serde_json::to_string(&requests).unwrap();
    assert!(
        !all.contains("EDITED"),
        "removed edit must not reach the model"
    );
}

// ---------------------------------------------------------------------------
// 4b. Tab pushes text into the running turn.
// ---------------------------------------------------------------------------
#[test]
fn tab_steers_text_into_the_running_turn() {
    let provider = ScriptedProvider::start(vec![
        shell_call("call_e2e_steer", "echo steer-window").held(),
        Step::text("steered answer delivered"),
    ]);
    // --yolo: no approval card, so the tool round trip is the turn's second model call.
    let mut tui = Tui::launch(&provider, &["--yolo"]);
    tui.wait_viewport("ready", |v| v.contains("e2e-branch"));
    tui.submit("begin the steerable turn");
    tui.wait_requests(&provider, 1);

    tui.type_text("please also mention pelicans");
    tui.wait_viewport("steer draft rendered", |view| {
        view.contains("please also mention pelicans")
    });
    std::thread::sleep(Duration::from_millis(350));
    tui.send(TAB);
    tui.wait_for_text("Pushed into the running turn.");
    provider.release(0); // let the model's first response finish; the turn continues

    tui.wait_for_text("steered answer delivered");
    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "steer rides the same turn: one continuation request"
    );
    let blob = serde_json::to_string(&messages(&requests[1])).unwrap();
    assert!(
        blob.contains("please also mention pelicans"),
        "steer must reach the model: {blob}"
    );
    assert!(
        blob.contains("steer-window") && blob.contains("call_e2e_steer"),
        "same turn: the continuation also carries the tool exchange: {blob}"
    );
    assert_eq!(
        user_texts(&requests[1])
            .iter()
            .filter(|t| t.contains("begin the steerable turn"))
            .count(),
        1,
        "the original prompt is not duplicated into a new turn"
    );
}

// ---------------------------------------------------------------------------
// 5. Prompt-cache prefix stability + footer usage across two turns.
// ---------------------------------------------------------------------------
#[derive(Deserialize)]
struct RawRequest<'a> {
    #[serde(borrow)]
    messages: &'a RawValue,
    #[serde(borrow, default)]
    tools: Option<&'a RawValue>,
    model: String,
}

#[test]
fn second_turn_prefix_is_byte_identical_and_footer_tracks_usage() {
    let provider = ScriptedProvider::start(vec![
        Step::text("first turn answer").with_usage(1200, 20, 0),
        Step::text("second turn answer").with_usage(90_000, 30, 60_000),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.wait_viewport("ready", |v| v.contains("e2e-branch"));

    tui.submit("turn one question");
    tui.wait_for_text("first turn answer");
    // Running-panel footer projects cache hits as `cache N%` (cache_footer_part).
    let after_one = tui.wait_viewport("ctx% after turn 1", |v| {
        footer_ctx_percent(v).is_some() && v.contains("cache 0%")
    });
    assert!(
        after_one.contains("cache 0%"),
        "no cache hit yet:\n{after_one}"
    );
    let ctx_one = footer_ctx_percent(&after_one).unwrap();

    tui.submit("turn two question");
    tui.wait_for_text("second turn answer");
    let after_two = tui.wait_viewport("cached figure after turn 2", |v| {
        v.contains("cache 66%") && footer_ctx_percent(v).is_some()
    });
    println!("{after_two}");
    let ctx_two = footer_ctx_percent(&after_two).unwrap();
    assert!(
        ctx_two > ctx_one,
        "ctx% must grow with reported prompt size: {ctx_one} -> {ctx_two}"
    );

    let raw = provider.raw_requests();
    assert_eq!(raw.len(), 2);
    let first: RawRequest = serde_json::from_str(&raw[0]).unwrap();
    let second: RawRequest = serde_json::from_str(&raw[1]).unwrap();
    assert_eq!(first.model, second.model);
    assert_eq!(
        first.tools.map(|t| t.get()),
        second.tools.map(|t| t.get()),
        "tool definitions must be byte-identical or the cache prefix breaks"
    );
    // messages array of request 1 without its closing bracket must be a literal
    // byte prefix of request 2's messages array.
    let one = first.messages.get();
    let prefix = &one[..one.rfind(']').unwrap()];
    assert!(
        second.messages.get().starts_with(prefix),
        "turn-1 messages (system + first turn) must be a byte prefix of turn-2 messages\n--1--\n{one}\n--2--\n{}",
        second.messages.get()
    );
    let second_users = user_texts(&provider.requests()[1]);
    assert!(second_users.first().unwrap().contains("turn one question"));
    assert!(second_users.last().unwrap().contains("turn two question"));
}

// ---------------------------------------------------------------------------
// Footer cwd (launch directory). Direct-context launches leave the footer at
// "no directory selected" even though turns run in the launch directory.
// ---------------------------------------------------------------------------
#[test]
fn footer_shows_launch_directory() {
    let provider = ScriptedProvider::start(vec![]);
    let tui = Tui::launch(&provider, &[]);
    let view = tui.wait_viewport("ready", |v| v.contains("e2e-branch"));
    let dir = tui
        .workspace
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        view.contains(&dir),
        "footer must show the launch directory {dir}:\n{view}"
    );
}

// ---------------------------------------------------------------------------
// Live shell tail: output streams while the command runs, user PATH tools
// (git) resolve inside the confined shell, and the model gets the full output.
// ---------------------------------------------------------------------------
#[test]
fn shell_output_streams_live_resolves_git_and_reaches_model() {
    let command = if cfg!(windows) {
        "foreach ($i in 1..3) { Write-Output \"e2e-live-line-$i\"; Start-Sleep -Seconds 1 }; git --version"
    } else {
        "for i in 1 2 3; do echo e2e-live-line-$i; sleep 1; done; git --version"
    };
    let provider = ScriptedProvider::start(vec![
        shell_call("call_e2e_live", command),
        Step::text("live tail finished."),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.wait_viewport("ready", |v| v.contains("e2e-branch"));
    tui.submit("run the live command");
    tui.wait_viewport("approval card", card_ready);
    tui.enter();

    // (a) a streamed line is visible while the command is still running:
    // the follow-up provider request cannot exist yet.
    let live = tui.wait_viewport("a streamed line before completion", |v| {
        v.contains("e2e-live-line-1")
    });
    println!("{live}");
    assert_eq!(
        provider.request_count(),
        1,
        "command must still be running when the line is visible"
    );

    tui.wait_for_text("live tail finished.");
    // (b) git resolved through the user's PATH.
    let screen = tui.screen_text();
    assert!(
        !screen.contains("not recognized"),
        "git must resolve on PATH:
{screen}"
    );

    // (c) the model received the complete output.
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    let msgs = messages(&requests[1]);
    let tool = msgs
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool result in follow-up");
    let text = content_text(tool);
    for needle in [
        "e2e-live-line-1",
        "e2e-live-line-2",
        "e2e-live-line-3",
        "git version",
    ] {
        assert!(
            text.contains(needle),
            "model output missing {needle:?}: {text}"
        );
    }

    // (d) EVT-002: live tool-output deltas stream to the TUI but are never
    // stored in the durable journal, while the settled tool result keeps the
    // full output.
    let (mut result_rows, mut delta_rows) = (0_i64, 0_i64);
    let mut stack = vec![tui.home.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("db" | "sqlite" | "sqlite3")
            ) {
                continue;
            }
            let Ok(conn) = rightkit_db::rusqlite::Connection::open_with_flags(
                &path,
                rightkit_db::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ) else {
                continue;
            };
            let count = |kind: &str| -> Option<i64> {
                conn.query_row("SELECT COUNT(*) FROM events WHERE kind = ?1", [kind], |r| {
                    r.get(0)
                })
                .ok()
            };
            result_rows += count("toolResultAdded").unwrap_or(0);
            delta_rows += count("toolOutputDelta").unwrap_or(0);
        }
    }
    assert!(result_rows > 0, "durable journal must hold the tool result");
    assert_eq!(
        delta_rows, 0,
        "toolOutputDelta must not be persisted to the durable journal"
    );
}
