//! One real probe per RightKit crate. A probe does the crate's actual work on this run's isolated data (files, SQLite,
//! sockets, child processes, loopback servers), never a stand-in, and returns what it observed. A probe that cannot run
//! here says why in its error (a missing asset, a credential the run does not have, a platform it does not support):
//! that is a failure the e2e run reports, never a silent skip.
use std::path::PathBuf;

use serde_json::Value;

/// What every probe gets: this run's private data root and a scratch directory under it.
pub struct Ctx {
    pub data: PathBuf,
    pub scratch: PathBuf,
}

impl Ctx {
    pub fn new(data: PathBuf) -> Result<Self, String> {
        let scratch = data.join("probes");
        std::fs::create_dir_all(&scratch)
            .map_err(|e| format!("create {}: {e}", scratch.display()))?;
        Ok(Self { data, scratch })
    }

    /// A fresh directory for one probe.
    pub fn dir(&self, name: &str) -> Result<PathBuf, String> {
        let d = self.scratch.join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).map_err(|e| format!("create {}: {e}", d.display()))?;
        Ok(d)
    }
}

pub type Probe = fn(&Ctx) -> Result<Value, String>;

/// A real OS credential store (Keychain, Credential Manager) in a namespace no other call shares, so each call is its own
/// "device": `app.rightsuite.rke2e.probe.<pid>.<tag>.<n>`, non-interactive (a store that would prompt is a failure).
pub fn device_store(tag: &str) -> Result<rightkit_credentials::Store, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "app.rightsuite.rke2e.probe.{}.{tag}.{n}",
        std::process::id()
    );
    let ns = rightkit_credentials::Namespace::new(name).map_err(|e| format!("namespace: {e}"))?;
    Ok(rightkit_credentials::Store::open(rightkit_credentials::Config::new(ns)).non_interactive())
}

pub mod agent_tools;
pub mod agents;
pub mod align;
#[cfg(target_os = "android")]
pub mod android;
pub mod app_settings;
pub mod asr;
pub mod audio;
pub mod automation;
pub mod browser;
pub mod cache;
pub mod channel_memory;
pub mod child_process;
pub mod compose;
pub mod contracts;
pub mod crash;
pub mod credentials;
pub mod crypto;
pub mod daemon;
pub mod db;
pub mod device;
pub mod docs;
pub mod embed;
pub mod ffmpeg;
pub mod file_watch;
pub mod fs;
pub mod fsindex;
pub mod google_identity;
pub mod hooks;
pub mod hotkeys;
pub mod http;
pub mod identity;
pub mod job_queue;
pub mod license;
pub mod llm;
pub mod logs;
pub mod mcp;
pub mod models;
pub mod motion;
pub mod oauth;
pub mod ocr;
pub mod onnx;
pub mod overlay;
#[cfg(windows)]
pub mod overlay_win;
pub mod ownership;
pub mod pdf;
pub mod platform_rules;
pub mod playback;
pub mod qa;
pub mod qa_driver;
pub mod record;
pub mod review;
pub mod sandbox;
pub mod search;
pub mod send;
pub mod sidecar;
pub mod speech_client;
pub mod spend;
pub mod sync;
pub mod sync_keys;
pub mod tauri_shell;
pub mod time;
pub mod transcript;
pub mod tts;
pub mod tts_qwen;
pub mod tts_qwen_sys;
pub mod typeset;
pub mod vad;
pub mod vault;
pub mod workflow;

