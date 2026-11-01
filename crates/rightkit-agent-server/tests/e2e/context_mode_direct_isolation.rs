//! Regression: an isolated `coderight serve` daemon with `RIGHTKIT_CONTEXT=off`
//! reaches health while Membrane is provably absent, and no Membrane
//! discovery/provisioning/activation/repair/residency effect executes.
//!
//! The defect this guards: qualification daemons used to rely on
//! `RIGHTKIT_QUAL_ISOLATED_MEMBRANE=1`, which only exempts the turn-time
//! binding gate. Startup still took the Membrane branch, and on Windows the
//! pinned client's stable install root resolves `%LOCALAPPDATA%` — which the
//! isolated env did not redirect — so the "isolated" daemon engaged the
//! machine's real installed Membrane runtime, including installer-owned
//! effect paths bounded by a 300s deadline. `/health` never served.
//!
//! This test redirects `LOCALAPPDATA`/`APPDATA` into the isolated home so
//! (a) no installed Membrane exists from the child's view, and (b) any
//! installed-root effect would land inside the test tree where it can be
//! observed. It then asserts the direct-mode marker, health, and an empty
//! redirect root.

use crate::common;

use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(30);

fn executable() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_coderight")
        .map(PathBuf::from)
        .expect("Cargo must provide CARGO_BIN_EXE_coderight for this integration test")
}

fn walk(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path.clone());
                }
                out.push(path);
            }
        }
    }
    out
}

#[derive(Debug)]
struct DirectDaemon {
    child: std::process::Child,
    port: u16,
    token: String,
}

impl Drop for DirectDaemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn context_unavailable_reaches_health_with_visible_degradation_and_no_effects() {
    let home = tempfile::tempdir().expect("isolated home");
    let home = home.path();
    // The Windows stable Membrane install root resolves under
    // %LOCALAPPDATA% (and roaming fallbacks under %APPDATA%). Redirecting
    // both into the isolated home means no installed Membrane exists from
    // this child's view — and any installed-root discovery/provisioning/
    // activation/repair effect would materialize inside this directory.
    let redirected_appdata = home.join("redirected-appdata");
    std::fs::create_dir_all(&redirected_appdata).expect("create redirect root");
    let db = home.join("direct-isolation.db");

    let child = rightkit_child_process::command_std(executable())
        .env("RIGHTKIT_CONTEXT", "off")
        .args(["serve", "--port", "0", "--db", &db.to_string_lossy()])
        .envs([crate::common::suite_env(home)])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("LOCALAPPDATA", &redirected_appdata)
        .env("APPDATA", &redirected_appdata)
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .env_remove("RIGHTKIT_SERVE_FORCE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn direct-mode daemon");

    // Own child before any fallible startup parsing so panic paths reap it.
    let mut daemon = DirectDaemon {
        child,
        port: 0,
        token: String::new(),
    };

    let stdout = daemon.child.stdout.take().expect("daemon startup stdout");
    let stderr = daemon.child.stderr.take().expect("daemon stderr");
    let (stderr_tx, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut captured = String::new();
        let mut reader = BufReader::new(stderr);
        let _ = reader.read_to_string(&mut captured);
        let _ = stderr_tx.send(captured);
    });

    let (contract_tx, contract_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line);
        let _ = contract_tx.send(match result {
            Ok(0) => Err("daemon exited before its startup contract".to_string()),
            Ok(_) => serde_json::from_str::<Value>(line.trim())
                .map_err(|error| format!("invalid startup contract: {error}")),
            Err(error) => Err(format!("could not read startup contract: {error}")),
        });
    });

    let contract = contract_rx
        .recv_timeout(DEADLINE)
        .expect("startup contract timed out")
        .expect("startup contract invalid");
    let port = contract["Port"].as_u64().expect("startup contract port") as u16;
    let token = contract["Token"]
        .as_str()
        .expect("startup contract token")
        .to_string();
    daemon.port = port;
    daemon.token = token;
    let url = format!("http://127.0.0.1:{}", daemon.port);

    // 1. The daemon reaches health even though no installed Membrane exists
    //    anywhere in its observable environment.
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(value) = common::get_json(&url, "/health", &daemon.token) {
            assert_eq!(value["ok"], true, "health must report ok: {value}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "direct-mode daemon did not reach health with Membrane absent"
        );
        thread::sleep(Duration::from_millis(50));
    }

    // 2. The unavailable canonical binding is visible on the black-box
    //    surface; context mode cannot select a local substitute.
    let metrics = common::get_json_or_panic(&url, "/runtime/resource-metrics", &daemon.token);
    assert_eq!(
        metrics["membraneBinding"]["bound"], false,
        "an unavailable binding must not be reported as bound: {metrics}"
    );
    assert_eq!(
        metrics["membraneBinding"]["mode"], "off",
        "unavailable binding must be reported as unbound: {metrics}"
    );
    let context = common::get_json_or_panic(&url, "/runtime/context/status", &daemon.token);
    assert_eq!(context["contextDegraded"], true, "{context}");
    assert!(
        context["degradedReason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "unavailable context needs a visible reason: {context}"
    );

    // 3. No Membrane effect ran: the redirected application-data root must
    //    contain no installed-Membrane artifacts — no discovered, provisioned,
    //    activated, repaired, or resident install state may exist there.
    let membrane_artifacts: Vec<_> = walk(&redirected_appdata)
        .into_iter()
        .filter(|path| {
            path.components().any(|component| {
                let name = component.as_os_str().to_string_lossy().to_lowercase();
                name.contains("membrane") || name.contains("orthic")
            })
        })
        .collect();
    assert!(
        membrane_artifacts.is_empty(),
        "unavailable-context startup produced installed-Membrane effects: {membrane_artifacts:?}"
    );

    // 4. Startup reports degradation while keeping local execution available.
    drop(daemon);
    let captured = stderr_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("daemon stderr capture");
    assert!(
        captured.contains("context") && captured.contains("unavailable"),
        "daemon log must explain unavailable context:\n{captured}"
    );
}
