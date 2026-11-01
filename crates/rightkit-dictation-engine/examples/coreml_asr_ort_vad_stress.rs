//! Stress the isolated macOS ORT VAD and Core ML ASR probe runtimes concurrently.
//!
//! Usage:
//!   coreml_asr_ort_vad_stress <models_dir> <vad.onnx> <16khz-mono.wav> [iterations]

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn load_wav(path: &std::path::Path) -> Result<Vec<f32>, String> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if wav.sample_rate != 16_000_u32 {
        return Err(format!(
            "{}: expected {} Hz WAV, got {} Hz",
            path.display(),
            16_000,
            wav.sample_rate
        ));
    }
    Ok(wav.samples)
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
    use rightkit_dictation_engine::vad::SpeechVad;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    let exception = rightkit_dictation_engine::coreml::exception_bridge_smoke()?;
    println!("objective-c exception boundary: caught {exception}");

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        return Err(
            "usage: coreml_asr_ort_vad_stress <models_dir> <vad.onnx> <16khz-mono.wav> [iterations]"
                .into(),
        );
    }
    let iterations = args
        .get(4)
        .and_then(|value| value.parse().ok())
        .unwrap_or(100usize);
    let source = load_wav(std::path::Path::new(&args[3]))?;
    if source.is_empty() || iterations == 0 {
        return Err("audio and iterations must be non-empty".into());
    }
    let probe: Vec<f32> = source.iter().copied().cycle().take(48_000).collect();
    let mut asr = AsrRuntime::load(std::path::Path::new(&args[1]), AsrEp::resolve_default())?;
    asr.transcribe(&probe)?;

    let mut vad = SpeechVad::with_model_path(Some(PathBuf::from(&args[2])));
    let ready_deadline = Instant::now() + Duration::from_secs(10);
    while vad.backend_name().is_none() && Instant::now() < ready_deadline {
        let _ = vad.observe(&probe[..512]);
        std::thread::sleep(Duration::from_millis(10));
    }
    if vad.backend_name() != Some("ort") {
        return Err("ORT VAD did not become ready".into());
    }
    vad.reset();

    let gate = Arc::new(Barrier::new(2));
    let asr_gate = Arc::clone(&gate);
    std::thread::scope(|scope| -> Result<(), String> {
        let asr_thread = scope.spawn(|| -> Result<(), String> {
            asr_gate.wait();
            for iteration in 0..iterations {
                asr.transcribe(&probe)?;
                if iteration % 10 == 9 {
                    eprintln!("asr iterations={}", iteration + 1);
                }
            }
            Ok(())
        });
        gate.wait();
        for iteration in 0..iterations * 1_000 {
            let offset = (iteration * 512) % probe.len();
            let mut frame = [0.0f32; 512];
            for (index, sample) in frame.iter_mut().enumerate() {
                *sample = probe[(offset + index) % probe.len()];
            }
            let _ = vad.observe(&frame);
        }
        asr_thread
            .join()
            .map_err(|_| "ASR stress thread panicked".to_string())??;
        Ok(())
    })?;
    println!("Core ML ASR + ORT VAD stress passed: iterations={iterations}");
    Ok(())
}
