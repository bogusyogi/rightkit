use crate::Qwen3TtsError;
use prost::Message;

pub const VOICE_CLONE_PROMPT_V2_SCHEMA_VERSION: u32 = 2;

/// Hand-written prost messages for `proto/voice_clone_prompt.proto`
/// (package `qwen3tts.prompt`). Field tags and wire types are the schema's;
/// `proto/voice_clone_prompt.proto` stays in the package as the reference.
pub mod proto {
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct TensorI32 {
        #[prost(uint32, repeated, tag = "1")]
        pub shape: ::prost::alloc::vec::Vec<u32>,
        #[prost(sint32, repeated, tag = "2")]
        pub values: ::prost::alloc::vec::Vec<i32>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct TensorF32 {
        #[prost(uint32, repeated, tag = "1")]
        pub shape: ::prost::alloc::vec::Vec<u32>,
        #[prost(float, repeated, tag = "2")]
        pub values: ::prost::alloc::vec::Vec<f32>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct VoiceClonePromptV2 {
        #[prost(uint32, tag = "1")]
        pub schema_version: u32,
        #[prost(string, tag = "2")]
        pub source: ::prost::alloc::string::String,
        #[prost(string, tag = "3")]
        pub model_id: ::prost::alloc::string::String,
        #[prost(uint32, tag = "4")]
        pub speaker_encoder_sample_rate_hz: u32,
        #[prost(bool, tag = "5")]
        pub x_vector_only_mode: bool,
        #[prost(bool, tag = "6")]
        pub icl_mode: bool,
        #[prost(string, tag = "7")]
        pub ref_text: ::prost::alloc::string::String,
        #[prost(message, optional, tag = "8")]
        pub ref_code: ::core::option::Option<TensorI32>,
        #[prost(message, optional, tag = "9")]
        pub ref_spk_embedding: ::core::option::Option<TensorF32>,
    }
}

pub use proto::{TensorF32, TensorI32, VoiceClonePromptV2};

fn validate_shape(shape: &[u32], values_len: usize, field_name: &str) -> Result<(), Qwen3TtsError> {
    if shape.is_empty() {
        return Err(Qwen3TtsError::InvalidInput(format!(
            "{field_name}.shape must not be empty"
        )));
    }
    let element_count = shape.iter().copied().try_fold(1usize, |acc, dim| {
        let dim = usize::try_from(dim).map_err(|_| {
            Qwen3TtsError::InvalidInput(format!("{field_name}.shape contains out-of-range dim"))
        })?;
        if dim == 0 {
            return Err(Qwen3TtsError::InvalidInput(format!(
                "{field_name}.shape must not contain zero"
            )));
        }
        acc.checked_mul(dim).ok_or_else(|| {
            Qwen3TtsError::InvalidInput(format!("{field_name}.shape overflows element count"))
        })
    })?;
    if element_count != values_len {
        return Err(Qwen3TtsError::InvalidInput(format!(
            "{field_name}.values length {values_len} does not match shape product {element_count}"
        )));
    }
    Ok(())
}

impl TensorI32 {
    fn validate(&self, field_name: &str) -> Result<(), Qwen3TtsError> {
        validate_shape(&self.shape, self.values.len(), field_name)
    }
}

impl TensorF32 {
    fn validate(&self, field_name: &str) -> Result<(), Qwen3TtsError> {
        validate_shape(&self.shape, self.values.len(), field_name)
    }
}

impl VoiceClonePromptV2 {
    pub fn from_protobuf_bytes(pb_bytes: &[u8]) -> Result<Self, Qwen3TtsError> {
        let prompt = Self::decode(pb_bytes).map_err(|err| {
            Qwen3TtsError::InvalidInput(format!("invalid voice clone prompt protobuf: {err}"))
        })?;
        prompt.validate()?;
        Ok(prompt)
    }

    pub fn to_protobuf_vec(&self) -> Result<Vec<u8>, Qwen3TtsError> {
        self.validate()?;
        Ok(self.encode_to_vec())
    }

    pub fn validate(&self) -> Result<(), Qwen3TtsError> {
        if self.schema_version != VOICE_CLONE_PROMPT_V2_SCHEMA_VERSION {
            return Err(Qwen3TtsError::InvalidInput(format!(
                "unsupported voice clone prompt schema_version: {}",
                self.schema_version
            )));
        }

        let speaker = self.ref_spk_embedding.as_ref().ok_or_else(|| {
            Qwen3TtsError::InvalidInput("voice clone prompt ref_spk_embedding is required".into())
        })?;
        speaker.validate("voice clone prompt ref_spk_embedding")?;
        if speaker.shape.len() != 1 {
            return Err(Qwen3TtsError::InvalidInput(
                "voice clone prompt ref_spk_embedding must be a 1D tensor".into(),
            ));
        }

        match (self.x_vector_only_mode, self.icl_mode) {
            (true, false) => {
                if let Some(ref_code) = &self.ref_code {
                    ref_code.validate("voice clone prompt ref_code")?;
                    if !ref_code.values.is_empty() {
                        return Err(Qwen3TtsError::InvalidInput(
                            "voice clone prompt ref_code must be empty in x-vector-only mode"
                                .into(),
                        ));
                    }
                }
            }
            (false, true) => {
                if self.ref_text.trim().is_empty() {
                    return Err(Qwen3TtsError::InvalidInput(
                        "voice clone prompt ref_text is required in icl_mode".into(),
                    ));
                }
                let ref_code = self.ref_code.as_ref().ok_or_else(|| {
                    Qwen3TtsError::InvalidInput(
                        "voice clone prompt ref_code is required in icl_mode".into(),
                    )
                })?;
                ref_code.validate("voice clone prompt ref_code")?;
                if ref_code.shape.len() != 2 {
                    return Err(Qwen3TtsError::InvalidInput(
                        "voice clone prompt ref_code must be a 2D tensor [frames, codebooks]"
                            .into(),
                    ));
                }
            }
            _ => {
                return Err(Qwen3TtsError::InvalidInput(
                    "voice clone prompt must use exactly one of x_vector_only_mode or icl_mode"
                        .into(),
                ));
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn speaker_embedding(&self) -> &[f32] {
        self.ref_spk_embedding
            .as_ref()
            .map(|tensor| tensor.values.as_slice())
            .unwrap_or(&[])
    }

    #[must_use]
    pub fn speaker_embedding_dim(&self) -> Option<usize> {
        self.ref_spk_embedding
            .as_ref()
            .and_then(|tensor| tensor.shape.first().copied())
            .map(|dim| dim as usize)
    }

    #[must_use]
    pub fn ref_code_shape(&self) -> Option<(usize, usize)> {
        self.ref_code.as_ref().and_then(|tensor| {
            if tensor.shape.len() == 2 {
                Some((tensor.shape[0] as usize, tensor.shape[1] as usize))
            } else {
                None
            }
        })
    }

    #[must_use]
    pub fn ref_code_values(&self) -> Option<&[i32]> {
        self.ref_code
            .as_ref()
            .map(|tensor| tensor.values.as_slice())
    }
}
