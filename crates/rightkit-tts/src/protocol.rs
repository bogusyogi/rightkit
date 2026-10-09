//! Worker wire protocol: one JSON object per line. Worker replies are
//! prefixed with [`REPLY_PREFIX`] so ggml/ORT noise on stdout cannot corrupt
//! the stream; the client ignores every other line.
use serde::{Deserialize, Serialize};

pub const REPLY_PREFIX: &str = "@@GQTS@@ ";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Generate one chunk. PCM16 LE mono samples are written to `out_pcm`.
    Synth {
        id: u64,
        text: String,
        #[serde(default)]
        voice_prompt: Option<String>,
        language_id: i32,
        max_frames: usize,
        temperature: f32,
        top_p: f32,
        top_k: i32,
        out_pcm: String,
    },
    /// Encode a reference WAV into an x-vector voice-clone prompt file.
    Clone {
        id: u64,
        wav: String,
        out: String,
    },
    Quit,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// Sent once after the engine loaded.
    Ready {
        backend: String,
        vocoder: String,
        load_ms: u64,
        version: String,
    },
    Done {
        id: u64,
        samples: usize,
        sample_rate: u32,
        gen_ms: u64,
    },
    Error {
        id: u64,
        message: String,
    },
}
