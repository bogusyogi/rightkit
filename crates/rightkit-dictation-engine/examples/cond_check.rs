//! Minimal raw-vs-conditioner ASR check (no sonora): for each wav, print RMS and
//! the Parakeet transcript of the RAW audio and the `condition_for_asr` output.
//!   RIGHTKIT_COREML_CU=cpuOnly cargo run --release --example cond_check -- \
//!     ../../model_registry/coreml/parakeet-tdt-v3  ~/hr-quiet-clips
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn load(path: &std::path::Path) -> Vec<f32> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    wav.samples
}

#[cfg(target_os = "macos")]
fn main() {
    use rightkit_dictation_core::audio_conditioning::{condition_for_asr, rms};
    let a: Vec<String> = std::env::args().collect();
    let model =
        rightkit_dictation_engine::coreml_asr::CoreMlParakeet::load(std::path::Path::new(&a[1]))
            .unwrap();
    let mut wavs: Vec<_> = std::fs::read_dir(&a[2])
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
        .collect();
    wavs.sort();
    for w in &wavs {
        let raw = load(w);
        if raw.is_empty() {
            continue;
        }
        let def = condition_for_asr(&raw, 16_000, "default");
        let tx = |x: &[f32]| model.transcribe(x).unwrap_or_else(|e| format!("<err {e}>"));
        println!(
            "\n=== {} ({:.1}s) ===",
            w.file_name().unwrap().to_string_lossy(),
            raw.len() as f32 / 16_000.0
        );
        println!(
            "  rms raw={:.4} default={:.4} (gain {:.1}x)",
            rms(&raw),
            rms(&def),
            rms(&def) / rms(&raw).max(1e-9)
        );
        println!("  RAW    : {}", tx(&raw));
        println!("  DEFAULT: {}", tx(&def));
    }
}
