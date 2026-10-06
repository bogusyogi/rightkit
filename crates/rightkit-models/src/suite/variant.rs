//! Canonical graph/precision metadata for the two shipped Parakeet artifacts.
//! Descriptors are compiled into the owner crate, never read from local claims.
//! Readiness still requires the catalog's complete manifest & per-file hashes.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct VariantDescriptor {
    pub id: &'static str,
    /// Immutable archive SHA-256, also used as the trusted manifest revision.
    pub revision: &'static str,
    pub format: &'static str,
    pub encoder_input_name: &'static str,
    pub encoder_input_dtype: &'static str,
    /// None is a graph-declared dynamic dimension, not a fixed batch of one.
    pub encoder_input_shape: &'static [Option<u64>],
    pub encoder_weights: &'static str,
    pub decoder_joint_weights: &'static str,
}

/// Native platform's sole approved Parakeet variant. Unsupported platforms
/// have no descriptor or hosted download. Package sources are never approved.
pub fn parakeet_variant_descriptor() -> Option<VariantDescriptor> {
    #[cfg(target_os = "macos")]
    {
        Some(VariantDescriptor {
            id: "parakeet-tdt-v3-coreml-compiled-5f8888117f47",
            revision: "5f8888117f4750828afeb7aa185a706299d0a0ef144190ce9554d82962d51899",
            format: "coreml_mlmodelc",
            encoder_input_name: "melspectrogram_features",
            encoder_input_dtype: "fp16",
            encoder_input_shape: &[Some(1), Some(128), Some(1500)],
            encoder_weights: "palettized_4bit",
            decoder_joint_weights: "int8_per_channel",
        })
    }
    #[cfg(windows)]
    {
        Some(VariantDescriptor {
            id: "parakeet_tdt_v3_static1500_qint8_20260722",
            revision: "f73e8671086bfe369e263468a91826be24cd59d826851ae531dddd5033c4b99e",
            format: "onnx",
            encoder_input_name: "audio_signal",
            encoder_input_dtype: "fp32",
            encoder_input_shape: &[None, Some(128), Some(1500)],
            // Pinned encoder graph: 180 MatMulNBits nodes, bits=4; remaining
            // initializers include FP16 & FP32. QInt8 names the decoder/joint.
            encoder_weights: "mixed_fp16_fp32_matmul_nbits_4bit",
            decoder_joint_weights: "dynamic_qint8",
        })
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    None
}
