//! The local voice layer over `rightkit-tts` (Qwen3-TTS "QTS"): model and runtime locations under
//! the host's data root, the shared ONNX Runtime resolution the local ONNX providers use, and a
//! client facade. The default build compiles none of ggml.

pub use rightkit_tts::SAMPLE_RATE_HZ;
pub use rightkit_tts::{chunk, wav};

pub mod client;
pub mod models;
