//! Speech tools over the shared speech service (`rightkit-speech-client`, the
//! first client of this crate). File transcription only: the service allowlist
//! carries health, capabilities, info and file requests, and nothing here
//! reaches dictation, paste, or history.

use crate::spec::*;
use rightkit_daemon::Suite;
use rightkit_speech_client::{self as speech, ClientError, HostRole, TranscribeOptions};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn map(e: ClientError) -> ToolError {
    match e {
        ClientError::NoService => ToolError::retryable(
            "speech_service_unavailable",
            "no live speech service (start HeardRight or another host)",
        ),
        ClientError::HandoffRetry => ToolError::retryable(
            "speech_service_handoff",
            "the speech host is handing off; retry",
        ),
        other => ToolError::new("speech_error", format!("{other:?}")),
    }
}

struct Transcribe {
    spec: ToolSpec,
    root: PathBuf,
}

impl Tool for Transcribe {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn call(&self, ctx: &CallCtx, args: Value) -> Result<Value, ToolError> {
        let path = Path::new(args["path"].as_str().unwrap_or_default());
        if !path.is_absolute() || !path.is_file() {
            return Err(ToolError::new(
                ERR_INVALID_ARGS,
                "`path` must be an absolute path to an existing media file",
            ));
        }
        let stall = Duration::from_secs(args["stall_timeout_secs"].as_u64().unwrap_or(120));
        let mut opts = TranscribeOptions::new(stall);
        if let Some(l) = args["language"].as_str() {
            opts = opts.language(l);
        }
        let token = speech::CancelToken::new();
        let t = token.clone();
        let cancel = ctx.cancel.clone();
        // Bridge our cancellation flag to the client's token.
        let watcher_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wd = watcher_done.clone();
        let w = std::thread::spawn(move || {
            while !wd.load(std::sync::atomic::Ordering::SeqCst) {
                if cancel.is_cancelled() {
                    t.cancel();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        opts = opts.cancel_token(token);
        let r =
            speech::transcribe_via_service_with(&self.root, HostRole::CutRight, path, &mut opts);
        watcher_done.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = w.join();
        let r = r.map_err(map)?;
        Ok(json!({"text": r.transcript.text, "engine": r.engine}))
    }
}

struct Capabilities {
    spec: ToolSpec,
    root: PathBuf,
}

impl Tool for Capabilities {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn call(&self, _ctx: &CallCtx, _args: Value) -> Result<Value, ToolError> {
        let mut c = speech::connect(&self.root).map_err(map)?;
        let caps = c.capabilities(Duration::from_secs(15)).map_err(map)?;
        let engines: Vec<Value> = caps
            .engines
            .iter()
            .map(|e| json!({"id": e.id, "languages": e.languages, "ready": e.ready}))
            .collect();
        Ok(json!({"capabilities": caps.capabilities, "engines": engines}))
    }
}

/// `speech.transcribe{path,language?}` and `speech.capabilities`. HeardRight's
/// `right-ctl heardright transcribe <file>` routes to `speech.transcribe`.
pub fn speech_tools(suite: &Suite) -> Vec<Arc<dyn Tool>> {
    let root = suite.root().to_path_buf();
    vec![
        Arc::new(Transcribe {
            spec: ToolSpec::new(
                "speech.transcribe",
                "Transcribe an audio or video FILE through the shared speech service (not dictation).",
                Effect::Observe,
                json!({"type": "object", "properties": {"path": {"type": "string"}, "language": {"type": "string"}, "stall_timeout_secs": {"type": "integer"}}, "required": ["path"]}),
            ),
            root: root.clone(),
        }),
        Arc::new(Capabilities { spec: ToolSpec::new("speech.capabilities", "Engines and options the speech service offers.", Effect::Observe, json!({"type": "object", "properties": {}})), root }),
    ]
}
