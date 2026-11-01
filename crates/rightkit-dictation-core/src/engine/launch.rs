//! The engine's launch frame: the one line the shell writes on the engine's stdin before hello
//! (`rightkit-job-queue` `with_launch_frame` / `read_launch_frame`). It carries the model CHOICE
//! the engine cannot derive itself (backend, which Parakeet variant the shell activated, whether
//! this process is a load check). Paths the engine CAN derive (app data, models base, bundled
//! resources) never travel here, and the frame is never read from the environment or argv.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// `schema` of the engine launch frame (wire id kept for compatibility with existing engines).
pub const LAUNCH_SCHEMA: &str = "heardright.engine.launch";
/// Highest launch-frame version this build understands.
pub const LAUNCH_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineLaunch {
    /// ASR backend alias (`parakeet-tdt`, `whisper`, ...).
    pub backend: String,
    /// macOS: the Parakeet CoreML bundle directory the shell resolved (shared variant or legacy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coreml_model_dir: Option<PathBuf>,
    /// Windows: the resolved Parakeet model directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asr_model_subdir: Option<String>,
    /// True for a temporary load-check engine that borrows the parent's model lease.
    #[serde(default)]
    pub load_check: bool,
}

/// Largest launch-frame line, matching `rightkit-job-queue` `MAX_LAUNCH_FRAME_BYTES`.
pub const MAX_LAUNCH_BYTES: usize = 4096;
