//! Replays saved recordings through the production keyword spotter the way the
//! live probe lane feeds it (a rolling 3 s snapshot at absolute stream
//! positions) and prints the ABSOLUTE wake onset the trigger cut receives.
//!
//! Usage: kws_onset_replay <kws-model-dir> <wav> [<wav>...]
//!          [--ort ONNXRUNTIME_LIB] [--step SAMPLES]
//! `--step` is the arrival size (default 320 = 20 ms); a chunk-split check runs
//! the same WAVs at several steps and expects the same onset.

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> Result<(), String> {
    use rightkit_dictation_engine::kws::Kws;
    use std::path::PathBuf;

    let mut positional = Vec::new();
    let mut ort = None;
    let mut step = 320_usize;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--ort") => ort = Some(PathBuf::from(args.next().ok_or("--ort needs a path")?)),
            Some("--step") => {
                step = args
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|step| *step > 0)
                    .ok_or("--step needs a positive sample count")?
            }
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    let mut positional = positional.into_iter();
    let model = positional.next().ok_or(
        "usage: kws_onset_replay <model-dir> <wav>... [--ort ONNXRUNTIME_LIB] [--step SAMPLES]",
    )?;

    // The spotter runs on the runtime rightkit-onnx binds.
    let src_tauri = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri");
    #[cfg(target_os = "windows")]
    let default_ort = Some(src_tauri.join("resources/runtime/onnxruntime.dll"));
    #[cfg(target_os = "macos")]
    let default_ort = std::env::var_os("ORT_DYLIB_PATH")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            src_tauri.join("resources/runtime/libonnxruntime.dylib"),
            src_tauri.join("native-sources/libonnxruntime.dylib"),
        ])
        .find(|path| path.is_file());
    if let Some(runtime) = ort.or(default_ort) {
        rightkit_onnx::configure_runtime_with_options(
            [rightkit_onnx::RuntimeCandidate::new(
                runtime,
                rightkit_onnx::CandidateSource::ExplicitFile,
            )],
            &rightkit_onnx::RuntimeBindingOptions {
                force_replace: true,
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
    }
    for wav in positional {
        let wav_audio = rightkit_audio::wav::read_wav_mono_f32(&wav).map_err(|e| e.to_string())?;
        if wav_audio.sample_rate != 16_000 {
            return Err(format!("{}: expected 16 kHz mono", wav.display()));
        }
        let audio: Vec<f32> = wav_audio.samples;
        // The onset does not depend on the feed step (checked at 160/320/800/1600:
        // identical onset, only the firing moment moves); one step keeps one row per file.
        let mut kws = Kws::load(&model)?;
        let mut fired = None;
        let mut total = step;
        while total <= audio.len() {
            let start = total.saturating_sub(48_000);
            let (result, wake_start) = kws.transcribe_cascade_result_with_wake_start(
                &audio[start..total],
                start,
                total,
                false,
            )?;
            if !result.text.is_empty() {
                let token_onset = result
                    .tokens
                    .first()
                    .map(|t| start + (t.start * 16_000.0).round() as usize);
                if result.text == "zephyr" {
                    if fired.is_none() {
                        fired = Some((total, result.text.clone(), token_onset));
                    }
                } else {
                    fired = Some((total, result.text, wake_start.or(token_onset)));
                    break;
                }
            }
            total += step;
        }
        match &fired {
            // TSV: path, onset, fired_at, samples, text (complete commands only)
            Some((at, text, Some(onset))) if text != "zephyr" => println!(
                "KWS\t{}\t{onset}\t{at}\t{}\t{text}\tstep={step}",
                wav.display(),
                audio.len()
            ),
            Some(_) => {}
            None => {}
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {}
