// Native CoreML Parakeet engine for Apple Silicon (the ANE).
//
// Runs the validated 4-stage static-shape `.mlmodelc` / `.mlpackage` bundle
// (MelSpectrogram · AudioEncoder · TextDecoder · Joint) directly on
// CoreML.framework via `objc2-core-ml` — in-process, no Python, no subprocess.
// See `model.rs` for the port's load-bearing details.

mod command_alignment;
mod decode_topk;
mod decode_window;
mod detok;
mod display;
mod engine;
mod frontend_lookahead_probe;
mod fused_probe;
mod hypothesis_batch;
mod input_reuse_probe;
mod model;
mod probe;
mod stats;
mod subtitles;
mod window_health;

pub use command_alignment::{CommandAlignment, CommandAlignmentMethod, MIN_COMMAND_LOGPROB_MEAN};
pub use frontend_lookahead_probe::{
    estimate_peak_bytes, EncodedWindow, FrontendLookaheadConfig, FrontendLookaheadCounters,
    FrontendLookaheadProbe, FrontendLookaheadStats, MemoryGate, MIN_AVAILABLE_MEMORY_BYTES,
    MIN_PHYSICAL_MEMORY_BYTES,
};
pub use model::{
    bundle_dir, CoreMlParakeet, PipelineMeta, ProductionActivation, TimedPiece, TimedTok,
};
pub use probe::compute_differential_probe;
pub use subtitles::render_subtitles;
pub use window_health::{
    clear_calibration_window_stats, clear_latest_asr_window_stats, install_asr_window_input,
    latest_asr_window_stats, take_calibration_window_stats, take_latest_asr_window_stats,
    AsrWindowInput, AsrWindowInputScope, AsrWindowStats, AsrWindowStatsEnvelope,
    CalibrationWindowStats, NoEmissionGap, TensorHealth, ASR_WINDOW_STATS_UPSTREAM_INPUTS_NEEDED,
};
