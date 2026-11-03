//! Where each model mode lives, and the per-mode engine set a worker keeps.
//!
//! A deployment may keep the 0.6B Base model in one folder and the 1.7B `CustomVoice` and
//! `VoiceDesign` exports in others, with one shared vocoder. [`ModeLocation`] names one mode's
//! folder (and optionally its vocoder); the client resolves every mode and forwards the result to
//! the worker as arguments, so the worker knows all locations before it reports ready.
use crate::protocol::Mode;
use std::path::{Path, PathBuf};

/// File name of the shared ONNX vocoder inside a model folder.
pub const VOCODER_FILE: &str = "qwen3-tts-vocoder.onnx";

/// The model folder of one mode and, optionally, its vocoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeLocation {
    pub mode: Mode,
    pub model_dir: PathBuf,
    /// Vocoder for this mode. `None` means `<model_dir>/qwen3-tts-vocoder.onnx` when that file
    /// exists, else the default vocoder.
    pub vocoder: Option<PathBuf>,
}

impl ModeLocation {
    pub fn new(mode: Mode, model_dir: impl Into<PathBuf>) -> Self {
        Self {
            mode,
            model_dir: model_dir.into(),
            vocoder: None,
        }
    }

    pub fn with_vocoder(mut self, vocoder: impl Into<PathBuf>) -> Self {
        self.vocoder = Some(vocoder.into());
        self
    }
}

/// Resolve the folder and vocoder for `mode`. The last entry for `mode` in `entries` wins; a mode
/// without an entry uses `default_dir` and `default_vocoder`. An entry without a vocoder uses
/// its own folder's vocoder when that file exists, else `default_vocoder`.
pub fn resolve_location(
    default_dir: &Path,
    default_vocoder: &Path,
    entries: &[ModeLocation],
    mode: Mode,
) -> (PathBuf, PathBuf) {
    let Some(entry) = entries.iter().rev().find(|e| e.mode == mode) else {
        return (default_dir.to_path_buf(), default_vocoder.to_path_buf());
    };
    let vocoder = match &entry.vocoder {
        Some(v) => v.clone(),
        None => {
            let local = entry.model_dir.join(VOCODER_FILE);
            if local.is_file() {
                local
            } else {
                default_vocoder.to_path_buf()
            }
        }
    };
    (entry.model_dir.clone(), vocoder)
}

/// Engines a worker holds, one per mode, loaded on first use and kept. Generic over the engine so
/// the policy is testable without a model.
#[derive(Debug)]
pub struct EngineCache<E> {
    engines: Vec<(Mode, E)>,
}

impl<E> Default for EngineCache<E> {
    fn default() -> Self {
        Self {
            engines: Vec::new(),
        }
    }
}

impl<E> EngineCache<E> {
    /// The engine for `mode`, loading it with `load` only when none is held. A failed load holds
    /// nothing, so the next request tries again.
    pub fn get_or_load(
        &mut self,
        mode: Mode,
        load: impl FnOnce() -> Result<E, String>,
    ) -> Result<&E, String> {
        if let Some(i) = self.engines.iter().position(|(m, _)| *m == mode) {
            return Ok(&self.engines[i].1);
        }
        let engine = load()?;
        self.engines.push((mode, engine));
        Ok(&self.engines[self.engines.len() - 1].1)
    }

    /// Modes whose engine is held, in load order.
    pub fn loaded(&self) -> Vec<Mode> {
        self.engines.iter().map(|(m, _)| *m).collect()
    }
}
