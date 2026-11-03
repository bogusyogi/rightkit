//! The `mic_play` step: play one of the owner's recordings as the app's microphone input
//! (rightkit#1451, see [`crate::mic`]).
//!
//! Fields: `file` (required; relative to `[audio.mic] recordings`, the owner's own folder), `timeout_s`
//! (default 120, at most 600). The step waits for the recording to finish. Result `{ file, sha256,
//! bytes, device, player, started_at, ended_at }`, also written to `mic-play-<n>.json` in the log
//! directory and recorded as evidence, so a receipt names the recording that played and when.
//!
//! The app's input device (the virtual loopback device) is selected by the scenario through the app,
//! as a user would; the selection is part of the app's real settings, which the journey's snapshot
//! restores. The step changes no system audio setting.
use super::{only, required, uint};
use crate::scenario::Ctx;
use crate::util::{err, Error, Result};
use serde_json::{Map, Value};
use std::time::Duration;

pub(super) fn run(ctx: &mut Ctx, p: &Map<String, Value>) -> Result<Value> {
    only(p, "mic_play", &["file", "timeout_s"])?;
    let file = required(p, "mic_play", "file")?;
    let timeout = Duration::from_secs(uint(p, "timeout_s")?.unwrap_or(120).clamp(1, 600));
    let Some(mic) = ctx.cfg.audio.mic.clone() else {
        return err("mic_play needs [audio.mic] in rightkit-qa.toml (recordings = the owner's folder of recordings, device = the virtual loopback device)");
    };
    let played = crate::mic::play(&mic, &file, timeout, ctx.tracker)?;
    let doc = played.to_json();
    let n = ctx
        .artifacts
        .iter()
        .filter(|(name, _)| name.contains("mic-play-"))
        .count()
        + 1;
    let path = ctx.log_dir.join(format!("mic-play-{n}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap_or_default())
        .map_err(|e| Error(format!("cannot write {}: {e}", path.display())))?;
    ctx.artifact(&format!("mic-play-{n}.json"), path);
    Ok(doc)
}
