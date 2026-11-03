//! Shared black-box rig for the `e2e_api_*` / `e2e_cli_*` suites.
//!
//! One `Rig` = isolated home + git workspace + independent scripted
//! OpenAI-compatible provider + a REAL `coderight serve` daemon child process.
//! Everything the tests assert is observed from outside the process: HTTP
//! responses, SSE/history events, files on disk, the provider's recorded
//! requests, CLI stdout/exit codes. Include with:
//!
//! ```ignore
//! use crate::support::{daemon_process, scripted_provider, e2e_api};
//! ```
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use crate::support::daemon_process::{self, TestDaemon};
use crate::support::scripted_provider::{ScriptedProvider, Step};

pub const MODEL: &str = "deepseek/deepseek-v4-pro";

pub struct Rig {
    // Rust drops fields in declaration order: stop daemon before TempDirs.
    pub daemon: TestDaemon,
    pub provider: ScriptedProvider,
    pub home: TempDir,
    pub workspace: TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Fields drop in declaration order: reap before home/workspace cleanup.
        self.daemon.stop();
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = rightkit_child_process::command_std(daemon_process::host_git_program())
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

impl Rig {
    /// Start a rig whose provider answers `script[i]` to the i-th model call.
    pub fn start(script: Vec<Step>) -> Rig {
        Self::start_with_env(script, &[])
    }

    /// Start a rig with environment variables scoped to its daemon child.
    pub fn start_with_env(script: Vec<Step>, extra_env: &[(&str, &str)]) -> Rig {
        let test_root = std::env::temp_dir();
        let home = tempfile::tempdir_in(&test_root).expect("temp home");
        let workspace = tempfile::tempdir_in(&test_root).expect("temp workspace");
        git(workspace.path(), &["init", "-q", "-b", "main"]);
        git(
            workspace.path(),
            &["config", "user.email", "e2e@example.test"],
        );
        git(workspace.path(), &["config", "user.name", "e2e"]);
        std::fs::write(workspace.path().join("README.md"), "seed\n").unwrap();
        git(workspace.path(), &["add", "."]);
        git(workspace.path(), &["commit", "-q", "-m", "init"]);
        let provider = ScriptedProvider::start(script);
        daemon_process::write_local_provider_route_config(home.path(), &provider.base_url(), MODEL);
        let temp_root = home.path().to_string_lossy().into_owned();
        let mut daemon_env = vec![("TEMP", temp_root.as_str()), ("TMP", temp_root.as_str())];
        daemon_env.extend_from_slice(extra_env);
        let daemon = daemon_process::start_daemon(home.path(), &daemon_env);
        Rig {
            home,
            workspace,
            provider,
            daemon,
        }
    }

    /// Kill the daemon hard and start a fresh one on the SAME home/db.
    pub fn restart(&mut self) {
        self.daemon.stop();
        self.daemon = daemon_process::start_daemon(self.home.path(), &[]);
    }

    pub fn url(&self) -> String {
        self.daemon.url()
    }
    pub fn token(&self) -> &str {
        &self.daemon.startup.token
    }
    pub fn ws(&self) -> &Path {
        self.workspace.path()
    }

    fn req(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let (status, text) = self.req_text(method, path, body);
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    pub fn req_text(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, String) {
        let url = format!("{}{}", self.url(), path);
        let mut request = match method {
            "GET" => rightkit_http::Request::get(url),
            "POST" => rightkit_http::Request::post(url),
            "PUT" => rightkit_http::Request::put(url),
            "PATCH" => rightkit_http::Request::new(rightkit_http::Method::Patch, url),
            "DELETE" => rightkit_http::Request::delete(url),
            m => panic!("method {m}"),
        }
        .bearer(self.token());
        if let Some(b) = body {
            request = request.json(b).expect("JSON request body");
        }
        let resp =
            daemon_process::send(&request).unwrap_or_else(|e| panic!("{method} {path}: {e:?}"));
        (resp.status, resp.text())
    }

    pub fn get(&self, path: &str) -> (u16, Value) {
        self.req("GET", path, None)
    }
    pub fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.req("POST", path, Some(&body))
    }
    pub fn put(&self, path: &str, body: Value) -> (u16, Value) {
        self.req("PUT", path, Some(&body))
    }
    pub fn patch(&self, path: &str, body: Value) -> (u16, Value) {
        self.req("PATCH", path, Some(&body))
    }
    pub fn delete(&self, path: &str) -> (u16, Value) {
        self.req("DELETE", path, None)
    }

    /// Unauthenticated request (for auth-boundary checks).
    pub fn anon(&self, method: &str, path: &str) -> u16 {
        let url = format!("{}{}", self.url(), path);
        let request = if method == "POST" {
            rightkit_http::Request::post(url)
                .json(&json!({}))
                .expect("JSON request body")
        } else {
            rightkit_http::Request::get(url)
        };
        daemon_process::send(&request).expect("anon request").status
    }

    /// Create a session in the rig workspace. `extra` is merged into the body.
    pub fn session(&self, id: &str, extra: Value) -> Value {
        let mut body = json!({
            "id": id,
            "workspaceRoot": self.ws().to_string_lossy(),
            "model": MODEL,
            "title": format!("e2e {id}"),
        });
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
            // Every session otherwise starts the built-in `context7` MCP default
            // (`npx -y @upstash/context7-mcp`, crates/mcp/src/config.rs default_mcp_servers)
            // on its first turn. The hermetic rig has no network, so npx stalls ~45s
            // before the turn reaches the provider and approval/question waits time out.
            // Tests that supply their own `mcpConfig` or set `noMcp` keep that choice.
            if !b.contains_key("mcpConfig") && !b.contains_key("noMcp") {
                b.insert("noMcp".to_string(), json!(true));
            }
        }
        let (status, resp) = self.post("/sessions", body);
        assert_eq!(status, 200, "create session {id}: {resp}");
        resp
    }

