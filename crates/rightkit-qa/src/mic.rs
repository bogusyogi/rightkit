//! Play an owner's recording as an app's microphone input (rightkit#1451).
//!
//! HeardRight's dictation, voice-command and wake-word journeys need audio at the app's capture
//! input. The installed app is not changed and carries no injection hook: the audio goes through a
//! virtual loopback device the owner has installed (BlackHole on macOS, VB-Cable on Windows), which
//! the app takes as its input device exactly as a user would pick it. The app's stored device
//! selection is part of its real settings, which the journey's snapshot and restore
//! ([`crate::snapshot`], rightkit#1437) put back. The real capture device is never touched, and
//! nothing here changes a system audio setting.
//!
//! ## What may play
//!
//! Adrian, 2026-11-03: "no dummy data, no fucking made up shit. real data." A made-up fixture WAV is
//! exactly that, so this module plays only a recording the owner supplied: `[audio.mic] recordings`
//! names the owner's own folder of recordings, and [`play`] refuses any file that does not resolve,
//! symlinks followed, inside it, is not a RIFF/WAVE file, or is missing. `rightkit-qa` ships no audio
//! and generates none. The record of each play ([`MicPlay`]: the file, its sha256 and size, the
//! device, the player and the start and end times) is written as evidence by the `mic_play` step, so a
//! receipt says which recording played and when.
//!
//! ## How it plays
//!
//! `sox <wav> -t coreaudio "<device>"` on macOS and `sox <wav> -t waveaudio "<device>"` on Windows,
//! run as an owned process with a deadline. `sox` is found at `[audio.mic] player` or on `PATH`. There
//! is no host-independent way to play to a named device without a native audio stack, so the player
//! is an external tool the owner installs with the loopback device; a missing player or device is an
//! error that says so, never a silent pass.

use crate::config::MicConfig;
use crate::process::{run_owned, RunOptions, Tracker};
use crate::util::{err, now_iso, sha256_hex, Error, Result};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// What one play did. Serialised into the step result and the evidence file.
#[derive(Debug, Clone)]
pub struct MicPlay {
    /// The recording, canonical, inside `recordings`.
    pub file: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub device: String,
    pub player: String,
    pub started_at: String,
    pub ended_at: String,
}

impl MicPlay {
    pub fn to_json(&self) -> Value {
        json!({
            "schema": "rightkit.qa.mic-play/1",
            "file": self.file.display().to_string(),
            "sha256": self.sha256,
            "bytes": self.bytes,
            "device": self.device,
            "player": self.player,
            "started_at": self.started_at,
            "ended_at": self.ended_at,
        })
    }
}

fn driver() -> Result<&'static str> {
    if cfg!(target_os = "macos") {
        Ok("coreaudio")
    } else if cfg!(windows) {
        Ok("waveaudio")
    } else {
        err("mic_play is supported on macOS and Windows only")
    }
}

/// `player` when it is an absolute file, else the first `sox` on `PATH`.
fn resolve_player(player: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = player.map(str::trim).filter(|p| !p.is_empty()) {
        let path = PathBuf::from(p);
        if !path.is_absolute() || !path.is_file() {
            return err(format!(
                "[audio.mic] player {p:?} is not an absolute path to a file"
            ));
        }
        return Ok(path);
    }
    let exe = if cfg!(windows) { "sox.exe" } else { "sox" };
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    err("sox was not found on PATH: install SoX beside the virtual loopback device, or name it in [audio.mic] player")
}

/// The recording `file` (relative to `recordings`, normal components only) as a canonical path
/// inside the owner's recordings folder.
fn resolve_recording(recordings: &Path, file: &str) -> Result<PathBuf> {
    if !recordings.is_absolute() {
        return err(format!(
            "[audio.mic] recordings {} is not an absolute path",
            recordings.display()
        ));
    }
    let root = crate::util::canonical_plain(recordings).map_err(|e| {
        Error(format!(
            "[audio.mic] recordings {} is not usable: {e}",
            recordings.display()
        ))
    })?;
    let rel = Path::new(file);
    if file.is_empty()
        || rel.is_absolute()
        || rel.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return err(format!(
            "mic_play file {file:?} must be a relative path inside the owner's recordings folder (no `..`)"
        ));
    }
    let full = crate::util::canonical_plain(&root.join(rel))
        .map_err(|e| Error(format!("the recording {file:?} cannot be read: {e}")))?;
    if !full.starts_with(&root) {
        return err(format!(
            "the recording {file:?} resolves outside the owner's recordings folder {}; refused",
            root.display()
        ));
    }
    if !full.is_file() {
        return err(format!("the recording {file:?} is not a file"));
    }
    Ok(full)
}

fn check_wav(bytes: &[u8], file: &Path) -> Result<()> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return err(format!("{} is not a RIFF/WAVE file", file.display()));
    }
    Ok(())
}

/// Play the owner's recording `file` into the virtual device and wait for it to finish (or the
/// deadline: the player's tree is then killed and the call is an error).
pub fn play(cfg: &MicConfig, file: &str, timeout: Duration, tracker: &Tracker) -> Result<MicPlay> {
    let driver = driver()?;
    if cfg.device.trim().is_empty() {
        return err("[audio.mic] device is required: the name of the virtual loopback device the app's input is set to");
    }
    let path = resolve_recording(Path::new(&cfg.recordings), file)?;
    let bytes =
        std::fs::read(&path).map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
    check_wav(&bytes, &path)?;
    let sha256 = sha256_hex(&bytes);
    let player = resolve_player(cfg.player.as_deref())?;
    let started_at = now_iso();
    let args = vec![
        path.display().to_string(),
        "-t".to_string(),
        driver.to_string(),
        cfg.device.clone(),
    ];
    let out = run_owned(
        &player.display().to_string(),
        &args,
        &RunOptions {
            timeout: Some(timeout),
            label: "mic_play".to_string(),
            ..RunOptions::default()
        },
        tracker,
    )?;
    let ended_at = now_iso();
    if out.timed_out {
        return err(format!(
            "playing {} to {:?} did not finish within {}s and was killed",
            path.display(),
            cfg.device,
            timeout.as_secs()
        ));
    }
    if out.code != Some(0) {
        return err(format!(
            "sox could not play {} to the device {:?} (exit {:?}); is the virtual loopback device installed and named exactly so? {}",
            path.display(),
            cfg.device,
            out.code,
            crate::util::tail(out.stderr.trim(), 400)
        ));
    }
    Ok(MicPlay {
        file: path,
        sha256,
        bytes: bytes.len() as u64,
        device: cfg.device.clone(),
        player: player.display().to_string(),
        started_at,
        ended_at,
    })
}
