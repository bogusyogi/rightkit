use std::path::{Path, PathBuf};

/// Paths to the main GGUF plus vocoder artifacts.
#[derive(Debug, Clone)]
pub struct ModelPaths {
    /// Main TTS checkpoint (e.g. `qwen3-tts-0.6b-f16.gguf`).
    pub main_gguf: PathBuf,
    /// Vocoder artifact exported for ONNX Runtime.
    pub vocoder_onnx: PathBuf,
}

impl ModelPaths {
    pub fn new(main_gguf: PathBuf, vocoder_onnx: PathBuf) -> Self {
        Self {
            main_gguf,
            vocoder_onnx,
        }
    }

    /// Resolve under a single directory using the conventional filenames.
    pub fn from_model_dir(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        Self {
            main_gguf: choose_model_file(
                dir,
                &[
                    "qwen3-tts-0.6b-f16.gguf",
                    "qwen3-tts-0.6b-q8_0.gguf",
                    "qwen3-tts-0.6b-q6_k.gguf",
                    "qwen3-tts-0.6b-q5_k.gguf",
                    "qwen3-tts-0.6b-q4_k.gguf",
                ],
            ),
            vocoder_onnx: dir.join("qwen3-tts-vocoder.onnx"),
        }
    }

    pub fn main_exists(&self) -> bool {
        self.main_gguf.is_file()
    }

    pub fn vocoder_exists(&self) -> bool {
        self.vocoder_onnx.is_file()
    }
}

/// Separate mode-specific 1.7B QTS exports. All three GGUF paths may name one bundled file.
#[derive(Debug, Clone)]
pub struct ModeModelPaths {
    pub talker_gguf: PathBuf,
    pub code_predictor_gguf: PathBuf,
    pub tokenizer_gguf: PathBuf,
    pub vocoder_onnx: PathBuf,
}

impl ModeModelPaths {
    pub fn from_model_dir(dir: impl AsRef<Path>, mode: crate::QtsMode) -> Self {
        let dir = dir.as_ref();
        let stem = format!("qwen3-tts-1.7b-{}", mode.as_str());
        let choose = |component: &str| {
            let candidates = ["f16", "q8_0", "q6_k", "q5_k", "q4_k"]
                .map(|quant| format!("{stem}-{component}-{quant}.gguf"));
            let refs = candidates.iter().map(String::as_str).collect::<Vec<_>>();
            choose_model_file(dir, &refs)
        };
        let bundled = ["f16", "q8_0", "q6_k", "q5_k", "q4_k"]
            .map(|quant| dir.join(format!("{stem}-{quant}.gguf")))
            .into_iter()
            .find(|path| path.is_file());
        Self {
            talker_gguf: bundled.clone().unwrap_or_else(|| choose("talker")),
            code_predictor_gguf: bundled.clone().unwrap_or_else(|| choose("code-predictor")),
            tokenizer_gguf: bundled.unwrap_or_else(|| dir.join(format!("{stem}-tokenizer.gguf"))),
            vocoder_onnx: dir.join("qwen3-tts-vocoder.onnx"),
        }
    }
}

fn choose_model_file(dir: &Path, candidates: &[&str]) -> PathBuf {
    for candidate in candidates {
        let path = dir.join(candidate);
        if path.is_file() {
            return path;
        }
    }

    dir.join(candidates[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_model_dir_names() {
        let p = ModelPaths::from_model_dir("/models");
        assert!(p.main_gguf.ends_with("qwen3-tts-0.6b-f16.gguf"));
        assert!(p.vocoder_onnx.ends_with("qwen3-tts-vocoder.onnx"));
    }

    #[test]
    fn from_model_dir_prefers_existing_main_quantized_files() {
        let dir = std::env::temp_dir().join("qwen_tts_native_paths_quantized");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("qwen3-tts-0.6b-q4_k.gguf"), b"").unwrap();

        let p = ModelPaths::from_model_dir(&dir);
        assert!(p.main_gguf.ends_with("qwen3-tts-0.6b-q4_k.gguf"));
        assert!(p.vocoder_onnx.ends_with("qwen3-tts-vocoder.onnx"));

        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn mode_specific_1_7b_paths_share_vocoder() {
        for mode in [
            crate::QtsMode::Base,
            crate::QtsMode::CustomVoice,
            crate::QtsMode::VoiceDesign,
        ] {
            let paths = ModeModelPaths::from_model_dir("/missing-qts-models", mode);
            let stem = format!("qwen3-tts-1.7b-{}", mode.as_str());
            assert!(paths
                .talker_gguf
                .ends_with(format!("{stem}-talker-f16.gguf")));
            assert!(paths
                .code_predictor_gguf
                .ends_with(format!("{stem}-code-predictor-f16.gguf")));
            assert!(paths
                .tokenizer_gguf
                .ends_with(format!("{stem}-tokenizer.gguf")));
            assert!(paths.vocoder_onnx.ends_with("qwen3-tts-vocoder.onnx"));
        }
    }
    #[test]
    fn mode_paths_resolve_split_quantization_or_bundled_export() {
        let fixture = crate::model::test_fixture::TestGguf::write(&[], &[], &[]);
        let dir = fixture.0.with_extension("models");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("qwen3-tts-1.7b-customvoice-talker-q4_k.gguf"), b"").unwrap();
        std::fs::write(
            dir.join("qwen3-tts-1.7b-customvoice-code-predictor-q8_0.gguf"),
            [],
        )
        .unwrap();
        let paths = ModeModelPaths::from_model_dir(&dir, crate::QtsMode::CustomVoice);
        assert!(paths
            .talker_gguf
            .ends_with("qwen3-tts-1.7b-customvoice-talker-q4_k.gguf"));
        assert!(paths
            .code_predictor_gguf
            .ends_with("qwen3-tts-1.7b-customvoice-code-predictor-q8_0.gguf"));
        std::fs::write(dir.join("qwen3-tts-1.7b-customvoice-q8_0.gguf"), b"").unwrap();
        let paths = ModeModelPaths::from_model_dir(&dir, crate::QtsMode::CustomVoice);
        assert!(paths
            .talker_gguf
            .ends_with("qwen3-tts-1.7b-customvoice-q8_0.gguf"));
        assert_eq!(paths.talker_gguf, paths.code_predictor_gguf);
        assert_eq!(paths.talker_gguf, paths.tokenizer_gguf);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
