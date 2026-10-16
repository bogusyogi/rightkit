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

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_json(extra: &str) -> String {
        format!(
            r#"{{"op":"synth","id":7,"text":"hi","voice_prompt":null,{extra}"language_id":2050,"max_frames":100,"temperature":0.9,"top_p":1.0,"top_k":50,"out_pcm":"/tmp/x.pcm"}}"#
        )
    }

    #[test]
    fn old_synth_json_without_mode_fields_still_parses_as_base() {
        let json = synth_json("");
        match serde_json::from_str::<Request>(&json).unwrap() {
            Request::Synth {
                mode,
                instruct,
                speaker,
                voice_prompt,
                ..
            } => {
                assert_eq!(mode, None);
                assert_eq!(instruct, None);
                assert_eq!(speaker, None);
                assert_eq!(voice_prompt, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn synth_round_trips_with_mode_fields() {
        let req = Request::Synth {
            id: 3,
            text: "hello".into(),
            voice_prompt: None,
            mode: Some(Mode::CustomVoice),
            instruct: Some("warm".into()),
            speaker: Some("vivian".into()),
            language_id: 2050,
            max_frames: 10,
            temperature: 0.5,
            top_p: 1.0,
            top_k: 20,
            out_pcm: "a.pcm".into(),
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.contains(r#""mode":"custom_voice""#), "{line}");
        assert!(line.contains(r#""speaker":"vivian""#), "{line}");
        match serde_json::from_str::<Request>(&line).unwrap() {
            Request::Synth {
                id,
                mode,
                instruct,
                speaker,
                ..
            } => {
                assert_eq!(id, 3);
                assert_eq!(mode, Some(Mode::CustomVoice));
                assert_eq!(instruct.as_deref(), Some("warm"));
                assert_eq!(speaker.as_deref(), Some("vivian"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn synth_round_trips_mode_and_fields_present_as_null() {
        let json = synth_json(r#""mode":"voice_design","instruct":"deep","speaker":null,"#);
        match serde_json::from_str::<Request>(&json).unwrap() {
            Request::Synth {
                mode,
                instruct,
                speaker,
                ..
            } => {
                assert_eq!(mode, Some(Mode::VoiceDesign));
                assert_eq!(instruct.as_deref(), Some("deep"));
                assert_eq!(speaker, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_keys_are_ignored_so_older_workers_accept_new_requests() {
        let json = synth_json(r#""mode":"base","future_field":{"a":1},"#);
        assert!(serde_json::from_str::<Request>(&json).is_ok());
    }

    #[test]
    fn mode_names_round_trip_on_the_wire_and_on_the_command_line() {
        for (mode, name) in [
            (Mode::Base, "base"),
            (Mode::CustomVoice, "custom_voice"),
            (Mode::VoiceDesign, "voice_design"),
        ] {
            assert_eq!(mode.as_str(), name);
            assert_eq!(Mode::parse(name), Some(mode));
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{name}\""));
        }
        assert_eq!(Mode::parse(" Voice_Design "), Some(Mode::VoiceDesign));
        assert_eq!(Mode::parse("customvoice"), None);
        assert_eq!(Mode::default(), Mode::Base);
    }
}
