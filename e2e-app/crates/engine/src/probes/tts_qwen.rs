//! rightkit-tts-qwen: Qwen3-TTS model resolution and validation: the conventional files are resolved in the model folder
//! (`RIGHTKIT_TTS_MODEL_DIR`, else the rightkit-tts per-user default) and the GGUF header and vocoder are validated. Synthesis is
//! rightkit-tts's probe. No model on this host is a reported failure naming the folder looked in.
use qts::{load_and_validate, ModelPaths};
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let dir = std::env::var_os("RIGHTKIT_TTS_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .ok_or("host_missing:models: needs the Qwen3-TTS model: RIGHTKIT_TTS_MODEL_DIR is not set (qwen3-tts-0.6b-*.gguf + qwen3-tts-vocoder.onnx)")?;
    let paths = ModelPaths::from_model_dir(&dir);
    if !paths.main_exists() {
        return Err(format!(
            "host_missing:models: needs the Qwen3-TTS model: no qwen3-tts-0.6b-*.gguf in {}",
            dir.display()
        ));
    }
    load_and_validate(&paths)
        .map_err(|e| format!("validate {}: {e}", paths.main_gguf.display()))?;
    Ok(json!({ "main": paths.main_gguf, "vocoder": paths.vocoder_onnx }))
}
