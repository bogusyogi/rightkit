use crate::{Qwen3TtsError, SynthesizeRequest, VoiceClonePromptV2};

/// Qwen3-TTS checkpoint family. Modes require corresponding model weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QtsMode {
    #[default]
    Base,
    CustomVoice,
    VoiceDesign,
}

impl QtsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::CustomVoice => "customvoice",
            Self::VoiceDesign => "voicedesign",
        }
    }
}

/// Names only; codec ids are resolved from each checkpoint's GGUF speaker table.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetSpeaker {
    Vivian,
    Serena,
    Uncle_Fu,
    Dylan,
    Eric,
    Ryan,
    Aiden,
    Ono_Anna,
    Sohee,
}

impl PresetSpeaker {
    pub const ALL: [Self; 9] = [
        Self::Vivian,
        Self::Serena,
        Self::Uncle_Fu,
        Self::Dylan,
        Self::Eric,
        Self::Ryan,
        Self::Aiden,
        Self::Ono_Anna,
        Self::Sohee,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vivian => "vivian",
            Self::Serena => "serena",
            Self::Uncle_Fu => "uncle_fu",
            Self::Dylan => "dylan",
            Self::Eric => "eric",
            Self::Ryan => "ryan",
            Self::Aiden => "aiden",
            Self::Ono_Anna => "ono_anna",
            Self::Sohee => "sohee",
        }
    }
}

impl PresetSpeaker {
    /// Parse a preset name (`as_str` form, e.g. `"uncle_fu"`). Matching ignores ASCII case and
    /// surrounding whitespace; any other spelling, including `"uncle-fu"`, returns `None`.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|speaker| speaker.as_str() == value)
    }
}

/// Additive mode API: existing `SynthesizeRequest` literals remain compatible.
#[derive(Debug, Clone, Default)]
pub struct ModeSynthesizeRequest<'a> {
    pub synthesis: SynthesizeRequest,
    pub mode: QtsMode,
    pub instruct: Option<String>,
    pub speaker: Option<PresetSpeaker>,
    pub voice_clone_prompt: Option<&'a VoiceClonePromptV2>,
    /// Optional Base embedding override; never used for VoiceDesign or presets.
    pub speaker_embedding: Option<&'a [f32]>,
}

impl ModeSynthesizeRequest<'_> {
    pub fn validate(&self) -> Result<(), Qwen3TtsError> {
        let error = |message: &str| Err(Qwen3TtsError::InvalidInput(message.into()));
        match self.mode {
            QtsMode::Base => {
                if self.voice_clone_prompt.is_none() {
                    return error("Base needs a voice-clone prompt");
                }
                if self.speaker.is_some() || self.instruct.is_some() {
                    return error("Base does not accept preset speaker or instruct");
                }
            }
            QtsMode::CustomVoice => {
                if self.speaker.is_none() {
                    return error("CustomVoice needs a preset speaker");
                }
                if self.voice_clone_prompt.is_some() || self.speaker_embedding.is_some() {
                    return error(
                        "CustomVoice does not accept voice-clone prompt or speaker embedding",
                    );
                }
            }
            QtsMode::VoiceDesign => {
                if self
                    .instruct
                    .as_ref()
                    .is_none_or(|text| text.trim().is_empty())
                {
                    return error("VoiceDesign needs nonempty instruct");
                }
                if self.speaker.is_some()
                    || self.voice_clone_prompt.is_some()
                    || self.speaker_embedding.is_some()
                {
                    return error("VoiceDesign does not accept speaker, speaker embedding or voice-clone prompt");
                }
            }
        }
        if let Some(prompt) = self.voice_clone_prompt {
            prompt.validate()?;
        }
        Ok(())
    }
}
