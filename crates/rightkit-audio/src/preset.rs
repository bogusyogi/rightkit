//! JSON presets: `podcast`, `talking-head`, `noisy-room`. Shipped as data
//! (`presets/*.json`, embedded at build time); a project may also load its own JSON.

use serde::{Deserialize, Serialize};

use crate::chain::DialogueChain;
use crate::ducking::Ducking;
use crate::error::AudioError;
use crate::profile::AudioGate;
use crate::room_tone::RoomToneParams;

pub const PRESET_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomTonePolicy {
    pub enabled: bool,
    /// Level of the room-tone bed relative to the captured sample.
    #[serde(default)]
    pub gain_db: f64,
    /// Each fill extends this far under the neighbouring speech (J/L-style overlap).
    #[serde(default = "default_overlap")]
    pub overlap_ms: i64,
    #[serde(default)]
    pub params: RoomToneParams,
}

fn default_overlap() -> i64 {
    80
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub schema_version: u32,
    pub name: String,
    pub description: String,
    pub chain: DialogueChain,
    pub room_tone: RoomTonePolicy,
    pub ducking: Ducking,
    pub gate: AudioGate,
}

const BUILTIN: [(&str, &str); 3] = [
    ("podcast", include_str!("../presets/podcast.json")),
    ("talking-head", include_str!("../presets/talking-head.json")),
    ("noisy-room", include_str!("../presets/noisy-room.json")),
];

impl Preset {
    pub fn builtin_names() -> Vec<&'static str> {
        BUILTIN.iter().map(|(n, _)| *n).collect()
    }

    pub fn builtin(name: &str) -> Result<Self, AudioError> {
        let (_, json) = BUILTIN
            .iter()
            .find(|(n, _)| *n == name)
            .ok_or_else(|| AudioError::Invalid(format!("unknown audio preset {name:?}")))?;
        Self::from_json(json)
    }

    pub fn from_json(json: &str) -> Result<Self, AudioError> {
        let preset: Preset = serde_json::from_str(json)?;
        preset.validate()?;
        Ok(preset)
    }

    pub fn to_json(&self) -> Result<String, AudioError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn validate(&self) -> Result<(), AudioError> {
        if self.schema_version != PRESET_SCHEMA_VERSION {
            return Err(AudioError::Invalid(format!(
                "unsupported preset schema_version {}",
                self.schema_version
            )));
        }
        self.chain.validate()?;
        self.ducking.validate()?;
        self.room_tone.params.validate()?;
        Ok(())
    }
}
