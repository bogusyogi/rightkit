//! Fake Pulse `membrane` binary wiring for black-box e2e tests (issue #33).
//!
//! The `fake-membrane` binary lives in `coderight-membrane-cli`, whose
//! `CARGO_BIN_EXE_*` variable is invisible to this package's tests, so the path is derived from
//! the test executable's own target directory (`target/<profile>/deps/e2e-*` ->
//! `target/<profile>/fake-membrane`). Building the `fake-membrane` bin of the
//! `coderight-membrane-cli` package (or any workspace-wide test build) produces it;
//! `RIGHTKIT_FAKE_MEMBRANE_BIN` overrides the location.
//!
//! Every fixture lives under one temp root. The spawned `coderight` gets `RIGHTKIT_MEMBRANE_BIN`
//! pointing at the fake, and `LOCALAPPDATA`/`APPDATA` redirected into the root so Pulse
//! discovery (notch-state, resident.json, installed Helpers path) can never reach the real home.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const SEP: char = '\u{1f}';

pub fn fake_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("RIGHTKIT_FAKE_MEMBRANE_BIN") {
        return PathBuf::from(path);
    }
    let name = if cfg!(windows) {
        "fake-membrane.exe"
    } else {
        "fake-membrane"
    };
    let exe = std::env::current_exe().expect("test executable path");
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(deps) = exe.parent() {
        dirs.push(deps.to_path_buf());
        if let Some(profile) = deps.parent() {
            dirs.push(profile.to_path_buf());
        }
    }
    dirs.iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| {
            panic!(
                "fake-membrane binary not found next to {}; build the fake-membrane bin of \
                 coderight-membrane-cli or set RIGHTKIT_FAKE_MEMBRANE_BIN",
                exe.display()
            )
        })
}

/// Refuse to run a fixture whose isolation root is not inside the OS temp directory.
pub fn assert_temp_rooted(path: &Path) {
    let temp = std::env::temp_dir();
    let temp = temp.canonicalize().unwrap_or(temp);
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    assert!(
        path.starts_with(&temp),
        "e2e isolation root {} must live under the temp dir {}, never the real home",
        path.display(),
        temp.display()
    );
}

pub struct FakeMembrane {
    pub root: TempDir,
    pub args_log: PathBuf,
    pub stdin_log: PathBuf,
    pub store: PathBuf,
    env: Vec<(String, String)>,
}

impl FakeMembrane {
    /// Fake present: `RIGHTKIT_MEMBRANE_BIN` names the fake, state redirected into a temp root.
    pub fn new() -> Self {
        Self::with_root(
            tempfile::tempdir().expect("membrane fixture root"),
            fake_binary(),
        )
    }

    /// Pulse absent: `RIGHTKIT_MEMBRANE_BIN` names a file that does not exist.
    pub fn absent() -> Self {
        let root = tempfile::tempdir().expect("membrane fixture root");
        let missing = root.path().join(if cfg!(windows) {
            "no-pulse.exe"
        } else {
            "no-pulse"
        });
        Self::with_root(root, missing)
    }

    fn with_root(root: TempDir, bin: PathBuf) -> Self {
        assert_temp_rooted(root.path());
        let local = root.path().join("local");
        let appdata = root.path().join("appdata");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&appdata).unwrap();
        let args_log = root.path().join("args.log");
        let stdin_log = root.path().join("stdin.log");
        let store = root.path().join("store.jsonl");
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let env = vec![
            ("RIGHTKIT_CONTEXT".to_owned(), "on".to_owned()),
            ("RIGHTKIT_DISABLE_DEFAULT_MCP".to_owned(), "1".to_owned()),
            ("RIGHTKIT_MEMBRANE_BIN".to_owned(), s(&bin)),
            ("LOCALAPPDATA".to_owned(), s(&local)),
            ("APPDATA".to_owned(), s(&appdata)),
            ("FAKE_MEMBRANE_MODE".to_owned(), "ok".to_owned()),
            ("FAKE_MEMBRANE_ARGS_LOG".to_owned(), s(&args_log)),
            ("FAKE_MEMBRANE_STDIN_LOG".to_owned(), s(&stdin_log)),
            ("FAKE_MEMBRANE_STORE".to_owned(), s(&store)),
        ];
        Self {
            root,
            args_log,
            stdin_log,
            store,
            env,
        }
    }

    /// Environment pairs for `Cli::run_env` / `daemon_process::start_daemon`.
    pub fn env(&self) -> Vec<(&str, &str)> {
        self.env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    /// Like [`env`](Self::env) plus extra pairs (later entries win).
    pub fn env_with<'a>(&'a self, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut all = self.env();
        all.extend_from_slice(extra);
        all
    }

    /// Every recorded membrane invocation, as its argv.
    pub fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(&self.args_log)
            .unwrap_or_default()
            .lines()
            .map(|l| l.split(SEP).map(str::to_owned).collect())
            .collect()
    }

    /// Calls whose argv starts with `prefix` (e.g. `["memory", "push"]`).
    pub fn calls_matching(&self, prefix: &[&str]) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c.len() >= prefix.len() && c.iter().zip(prefix).all(|(a, b)| a == b))
            .collect()
    }

    /// Recorded stdin documents for `tag` (`adapt-observe` | `transcript-append`).
    pub fn stdin_docs(&self, tag: &str) -> Vec<serde_json::Value> {
        let prefix = format!("{tag}\t");
        std::fs::read_to_string(&self.stdin_log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix(&prefix))
            .filter_map(|body| serde_json::from_str(body).ok())
            .collect()
    }

    /// Block until `ready` holds for the recorded calls, or fail with the call log.
    pub fn wait_for(
        &self,
        what: &str,
        secs: u64,
        ready: impl Fn(&[Vec<String>]) -> bool,
    ) -> Vec<Vec<String>> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let calls = self.calls();
            if ready(&calls) {
                return calls;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; membrane calls so far: {calls:?}"
            );
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

/// `--cursor` values of every `transcript append` call, in call order.
pub fn append_cursors(calls: &[Vec<String>]) -> Vec<String> {
    calls
        .iter()
        .filter(|c| c.len() >= 2 && c[0] == "transcript" && c[1] == "append")
        .filter_map(|c| {
            c.windows(2)
                .find(|w| w[0] == "--cursor")
                .map(|w| w[1].clone())
        })
        .collect()
}
