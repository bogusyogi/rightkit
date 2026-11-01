//! Shared PTY harness for the `tui_e2e_*` suites.
//!
//! The REAL `coderight` binary runs inside a PTY (ConPTY on Windows); its
//! terminal output is parsed by a real VT100 emulator; keystrokes are sent as
//! a user types them; an independent scripted OpenAI-compatible provider
//! records what the MODEL actually receives. Every `Tui` owns its home,
//! workspace and (caller-supplied) provider, so tests are isolated.
#![allow(dead_code)]

#[path = "tui_provider.rs"]
pub mod scripted_provider;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
pub use scripted_provider::{ScriptedProvider, Step};
pub use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tempfile::TempDir;

pub const ROWS: u16 = 36;
pub const COLS: u16 = 120;
/// Generous: under a parallel run the first turn of each fresh daemon can take
/// >30s to reach the provider (observed: 0 requests after 29s, tools stalling 28s).
pub const WAIT: Duration = Duration::from_secs(120);
pub const MODEL: &str = "deepseek/deepseek-v4-pro";

pub const ESC: &[u8] = b"\x1b";
pub const UP: &[u8] = b"\x1b[A";
pub const DOWN: &[u8] = b"\x1b[B";
pub const RIGHT: &[u8] = b"\x1b[C";
pub const LEFT: &[u8] = b"\x1b[D";
pub const HOME: &[u8] = b"\x1b[H";
pub const END: &[u8] = b"\x1b[F";
pub const TAB: &[u8] = b"\t";
pub const BACKTAB: &[u8] = b"\x1b[Z";
pub const BACKSPACE: &[u8] = b"\x7f";
pub const CTRL_A: &[u8] = b"\x01";
pub const CTRL_C: &[u8] = b"\x03";
pub const CTRL_E: &[u8] = b"\x05";
pub const CTRL_K: &[u8] = b"\x0b";
pub const CTRL_R: &[u8] = b"\x12";
pub const CTRL_U: &[u8] = b"\x15";
pub const CTRL_W: &[u8] = b"\x17";
pub const CTRL_X: &[u8] = b"\x18";
pub const CTRL_Y: &[u8] = b"\x19";
pub const F1: &[u8] = b"\x1bOP";
/// Alt+Enter: inserts a newline instead of submitting.
pub const ALT_ENTER: &[u8] = b"\x1b\r";

pub struct Tui {
    pub parser: Arc<Mutex<vt100::Parser>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Option<Box<dyn MasterPty + Send>>,
    reader: Option<JoinHandle<()>>,
    reader_done: Arc<AtomicBool>,
    /// Every byte the child wrote to the terminal (escape sequences included).
    pub raw: Arc<Mutex<Vec<u8>>>,
    provider_count: Arc<AtomicUsize>,
    pub home: TempDir,
    pub workspace: TempDir,
    pub rows: u16,
    pub cols: u16,
}

pub struct LaunchOptions<'a> {
    pub args: &'a [&'a str],
    pub rows: u16,
    pub cols: u16,
    /// Files written into the workspace before launch (path, contents). Committed.
    pub files: &'a [(&'a str, &'a str)],
    /// Extra TOML appended to `<app root>/config.toml`.
    pub extra_config: &'a str,
    /// Extra environment variables for the child.
    pub env: &'a [(&'a str, &'a str)],
    /// Reuse an earlier run's home / workspace directory (relaunch scenarios).
    pub reuse: Option<(std::path::PathBuf, std::path::PathBuf)>,
    /// Launch with cwd = the harness home dir, which opens the launch Home picker.
    pub cwd_home: bool,
    /// Skip pre-trusting the temp workspace (trust-specific journeys).
    pub untrusted: bool,
}

impl Default for LaunchOptions<'_> {
    fn default() -> Self {
        LaunchOptions {
            args: &[],
            rows: ROWS,
            cols: COLS,
            files: &[],
            extra_config: "",
            env: &[],
            reuse: None,
            cwd_home: false,
            untrusted: false,
        }
    }
}

impl Tui {
    pub fn launch(provider: &ScriptedProvider, extra_args: &[&str]) -> Tui {
        Tui::launch_with(
            provider,
            LaunchOptions {
                args: extra_args,
                ..Default::default()
            },
        )
    }

