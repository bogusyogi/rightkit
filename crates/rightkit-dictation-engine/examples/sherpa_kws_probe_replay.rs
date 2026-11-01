#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> Result<(), String> {
    use std::path::PathBuf;
    use std::time::Instant;

    use rightkit_text_polish::text_pipeline::parse_control_command;

    let usage = "usage: sherpa_kws_probe_replay <model-dir> <wav> [<wav>...]";
    let mut args = std::env::args_os().skip(1);
    let model = args.next().map(PathBuf::from).ok_or(usage)?;
    let wavs = args.map(PathBuf::from).collect::<Vec<_>>();
    if wavs.is_empty() {
        return Err(usage.into());
    }
    #[cfg(target_os = "windows")]
    let _ort = unsafe {
        libloading::Library::new(
            model
                .parent()
                .ok_or("model directory has no resource parent")?
                .join("runtime/onnxruntime.dll"),
        )
    }
    .map_err(|e| format!("preload bundled ONNX Runtime: {e}"))?;
    for wav in wavs {
        let wav_audio = rightkit_audio::wav::read_wav_mono_f32(&wav).map_err(|e| e.to_string())?;
        if wav_audio.sample_rate != 16_000 {
            return Err(format!(
                "{}: expected 16k wav, got {}Hz",
                wav.display(),
                wav_audio.sample_rate
            ));
        }
        let audio = wav_audio.samples;
        let mut probe = rightkit_dictation_engine::sherpa_kws::SherpaKws::load(&model)?;
        let started = Instant::now();
        let mut result = None;
        for chunk in audio.chunks(1_600) {
            let current = probe.transcribe_result(chunk)?;
            if !current.text.is_empty() {
                result = Some(current);
                break;
            }
        }
        let result = result.ok_or_else(|| format!("no keyword: {}", wav.display()))?;
        println!("clip={}", wav.display());
        println!("text={}", result.text);
        println!("control={}", parse_control_command(&result.text).is_some());
        for token in result.tokens {
            println!(
                "token={:?}\tstart={:.3}\tend={:.3}",
                token.text, token.start, token.end
            );
        }
        println!("latency_ms={}", started.elapsed().as_millis());
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    panic!("macOS and Windows only");
}