/// Every probe, by crate name, in name order.
pub fn all() -> Vec<(&'static str, Probe)> {
    let mut v: Vec<(&'static str, Probe)> = Vec::new();
    v.push(("rightkit-agent-tools", agent_tools::probe));
    v.push(("rightkit-agents", agents::probe));
    v.push(("rightkit-align", align::probe));
    #[cfg(target_os = "android")]
    v.push(("rightkit-android", android::probe));
    v.push(("rightkit-app-settings", app_settings::probe));
    v.push(("rightkit-asr", asr::probe));
    v.push(("rightkit-audio", audio::probe));
    v.push(("rightkit-automation", automation::probe));
    v.push(("rightkit-browser", browser::probe));
    v.push(("rightkit-cache", cache::probe));
    v.push(("rightkit-channel-memory", channel_memory::probe));
    v.push(("rightkit-child-process", child_process::probe));
    v.push(("rightkit-compose", compose::probe));
    v.push(("rightkit-contracts", contracts::probe));
    v.push(("rightkit-crash", crash::probe));
    v.push(("rightkit-credentials", credentials::probe));
    v.push(("rightkit-crypto", crypto::probe));
    v.push(("rightkit-daemon", daemon::probe));
    v.push(("rightkit-db", db::probe));
    v.push(("rightkit-device", device::probe));
    v.push(("rightkit-docs", docs::probe));
    v.push(("rightkit-embed", embed::probe));
    v.push(("rightkit-ffmpeg", ffmpeg::probe));
    v.push(("rightkit-file-watch", file_watch::probe));
    v.push(("rightkit-fs", fs::probe));
    v.push(("rightkit-fsindex", fsindex::probe));
    v.push(("rightkit-google-identity", google_identity::probe));
    v.push(("rightkit-hooks", hooks::probe));
    v.push(("rightkit-hotkeys", hotkeys::probe));
    v.push(("rightkit-http", http::probe));
    v.push(("rightkit-identity", identity::probe));
    v.push(("rightkit-job-queue", job_queue::probe));
    v.push(("rightkit-license", license::probe));
    v.push(("rightkit-llm", llm::probe));
    v.push(("rightkit-logs", logs::probe));
    v.push(("rightkit-mcp", mcp::probe));
    v.push(("rightkit-models", models::probe));
    v.push(("rightkit-motion", motion::probe));
    v.push(("rightkit-oauth", oauth::probe));
    v.push(("rightkit-ocr", ocr::probe));
    v.push(("rightkit-onnx", onnx::probe));
    v.push(("rightkit-overlay", overlay::probe));
    #[cfg(windows)]
    v.push(("rightkit-overlay-win", overlay_win::probe));
    v.push(("rightkit-ownership", ownership::probe));
    v.push(("rightkit-pdf", pdf::probe));
    v.push(("rightkit-platform-rules", platform_rules::probe));
    v.push(("rightkit-playback", playback::probe));
    v.push(("rightkit-qa", qa::probe));
    v.push(("rightkit-qa-driver", qa_driver::probe));
    v.push(("rightkit-record", record::probe));
    v.push(("rightkit-review", review::probe));
    v.push(("rightkit-sandbox", sandbox::probe));
    v.push(("rightkit-search", search::probe));
    v.push(("rightkit-send", send::probe));
    v.push(("rightkit-sidecar", sidecar::probe));
    v.push(("rightkit-speech-client", speech_client::probe));
    v.push(("rightkit-spend", spend::probe));
    v.push(("rightkit-sync", sync::probe));
    v.push(("rightkit-sync-keys", sync_keys::probe));
    v.push(("rightkit-tauri-shell", tauri_shell::probe));
    v.push(("rightkit-time", time::probe));
    v.push(("rightkit-transcript", transcript::probe));
    v.push(("rightkit-tts", tts::probe));
    v.push(("rightkit-tts-qwen", tts_qwen::probe));
    v.push(("rightkit-tts-qwen-sys", tts_qwen_sys::probe));
    v.push(("rightkit-typeset", typeset::probe));
    v.push(("rightkit-vad", vad::probe));
    v.push(("rightkit-vault", vault::probe));
    v.push(("rightkit-workflow", workflow::probe));
    v
}

/// A probe's error that starts with `host_missing:<need>:` means this host lacks something the crate needs (a model, a
/// runtime, an OS permission, a service), not that the crate is broken. It is reported as its own state, and it is a
/// failure when the host profile says the need is provisioned (`RKE2E_PROVISIONED`, a comma list of needs).
pub const HOST_MISSING: &str = "host_missing:";

fn provisioned(need: &str) -> bool {
    std::env::var("RKE2E_PROVISIONED")
        .map(|v| v.split(',').any(|n| n.trim() == need))
        .unwrap_or(false)
}

/// The result row of one probe: `state` is `pass`, `fail` or `host_missing` (with `need`); `ok` is `state == pass`.
fn row(name: &str, ms: u64, outcome: Result<Value, String>) -> Value {
    let error = match outcome {
        Ok(detail) => {
            return serde_json::json!({ "name": name, "state": "pass", "ok": true, "ms": ms, "detail": detail })
        }
        Err(error) => error,
    };
    match error
        .strip_prefix(HOST_MISSING)
        .and_then(|rest| rest.split_once(':'))
    {
        Some((need, why)) if !provisioned(need) => serde_json::json!({
            "name": name, "state": "host_missing", "ok": false, "ms": ms, "need": need, "error": why.trim()
        }),
        Some((need, why)) => serde_json::json!({
            "name": name, "state": "fail", "ok": false, "ms": ms, "need": need,
            "error": format!("{} (the host profile provisions {need})", why.trim())
        }),
        None => {
            serde_json::json!({ "name": name, "state": "fail", "ok": false, "ms": ms, "error": error })
        }
    }
}

/// Run one probe in this process; a panic inside it is a failure, never a crash of the engine.
pub fn run(ctx: &Ctx, name: &str) -> Value {
    let Some((_, probe)) = all().into_iter().find(|(n, _)| *n == name) else {
        return row(name, 0, Err("no such probe".into()));
    };
    let started = std::time::Instant::now();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe(ctx)));
    let ms = started.elapsed().as_millis() as u64;
    row(
        name,
        ms,
        outcome.unwrap_or_else(|_| Err("the probe panicked".into())),
    )
}