    pub fn send(&self, id: &str, text: &str) -> (u16, Value) {
        self.post(&format!("/sessions/{id}/messages"), json!({ "text": text }))
    }

    /// Durable event history for a session.
    pub fn history(&self, id: &str) -> Vec<Value> {
        let (status, v) = self.get(&format!("/sessions/{id}/events/history"));
        assert_eq!(status, 200, "history {id}: {v}");
        v.as_array().cloned().unwrap_or_default()
    }

    pub fn kinds(&self, id: &str) -> Vec<String> {
        self.history(id).iter().map(kind_of).collect()
    }

    pub fn find_event(&self, id: &str, kind: &str) -> Option<Value> {
        self.history(id).into_iter().find(|e| kind_of(e) == kind)
    }

    fn timeout_diagnostics(&self) -> String {
        // Protected, isolated fixture output: retain observable session state
        // instead of losing every pre-provider failure behind a generic timeout.
        let (status, sessions) = self.get("/sessions");
        let mut history = Vec::new();
        if let Some(sessions) = sessions.as_array() {
            for session in sessions {
                if let Some(id) = session["id"].as_str() {
                    let (status, events) = self.get(&format!("/sessions/{id}/events/history"));
                    history.push(json!({"id": id, "status": status, "events": events}));
                }
            }
        }
        let log_path = std::env::temp_dir().join(format!(
            "coderight-e2e-daemon-{}.log",
            self.home
                .path()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
        ));
        let log = std::fs::read_to_string(&log_path)
            .unwrap_or_else(|error| format!("{}: {error}", log_path.display()));
        for (i, r) in self.provider.raw_requests().iter().enumerate() {
            let head: String = r.chars().take(700).collect();
            let tail: String = r
                .chars()
                .rev()
                .take(500)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            eprintln!("DBGREQ {i}: {head} ... {tail}");
        }
        format!(
            "provider_calls={}; sessions_status={status}; sessions={sessions}; history={history:?}; daemon_log={log}",
            self.provider.request_count(),
        )
    }

    /// Poll until `f` returns Some, or panic after `secs` with `what`.
    pub fn wait<T>(&self, what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; {}",
                self.timeout_diagnostics(),
            );
            std::thread::sleep(Duration::from_millis(60));
        }
    }

    /// Wait until the session history contains `kind` and return that event.
    pub fn wait_event(&self, id: &str, kind: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(60);
        self.wait(&format!("{kind} on {id}"), 60, || {
            let events = self.history(id);
            if let Some(event) = events.iter().find(|event| kind_of(event) == kind) {
                return Some(event.clone());
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {kind} on {id}; provider_calls={}; events={events:?}; database={}",
                self.provider.request_count(), self.daemon.startup.database_path,
            );
            None
        })
    }

    /// Wait for the provider to have received at least `n` model calls.
    pub fn wait_provider_calls(&self, n: usize) {
        self.wait(&format!("{n} provider calls"), 60, || {
            (self.provider.request_count() >= n).then_some(())
        });
    }

    /// Wait until the session's transcript has an assistant message containing `needle`.
    pub fn wait_assistant(&self, id: &str, needle: &str) -> Value {
        self.wait(&format!("assistant text {needle:?}"), 60, || {
            let (_, t) = self.get(&format!("/sessions/{id}/transcript"));
            t.as_array()?
                .iter()
                .find(|m| m["$kind"] == "assistant" && m.to_string().contains(needle))
                .cloned()
        })
    }

    /// Run the `coderight` CLI against this rig's daemon + isolated home.
    pub fn cli(&self, args: &[&str]) -> Output {
        self.cli_in(args, self.ws())
    }

    pub fn cli_in(&self, args: &[&str], cwd: &Path) -> Output {
        let mut cmd = isolated_cli(self.home.path(), cwd);
        cmd.args(args)
            .env("RIGHTKIT_DAEMON_URL", self.url())
            .env("RIGHTKIT_DAEMON_TOKEN", self.token());
        cmd.output().expect("run coderight cli")
    }
}

/// `kind` of a protocol event regardless of the tag spelling.
pub fn kind_of(e: &Value) -> String {
    // `$kind` first: `runFailed` carries its own `kind` field (failure class).
    for key in ["$kind", "kind", "type"] {
        if let Some(s) = e.get(key).and_then(Value::as_str) {
            return s.to_string();
        }
    }
    String::new()
}

/// A `coderight` command isolated from the host (no daemon discovery, no keychain).
pub fn isolated_cli(home: &Path, cwd: &Path) -> Command {
    let mut c = rightkit_child_process::command_std(daemon_process::executable());
    c.current_dir(cwd)
        .envs([crate::common::suite_env(home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .env("RIGHTKIT_DISABLE_DEFAULT_MCP", "1")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        .env("RIGHTKIT_QUAL_ISOLATED_MEMBRANE", "1")
        .env("RIGHTKIT_NO_AUTOSTART", "1")
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .env_remove("RIGHTKIT_SERVE_FORCE")
        .stdin(Stdio::null());
    daemon_process::restore_host_env(&mut c);
    c
}

pub fn stdout_json(out: &Output) -> Value {
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {text}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

pub fn temp_path(dir: &TempDir, rel: &str) -> PathBuf {
    dir.path().join(rel)
}