    pub fn launch_with(provider: &ScriptedProvider, opts: LaunchOptions<'_>) -> Tui {
        let provider_count = provider.request_counter();
        let home = TempDir::new().expect("temp home");
        let workspace = TempDir::new().expect("temp workspace");
        let (home_path, ws_path, reused) = match &opts.reuse {
            Some((h, w)) => (h.clone(), w.clone(), true),
            None => (
                home.path().to_path_buf(),
                workspace.path().to_path_buf(),
                false,
            ),
        };
        let cfg = crate::common::app_root(&home_path);
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("config.toml"),
            format!(
                "[model]\ndefault-model = \"{MODEL}\"\n\n[provider-routes.qual-fixture]\n\
                 display-name = \"Qualification fixture\"\nbase-url = \"{}\"\nmodel = \"{MODEL}\"\n\
                 api-key-required = false\n{}",
                provider.base_url(),
                opts.extra_config
            ),
        )
        .unwrap();
        if !reused {
            git(&ws_path, &["init", "-q", "-b", "e2e-branch"]);
            git(&ws_path, &["config", "user.email", "e2e@example.test"]);
            git(&ws_path, &["config", "user.name", "e2e"]);
            std::fs::write(
                ws_path.join("README.md"),
                "e2e
",
            )
            .unwrap();
            for (rel, body) in opts.files {
                let p = ws_path.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, body).unwrap();
            }
            git(&ws_path, &["add", "."]);
            git(&ws_path, &["commit", "-q", "-m", "init"]);
        }

        let trust_store = crate::common::app_root(&home_path).join("workspace-trust.json");
        if !opts.untrusted {
            // STATUS D2: an untrusted folder shows the first-launch trust
            // notice. Pre-trust every spelling of the temp workspace root.
            let mut roots = vec![ws_path.to_string_lossy().to_string()];
            if let Ok(canon) = std::fs::canonicalize(&ws_path) {
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
                rows: opts.rows,
                cols: opts.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_coderight"));
        cmd.env("RIGHTKIT_CONTEXT", "off");
        cmd.args(["--model", &format!("qual-fixture::{MODEL}")]);
        // The built-in context7 default MCP server spawns `npx -y @upstash/context7-mcp`;
        // its handshake stalls the first provider request (~45s). TUI journeys do not
        // exercise MCP, so keep it off.
        cmd.args(opts.args);
        cmd.cwd(if opts.cwd_home { &home_path } else { &ws_path });
        cmd.env(
            crate::common::suite_env(&home_path).0,
            home_path.as_os_str(),
        );
        for (k, v) in [
            ("RIGHTKIT_DISABLE_DEFAULT_MCP", std::ffi::OsStr::new("1")),
            ("HOME", home_path.as_os_str()),
            ("USERPROFILE", home_path.as_os_str()),
            ("XDG_CONFIG_HOME", home_path.join("config").as_os_str()),
            ("XDG_DATA_HOME", home_path.join("data").as_os_str()),
            // Windows user-data roots: isolate them under the harness home so a run never
            // touches the real profile or depends on the runner's inherited environment.
            ("LOCALAPPDATA", home_path.join("local").as_os_str()),
            ("APPDATA", home_path.join("roaming").as_os_str()),
        ] {
            cmd.env(k, v);
        }
        // Never execute real installed vendor CLIs (agy's detached updater
        // opens visible consoles); vendor tests override with "0".
        cmd.env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1");
        cmd.env("RIGHTKIT_WORKSPACE_TRUST_STORE", &trust_store);
        for (k, v) in opts.env {
            cmd.env(k, v);
        }
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
        let parser = Arc::new(Mutex::new(vt100::Parser::new(opts.rows, opts.cols, 2000)));
        let sink = parser.clone();
        let raw = Arc::new(Mutex::new(Vec::<u8>::new()));
        let raw_sink = raw.clone();
        let reader_done = Arc::new(AtomicBool::new(false));
        let reader_done_sink = reader_done.clone();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                raw_sink.lock().unwrap().extend_from_slice(&buf[..n]);
                sink.lock().unwrap().process(&buf[..n]);
            }
            reader_done_sink.store(true, Ordering::Release);
        });
        Tui {
            parser,
            writer: Mutex::new(Some(writer)),
            child,
            master: Some(pty.master),
            reader: Some(reader),
            reader_done,
            raw,
            provider_count,
            home,
            workspace,
            rows: opts.rows,
            cols: opts.cols,
        }
    }

    /// Wait for the launch Home picker, leaving it open for navigation journeys.
    pub fn ready_home(&self) -> String {
        self.wait_viewport("launch project picker", |view| {
            view.contains("choose a project or chat")
                && view.contains("this folder")
                && !view.contains("Starting context")
                && !view.contains("Resolving provider")
        })
    }

    /// Wait for the ready footer (branch + model), dismissing the launch Home
    /// picker with its documented “stay in this folder” action when present.
    pub fn ready(&self) -> String {
        let view = self.wait_viewport("ready footer or launch project picker", |view| {
            (view.contains("e2e-branch") && view.contains(MODEL))
                || view.contains("choose a project or chat")
        });
        if view.contains("choose a project or chat") {
            assert!(
                view.contains("this folder") && view.contains("Esc stay"),
                "launch picker does not offer the expected stay-in-folder action:\n{view}"
            );
            self.send(ESC);
        }
        // The branch + model footer is already visible while context is still
        // starting; wait until startup progress has settled so the welcome card
        // and final footer are on screen before callers assert on the layout.
        self.wait_viewport("ready footer", |view| {
            view.contains("e2e-branch")
                && view.contains(MODEL)
                && !view.contains("Starting context")
                && !view.contains("Resolving provider")
        })
    }

    /// Wait for the idle composer without requiring the full footer (narrow or short terminals).
    pub fn ready_loose(&self) -> String {
        let view = self.wait_viewport("idle composer or launch project picker", |view| {
            view.contains("Ask anything") || view.contains("choose a project or chat")
        });
        if view.contains("choose a project or chat") {
            assert!(
                view.contains("this folder") && view.contains("Esc stay"),
                "launch picker does not offer the expected stay-in-folder action:\n{view}"
            );
            self.send(ESC);
        }
        self.wait_viewport("idle composer", |view| view.contains("Ask anything"))
    }

    /// Scrollback plus the visible viewport.
    pub fn screen_text(&self) -> String {
        let cols = self.cols;
        let mut p = self.parser.lock().unwrap();
        p.screen_mut().set_scrollback(usize::MAX);
        let max = p.screen().scrollback();
        let mut lines: Vec<String> = Vec::new();
        for offset in (1..=max).rev() {
            p.screen_mut().set_scrollback(offset);
            if let Some(row) = p.screen().rows(0, cols).next() {
                lines.push(row.trim_end().to_string());
            }
        }
        p.screen_mut().set_scrollback(0);
        lines.extend(p.screen().rows(0, cols).map(|r| r.trim_end().to_string()));
        lines.join("\n")
    }

    pub fn viewport(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    /// (row, col) of the terminal cursor in the viewport.
    pub fn cursor(&self) -> (u16, u16) {
        self.parser.lock().unwrap().screen().cursor_position()
    }

    pub fn send(&self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        let writer = writer.as_mut().expect("write keys after PTY shutdown");
        writer.write_all(bytes).expect("write keys");
        writer.flush().unwrap();
    }

    pub fn type_text(&mut self, text: &str) {
        self.send(text.as_bytes());
    }

    pub fn enter(&mut self) {
        self.send(b"\r");
    }

    /// Type, then pause like a person before the next keystroke. The composer
    /// treats keys arriving in a fast burst as a paste, so this pause is part
    /// of the simulated typing, not a synchronisation wait.
    pub fn type_slow(&mut self, text: &str) {
        self.type_text(text);
        std::thread::sleep(Duration::from_millis(350));
    }

    /// Type a line the way a person does, then press Enter.
    pub fn submit(&mut self, text: &str) {
        self.type_text(text);
        // Wait until the typed text is on screen (a loaded machine delays echo), so the
        // Enter below cannot land inside the paste-burst window of late-arriving keys.
        let needle: String = text.chars().filter(|c| *c != '\n').take(16).collect();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.viewport().contains(&needle) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(350));
        self.enter();
    }

    /// Press a key after the human pause (keys right after typed text would
    /// otherwise be swallowed into the paste-burst window).
    pub fn key(&mut self, bytes: &[u8]) {
        std::thread::sleep(Duration::from_millis(350));
        self.send(bytes);
    }

    /// Terminal bracketed paste.
    pub fn paste(&mut self, text: &str) {
        let mut data = b"\x1b[200~".to_vec();
        data.extend_from_slice(text.as_bytes());
        data.extend_from_slice(b"\x1b[201~");
        self.send(&data);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.master
            .as_ref()
            .expect("resize after PTY shutdown")
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize pty");
        self.parser
            .lock()
            .unwrap()
            .screen_mut()
            .set_size(rows, cols);
        self.rows = rows;
        self.cols = cols;
    }

    /// Poll the full screen (scrollback + viewport) until `pred` holds.
    pub fn wait_for(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.screen_text();
            if pred(&text) {
                return text;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what}\n--- provider requests ---\n{}\n--- screen ---\n{text}\n--- raw PTY ---\n{}\n--------------",
                    self.provider_count.load(Ordering::Acquire),
                    self.raw_output()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn wait_for_text(&self, needle: &str) -> String {
        self.wait_for(&format!("{needle:?}"), |s| s.contains(needle))
    }

    pub fn wait_viewport(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let view = self.viewport();
            if pred(&view) {
                return view;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what}\n--- provider requests ---\n{}\n--- screen ---\n{}\n--- raw PTY ---\n{}\n--------------",
                    self.provider_count.load(Ordering::Acquire),
                    self.screen_text(),
                    self.raw_output()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn wait_viewport_text(&self, needle: &str) -> String {
        self.wait_viewport(&format!("{needle:?}"), |v| v.contains(needle))
    }

    pub fn wait_requests(&self, provider: &ScriptedProvider, n: usize) {
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

    pub fn raw_output(&self) -> String {
        String::from_utf8_lossy(&self.raw.lock().unwrap()).to_string()
    }

    /// Stop the process (simulates quitting) without dropping the temp dirs.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.close_pty_after_exit();
    }

    /// Wait for the process to exit on its own.
    pub fn wait_exit(&mut self) {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                self.close_pty_after_exit();
                assert!(
                    self.reader_done.load(Ordering::Acquire),
                    "PTY did not reach EOF after child exit\n--- raw PTY ---\n{:?}",
                    self.raw_output()
                );
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "process did not exit\n--- screen ---\n{}\n--------------",
                    self.screen_text()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The composer prompt row (the line holding the `▌` marker), if any.
    pub fn prompt_line(&self) -> Option<String> {
        self.viewport()
            .lines()
            .rev()
            .find(|l| l.contains('▌'))
            .map(|l| l.to_string())
    }

    /// Drain terminal bytes after child exit without risking an unbounded
    /// join: ConPTY readers can remain blocked while the master is alive.
    fn close_pty_after_exit(&mut self) {
        // Child exit is not ConPTY EOF. Release both input and pseudo-console
        // handles, then let the reader observe shutdown's final mode resets.
        // On Windows 11 24H2 ClosePseudoConsole returns before this completes.
        self.writer.lock().unwrap().take();
        self.master.take();
        self.drain_reader();
    }

    fn drain_reader(&mut self) {
        let deadline = Instant::now() + WAIT;
        while !self.reader_done.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if self.reader_done.load(Ordering::Acquire) {
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.close_pty_after_exit();
    }
}

pub fn git(dir: &Path, args: &[&str]) {
    let status = rightkit_child_process::command_std("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

pub fn content_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn messages(request: &Value) -> Vec<Value> {
    request["messages"].as_array().cloned().unwrap_or_default()
}

pub fn user_texts(request: &Value) -> Vec<String> {
    messages(request)
        .iter()
        .filter(|m| m["role"] == "user")
        .map(content_text)
        .collect()
}

pub fn last_user_text(request: &Value) -> String {
    user_texts(request).pop().unwrap_or_default()
}

pub fn has_card(view: &str) -> bool {
    view.contains("Approve ")
}

pub fn shell_call(id: &str, command: &str) -> Step {
    Step::tool(id, "shell", json!({ "command": command }))
}
