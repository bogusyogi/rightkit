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
        std::fs::create_dir_all(&scratch).map_err(|e| format!("create {}: {e}", scratch.display()))?;
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

/// Run one probe; a panic inside it is a failure, never a crash of the engine.
pub fn run(ctx: &Ctx, name: &str) -> Value {
    let Some((_, probe)) = all().into_iter().find(|(n, _)| *n == name) else {
        return serde_json::json!({ "name": name, "ok": false, "error": "no such probe" });
    };
    let started = std::time::Instant::now();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe(ctx)));
    let ms = started.elapsed().as_millis() as u64;
    match outcome {
        Ok(Ok(detail)) => serde_json::json!({ "name": name, "ok": true, "ms": ms, "detail": detail }),
        Ok(Err(error)) => serde_json::json!({ "name": name, "ok": false, "ms": ms, "error": error }),
        Err(_) => serde_json::json!({ "name": name, "ok": false, "ms": ms, "error": "the probe panicked" }),
    }
}
