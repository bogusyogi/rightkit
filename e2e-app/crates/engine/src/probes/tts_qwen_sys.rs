//! rightkit-tts-qwen-sys: the vendored ggml links and runs: the CPU backend initialises and the backend registry loads its devices.
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    // SAFETY: process-wide ggml initialisation with no arguments; idempotent, as rightkit-tts-qwen calls it before every model load.
    let devices = unsafe {
        ggml_sys::ggml_cpu_init();
        ggml_sys::ggml_backend_load_all();
        ggml_sys::ggml_backend_dev_count()
    };
    if devices == 0 {
        return Err("ggml registered no backend device (not even the CPU)".into());
    }
    Ok(json!({ "ggml_devices": devices }))
}
