// Shared native CoreML.framework bridge (Apple Silicon / ANE).
//
// Generic `MLModel`/`MLMultiArray` plumbing reused by every on-device CoreML
// engine — Parakeet TDT (`coreml_asr`) and WhisperKit (`whisper_coreml`).
// Pure Rust on `objc2-core-ml`; no Swift, no subprocess.
//
// THE load-bearing subtlety: CoreML prediction outputs are NOT guaranteed
// C-contiguous — they can carry arbitrary element strides. `read_f16` gathers
// through `MLMultiArray.strides()`; reading `dataPointer` linearly would
// scramble the layout (and min/max/mean wouldn't reveal it). See
// docs/ARCHITECTURE.md.
#![allow(deprecated)]

pub mod aligned_probe;
mod multiarray;
mod stage;

pub use multiarray::{f16_set, i32_set, ml_f16, ml_f32, ml_i32, read_f16, read_f32};
pub(crate) use stage::inference_lease;
pub(crate) use stage::{clear_evaluator_output_backings, set_evaluator_output_backings};
pub use stage::{exception_bridge_smoke, nserr, Stage};
