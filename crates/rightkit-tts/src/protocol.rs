//! Worker wire protocol: one JSON object per line. Worker replies are
//! prefixed with [`REPLY_PREFIX`] so ggml/ORT noise on stdout cannot corrupt
//! the stream; the client ignores every other line.
use serde::{Deserialize, Serialize};

pub const REPLY_PREFIX: &str = "@@GQTS@@ ";

/// Model mode a worker is loaded for. `Base` (also the meaning of an absent
/// `mode` on the wire) is the 0.6B voice-clone path; the other two need the
/// 1.7B mode exports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Base,
    CustomVoice,
    VoiceDesign,
}

impl Mode {
    /// Wire and command-line name: `base`, `custom_voice`, `voice_design`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::CustomVoice => "custom_voice",
            Self::VoiceDesign => "voice_design",
        }
    }

    /// Parse a wire or command-line name, ignoring ASCII case and surrounding whitespace.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "base" => Some(Self::Base),
            "custom_voice" => Some(Self::CustomVoice),
            "voice_design" => Some(Self::VoiceDesign),
            _ => None,
        }
    }
}

/// Preset speaker names `CustomVoice` accepts, in every build (no `engine` feature needed). They
/// are the `as_str` forms of `rightkit-tts-qwen`'s `PresetSpeaker::ALL`; a test under the `engine`
/// feature keeps the two lists equal.
pub const PRESET_SPEAKERS: &[&str] = &[
    "vivian", "serena", "uncle_fu", "dylan", "eric", "ryan", "aiden", "ono_anna", "sohee",
];

/// True when `name` is a preset speaker, matched as `PresetSpeaker::parse` does: ASCII case and
/// surrounding whitespace are ignored, any other spelling (`uncle-fu`) is not a preset.
pub fn is_preset_speaker(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    PRESET_SPEAKERS.contains(&name.as_str())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Generate one chunk. PCM16 LE mono samples are written to `out_pcm`.
    Synth {
        id: u64,
        text: String,
        #[serde(default)]
        voice_prompt: Option<String>,
        #[serde(default)]
        mode: Option<Mode>,
        #[serde(default)]
        instruct: Option<String>,
        #[serde(default)]
        speaker: Option<String>,
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