/// How long one probe may run in `probe all` before its process tree is killed (`RKE2E_PROBE_DEADLINE_SECS`, default 180).
fn deadline() -> std::time::Duration {
    let secs = std::env::var("RKE2E_PROBE_DEADLINE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(180);
    std::time::Duration::from_secs(secs)
}

/// Run one probe in its own engine process (`probe run`) under a deadline, through rightkit-child-process: a native abort,
/// a hang or a blocking OS prompt costs that probe's row, never the other probes'.
pub fn run_isolated(name: &str) -> Value {
    let started = std::time::Instant::now();
    let ms = || started.elapsed().as_millis() as u64;
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            return row(
                name,
                0,
                Err(format!("cannot locate the engine executable: {e}")),
            )
        }
    };
    let input = serde_json::json!({ "name": name }).to_string();
    let options = rightkit_child_process::RunOptions::new(deadline()).stdout_cap(8 * 1024 * 1024);
    let args = ["probe", "run", "--input", input.as_str()];
    let out = match rightkit_child_process::run(&exe, args, &options) {
        Ok(out) => out,
        Err(e) => return row(name, ms(), Err(format!("cannot start `probe run`: {e}"))),
    };
    let stderr = String::from_utf8_lossy(&out.stderr.bytes);
    let mut tail: Vec<&str> = stderr.lines().rev().take(8).collect();
    tail.reverse();
    let tail = tail.join(" | ");
    if out.timed_out {
        let secs = deadline().as_secs();
        return row(
            name,
            ms(),
            Err(format!(
                "timed out after {secs}s and was killed; stderr: {tail}"
            )),
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout.bytes);
    // The CLI prints one pretty JSON document: the row itself, or an envelope carrying it as `result`.
    let found = serde_json::from_str::<Value>(stdout.trim())
        .ok()
        .map(|v| match v.get("result") {
            Some(inner) if v.get("state").is_none() => inner.clone(),
            _ => v,
        })
        .filter(|v| v.get("state").is_some());
    match found {
        Some(mut v) => {
            v["ms"] = serde_json::json!(ms());
            v
        }
        None => row(
            name,
            ms(),
            Err(format!(
                "`probe run` exited {:?} with no result; stderr: {tail}",
                out.status
            )),
        ),
    }
}
